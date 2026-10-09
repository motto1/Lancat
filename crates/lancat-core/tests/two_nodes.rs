//! Two Lancat nodes in one process on loopback (different ports, discovery off):
//! text both ways, emoji, typing, a multi-chunk file, rename on collision, folders, blacklist.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use lancat_core::{CoreEvent, EmojiPayload, Node, NodeConfig};
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

async fn wait_for<F: FnMut(&CoreEvent) -> bool>(rx: &mut Receiver<CoreEvent>, mut f: F) -> CoreEvent {
    let fut = async {
        loop {
            match rx.recv().await {
                Ok(ev) if f(&ev) => return ev,
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("channel closed: {e}"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(30), fut).await.expect("timed out waiting for event")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_nodes_exchange_text_and_files() {
    let ta = tempfile::tempdir().unwrap();
    let tb = tempfile::tempdir().unwrap();
    let a = start(ta.path(), "alpha").await;
    let b = start(tb.path(), "bravo").await;
    let mut ea = a.subscribe();
    let mut eb = b.subscribe();

    // A learns B by address; B learns A from the link/hello.
    let pb = a.add_peer_addr(&format!("127.0.0.1:{}", b.port())).await.unwrap();
    assert_eq!(pb.name, "bravo");
    assert!(pb.online);
    let a_id = a.me().id;
    let b_id = b.me().id;

    // wait until B knows A (dial from the smaller id, or the first message)
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        while !b.is_online(&a_id) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;

    // ---- text A -> B, acked
    let m = a.send_text(&b_id, "你好，bravo 👋").await.unwrap();
    let ev = wait_for(&mut eb, |e| matches!(e, CoreEvent::Message { message } if message.id == m.id)).await;
    if let CoreEvent::Message { message } = ev {
        assert!(!message.outgoing);
        assert_eq!(message.peer_id, a_id);
        assert_eq!(message.body["text"], "你好，bravo 👋");
    }
    if m.status != "delivered" {
        wait_for(&mut ea, |e| matches!(e, CoreEvent::MessageUpdate { message } if message.id == m.id && message.status == "delivered")).await;
    }
    assert_eq!(b.peers().iter().find(|p| p.id == a_id).unwrap().unread, 1);
    b.mark_read(&a_id).unwrap();
    assert_eq!(b.peers().iter().find(|p| p.id == a_id).unwrap().unread, 0);

    // ---- text B -> A
    let m2 = b.send_text(&a_id, "hi alpha").await.unwrap();
    wait_for(&mut ea, |e| matches!(e, CoreEvent::Message { message } if message.id == m2.id)).await;

    // ---- emoji A -> B
    let em = a
        .send_emoji(&b_id, EmojiPayload { kind: "noto".into(), code: "1f600".into(), alt: "😀".into(), data: None })
        .await
        .unwrap();
    wait_for(&mut eb, |e| matches!(e, CoreEvent::Message { message } if message.id == em.id && message.kind == "emoji")).await;

    // ---- file A -> B: 9 MiB + 123 bytes => 3 chunks
    let src = ta.path().join("big.bin");
    let data: Vec<u8> = (0..(9 * 1024 * 1024 + 123u32)).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    std::fs::write(&src, &data).unwrap();
    let sent = a.send_files(&[b_id.clone()], &[src.clone()]).await.unwrap();
    assert_eq!(sent.len(), 1);
    let fid = sent[0].id.clone();
    wait_for(&mut ea, |e| matches!(e, CoreEvent::MessageUpdate { message } if message.id == fid && (message.status == "done" || message.status == "failed"))).await;
    assert_eq!(a.store.message(&fid).unwrap().unwrap().status, "done", "{:?}", a.store.message(&fid).unwrap());
    let recv = b.store.message(&fid).unwrap().expect("receiver has the file message");
    assert_eq!(recv.status, "done");
    let saved = recv.body["path"].as_str().unwrap().to_string();
    assert_eq!(std::fs::read(&saved).unwrap(), data);
    assert_eq!(Path::new(&saved), tb.path().join("recv").join("big.bin"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&saved).unwrap().permissions().mode() & 0o111, 0, "never executable");
    }

    // ---- same name again -> renamed, plus a folder and an empty file
    let folder = ta.path().join("album");
    std::fs::create_dir_all(folder.join("sub")).unwrap();
    std::fs::write(folder.join("a.txt"), "aaa").unwrap();
    std::fs::write(folder.join("sub").join("empty.txt"), "").unwrap();
    let sent = a.send_files(&[b_id.clone()], &[src.clone(), folder.clone()]).await.unwrap();
    assert_eq!(sent.len(), 3);
    for m in &sent {
        let id = m.id.clone();
        if a.store.message(&id).unwrap().unwrap().status != "done" {
            wait_for(&mut ea, |e| matches!(e, CoreEvent::MessageUpdate { message } if message.id == id && (message.status == "done" || message.status == "failed"))).await;
        }
        assert_eq!(a.store.message(&id).unwrap().unwrap().status, "done");
    }
    let r = tb.path().join("recv");
    assert!(r.join("big (1).bin").exists());
    assert_eq!(std::fs::read_to_string(r.join("album").join("a.txt")).unwrap(), "aaa");
    assert!(r.join("album").join("sub").join("empty.txt").exists());

    // ---- history
    let hist = b.messages(&a_id, None, 100).unwrap();
    assert!(hist.len() >= 6, "history has {}", hist.len());
    b.clear_history(None).unwrap();
    assert!(b.messages(&a_id, None, 100).unwrap().is_empty());

    // ---- blacklist: B blocks A, A's message is not stored
    b.set_blocked(&a_id, true).unwrap();
    let blocked = a.send_text(&b_id, "should be dropped").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(b.store.message(&blocked.id).unwrap().is_none());

    a.shutdown();
    b.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_messages_are_queued_and_flushed() {
    let ta = tempfile::tempdir().unwrap();
    let tb = tempfile::tempdir().unwrap();
    let a = start(ta.path(), "a").await;
    let b = start(tb.path(), "b").await;
    a.add_peer_addr(&format!("127.0.0.1:{}", b.port())).await.unwrap();
    let b_id = b.me().id;
    let b_port = b.port();
    b.shutdown();
    drop(b);
    // B left with a goodbye, so A already has it offline; verify that a send to an
    // unreachable peer stays pending and is delivered when B comes back on the same port.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let m = a.send_text(&b_id, "queued").await.unwrap();
    assert_ne!(m.status, "delivered");

    // restart B with the same data dir (same identity) and port
    let mut cfg = NodeConfig::new(tb.path().join("data"));
    cfg.port = b_port;
    cfg.enable_mdns = false;
    cfg.enable_udp = false;
    cfg.recv_dir_override = Some(tb.path().join("recv"));
    let b2 = loop {
        match Node::start(cfg.clone()).await {
            Ok(n) => break n,
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    };
    assert_eq!(b2.me().id, b_id, "identity persisted");
    let mut ea = a.subscribe();
    a.add_peer_addr(&format!("127.0.0.1:{b_port}")).await.unwrap();
    // flush happens on reconnect
    let mut ok = a.store.message(&m.id).unwrap().unwrap().status == "delivered";
    if !ok {
        ok = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(CoreEvent::MessageUpdate { message }) = ea.recv().await {
                    if message.id == m.id && message.status == "delivered" {
                        return true;
                    }
                }
            }
        })
        .await
        .unwrap_or(false)
            || a.store.message(&m.id).unwrap().unwrap().status == "delivered";
    }
    assert!(ok, "queued message delivered after reconnect");
    assert!(b2.store.message(&m.id).unwrap().is_some());
    a.shutdown();
    b2.shutdown();
}
