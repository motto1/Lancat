//! Interop: the FFI node (exactly what the Android app runs) against a real desktop Lancat that is
//! already running on this machine / LAN (Windows or Linux, v0.2.8).
//!
//!   LANCAT_DESKTOP=192.168.3.195:47820 cargo test --test desktop_interop -- --nocapture
//!
//! Optional: LANCAT_DESKTOP_RECV = the desktop's receive folder, to verify the file's blake3 there.
//! LANCAT_DESKTOP_NAME = match the desktop by name; LANCAT_FFI_LOOPBACK=1 = bind 127.0.0.1, no UDP/mDNS
//! (talk to a throwaway desktop test instance only). LANCAT_EXPECT_INBOUND=<path> = also wait for the
//! desktop to send us a text, an emoji and that file (scripted with LANCAT_SLINT_SCRIPT).
//! Sends one labelled text message and one small file to the desktop. Skipped without LANCAT_DESKTOP.

use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use lancat_ffi::{CoreListener, FfiConfig, FfiEvent, LancatNode};

struct Tx(Mutex<mpsc::Sender<FfiEvent>>);
impl CoreListener for Tx {
    fn on_event(&self, event: FfiEvent) {
        let _ = self.0.lock().unwrap().send(event);
    }
}

fn wait_ev(rx: &mpsc::Receiver<FfiEvent>, secs: u64, mut f: impl FnMut(&FfiEvent) -> bool) -> FfiEvent {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        let left = end.saturating_duration_since(Instant::now());
        let ev = rx.recv_timeout(left).expect("timeout waiting for event");
        if f(&ev) {
            return ev;
        }
    }
}

#[test]
fn ffi_node_talks_to_running_desktop() {
    let Ok(target) = std::env::var("LANCAT_DESKTOP") else {
        eprintln!("SKIP: set LANCAT_DESKTOP=ip:port of a running desktop Lancat");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    std::fs::create_dir_all(t.join("data")).unwrap();
    std::fs::create_dir_all(t.join("recv")).unwrap();
    let loopback = std::env::var_os("LANCAT_FFI_LOOPBACK").is_some();
    let want_name = std::env::var("LANCAT_DESKTOP_NAME").ok();
    let (tx, rx) = mpsc::channel();
    let node: Arc<LancatNode> = LancatNode::start(
        FfiConfig {
            data_dir: t.join("data").to_string_lossy().into(),
            device_name: Some("lancat-android-v2".into()),
            port: 47852,
            udp_port: 47852,
            receive_dir: Some(t.join("recv").to_string_lossy().into()),
            enable_mdns: false,
            enable_udp: !loopback,
            extra_probe_targets: vec![target.clone()],
            bind_ip: if loopback { Some("127.0.0.1".into()) } else { None },
            port_fallback: true,
            init_logging: true,
        },
        Box::new(Tx(Mutex::new(tx))),
    )
    .expect("start ffi node");
    println!("ffi node {} port {}", node.me().id, node.port());

    // the desktop answers the unicast probe; add it by address too (same as the app's manual-add)
    let host = target.split(':').next().unwrap().to_string();
    let desk = {
        let end = Instant::now() + Duration::from_secs(90);
        loop {
            if let Some(p) = node.peers().into_iter().find(|p| {
                p.online && p.os != "android" && want_name.as_ref().map(|n| &p.name == n).unwrap_or(p.addr == host)
            }) {
                break p;
            }
            let _ = rt0().block_on(node.add_peer(target.clone()));
            if Instant::now() > end {
                panic!("desktop at {target} not discovered; peers: {:?}", node.peers());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    };
    println!("desktop peer: {} os={} ver={} addr={}:{}", desk.name, desk.os, desk.ver, desk.addr, desk.port);

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();

    // text
    let sent = rt.block_on(node.send_text(vec![desk.id.clone()], "Lancat 安卓 v2 互通测试（自动化，可删除）".into())).unwrap();
    let mid = sent[0].id.clone();
    wait_ev(&rx, 20, |e| matches!(e, FfiEvent::MessageUpdate { message } if message.id == mid && message.status == "delivered"));
    println!("ok: text delivered to desktop");

    // emoji
    let em = rt
        .block_on(node.send_emoji(
            vec![desk.id.clone()],
            lancat_ffi::FfiEmoji { kind: "noto".into(), code: "1f44b".into(), alt: "👋".into(), data: None },
        ))
        .unwrap();
    let eid = em[0].id.clone();
    wait_ev(&rx, 20, |e| matches!(e, FfiEvent::MessageUpdate { message } if message.id == eid && message.status == "delivered"));
    println!("ok: animated emoji delivered");

    // file (2 MiB, incompressible)
    let src = t.join("lancat-android-interop.bin");
    let mut x: u64 = 0x9e3779b97f4a7c15;
    let mut buf = Vec::with_capacity(2 << 20);
    while buf.len() < 2 << 20 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        buf.extend_from_slice(&x.to_le_bytes());
    }
    std::fs::write(&src, &buf).unwrap();
    let hash = blake3::hash(&buf).to_hex().to_string();
    let files = rt.block_on(node.send_files(vec![desk.id.clone()], vec![src.to_string_lossy().into()])).unwrap();
    let fid = files[0].id.clone();
    let mut progressed = false;
    wait_ev(&rx, 60, |e| match e {
        FfiEvent::Progress { msg_id, .. } if *msg_id == fid => {
            progressed = true;
            false
        }
        FfiEvent::MessageUpdate { message } if message.id == fid => {
            if message.status == "failed" {
                panic!("file failed: {:?}", message.file);
            }
            message.status == "done"
        }
        _ => false,
    });
    println!("ok: 2 MiB file done on desktop (progress events: {progressed}), blake3 {hash}");
    if let Ok(dir) = std::env::var("LANCAT_DESKTOP_RECV") {
        let mut best: Option<std::path::PathBuf> = None;
        for ent in std::fs::read_dir(&dir).unwrap().flatten() {
            let n = ent.file_name().to_string_lossy().to_string();
            if n.starts_with("lancat-android-interop") {
                let p = ent.path();
                let newer = best.as_ref().map(|b| b.metadata().unwrap().modified().unwrap() < p.metadata().unwrap().modified().unwrap()).unwrap_or(true);
                if newer {
                    best = Some(p);
                }
            }
        }
        let got = best.expect("received file not found in LANCAT_DESKTOP_RECV");
        assert_eq!(lancat_ffi::hash_file(got.to_string_lossy().into()).unwrap(), hash);
        println!("ok: desktop saved {} with matching blake3", got.display());
        let _ = std::fs::remove_file(Path::new(&got));
    }
    if let Ok(expect) = std::env::var("LANCAT_EXPECT_INBOUND") {
        let want = lancat_ffi::hash_file(expect.clone()).unwrap();
        let (mut got_text, mut got_emoji, mut got_file) = (false, false, false);
        wait_ev(&rx, 120, |e| {
            match e {
                FfiEvent::Message { message } if !message.outgoing && message.kind == "text" => {
                    println!("in: text {:?}", message.text);
                    got_text = true;
                }
                FfiEvent::Message { message } if !message.outgoing && message.kind == "emoji" => {
                    println!("in: emoji {:?}", message.emoji.as_ref().map(|e| e.code.clone()));
                    got_emoji = true;
                }
                FfiEvent::MessageUpdate { message } if !message.outgoing && message.kind == "file" && message.status == "done" => {
                    let f = message.file.as_ref().unwrap();
                    assert_eq!(lancat_ffi::hash_file(f.path.clone()).unwrap(), want, "inbound file hash");
                    println!("in: file {} done, blake3 matches", f.name);
                    got_file = true;
                }
                FfiEvent::MessageUpdate { message } if !message.outgoing && message.status == "failed" => panic!("inbound failed {:?}", message.file),
                _ => {}
            }
            got_text && got_emoji && got_file
        });
        println!("ok: desktop -> ffi text, emoji, file");
    }
    node.shutdown();
}

fn rt0() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}
