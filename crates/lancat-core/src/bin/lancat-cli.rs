//! Headless Lancat peer, for testing and scripting.
//!
//! ```text
//! lancat-cli serve [--port 47820] [--udp-port 47820] [--data DIR] [--name NAME] [--recv DIR] [--no-mdns] [--no-udp]
//!                  [--peer IP:PORT]... [--probe IP:UDPPORT]...
//!     prints core events as JSON lines; stdin commands:
//!       peers | refresh | text <peer> <msg> | emoji <peer> <codepoint> | file <peer> <path> | clear | quit
//!     (<peer> = id prefix or name)
//! lancat-cli send --to IP:PORT [--text MSG]... [--file PATH]... [--data DIR] [--timeout SECS]
//!     one-shot: starts an ephemeral peer, sends, waits for delivery/verification, exits 0 on success
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use lancat_core::{CoreEvent, EmojiPayload, Node, NodeConfig};
use tokio::io::{AsyncBufReadExt, BufReader};

struct Args {
    cmd: String,
    port: u16,
    data: Option<PathBuf>,
    name: Option<String>,
    recv: Option<PathBuf>,
    mdns: bool,
    udp: bool,
    peers: Vec<String>,
    udp_port: u16,
    probes: Vec<std::net::SocketAddr>,
    to: Option<String>,
    texts: Vec<String>,
    files: Vec<PathBuf>,
    timeout: u64,
}

fn parse() -> Result<Args> {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().unwrap_or_else(|| "serve".into());
    let mut a = Args {
        cmd,
        port: lancat_core::protocol::DEFAULT_PORT,
        data: None,
        name: None,
        recv: None,
        mdns: true,
        udp: true,
        peers: vec![],
        udp_port: lancat_core::protocol::DEFAULT_PORT,
        probes: vec![],
        to: None,
        texts: vec![],
        files: vec![],
        timeout: 60,
    };
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or_else(|| anyhow!("{k} needs a value"));
        match k.as_str() {
            "--port" => a.port = v()?.parse()?,
            "--data" => a.data = Some(v()?.into()),
            "--name" => a.name = Some(v()?),
            "--recv" => a.recv = Some(v()?.into()),
            "--no-mdns" => a.mdns = false,
            "--no-udp" => a.udp = false,
            "--peer" => a.peers.push(v()?),
            "--udp-port" => a.udp_port = v()?.parse()?,
            "--probe" => a.probes.push(v()?.parse()?),
            "--to" => a.to = Some(v()?),
            "--text" => a.texts.push(v()?),
            "--file" => a.files.push(v()?.into()),
            "--timeout" => a.timeout = v()?.parse()?,
            "-h" | "--help" => {
                println!("{}", include_str!("lancat-cli.rs").lines().take(13).collect::<Vec<_>>().join("\n"));
                std::process::exit(0);
            }
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(a)
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).target(env_logger::Target::Stderr).init();
    let a = parse()?;
    match a.cmd.as_str() {
        "serve" => serve(a).await,
        "send" => send(a).await,
        c => bail!("unknown command {c} (serve | send)"),
    }
}

fn config(a: &Args, default_data: PathBuf) -> NodeConfig {
    let mut cfg = NodeConfig::new(a.data.clone().unwrap_or(default_data));
    cfg.port = a.port;
    cfg.udp_port = a.udp_port;
    cfg.extra_probe_targets = a.probes.clone();
    cfg.enable_mdns = a.mdns;
    cfg.enable_udp = a.udp;
    cfg.name_override = a.name.clone();
    cfg.recv_dir_override = a.recv.clone();
    cfg.with_env()
}

fn find_peer(node: &Node, q: &str) -> Option<String> {
    node.peers().into_iter().find(|p| p.id.starts_with(q) || p.name == q).map(|p| p.id)
}

async fn serve(a: Args) -> Result<()> {
    let default = dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("lancat-cli");
    let node = Node::start(config(&a, default)).await?;
    eprintln!("me: {}  port {}  recv {}", serde_json::to_string(&node.me())?, node.port(), node.receive_dir().display());
    for p in &a.peers {
        match node.add_peer_addr(p).await {
            Ok(v) => eprintln!("added peer {} ({})", v.name, v.id),
            Err(e) => eprintln!("add peer {p}: {e}"),
        }
    }
    let mut rx = node.subscribe();
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let Ok(s) = serde_json::to_string(&ev) {
                println!("{s}");
            }
        }
    });
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            l = lines.next_line() => {
                let Ok(Some(l)) = l else {
                    // stdin closed: keep serving until Ctrl-C
                    tokio::signal::ctrl_c().await.ok();
                    break;
                };
                if let Err(e) = command(&node, l.trim()).await { eprintln!("error: {e}"); }
                if l.trim() == "quit" { break; }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    node.shutdown();
    // let the bye over the links flush, then exit: the blocking stdin reader would otherwise keep
    // the runtime (and the process) alive
    tokio::time::sleep(Duration::from_millis(200)).await;
    std::process::exit(0)
}

async fn command(node: &Arc<Node>, l: &str) -> Result<()> {
    let mut parts = l.splitn(3, ' ');
    let cmd = parts.next().unwrap_or("");
    let peer = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("");
    let resolve = || find_peer(node, peer).ok_or_else(|| anyhow!("no such peer {peer}"));
    match cmd {
        "" | "quit" => {}
        "peers" => {
            for p in node.peers() {
                eprintln!("{} {:<20} {:<8} {}:{} unread={}", if p.online { "●" } else { "○" }, p.name, &p.id[..8.min(p.id.len())], p.addr, p.port, p.unread);
            }
        }
        "refresh" => node.refresh_peers(),
        "add" => {
            node.add_peer_addr(peer).await?;
        }
        "text" => {
            node.send_text(&resolve()?, rest).await?;
        }
        "emoji" => {
            let e = EmojiPayload { kind: "noto".into(), code: rest.into(), alt: String::new(), data: None };
            node.send_emoji(&resolve()?, e).await?;
        }
        "file" => {
            node.send_files(&[resolve()?], &[PathBuf::from(rest)]).await?;
        }
        "clear" => node.clear_history(None)?,
        _ => bail!("unknown command"),
    }
    Ok(())
}

async fn send(a: Args) -> Result<()> {
    let to = a.to.clone().ok_or_else(|| anyhow!("--to IP:PORT required"))?;
    let tmp = std::env::temp_dir().join(format!("lancat-send-{}", std::process::id()));
    let mut a = a;
    a.port = 0;
    a.mdns = false;
    a.udp = false;
    let node = Node::start(config(&a, tmp.clone())).await?;
    let peer = node.add_peer_addr(&to).await?;
    eprintln!("sending to {} ({})", peer.name, peer.id);
    let mut rx = node.subscribe();
    let mut pending = std::collections::HashSet::new();
    for t in &a.texts {
        let m = node.send_text(&peer.id, t).await?;
        if m.status != "delivered" {
            pending.insert(m.id);
        }
    }
    if !a.files.is_empty() {
        for m in node.send_files(&[peer.id.clone()], &a.files).await? {
            pending.insert(m.id);
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(a.timeout);
    let mut failed = false;
    while !pending.is_empty() {
        let ev = tokio::time::timeout_at(deadline, rx.recv()).await.map_err(|_| anyhow!("timeout, still pending: {pending:?}"))??;
        if let CoreEvent::MessageUpdate { message } = ev {
            match message.status.as_str() {
                "delivered" | "done" => {
                    eprintln!("ok   {} {}", message.kind, lancat_core::store::preview(&message));
                    pending.remove(&message.id);
                }
                "failed" => {
                    eprintln!("FAIL {} {}", message.kind, message.body);
                    pending.remove(&message.id);
                    failed = true;
                }
                _ => {}
            }
        }
    }
    node.shutdown();
    let _ = std::fs::remove_dir_all(&tmp);
    if failed {
        bail!("some items failed");
    }
    Ok(())
}
