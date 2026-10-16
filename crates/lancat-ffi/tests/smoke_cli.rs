//! Smoke test: the FFI node (as Kotlin drives it) against a local `lancat-cli serve`.
//!
//! Isolated: both bind 127.0.0.1 only, ports 47841 (cli) / 47842 (ffi), mDNS and UDP discovery off,
//! fresh temp data dirs. Nothing is sent to any other device.
//!
//!   LANCAT_CLI=G:\lancat-work\target-ffi-cli\release\lancat-cli.exe cargo test --test smoke_cli -- --nocapture
//!
//! Without LANCAT_CLI the test is skipped (prints why).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lancat_ffi::{CoreListener, FfiConfig, FfiEvent, LancatNode};

const CLI_PORT: u16 = 47841;
const FFI_PORT: u16 = 47842;

struct Tx(Mutex<mpsc::Sender<FfiEvent>>);
impl CoreListener for Tx {
    fn on_event(&self, event: FfiEvent) {
        let _ = self.0.lock().unwrap().send(event);
    }
}

struct Cli {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
}

impl Cli {
    fn start(exe: &str, data: &Path, recv: &Path) -> Cli {
        let mut child = Command::new(exe)
            .args(["serve", "--port", &CLI_PORT.to_string(), "--udp-port", &CLI_PORT.to_string(), "--no-mdns", "--no-udp", "--name", "smoke-cli"])
            .arg("--data")
            .arg(data)
            .arg("--recv")
            .arg(recv)
            .env("LANCAT_BIND_IP", "127.0.0.1")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn lancat-cli");
        let stdin = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(l);
            }
        });
        Cli { child, stdin, lines }
    }

    fn cmd(&mut self, l: &str) {
        writeln!(self.stdin, "{l}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Waits for a JSON event line containing every needle.
    fn wait(&self, needles: &[&str], secs: u64) -> String {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let l = self.lines.recv_timeout(left).unwrap_or_else(|_| panic!("cli: timeout waiting for {needles:?}"));
            if needles.iter().all(|n| l.contains(n)) {
                return l;
            }
        }
    }
}

impl Drop for Cli {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        std::thread::sleep(Duration::from_millis(500));
        let _ = self.child.kill();
    }
}

fn wait_ev(rx: &mpsc::Receiver<FfiEvent>, secs: u64, mut f: impl FnMut(&FfiEvent) -> bool) -> FfiEvent {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        let left = end.saturating_duration_since(Instant::now());
        let ev = rx.recv_timeout(left).expect("ffi: timeout waiting for event");
        if f(&ev) {
            return ev;
        }
    }
}

fn random_file(path: &Path, len: usize, seed: u64) {
    // xorshift: incompressible, reproducible
    let mut x = seed | 1;
    let mut buf = Vec::with_capacity(len);
    while buf.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        buf.extend_from_slice(&x.to_le_bytes());
    }
    buf.truncate(len);
    std::fs::write(path, buf).unwrap();
}

fn b3(p: &Path) -> String {
    blake3::hash(&std::fs::read(p).unwrap()).to_hex().to_string()
}

#[test]
fn ffi_and_cli_exchange_text_and_files() {
    let Ok(exe) = std::env::var("LANCAT_CLI") else {
        eprintln!("SKIP: set LANCAT_CLI to a lancat-cli binary");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    for d in ["cli-data", "cli-recv", "ffi-data", "ffi-recv", "src"] {
        std::fs::create_dir_all(t.join(d)).unwrap();
    }
    let mut cli = Cli::start(&exe, &t.join("cli-data"), &t.join("cli-recv"));

    let (tx, rx) = mpsc::channel();
    let node: Arc<LancatNode> = LancatNode::start(
        FfiConfig {
            data_dir: t.join("ffi-data").to_string_lossy().into(),
            device_name: Some("smoke-ffi".into()),
            port: FFI_PORT,
            udp_port: FFI_PORT,
            receive_dir: Some(t.join("ffi-recv").to_string_lossy().into()),
            enable_mdns: false,
            enable_udp: false,
            extra_probe_targets: vec![],
            bind_ip: Some("127.0.0.1".into()),
            port_fallback: false,
            init_logging: false,
        },
        Box::new(Tx(Mutex::new(tx))),
    )
    .expect("start ffi node");
    let me = node.me();
    eprintln!("ffi node {} {} port {} ips {:?}", me.name, me.id, me.port, me.ips);
    assert_eq!(me.port, FFI_PORT);

    // cli has to be listening before we add it
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut peer = None;
    for _ in 0..40 {
        match rt.block_on(node.add_peer(format!("127.0.0.1:{CLI_PORT}"))) {
            Ok(p) => {
                peer = Some(p);
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
    let peer = peer.expect("add_peer 127.0.0.1:47841");
    assert_eq!(peer.name, "smoke-cli");
    let pid = peer.id.clone();
    // wait until the link is up (online)
    wait_ev(&rx, 20, |e| matches!(e, FfiEvent::Peer { peer } if peer.id == pid && peer.online));
    eprintln!("peer online: {} {}", peer.name, peer.id);

    // ---- ffi -> cli: text
    let sent = rt.block_on(node.send_text(vec![pid.clone()], "你好 from ffi".into())).unwrap();
    assert_eq!(sent.len(), 1);
    cli.wait(&["\"type\":\"message\"", "你好 from ffi"], 20);
    let mid = sent[0].id.clone();
    if sent[0].status != "delivered" {
        wait_ev(&rx, 20, |e| matches!(e, FfiEvent::MessageUpdate { message } if message.id == mid && message.status == "delivered"));
    }
    eprintln!("ok: text ffi -> cli delivered");

    // ---- ffi -> cli: emoji (sticker)
    rt.block_on(node.send_emoji(vec![pid.clone()], lancat_ffi::FfiEmoji { kind: "noto".into(), code: "1f600".into(), alt: String::new(), data: None })).unwrap();
    cli.wait(&["\"kind\":\"emoji\"", "1f600"], 20);
    eprintln!("ok: emoji ffi -> cli");

    // ---- ffi -> cli: file (8 MiB + odd tail), hash must match
    let src = t.join("src").join("ffi-to-cli.bin");
    random_file(&src, 8 * 1024 * 1024 + 12345, 0x5eed);
    let msgs = rt.block_on(node.send_files(vec![pid.clone()], vec![src.to_string_lossy().into()])).unwrap();
    let fid = msgs[0].id.clone();
    let mut saw_progress = false;
    let done = wait_ev(&rx, 60, |e| match e {
        FfiEvent::Progress { msg_id, .. } if *msg_id == fid => {
            saw_progress = true;
            false
        }
        FfiEvent::MessageUpdate { message } if message.id == fid => {
            assert_ne!(message.status, "failed", "transfer failed: {:?}", message.file);
            message.status == "done"
        }
        _ => false,
    });
    let FfiEvent::MessageUpdate { message } = done else { unreachable!() };
    let got = t.join("cli-recv").join("ffi-to-cli.bin");
    cli.wait(&["message_update", "ffi-to-cli.bin", "\"status\":\"done\""], 30);
    assert_eq!(b3(&src), b3(&got), "hash mismatch ffi -> cli");
    assert_eq!(message.file.as_ref().unwrap().hash, lancat_ffi::hash_file(src.to_string_lossy().into()).unwrap());
    eprintln!("ok: file ffi -> cli, blake3 {} (progress events: {saw_progress})", b3(&got));

    // ---- cli -> ffi: text
    cli.cmd("text smoke-ffi hello from cli");
    let ev = wait_ev(&rx, 20, |e| matches!(e, FfiEvent::Message { message } if !message.outgoing && message.text.as_deref() == Some("hello from cli")));
    let FfiEvent::Message { message } = ev else { unreachable!() };
    assert_eq!(message.peer_id, pid);
    eprintln!("ok: text cli -> ffi");

    // ---- cli -> ffi: file, hash must match
    let src2 = t.join("src").join("cli-to-ffi.bin");
    random_file(&src2, 5 * 1024 * 1024 + 777, 0xbeef);
    cli.cmd(&format!("file smoke-ffi {}", src2.display()));
    let mut inc_id = String::new();
    wait_ev(&rx, 60, |e| match e {
        FfiEvent::Message { message } if !message.outgoing && message.kind == "file" => {
            inc_id = message.id.clone();
            false
        }
        FfiEvent::MessageUpdate { message } if !message.outgoing && message.kind == "file" && message.status == "done" => true,
        FfiEvent::MessageUpdate { message } if !message.outgoing && message.status == "failed" => panic!("incoming failed {:?}", message.file),
        _ => false,
    });
    let got2 = t.join("ffi-recv").join("cli-to-ffi.bin");
    assert_eq!(b3(&src2), b3(&got2), "hash mismatch cli -> ffi");
    eprintln!("ok: file cli -> ffi, blake3 {}", b3(&got2));

    // ---- history paging (what LancatCoreRepository.loadOlder uses)
    let all = node.messages(pid.clone(), None, 50).unwrap();
    assert!(all.len() >= 5, "history has {} messages", all.len());
    assert!(all.windows(2).all(|w| w[0].ts < w[1].ts), "history ascending");
    let older = node.messages(pid.clone(), Some(all.last().unwrap().ts), 2).unwrap();
    assert_eq!(older.len(), 2);
    assert!(older.last().unwrap().ts < all.last().unwrap().ts);
    eprintln!("ok: history {} messages, paging works", all.len());

    // ---- pause / resume (retry) / cancel on a bigger file
    let big = t.join("src").join("big.bin");
    random_file(&big, 256 * 1024 * 1024, 0xb16);
    let msgs = rt.block_on(node.send_files(vec![pid.clone()], vec![big.to_string_lossy().into()])).unwrap();
    let bid = msgs[0].id.clone();
    // pause as soon as bytes flow
    wait_ev(&rx, 60, |e| matches!(e, FfiEvent::Progress { msg_id, done, .. } if *msg_id == bid && *done > 0));
    node.pause_transfer(bid.clone()).unwrap();
    let paused = wait_ev(&rx, 30, |e| matches!(e, FfiEvent::MessageUpdate { message } if message.id == bid && matches!(message.status.as_str(), "paused" | "done")));
    let FfiEvent::MessageUpdate { message } = paused else { unreachable!() };
    if message.status == "paused" {
        eprintln!("ok: paused");
        rt.block_on(node.retry_transfer(bid.clone())).unwrap();
    } else {
        eprintln!("note: finished before the pause landed");
    }
    wait_ev(&rx, 180, |e| match e {
        FfiEvent::MessageUpdate { message } if message.id == bid => {
            assert_ne!(message.status, "failed", "big failed: {:?}", message.file);
            message.status == "done"
        }
        _ => false,
    });
    assert_eq!(b3(&big), b3(&t.join("cli-recv").join("big.bin")), "hash mismatch big (pause/resume)");
    eprintln!("ok: big file resumed and verified");

    let c = rt.block_on(node.send_files(vec![pid.clone()], vec![big.to_string_lossy().into()])).unwrap();
    let cid = c[0].id.clone();
    wait_ev(&rx, 60, |e| matches!(e, FfiEvent::Progress { msg_id, done, .. } if *msg_id == cid && *done > 0));
    node.cancel_transfer(cid.clone()).unwrap();
    wait_ev(&rx, 30, |e| matches!(e, FfiEvent::MessageUpdate { message } if message.id == cid && matches!(message.status.as_str(), "cancelled" | "done")));
    eprintln!("ok: cancel");

    // ---- block / clear / settings
    node.set_blocked(pid.clone(), true).unwrap();
    assert!(node.peer(pid.clone()).unwrap().blocked);
    node.set_blocked(pid.clone(), false).unwrap();
    let mut s = node.settings();
    s.theme = "dark".into();
    s.manual_peers.push(format!("127.0.0.1:{CLI_PORT}"));
    let s2 = node.set_settings(s).unwrap();
    assert_eq!(s2.theme, "dark");
    assert!(node.settings_json().contains("\"theme\":\"dark\""));
    node.clear_history(Some(pid.clone())).unwrap();
    assert!(node.messages(pid.clone(), None, 50).unwrap().is_empty());
    wait_ev(&rx, 10, |e| matches!(e, FfiEvent::HistoryCleared { .. }));
    eprintln!("ok: block / settings / clear");

    node.shutdown();
    drop(cli);
    let _ = PathBuf::new();
}
