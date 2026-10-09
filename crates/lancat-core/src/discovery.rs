//! Discovery: event-triggered UDP probing plus mDNS (`_lancat._tcp.local.`) on physical LANs.
//!
//! Nothing is sent periodically while peers are known. A *probe burst* ("who's there?", sent at
//! 0 s, 1 s and 3 s, then silence) is triggered by:
//! * app start,
//! * a network change (an IPv4 interface/address appeared or went away; polled every 5 s with
//!   `getifaddrs`, which is a single cheap syscall and sends nothing),
//! * a wake from sleep (wall clock jumped ahead of the monotonic clock, or a watcher tick was late),
//! * a manual refresh ([`Node::refresh_peers`]).
//!
//! Each round probes every private IPv4 interface, virtual LANs included: the directed broadcast
//! of each broadcast-capable interface, plus unicast to every online Tailscale peer
//! (`tailscale status --json`), to the last-known address of every remembered device and to
//! manual peers. A receiver adds the prober and answers by unicast with its own device info,
//! so one exchange teaches both sides about each other. Liveness afterwards is the per-peer
//! WebSocket link (ping every 30 s, see `link.rs`), not broadcast silence.
//!
//! A clean exit sends `bye` (UDP unicast + over the link) so peers mark us offline at once.
//! While no peer at all is online, one probe round goes out every 3 minutes as a fallback.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent, ServiceInfo};
use socket2::{Domain, Protocol, Socket, Type};

use crate::netutil;
use crate::node::Node;
use crate::protocol::*;

pub(crate) struct Mdns {
    daemon: ServiceDaemon,
    fullname: Option<String>,
}

pub(crate) fn start(node: Arc<Node>) {
    if node.cfg.enable_mdns {
        match ServiceDaemon::new() {
            Ok(daemon) => {
                // mDNS only on physical LANs: overlay VPNs (Tailscale, WireGuard ...) carry no
                // multicast and container bridges have nobody to find; UDP probing covers those.
                if let Ok(list) = if_addrs::get_if_addrs() {
                    for i in list {
                        if netutil::is_virtual_iface(&i.name) {
                            let _ = daemon.disable_interface(IfKind::Name(i.name.clone()));
                        }
                    }
                }
                let _ = daemon.disable_interface(IfKind::IPv6);
                *node.mdns.lock().unwrap() = Some(Mdns { daemon, fullname: None });
                announce(&node);
                browse(node.clone());
            }
            Err(e) => log::warn!("mDNS unavailable: {e}"),
        }
    }
    if node.cfg.enable_udp {
        match udp_socket(node.cfg.udp_port) {
            Ok(sock) => {
                let sock = Arc::new(sock);
                let _ = node.udp.set(sock.clone());
                tokio::spawn(udp_recv_loop(node.clone(), sock.clone()));
                tokio::spawn(probe_loop(node.clone(), sock));
                tokio::spawn(netwatch_loop(node.clone()));
                node.kick_probe("startup");
            }
            Err(e) => log::warn!("UDP discovery unavailable: {e}"),
        }
    }
}

fn announce(node: &Node) {
    let me = node.me();
    let mut guard = node.mdns.lock().unwrap();
    let Some(m) = guard.as_mut() else { return };
    let mut props = HashMap::new();
    props.insert("id".to_string(), me.id.clone());
    props.insert("name".to_string(), me.name.clone());
    props.insert("os".to_string(), me.os.clone());
    props.insert("ver".to_string(), me.ver.clone());
    let short: String = me.id.chars().filter(|c| c.is_ascii_alphanumeric()).take(12).collect();
    let instance = format!("lancat-{short}");
    let host = format!("lancat-{short}.local.");
    match ServiceInfo::new(SERVICE_TYPE, &instance, &host, (), me.port, props) {
        Ok(info) => {
            let info = info.enable_addr_auto();
            let fullname = info.get_fullname().to_string();
            if let Err(e) = m.daemon.register(info) {
                log::warn!("mDNS register failed: {e}");
            } else {
                m.fullname = Some(fullname);
            }
        }
        Err(e) => log::warn!("mDNS service info: {e}"),
    }
}

/// Re-registers after a rename.
pub(crate) fn reannounce(node: &Node) {
    {
        let guard = node.mdns.lock().unwrap();
        if let Some(m) = guard.as_ref() {
            if let Some(f) = &m.fullname {
                let _ = m.daemon.unregister(f);
            }
        }
    }
    announce(node);
}

pub(crate) fn stop(node: &Node) {
    if let Some(m) = node.mdns.lock().unwrap().take() {
        if let Some(f) = &m.fullname {
            let _ = m.daemon.unregister(f);
        }
        let _ = m.daemon.shutdown();
    }
}

fn browse(node: Arc<Node>) {
    let rx = {
        let guard = node.mdns.lock().unwrap();
        match guard.as_ref().map(|m| m.daemon.browse(SERVICE_TYPE)) {
            Some(Ok(rx)) => rx,
            Some(Err(e)) => {
                log::warn!("mDNS browse failed: {e}");
                return;
            }
            None => return,
        }
    };
    let mut stop = node.shutdown_rx();
    tokio::spawn(async move {
        loop {
            let ev = tokio::select! {
                ev = rx.recv_async() => match ev { Ok(ev) => ev, Err(_) => break },
                _ = stop.changed() => break,
            };
            if let ServiceEvent::ServiceResolved(svc) = ev {
                let get = |k: &str| svc.get_property_val_str(k).unwrap_or("").to_string();
                let info = DeviceInfo { id: get("id"), name: get("name"), os: get("os"), ver: get("ver"), port: svc.get_port() };
                if info.id.is_empty() || info.id == node.me().id || node.store.is_blocked(&info.id) {
                    continue;
                }
                // prefer a private IPv4 address
                let mut addrs: Vec<IpAddr> = svc.get_addresses().iter().map(|a| a.to_ip_addr()).filter(netutil::is_private).collect();
                addrs.sort_by_key(|a| (!a.is_ipv4(), a.is_loopback()));
                if let Some(ip) = addrs.first() {
                    if !node.is_blocked_addr(ip) {
                        node.peer_seen(info, *ip);
                    }
                }
            }
        }
    });
}

fn udp_socket(port: u16) -> std::io::Result<tokio::net::UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    s.set_reuse_port(true)?;
    s.set_broadcast(true)?;
    s.set_nonblocking(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
    tokio::net::UdpSocket::from_std(s.into())
}

fn beacon(node: &Node, op: BeaconOp) -> Vec<u8> {
    serde_json::to_vec(&Beacon { lancat: 1, op, device: node.me() }).unwrap_or_default()
}

/// Waits for kicks; runs a 3-round burst per kick, and the idle fallback round.
async fn probe_loop(node: Arc<Node>, sock: Arc<tokio::net::UdpSocket>) {
    let mut stop = node.shutdown_rx();
    // (burst start, index of the next round)
    let mut burst: Option<(tokio::time::Instant, usize)> = None;
    let mut idle_next = tokio::time::Instant::now() + Duration::from_secs(IDLE_PROBE_SECS);
    let mut ts_cache: Vec<Ipv4Addr> = vec![];
    loop {
        let deadline = match burst {
            Some((t0, i)) => t0 + Duration::from_secs(PROBE_BURST_SECS[i]),
            None => idle_next,
        };
        tokio::select! {
            _ = node.probe_kick.notified() => {
                // a new trigger restarts the schedule (several triggers at once = one burst)
                burst = Some((tokio::time::Instant::now(), 0));
                continue;
            }
            _ = tokio::time::sleep_until(deadline) => {}
            _ = stop.changed() => break,
        }
        match burst {
            Some((t0, i)) => {
                probe_round(&node, &sock, i == 0, &mut ts_cache).await;
                burst = if i + 1 < PROBE_BURST_SECS.len() { Some((t0, i + 1)) } else { None };
                idle_next = tokio::time::Instant::now() + Duration::from_secs(IDLE_PROBE_SECS);
            }
            None => {
                if node.online_count() == 0 {
                    log::debug!("no peer online: idle fallback probe");
                    probe_round(&node, &sock, true, &mut ts_cache).await;
                }
                idle_next = tokio::time::Instant::now() + Duration::from_secs(IDLE_PROBE_SECS);
            }
        }
    }
}

/// Where one probe round goes. Split out so it can be logged and tested.
#[derive(Debug, Default)]
pub(crate) struct ProbeTargets {
    pub broadcast: BTreeSet<Ipv4Addr>,
    pub tailscale: BTreeSet<Ipv4Addr>,
    pub known: BTreeSet<SocketAddr>,
}

impl ProbeTargets {
    fn all(&self, port: u16) -> BTreeSet<SocketAddr> {
        let mut v: BTreeSet<SocketAddr> = self.broadcast.iter().map(|b| SocketAddr::from((*b, port))).collect();
        v.extend(self.tailscale.iter().map(|t| SocketAddr::from((*t, port))));
        v.extend(self.known.iter().copied());
        v
    }
}

async fn probe_round(node: &Arc<Node>, sock: &tokio::net::UdpSocket, first: bool, ts_cache: &mut Vec<Ipv4Addr>) {
    let port = node.cfg.udp_port;
    let ifaces = netutil::probe_ifaces();
    let mut t = ProbeTargets::default();
    for i in &ifaces {
        if let Some(b) = i.broadcast {
            t.broadcast.insert(b);
        }
    }
    if ifaces.iter().any(|i| i.broadcast.is_some()) {
        t.broadcast.insert(Ipv4Addr::BROADCAST);
    }
    // point-to-point overlays: enumerate peers. Only Tailscale exposes a peer list without root.
    if first {
        *ts_cache = if ifaces.iter().any(|i| i.is_tailscale()) { netutil::tailscale_peers().await } else { vec![] };
    }
    let mine: BTreeSet<Ipv4Addr> = ifaces.iter().map(|i| i.ip).collect();
    t.tailscale.extend(ts_cache.iter().filter(|ip| !mine.contains(ip)));
    // last-known addresses of remembered devices that are not online right now
    for d in node.store.devices().unwrap_or_default() {
        if d.blocked || node.is_online(&d.id) {
            continue;
        }
        if let Ok(IpAddr::V4(ip)) = d.last_addr.parse::<IpAddr>() {
            if netutil::is_private(&IpAddr::V4(ip)) && !ip.is_loopback() && !mine.contains(&ip) {
                t.known.insert(SocketAddr::from((ip, port)));
            }
        }
    }
    let manual = node.store.settings().manual_peers;
    for a in &manual {
        let ip = a.parse::<SocketAddr>().map(|s| s.ip()).or_else(|_| a.trim().parse::<IpAddr>());
        if let Ok(IpAddr::V4(ip)) = ip {
            t.known.insert(SocketAddr::from((ip, port)));
        }
    }
    t.known.extend(node.cfg.extra_probe_targets.iter().copied());

    if first {
        let ifs: Vec<String> = ifaces.iter().map(|i| format!("{}={}/{}", i.name, i.ip, i.prefix)).collect();
        log::info!(
            "probe: ifaces [{}] broadcast {:?} tailscale-unicast {:?} last-known/manual {:?}",
            ifs.join(" "),
            t.broadcast,
            t.tailscale,
            t.known
        );
        // manual peers also get an HTTP /info probe (UDP may be filtered on their network)
        for a in manual {
            let node = node.clone();
            tokio::spawn(async move {
                if let Err(e) = node.add_peer_addr(&a).await {
                    log::debug!("manual peer {a}: {e}");
                }
            });
        }
    }
    let payload = beacon(node, BeaconOp::Probe);
    for to in t.all(port) {
        if let Err(e) = sock.send_to(&payload, to).await {
            log::debug!("probe to {to}: {e}");
        }
    }
}

/// Probes one address right away (e.g. a peer whose link just dropped).
pub(crate) fn probe_addr(node: &Node, ip: IpAddr) {
    if let Some(sock) = node.udp.get() {
        let _ = sock.try_send_to(&beacon(node, BeaconOp::Probe), SocketAddr::new(ip, node.cfg.udp_port));
    }
}

/// Clean exit: unicast `bye` to every online peer (synchronous, so it goes out before the process ends).
pub(crate) fn goodbye(node: &Node) {
    let Some(sock) = node.udp.get() else { return };
    let payload = beacon(node, BeaconOp::Bye);
    let addrs: Vec<IpAddr> = node.peers.read().unwrap().values().filter(|p| p.online).map(|p| p.addr).collect();
    let mut targets: BTreeSet<SocketAddr> = addrs.into_iter().map(|ip| SocketAddr::new(ip, node.cfg.udp_port)).collect();
    targets.extend(node.cfg.extra_probe_targets.iter().copied());
    for to in targets {
        let _ = sock.try_send_to(&payload, to);
    }
}

/// Watches for network changes and wake-ups. Sends nothing itself; it only kicks a probe burst.
async fn netwatch_loop(node: Arc<Node>) {
    let mut stop = node.shutdown_rx();
    let snapshot = || -> BTreeSet<(String, Ipv4Addr)> { netutil::probe_ifaces().into_iter().map(|i| (i.name, i.ip)).collect() };
    let mut last = snapshot();
    let period = Duration::from_secs(NETWATCH_SECS);
    let mut tick = tokio::time::interval(period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    let mut mono = Instant::now();
    let mut wall = SystemTime::now();
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = stop.changed() => break,
        }
        let dm = mono.elapsed();
        let dw = wall.elapsed().unwrap_or_default();
        mono = Instant::now();
        wall = SystemTime::now();
        // Linux/macOS: the monotonic clock stops during suspend, the wall clock does not.
        // Windows: the tick itself arrives late.
        if dw > dm + Duration::from_secs(10) || dm > period * 4 {
            node.kick_probe(&format!("wake from sleep (wall +{}s, mono +{}s)", dw.as_secs(), dm.as_secs()));
            last = snapshot();
            continue;
        }
        let now = snapshot();
        if now != last {
            let added: Vec<String> = now.difference(&last).map(|(n, ip)| format!("+{n}={ip}")).collect();
            let removed: Vec<String> = last.difference(&now).map(|(n, ip)| format!("-{n}={ip}")).collect();
            last = now;
            node.kick_probe(&format!("network change {} {}", added.join(" "), removed.join(" ")));
        }
    }
}

async fn udp_recv_loop(node: Arc<Node>, sock: Arc<tokio::net::UdpSocket>) {
    let mut stop = node.shutdown_rx();
    let mut buf = vec![0u8; 2048];
    // answer each (prober, source) at most once per 500 ms: one probe round reaches us
    // several times (subnet broadcast + 255.255.255.255 + unicast)
    let mut answered: HashMap<(String, SocketAddr), Instant> = HashMap::new();
    loop {
        let (n, from) = tokio::select! {
            r = sock.recv_from(&mut buf) => match r { Ok(v) => v, Err(_) => { tokio::time::sleep(Duration::from_millis(200)).await; continue } },
            _ = stop.changed() => break,
        };
        if !netutil::is_private(&from.ip()) || node.is_blocked_addr(&from.ip()) {
            continue;
        }
        let Ok(b) = serde_json::from_slice::<Beacon>(&buf[..n]) else { continue };
        if b.lancat != 1 || b.device.id.is_empty() || b.device.id == node.me().id || node.store.is_blocked(&b.device.id) {
            continue;
        }
        match b.op {
            BeaconOp::Probe => {
                let key = (b.device.id.clone(), from);
                let now = Instant::now();
                answered.retain(|_, t| now.duration_since(*t) < Duration::from_secs(5));
                let fresh = answered.get(&key).is_none_or(|t| now.duration_since(*t) > Duration::from_millis(500));
                if fresh {
                    answered.insert(key, now);
                    log::debug!("probe from {} ({}) at {from}: replying", b.device.name, b.device.id);
                    let _ = sock.send_to(&beacon(&node, BeaconOp::Reply), from).await;
                }
                node.peer_seen(b.device, from.ip());
            }
            BeaconOp::Reply | BeaconOp::Announce => node.peer_seen(b.device, from.ip()),
            BeaconOp::Bye => node.peer_bye(&b.device.id, "udp bye"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_merge() {
        let mut t = ProbeTargets::default();
        t.broadcast.insert("192.168.1.255".parse().unwrap());
        t.tailscale.insert("100.101.1.2".parse().unwrap());
        t.known.insert("192.168.1.9:47820".parse().unwrap());
        t.known.insert("100.101.1.2:47820".parse().unwrap());
        let all = t.all(47820);
        assert_eq!(all.len(), 3, "{all:?}");
    }
}
