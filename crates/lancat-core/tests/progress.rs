//! Progress moves within chunks (not only in whole 4 MiB steps) on both sides.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use lancat_core::{CoreEvent, Node, NodeConfig};

async fn start(dir: &Path, name: &str) -> Arc<Node> {
    let mut cfg = NodeConfig::new(dir.join("data"));
    cfg.port = 0;
    cfg.enable_mdns = false;
    cfg.enable_udp = false;
    cfg.name_override = Some(name.into());
    cfg.recv_dir_override = Some(dir.join("recv"));
    Node::start(cfg).await.expect("start node")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn progress_is_byte_level() {
    let ta = tempfile::tempdir().unwrap();
    let tb = tempfile::tempdir().unwrap();
    let a = start(ta.path(), "alpha").await;
    let b = start(tb.path(), "bravo").await;
    let mut ea = a.subscribe();
    let mut eb = b.subscribe();
    a.add_peer_addr(&format!("127.0.0.1:{}", b.port())).await.unwrap();
    let b_id = b.me().id;
    let src = ta.path().join("big.bin");
    let size: u64 = 256 * 1024 * 1024;
    {
        let f = std::fs::File::create(&src).unwrap();
        f.set_len(size).unwrap();
    }
    a.send_files(&[b_id.clone()], &[src.clone()]).await.unwrap();
    let chunk = lancat_core::protocol::CHUNK_SIZE;
    let collect = |rx: &mut tokio::sync::broadcast::Receiver<CoreEvent>, outgoing: bool| {
        let mut v = Vec::new();
        while let Ok(e) = rx.try_recv() {
            if let CoreEvent::Progress { done, total, outgoing: o, .. } = e {
                if o == outgoing {
                    v.push((done, total));
                }
            }
        }
        v
    };
    let mut out = Vec::new();
    let mut inc = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        out.extend(collect(&mut ea, true));
        inc.extend(collect(&mut eb, false));
        if inc.iter().any(|&(d, t)| d == t) && out.iter().any(|&(d, t)| d == t) {
            break;
        }
    }
    let mid = |v: &[(u64, u64)]| v.iter().filter(|&&(d, t)| d > 0 && d < t && d % chunk != 0).count();
    eprintln!("outgoing {} events ({} mid-chunk), incoming {} events ({} mid-chunk)", out.len(), mid(&out), inc.len(), mid(&inc));
    assert!(mid(&out) > 0, "sender progress only in whole chunks: {out:?}");
    assert!(mid(&inc) > 0, "receiver progress only in whole chunks: {inc:?}");
    // never goes backwards by more than a slice on either side
    for v in [&out, &inc] {
        for w in v.windows(2) {
            assert!(w[1].0 + 4 * 1024 * 1024 >= w[0].0, "went back: {:?}", w);
        }
    }
}
