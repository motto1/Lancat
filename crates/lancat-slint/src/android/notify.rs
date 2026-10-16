//! 系统通知：独立线程订阅 core 事件（不依赖界面：Activity 被销毁、只剩前台服务时照样工作）。
//! * 新消息：应用不在前台时通知（点开跳到那个会话）。
//! * 文件：每个传输一条通知，带进度条、速度、暂停 / 继续 / 取消；完成后点开文件，失败带原因和「重试」。
//! * 通知按钮由 Java 记下，这里轮询取走后直接调节点（暂停 / 取消 / 重试会从断点继续）。

use lancat_core::{CoreEvent, Message, Node};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::jvm;

struct Xfer {
    name: String,
    peer: String,
    peer_name: String,
    outgoing: bool,
    done: u64,
    total: u64,
    rate: f64,
    base: u64,
    at: Instant,
    shown: Option<Instant>,
    dirty: bool,
    paused: bool,
}

pub fn start(node: Arc<Node>, rt: tokio::runtime::Handle) {
    let mut rx = node.subscribe();
    let (tx, crx) = mpsc::channel::<CoreEvent>();
    rt.spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
    let _ = std::thread::Builder::new().name("lancat-notify".into()).spawn(move || run(node, rt, crx));
}

fn s(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn peer_name(node: &Node, id: &str) -> String {
    node.peer_view(id).map(|p| p.name).unwrap_or_else(|| "对方".into())
}

fn run(node: Arc<Node>, rt: tokio::runtime::Handle, rx: mpsc::Receiver<CoreEvent>) {
    let mut xf: HashMap<String, Xfer> = HashMap::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(350)) {
            Ok(ev) => on_event(&node, &mut xf, ev),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        for a in jvm::take_actions() {
            let (Some(act), Some(id)) = (a.first(), a.get(1)) else { continue };
            let id = id.clone();
            log::info!("notification action {act} {id}");
            match act.as_str() {
                "pause" => {
                    let _ = node.pause_transfer(&id);
                }
                "cancel" => {
                    let _ = node.cancel_transfer(&id);
                }
                "resume" | "retry" => {
                    let n = node.clone();
                    rt.spawn(async move {
                        if let Err(e) = n.retry_transfer(&id).await {
                            log::warn!("retry {id}: {e}");
                        }
                    });
                }
                _ => {}
            }
        }
        // 节流：每个传输最多 0.7 s 更新一次通知（系统对更新频率有限制）
        let now = Instant::now();
        for (id, x) in xf.iter_mut() {
            if x.dirty && x.shown.is_none_or(|t| now.duration_since(t) >= Duration::from_millis(700)) {
                show_progress(id, x);
                x.shown = Some(now);
                x.dirty = false;
            }
        }
    }
}

fn title(x: &Xfer) -> String {
    if x.outgoing {
        format!("发送给 {}：{}", x.peer_name, x.name)
    } else {
        format!("来自 {}：{}", x.peer_name, x.name)
    }
}

fn show_progress(id: &str, x: &Xfer) {
    let pct = if x.total > 0 { (x.done as f64 * 100.0 / x.total as f64) as i32 } else { -1 };
    let mut text = if x.total > 0 { format!("{} / {}", crate::human(x.done), crate::human(x.total)) } else { "等待对方…".to_string() };
    if x.paused {
        text = format!("已暂停 · {text}");
    } else if x.rate >= 1.0 {
        text += &format!(" · {}/s", crate::human(x.rate as u64));
    }
    let actions = match (x.outgoing, x.paused) {
        (true, false) => "pause,cancel",
        (true, true) => "resume,cancel",
        (false, _) => "cancel",
    };
    jvm::notify_progress(id, &x.peer, &title(x), &text, pct, actions);
}

fn xfer_of(node: &Node, m: &Message) -> Xfer {
    Xfer {
        name: s(&m.body, "name"),
        peer: m.peer_id.clone(),
        peer_name: peer_name(node, &m.peer_id),
        outgoing: m.outgoing,
        done: 0,
        total: m.body.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
        rate: 0.0,
        base: 0,
        at: Instant::now(),
        shown: None,
        dirty: true,
        paused: false,
    }
}

fn on_event(node: &Node, xf: &mut HashMap<String, Xfer>, ev: CoreEvent) {
    match ev {
        CoreEvent::Message { message: m } => {
            if m.kind == "file" {
                on_file(node, xf, &m);
                return;
            }
            if m.outgoing || jvm::is_foreground() {
                return;
            }
            if node.peer_view(&m.peer_id).is_some_and(|p| p.blocked) {
                return;
            }
            let text = match m.kind.as_str() {
                "text" => s(&m.body, "text"),
                "emoji" => "[表情]".to_string(),
                _ => "新消息".to_string(),
            };
            jvm::notify_message(&m.peer_id, &peer_name(node, &m.peer_id), &text);
        }
        CoreEvent::MessageUpdate { message: m } if m.kind == "file" => on_file(node, xf, &m),
        CoreEvent::Progress { msg_id, done, total, .. } => {
            if let Some(x) = xf.get_mut(&msg_id) {
                let now = Instant::now();
                x.total = total.max(x.total);
                if done < x.base {
                    x.base = done;
                    x.at = now;
                }
                let dt = now.duration_since(x.at).as_secs_f64();
                if dt >= 0.5 {
                    let inst = (done - x.base) as f64 / dt;
                    x.rate = if x.rate > 0.0 { x.rate * 0.6 + inst * 0.4 } else { inst };
                    x.at = now;
                    x.base = done;
                }
                x.done = done;
                x.paused = false;
                x.dirty = true;
            }
        }
        _ => {}
    }
}

fn on_file(node: &Node, xf: &mut HashMap<String, Xfer>, m: &Message) {
    let st = m.status.as_str();
    match st {
        "pending" | "sent" | "delivered" | "transferring" | "receiving" | "received" => {
            let x = xf.entry(m.id.clone()).or_insert_with(|| xfer_of(node, m));
            x.paused = false;
            x.dirty = true;
        }
        "paused" => {
            let x = xf.entry(m.id.clone()).or_insert_with(|| xfer_of(node, m));
            x.paused = true;
            x.rate = 0.0;
            show_progress(&m.id, x);
            x.shown = Some(Instant::now());
            x.dirty = false;
        }
        "done" => {
            let x = xf.remove(&m.id).unwrap_or_else(|| xfer_of(node, m));
            let path = s(&m.body, "path");
            if !m.outgoing && !path.is_empty() {
                jvm::scan_file(&path);
            }
            // 历史消息（启动时重放的旧状态）不弹完成通知：只处理这次运行里见过的传输
            if x.shown.is_some() || x.dirty {
                let text = if m.outgoing { format!("已发送 · {}", crate::human(x.total)) } else { format!("已保存 · {} · 点击打开", crate::human(x.total)) };
                jvm::notify_done(&m.id, &x.peer, &title(&x), &text, if m.outgoing { "" } else { &path }, true, "");
            }
        }
        "failed" => {
            let x = xf.remove(&m.id).unwrap_or_else(|| xfer_of(node, m));
            let code = s(&m.body, "code");
            let err = s(&m.body, "error");
            let why = match crate::failure_info(&code) {
                Some((r, hint, _)) if !hint.is_empty() => format!("{r}：{hint}"),
                Some((r, _, _)) => r.to_string(),
                None if !err.is_empty() => err,
                None => "传输失败".into(),
            };
            jvm::notify_done(&m.id, &x.peer, &format!("失败 · {}", title(&x)), &why, "", false, if m.outgoing { "retry" } else { "" });
        }
        "cancelled" => {
            xf.remove(&m.id);
            jvm::cancel_notify(&m.id);
        }
        _ => {}
    }
}
