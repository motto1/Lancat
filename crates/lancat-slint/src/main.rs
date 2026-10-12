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
mod dock;
#[cfg(target_os = "linux")]
mod wl;
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
    /// nothing was online at start: open the first peer that comes online
    auto_select: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    // try_borrow_mut：停靠回调可能在 App 方法里同步回到这里，嵌套那次跳过而不是 panic
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
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

pub(crate) fn human(n: u64) -> String {
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
                let online = peer.online && !peer.blocked;
                let id = peer.id.clone();
                match self.peers.iter_mut().find(|p| p.id == peer.id) {
                    Some(p) => *p = peer,
                    None => self.peers.push(peer),
                }
                self.rebuild_peers();
                #[cfg(windows)]
                dock::refresh_visual();
                if self.auto_select && online && self.selected.is_empty() {
                    self.auto_select = false;
                    self.selected = id;
                    self.update_header();
                    self.load_messages();
                }
            }
            CoreEvent::PeerRemoved { peer_id } => {
                self.peers.retain(|p| p.id != peer_id);
                if self.selected == peer_id {
                    self.selected.clear();
                    self.load_messages();
                }
                self.rebuild_peers();
            }
            CoreEvent::Message { message } => {
                #[cfg(windows)]
                dock::on_incoming(&message);
                self.upsert_message(message);
                #[cfg(windows)]
                dock::refresh_visual();
            }
            CoreEvent::MessageUpdate { message } => self.upsert_message(message),
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
                    // LANCAT_SLINT_THEME=light|dark：测试用，覆盖设置里的主题
    let dark = theme_is_dark(&env("LANCAT_SLINT_THEME").unwrap_or(settings.theme.clone()));
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

impl App {
    /// 拖放 / 附件：发给当前会话（「全部」时发给所有在线设备）。
    #[allow(dead_code)]
    fn send_files(&mut self, files: Vec<String>) {
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
        let node = self.node.clone();
        let paths: Vec<std::path::PathBuf> = files.into_iter().map(Into::into).collect();
        self.rt.spawn(async move {
            match node.send_files(&targets, &paths).await {
                Ok(ms) => post(move |app| {
                    for m in ms {
                        app.upsert_message(m);
                    }
                }),
                Err(e) => {
                    let e = e.to_string();
                    log::warn!("send_files failed: {e}");
                    post(move |app| {
                        if let Some(ui) = app.ui() {
                            ui.set_toast(format!("发送失败：{e}").into());
                        }
                    });
                }
            }
        });
    }

    /// 停靠打开面板：从 toast 打开时切到发消息的那个会话。
    #[allow(dead_code)]
    fn on_dock_open(&mut self, reason: &str) {
        if let Some(id) = reason.strip_prefix("toast:") {
            if self.peers.iter().any(|p| p.id == id) && self.selected != id {
                self.selected = id.to_string();
                self.update_header();
                self.load_messages();
            }
        }
        if !self.selected.is_empty() {
            let _ = self.node.mark_read(&self.selected);
        }
        if let Some(ui) = self.ui() {
            if reason == "tray" || reason.starts_with("toast:") {
                ui.invoke_focus_input();
            }
        }
    }
}

/// Run `f` with the app on the UI thread (from any thread).
fn post(f: impl FnOnce(&mut App) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        with_app(f);
    });
}

fn open_settings() {
    with_app(|app| {
        if let Some(ui) = app.ui() {
            ui.set_toast("设置页在阶段 4 补齐".into());
        }
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

pub(crate) fn data_dir() -> std::path::PathBuf {
    env("LANCAT_DATA_DIR").map(Into::into).unwrap_or_else(|| dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("lancat-slint-dev"))
}

fn main() {
    let _ = t0();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // 单实例：已有实例（同一数据目录）在跑就把命令行转给它然后退出
    #[cfg(windows)]
    if win::dock_mode() && dock::forward_to_running() {
        return;
    }

    #[cfg(windows)]
    slint::platform::set_platform(Box::new(win::Win32Platform::new())).expect("set platform");
    // 阶段 3 spike：LANCAT_SLINT_WL=1 用自写的 SCTK layer-shell 后端（默认仍是 winit）
    #[cfg(target_os = "linux")]
    if env("LANCAT_SLINT_WL").as_deref() == Some("1") {
        match wl::WlPlatform::new() {
            Ok(p) => slint::platform::set_platform(Box::new(p)).expect("set platform"),
            Err(e) => log::warn!("wayland layer-shell backend unavailable ({e}); using winit"),
        }
    }

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
    // LANCAT_SLINT_THEME=light|dark：测试用，覆盖设置里的主题
    let dark = theme_is_dark(&env("LANCAT_SLINT_THEME").unwrap_or(settings.theme.clone()));
    ui.global::<Theme>().set_dark(dark);
    // the Acrylic tint (also the opaque fallback with transparency effects off) follows the app theme
    #[cfg(windows)]
    win::refresh_material(&ui, dark);
    // accent 固定 #0a84ff（与旧版相同，不跟系统强调色；app.slint 的默认值）

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
        auto_select: true,
    };
    // first online peer is the default conversation
    app.rebuild_peers();
    if let Some(p) = app.peers.iter().find(|p| p.online && !p.blocked) {
        app.selected = p.id.clone();
        app.auto_select = false;
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
                #[cfg(windows)]
                dock::note_typed();
                return;
            }
            app.last_typing_sent = Some(Instant::now());
            #[cfg(windows)]
            dock::note_typed();
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
    ui.on_open_settings(open_settings);
    ui.on_header_pressed(|| {
        // 按住标题栏拖动可以把停靠条移到别的边（旧版 dock_move_start）
        #[cfg(windows)]
        dock::begin_move(false);
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
    #[cfg(windows)]
    if win::dock_mode() {
        let w1 = ui.as_weak();
        let w2 = ui.as_weak();
        dock::init(
            node.clone(),
            dock::Hooks {
                composer: Box::new(move || w1.upgrade().map(|u| (u.get_composer_focused(), !u.get_draft().is_empty())).unwrap_or((false, false))),
                on_open: Box::new(|reason| {
                    let r = reason.to_string();
                    with_app(move |app| app.on_dock_open(&r));
                }),
                on_close: Box::new(|| {}),
                on_drag_hint: Box::new(move |n| {
                    if let Some(u) = w2.upgrade() {
                        let target = with_app(|app| if app.selected.is_empty() { "所有在线设备".to_string() } else { app.peers.iter().find(|p| p.id == app.selected).map(|p| p.name.clone()).unwrap_or_default() }).unwrap_or_default();
                        u.set_drop_hint(match n {
                            Some(n) => format!("松开发送 {n} 个文件给 {target}").into(),
                            None => "".into(),
                        });
                    }
                }),
                on_drop: Box::new(|files, _pos| {
                    with_app(move |app| app.send_files(files));
                }),
                on_settings: Box::new(|| {
                    dock::open_panel("probe");
                    open_settings();
                }),
            },
        );
        let args: Vec<String> = std::env::args().skip(1).collect();
        dock::handle_args(&args, false);
    }
    ui.run().expect("run");
    node.shutdown();
}
