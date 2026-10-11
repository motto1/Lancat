//! Pausing, resuming and cancelling file transfers between two loopback nodes.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use lancat_core::{CoreEvent, Node, NodeConfig};
use tokio::sync::broadcast::Receiver;

async fn start(dir: &Path, name: &str) -> Arc<Node> {
    let mut cfg = NodeConfig::new(dir.join("data"));
    cfg.port = 0;
    cfg.enable_mdns = false;
    cfg.enable_udp = false;
    cfg.name_override = Some(name.into());
    cfg.recv_dir_override = Some(dir.join("recv"));
    Node::start(cfg).await.expect("start node")
}

async fn status_is(rx: &mut Receiver<CoreEvent>, id: &str, want: &[&str]) -> String {
    let fut = async {
        loop {
            match rx.recv().await {
                Ok(CoreEvent::MessageUpdate { message }) if message.id == id && want.contains(&message.status.as_str()) => return message.status,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("channel closed: {e}"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(60), fut).await.expect("timed out waiting for status")
}

async fn eventually(mut f: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("condition never became true");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_resume_and_cancel() {
    let ta = tempfile::tempdir().unwrap();
    let tb = tempfile::tempdir().unwrap();
    let a = start(ta.path(), "alpha").await;
    let b = start(tb.path(), "bravo").await;
    let mut ea = a.subscribe();
    let pb = a.add_peer_addr(&format!("127.0.0.1:{}", b.port())).await.unwrap();
    let b_id = pb.id.clone();
    let a_id = a.me().id;
    eventually(|| b.is_online(&a_id)).await;

    let src = ta.path().join("big.bin");
    let data: Vec<u8> = (0..(48 * 1024 * 1024u32)).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    std::fs::write(&src, &data).unwrap();

    // ---- pause right away, then resume to completion
    let id = a.send_files(&[b_id.clone()], &[src.clone()]).await.unwrap()[0].id.clone();
    a.pause_transfer(&id).unwrap();
    eventually(|| a.store.message(&id).unwrap().unwrap().status == "paused").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.store.message(&id).unwrap().unwrap().status, "paused", "a paused transfer stays paused");
    a.retry_transfer(&id).await.unwrap();
    status_is(&mut ea, &id, &["done", "failed"]).await;
    assert_eq!(a.store.message(&id).unwrap().unwrap().status, "done");
    let got = b.store.message(&id).unwrap().unwrap();
    assert_eq!(got.status, "done");
    assert_eq!(std::fs::read(got.body["path"].as_str().unwrap()).unwrap(), data);

    // ---- sender cancels: both sides end up cancelled (or the receiver never heard of it)
    let id = a.send_files(&[b_id.clone()], &[src.clone()]).await.unwrap()[0].id.clone();
    tokio::time::sleep(Duration::from_millis(150)).await;
    a.cancel_transfer(&id).unwrap();
    eventually(|| a.store.message(&id).unwrap().unwrap().status == "cancelled").await;
    eventually(|| b.store.message(&id).unwrap().map_or(true, |m| m.status == "cancelled")).await;
    // a cancelled transfer is not resumed on reconnect
    a.retry_transfer(&id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(a.store.message(&id).unwrap().unwrap().status, "cancelled");

    // ---- receiver cancels while receiving: the sender stops
    let id = a.send_files(&[b_id.clone()], &[src.clone()]).await.unwrap()[0].id.clone();
    eventually(|| b.store.message(&id).unwrap().is_some_and(|m| m.status == "receiving")).await;
    b.cancel_transfer(&id).unwrap();
    let st = status_is(&mut ea, &id, &["cancelled", "done", "failed"]).await;
    // loopback may finish before the cancel lands; anything but a failure is fine
    assert_ne!(st, "failed", "{:?}", a.store.message(&id).unwrap());
    if st == "cancelled" {
        assert_eq!(b.store.message(&id).unwrap().unwrap().status, "cancelled");
        let part_dir = tb.path().join("recv").join(".lancat-part");
        eventually(|| std::fs::read_dir(&part_dir).map_or(true, |mut d| d.next().is_none())).await;
    }
}
