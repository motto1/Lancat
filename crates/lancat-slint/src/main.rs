//! Lancat 面板（Slint 版）。阶段 2 骨架：直接起 lancat-core 节点，会话列表 + 聊天页 + 输入框，
//! 收发文本。Windows 用自写 Win32 Platform（软件渲染 → 预乘 BGRA DIB，Acrylic + DWM 阴影 + 淡入），
//! Linux 暂用 winit 后端（阶段 3 换 SCTK layer-shell）。
//!
//! 测试隔离（硬性规则）：默认数据目录 `<data>/lancat-slint-dev`、端口 47840、关闭 mDNS/UDP 发现，
//! 所以不会碰到局域网里的真实设备；对端只能用 LANCAT_PROBE / 手动地址指定本机另一实例。
//! `LANCAT_SLINT_DISCOVERY=1` 才打开发现（正式替换旧版之前不要用）。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

slint::include_modules!();

#[cfg(windows)]
mod win;

use lancat_core::{CoreEvent, Message, Node, NodeConfig, PeerView};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

pub(crate) fn t0() -> Instant {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *T0.get_or_init(Instant::now)
}

struct App {
    ui: slint::Weak<PanelWindow>,
    node: Arc<Node>,
    rt: tokio::runtime::Handle,
    peers: Vec<PeerView>,
    peer_model: Rc<VecModel<PeerItem>>,
    msgs: Vec<Message>,
    msg_model: Rc<VecModel<MsgItem>>,
    selected: String,
    typing: std::collections::HashMap<String, bool>,
    last_typing_sent: Option<Instant>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.borrow_mut().as_mut().map(f))
}

const TINTS: [u32; 8] = [0xff9f0a, 0x30d158, 0x64d2ff, 0xbf5af2, 0xff375f, 0x5e5ce6, 0xffd60a, 0x0a84ff];

fn tint(id: &str) -> slint::Color {
    let h = id.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let c = TINTS[(h as usize) % TINTS.len()];
    slint::Color::from_rgb_u8((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

fn peer_item(p: &PeerView) -> PeerItem {
    let initial: String = p.name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
    PeerItem { id: p.id.clone().into(), name: p.name.clone().into(), initial: initial.into(), tint: tint(&p.id), online: p.online, unread: p.unread as i32 }
}

fn human(n: u64) -> String {
    let f = n as f64;
    if f >= 1073741824.0 {
        format!("{:.1} GB", f / 1073741824.0)
    } else if f >= 1048576.0 {
        format!("{:.1} MB", f / 1048576.0)
    } else if f >= 1024.0 {
        format!("{:.0} KB", f / 1024.0)
    } else {
        format!("{n} B")
    }
}

fn status_label(s: &str) -> &'static str {
    match s {
        "pending" => "等待中",
        "sent" => "已发送",
        "delivered" => "已送达",
        "transferring" | "receiving" => "传输中",
        "paused" => "已暂停",
        "done" => "完成",
        "failed" => "失败",
        "cancelled" => "已取消",
        "received" => "已接收",
        _ => "",
    }
}

fn msg_item(m: &Message, first: bool, progress: Option<f32>) -> MsgItem {
    let mut it = MsgItem { id: m.id.clone().into(), outgoing: m.outgoing, kind: m.kind.clone().into(), status: m.status.clone().into(), first, progress: -1.0, ..Default::default() };
    match m.kind.as_str() {
        "text" => it.text = m.body.get("text").and_then(|v| v.as_str()).unwrap_or("").into(),
        "emoji" => {
            // EmojiPayload：阶段 4 换 ThorVG 渲染 Lottie 帧；骨架先显示字形/码点
            let cp = m.body.get("code").or_else(|| m.body.get("id")).and_then(|v| v.as_str()).unwrap_or("");
            let glyph: String = cp.split('-').filter_map(|h| u32::from_str_radix(h, 16).ok()).filter_map(char::from_u32).collect();
            it.text = if glyph.is_empty() { "🙂".into() } else { glyph.into() };
        }
        "file" => {
            let name = m.body.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let size = m.body.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            it.fname = name.into();
            it.fmeta = format!("{} · {}", human(size), status_label(&m.status)).into();
            if let Some(p) = progress {
                it.progress = p;
            } else if matches!(m.status.as_str(), "transferring" | "receiving" | "paused") {
                it.progress = 0.0;
            }
        }
        _ => it.text = format!("[{}]", m.kind).into(),
    }
    it
}

impl App {
    fn ui(&self) -> Option<PanelWindow> {
        self.ui.upgrade()
    }

    fn rebuild_peers(&mut self) {
        self.peers.sort_by(|a, b| b.online.cmp(&a.online).then(b.last_ts.cmp(&a.last_ts)).then(a.name.cmp(&b.name)));
        let items: Vec<PeerItem> = self.peers.iter().filter(|p| !p.blocked).map(peer_item).collect();
        self.peer_model.set_vec(items);
        self.update_header();
    }

    fn update_header(&self) {
        let Some(ui) = self.ui() else { return };
        if self.selected.is_empty() {
            let online = self.peers.iter().filter(|p| p.online && !p.blocked).count();
            ui.set_header_title("全部".into());
            ui.set_subtitle(if online == 0 { "正在寻找局域网内的设备…".into() } else { format!("{online} 台设备在线（群发）").into() });
            ui.set_subtitle_accent(false);
        } else if let Some(p) = self.peers.iter().find(|p| p.id == self.selected) {
            ui.set_header_title(p.name.clone().into());
            let typing = self.typing.get(&p.id).copied().unwrap_or(false);
            ui.set_subtitle(if typing { "正在输入…".into() } else if p.online { format!("在线 · {}", p.addr).into() } else { "离线".into() });
            ui.set_subtitle_accent(typing);
        }
        ui.set_selected_id(self.selected.clone().into());
    }

    fn load_messages(&mut self) {
        self.msgs = if self.selected.is_empty() {
            Vec::new()
        } else {
            let mut v = self.node.messages(&self.selected, None, 80).unwrap_or_default();
            v.sort_by_key(|m| m.ts);
            v
        };
        self.rebuild_messages();
        if !self.selected.is_empty() {
            let _ = self.node.mark_read(&self.selected);
        }
    }

    fn rebuild_messages(&mut self) {
        let mut items = Vec::with_capacity(self.msgs.len());
        let mut prev: Option<&Message> = None;
        for m in &self.msgs {
            let first = prev.map_or(true, |p| p.outgoing != m.outgoing || m.ts - p.ts > 120_000);
            items.push(msg_item(m, first, None));
            prev = Some(m);
        }
        self.msg_model.set_vec(items);
        if let Some(ui) = self.ui() {
            let w = ui.as_weak();
            // after layout: scroll to the newest message
            slint::Timer::single_shot(std::time::Duration::from_millis(0), move || {
                if let Some(ui) = w.upgrade() {
                    ui.invoke_scroll_to_end();
                }
            });
        }
    }

    fn upsert_message(&mut self, m: Message) {
        if m.peer_id != self.selected {
            return;
        }
        if let Some(i) = self.msgs.iter().position(|x| x.id == m.id) {
            let first = self.msg_model.row_data(i).map(|r| r.first).unwrap_or(true);
            self.msgs[i] = m;
            self.msg_model.set_row_data(i, msg_item(&self.msgs[i], first, None));
        } else {
            self.msgs.push(m);
            self.rebuild_messages();
            let _ = self.node.mark_read(&self.selected);
        }
    }

    fn on_core(&mut self, ev: CoreEvent) {
        match ev {
            CoreEvent::Peer { peer } => {
                match self.peers.iter_mut().find(|p| p.id == peer.id) {
                    Some(p) => *p = peer,
                    None => self.peers.push(peer),
                }
                self.rebuild_peers();
            }
            CoreEvent::PeerRemoved { peer_id } => {
                self.peers.retain(|p| p.id != peer_id);
                if self.selected == peer_id {
                    self.selected.clear();
                    self.load_messages();
                }
                self.rebuild_peers();
            }
            CoreEvent::Message { message } | CoreEvent::MessageUpdate { message } => self.upsert_message(message),
            CoreEvent::Typing { peer_id, typing } => {
                self.typing.insert(peer_id, typing);
                self.update_header();
            }
            CoreEvent::Progress { msg_id, done, total, .. } => {
                if let Some(i) = self.msgs.iter().position(|x| x.id == msg_id) {
                    let first = self.msg_model.row_data(i).map(|r| r.first).unwrap_or(true);
                    let p = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                    self.msg_model.set_row_data(i, msg_item(&self.msgs[i], first, Some(p)));
                }
            }
            CoreEvent::HistoryCleared { .. } => self.load_messages(),
            CoreEvent::Settings { settings } => {
                if let Some(ui) = self.ui() {
                    let dark = theme_is_dark(&settings.theme);
                    ui.global::<Theme>().set_dark(dark);
                    #[cfg(windows)]
                    win::refresh_material(&ui, dark);
                }
            }
            CoreEvent::Alert { detail, .. } => {
                if let Some(ui) = self.ui() {
                    ui.set_toast(if detail.is_empty() { "检测到网络问题，请到设置里查看".into() } else { detail.into() });
                }
            }
        }
    }

    fn send_text(&mut self, text: String) {
        let text = text.trim_end_matches(['\n', '\r']).to_string();
        if text.trim().is_empty() {
            return;
        }
        let targets: Vec<String> = if self.selected.is_empty() {
            self.peers.iter().filter(|p| p.online && !p.blocked).map(|p| p.id.clone()).collect()
        } else {
            vec![self.selected.clone()]
        };
        if targets.is_empty() {
            if let Some(ui) = self.ui() {
                ui.set_toast("没有在线设备".into());
            }
            return;
        }
        for id in targets {
            let node = self.node.clone();
            let text = text.clone();
            self.rt.spawn(async move {
                match node.send_text(&id, &text).await {
                    Ok(m) => post(move |app| app.upsert_message(m)),
                    Err(e) => {
                        let e = e.to_string();
                        log::warn!("send_text to {id} failed: {e}");
                        post(move |app| {
                            if let Some(ui) = app.ui() {
                                ui.set_toast(format!("发送失败：{e}").into());
                            }
                        });
                    }
                }
            });
        }
    }
}

/// Run `f` with the app on the UI thread (from any thread).
fn post(f: impl FnOnce(&mut App) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        with_app(f);
    });
}

pub(crate) fn theme_is_dark(theme: &str) -> bool {
    #[cfg(windows)]
    {
        win::theme_is_dark(theme)
    }
    #[cfg(not(windows))]
    {
        theme == "dark"
    }
}

fn data_dir() -> std::path::PathBuf {
    env("LANCAT_DATA_DIR").map(Into::into).unwrap_or_else(|| dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("lancat-slint-dev"))
}

fn main() {
    let _ = t0();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    #[cfg(windows)]
    slint::platform::set_platform(Box::new(win::Win32Platform::new())).expect("set platform");

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().thread_name("lancat-core").build().expect("tokio");
    let mut cfg = NodeConfig::new(data_dir()).with_env();
    if env("LANCAT_PORT").is_none() {
        cfg.port = 47840;
        cfg.udp_port = 47840;
    }
    if env("LANCAT_SLINT_DISCOVERY").as_deref() != Some("1") {
        cfg.enable_mdns = false;
        cfg.enable_udp = false;
    }
    log::info!("lancat-slint: data {:?} port {} mdns {} udp {}", cfg.data_dir, cfg.port, cfg.enable_mdns, cfg.enable_udp);
    let node = rt.block_on(Node::start(cfg)).expect("start lancat node");
    log::info!("node started in {} ms", t0().elapsed().as_millis());

    let ui = PanelWindow::new().expect("create panel");
    let settings = node.settings();
    let dark = theme_is_dark(&settings.theme);
    ui.global::<Theme>().set_dark(dark);
    #[cfg(windows)]
    if let Some(c) = win::accent_color() {
        ui.global::<Theme>().set_accent(c);
    }

    let peer_model = Rc::new(VecModel::<PeerItem>::default());
    let msg_model = Rc::new(VecModel::<MsgItem>::default());
    ui.set_peers(ModelRc::from(peer_model.clone()));
    ui.set_messages(ModelRc::from(msg_model.clone()));

    let mut app = App {
        ui: ui.as_weak(),
        node: node.clone(),
        rt: rt.handle().clone(),
        peers: node.peers(),
        peer_model,
        msgs: Vec::new(),
        msg_model,
        selected: String::new(),
        typing: Default::default(),
        last_typing_sent: None,
    };
    // first online peer is the default conversation
    app.rebuild_peers();
    if let Some(p) = app.peers.iter().find(|p| p.online && !p.blocked) {
        app.selected = p.id.clone();
    }
    app.update_header();
    app.load_messages();
    APP.with(|a| *a.borrow_mut() = Some(app));

    ui.on_select_peer(|id: SharedString| {
        with_app(|app| {
            app.selected = id.to_string();
            app.update_header();
            app.load_messages();
        });
    });
    ui.on_send_text(|t: SharedString| {
        with_app(|app| app.send_text(t.to_string()));
    });
    ui.on_typing(|| {
        with_app(|app| {
            if app.selected.is_empty() || app.last_typing_sent.is_some_and(|t| t.elapsed().as_secs() < 3) {
                return;
            }
            app.last_typing_sent = Some(Instant::now());
            app.node.send_typing(&app.selected, true);
        });
    });
    ui.on_refresh(|| {
        with_app(|app| {
            app.node.refresh_peers();
            if let Some(ui) = app.ui() {
                ui.set_refreshing(true);
                let w = ui.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
                    if let Some(ui) = w.upgrade() {
                        ui.set_refreshing(false);
                    }
                });
            }
        });
    });
    ui.on_escape(|| {
        #[cfg(windows)]
        win::request_hide();
    });
    ui.on_open_settings(|| {
        with_app(|app| {
            if let Some(ui) = app.ui() {
                ui.set_toast("设置页在阶段 4 补齐".into());
            }
        });
    });
    // toast auto-hide (2.6 s, like the old panel)
    {
        let w = ui.as_weak();
        let timer = Box::leak(Box::new(slint::Timer::default()));
        timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(2600), move || {
            if let Some(ui) = w.upgrade() {
                if !ui.get_toast().is_empty() {
                    ui.set_toast("".into());
                }
            }
        });
    }

    // core events → UI thread
    let mut rx = node.subscribe();
    rt.spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => post(move |app| app.on_core(ev)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => log::warn!("ui lagged {n} core events"),
                Err(_) => break,
            }
        }
    });

    // test hook: LANCAT_SLINT_AUTOSEND="text" sends once to the selected peer after 2 s (no real
    // input device needed; the test peer is a local throwaway instance)
    if let Some(text) = env("LANCAT_SLINT_AUTOSEND") {
        slint::Timer::single_shot(std::time::Duration::from_secs(2), move || {
            with_app(|app| app.send_text(text));
        });
    }

    ui.show().expect("show");
    log::info!("panel shown {} ms after start", t0().elapsed().as_millis());
    ui.run().expect("run");
    node.shutdown();
}
