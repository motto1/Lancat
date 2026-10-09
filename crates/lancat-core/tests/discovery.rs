//! Event-triggered discovery on loopback: probe bursts (3 rounds then silence), unicast reply,
//! both sides learning each other from one exchange, and goodbye on clean exit.
//! Nodes get their own UDP ports and reach each other through `extra_probe_targets`.

use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lancat_core::protocol::{Beacon, BeaconOp, DeviceInfo};
use lancat_core::{Node, NodeConfig};

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn start(dir: &Path, name: &str, udp_port: u16, probe: Vec<SocketAddr>) -> Arc<Node> {
    let mut cfg = NodeConfig::new(dir.join("data"));
    cfg.port = 0;
    cfg.udp_port = udp_port;
    cfg.enable_mdns = false;
    cfg.enable_udp = true;
    cfg.name_override = Some(name.into());
    cfg.recv_dir_override = Some(dir.join("recv"));
    cfg.extra_probe_targets = probe;
    Node::start(cfg).await.expect("start node")
}

async fn until(limit: Duration, mut f: impl FnMut() -> bool) -> Option<Duration> {
    let t0 = Instant::now();
    while t0.elapsed() < limit {
        if f() {
            return Some(t0.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

fn recv_beacons(sock: &UdpSocket, window: Duration) -> Vec<(Beacon, SocketAddr)> {
    let mut out = vec![];
    let mut buf = [0u8; 2048];
    let t0 = Instant::now();
    while t0.elapsed() < window {
        sock.set_read_timeout(Some((window - t0.elapsed()).max(Duration::from_millis(1)))).unwrap();
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                if let Ok(b) = serde_json::from_slice::<Beacon>(&buf[..n]) {
                    out.push((b, from));
                }
            }
            Err(_) => break,
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_reply_both_sides_learn_and_goodbye() {
    let ta = tempfile::tempdir().unwrap();
    let tb = tempfile::tempdir().unwrap();
    let (pa, pb) = (free_udp_port(), free_udp_port());
    let a = start(ta.path(), "alpha", pa, vec![]).await;
    // let A's own startup burst pass: A knows nobody, B is not there yet
    tokio::time::sleep(Duration::from_millis(200)).await;
    let t0 = Instant::now();
    let b = start(tb.path(), "bravo", pb, vec![SocketAddr::from(([127, 0, 0, 1], pa))]).await;
    let (a_id, b_id) = (a.me().id, b.me().id);

    // B's first probe reaches A: A adds B, and B learns A from A's unicast reply
    let both = until(Duration::from_secs(3), || a.is_online(&b_id) && b.is_online(&a_id)).await;
    let took = t0.elapsed();
    assert!(both.is_some(), "both sides should know each other");
    assert!(took < Duration::from_millis(1500), "discovery took {took:?}");
    eprintln!("mutual discovery after {took:?}");
    let pv = a.peers().into_iter().find(|p| p.id == b_id).unwrap();
    assert_eq!(pv.name, "bravo");
    assert_eq!(pv.port, b.port());

    // still online a while later (link up, no heartbeat needed)
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(a.is_online(&b_id) && b.is_online(&a_id));

    // clean exit: A marks B offline right away
    let t1 = Instant::now();
    b.shutdown();
    let gone = until(Duration::from_secs(2), || !a.is_online(&b_id)).await;
    assert!(gone.is_some(), "goodbye should mark B offline immediately");
    eprintln!("offline after goodbye: {:?}", t1.elapsed());
    assert!(!a.peers().into_iter().find(|p| p.id == b_id).unwrap().online);
    a.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn burst_is_three_probes_then_silence_and_refresh_repeats_it() {
    let ta = tempfile::tempdir().unwrap();
    let watcher = UdpSocket::bind("127.0.0.1:0").unwrap();
    let w = watcher.local_addr().unwrap();
    let a = start(ta.path(), "alpha", free_udp_port(), vec![w]).await;

    // startup burst: rounds at 0 s, 1 s, 3 s, then nothing
    let got = tokio::task::spawn_blocking({
        let s = watcher.try_clone().unwrap();
        move || recv_beacons(&s, Duration::from_millis(6500))
    })
    .await
    .unwrap();
    assert_eq!(got.len(), 3, "startup burst: {} probes", got.len());
    assert!(got.iter().all(|(b, _)| b.op == BeaconOp::Probe && b.device.id == a.me().id));

    // idle: no further traffic
    let idle = tokio::task::spawn_blocking({
        let s = watcher.try_clone().unwrap();
        move || recv_beacons(&s, Duration::from_secs(4))
    })
    .await
    .unwrap();
    assert!(idle.is_empty(), "idle node sent {} datagrams", idle.len());

    // manual refresh: another burst of 3
    a.refresh_peers();
    let again = tokio::task::spawn_blocking({
        let s = watcher.try_clone().unwrap();
        move || recv_beacons(&s, Duration::from_millis(4500))
    })
    .await
    .unwrap();
    assert_eq!(again.len(), 3, "refresh burst: {} probes", again.len());
    a.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_gets_unicast_reply_and_bye_marks_offline() {
    let ta = tempfile::tempdir().unwrap();
    let pa = free_udp_port();
    let a = start(ta.path(), "alpha", pa, vec![]).await;
    let fake = UdpSocket::bind("127.0.0.1:0").unwrap();
    let dev = DeviceInfo { id: "zzzz-fake-peer".into(), name: "fake".into(), os: "linux".into(), ver: "0".into(), port: 1 };
    let send = |op| {
        let b = Beacon { lancat: 1, op, device: dev.clone() };
        fake.send_to(&serde_json::to_vec(&b).unwrap(), ("127.0.0.1", pa)).unwrap();
    };
    // one probe round can reach a node twice (subnet broadcast + 255.255.255.255): answered once
    send(BeaconOp::Probe);
    send(BeaconOp::Probe);
    let replies = recv_beacons(&fake, Duration::from_millis(800));
    assert_eq!(replies.len(), 1, "exactly one unicast reply");
    assert_eq!(replies[0].0.op, BeaconOp::Reply);
    assert_eq!(replies[0].0.device.id, a.me().id);
    assert!(until(Duration::from_secs(1), || a.is_online(&dev.id)).await.is_some(), "prober added");

    send(BeaconOp::Bye);
    assert!(until(Duration::from_secs(1), || !a.is_online(&dev.id)).await.is_some(), "bye marks offline");
    a.shutdown();
}
