//! Android 入口（Slint 官方 android-activity 后端，NativeActivity）。界面与桌面是同一套 .slint 和同一个 App：
//! `Fx.touch` 打开触屏适配（铺满竖屏、长按代替右键、点外面关弹层、分享选择器点头像发送）。
//!
//! * 节点（lancat-core）每个进程只起一次，放在全局 CORE 里：Activity 被销毁 / 重建时界面重来，节点、
//!   前台服务（组播锁 + WakeLock）和通知线程一直在，后台照样能被发现、能收文件。
//! * Slint 在 Android 上只有一个窗口：设置页与面板轮流挂到同一个窗口上（WindowInner::set_component）。
//! * Java 侧事件（分享进来的文件、通知点击、返回键、前后台）由界面线程的定时器轮询（jvm::poll）。

pub mod dock;
pub mod jvm;
mod notify;

use crate::*;
use i_slint_core::item_tree::ItemTreeRc;
use i_slint_core::window::WindowInner;
use std::sync::OnceLock;

struct Core {
    rt: tokio::runtime::Runtime,
    node: Arc<Node>,
}

static CORE: OnceLock<Core> = OnceLock::new();

thread_local! {
    static PANEL: RefCell<Option<slint::Weak<PanelWindow>>> = const { RefCell::new(None) };
    static PANEL_RC: RefCell<Option<ItemTreeRc>> = const { RefCell::new(None) };
    static SETTINGS_RC: RefCell<Option<ItemTreeRc>> = const { RefCell::new(None) };
    static SETTINGS_SHOWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// 系统分享进来、还没选设备的文件
    static PENDING: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static POLL: slint::Timer = slint::Timer::default();
    static LIGHT_BARS: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[no_mangle]
fn android_main(app: slint::android::AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("lancat"));
    std::panic::set_hook(Box::new(|info| {
        log::error!("panic: {info}");
    }));
    let _ = crate::t0();
    if let Err(e) = jvm::init() {
        log::error!("jni init: {e}");
    }
    if let Err(e) = slint::android::init(app) {
        log::error!("slint android init: {e}");
        return;
    }
    let core = match core() {
        Ok(c) => c,
        Err(e) => {
            log::error!("start lancat node: {e}");
            jvm::notify_message("__err__", "Lancat 无法启动", &e);
            return;
        }
    };
    run_ui(core);
    log::info!("android_main: activity gone, node keeps running");
}

fn core() -> Result<&'static Core, String> {
    if let Some(c) = CORE.get() {
        return Ok(c);
    }
    let files = jvm::files_dir();
    let dir = if files.is_empty() { std::env::temp_dir() } else { std::path::PathBuf::from(files) };
    // data_dir()（贴纸缓存等）读这个变量；此时还没有别的线程
    std::env::set_var("LANCAT_DATA_DIR", &dir);
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().thread_name("lancat-core").build().map_err(|e| e.to_string())?;
    let cfg = NodeConfig::new(dir);
    let wanted = cfg.port;
    let node = rt.block_on(Node::start(cfg)).map_err(|e| format!("{e:#}"))?;
    let mut s = node.settings();
    let mut changed = false;
    if s.device_name.trim().is_empty() {
        let n = jvm::device_name();
        if !n.is_empty() {
            s.device_name = n;
            changed = true;
        }
    }
    let rd = jvm::receive_dir();
    if !rd.is_empty() && s.receive_dir != rd {
        s.receive_dir = rd;
        changed = true;
    }
    if changed {
        if let Err(e) = node.set_settings(s) {
            log::warn!("settings: {e}");
        }
    }
    log::info!("node started in {} ms, port {}", t0().elapsed().as_millis(), node.port());
    diag::init(node.clone(), rt.handle().clone(), wanted);
    notify::start(node.clone(), rt.handle().clone());
    jvm::start_service();
    let _ = CORE.set(Core { rt, node });
    Ok(CORE.get().unwrap())
}

fn run_ui(core: &'static Core) {
    let node = core.node.clone();
    let ui = match PanelWindow::new() {
        Ok(u) => u,
        Err(e) => {
            log::error!("create panel: {e}");
            return;
        }
    };
    install_fx!(ui, false);
    compose::set_panel(ui.as_weak());
    ui.global::<Theme>().set_native_glass(false);
    PANEL.with(|p| *p.borrow_mut() = Some(ui.as_weak()));
    PANEL_RC.with(|p| *p.borrow_mut() = Some(WindowInner::from_pub(ui.window()).component()));
    let settings = node.settings();
    let peer_model = Rc::new(VecModel::<PeerItem>::default());
    let msg_model = Rc::new(VecModel::<MsgItem>::default());
    ui.set_peers(ModelRc::from(peer_model.clone()));
    ui.set_messages(ModelRc::from(msg_model.clone()));
    ui.set_me_name(node.me().name.into());
    let app = App {
        ui: ui.as_weak(),
        node: node.clone(),
        rt: core.rt.handle().clone(),
        peers: node.peers(),
        peer_model,
        msgs: Vec::new(),
        has_more: false,
        cache: HashMap::new(),
        arrivals: Default::default(),
        loading_older: false,
        msg_model,
        current: String::new(),
        broadcast: false,
        selected: Vec::new(),
        typing: Default::default(),
        typing_timer: Default::default(),
        progress: Default::default(),
        last_typing_sent: None,
        typing_peer: None,
        refreshing_until: None,
        dialog_open: false,
        visible: true,
        toast_timer: Default::default(),
        release_timer: Default::default(),
        emoji: emoji::EmojiState::new(node.clone(), &settings),
        settings: None,
        stick: true,
    };
    APP.with(|a| *a.borrow_mut() = Some(app));
    with_app(|app| {
        app.apply_theme(&settings);
        app.rebuild_peers();
        if let Some(id) = app.peers.first().map(|p| p.id.clone()) {
            app.open(&id);
        } else {
            app.update_header();
        }
        if let Some(ui) = app.ui() {
            ui.set_panel_visible(true);
        }
        app.diag_status(diag::status(), diag::last());
    });
    wire_ui(&ui);
    ui.on_pick_touch(|x, y, up| {
        with_app(|app| app.pick_touch(x, y, up));
    });
    slint::Timer::single_shot(Duration::from_millis(1500), || {
        with_app(|app| app.emoji.prewarm());
    });
    system_theme_watch();

    // core events → UI thread（这一个界面的生命周期内）
    let mut rx = node.subscribe();
    let sub = core.rt.spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => post(move |app| app.on_core(ev)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => log::warn!("ui lagged {n} core events"),
                Err(_) => break,
            }
        }
    });

    POLL.with(|t| t.start(slint::TimerMode::Repeated, Duration::from_millis(150), poll_java));
    if let Err(e) = ui.show() {
        log::error!("show: {e}");
    }
    log::info!("panel shown {} ms after start", t0().elapsed().as_millis());
    if let Err(e) = slint::run_event_loop() {
        log::error!("event loop: {e}");
    }
    // Activity 销毁：拆掉这一个界面（节点留着）
    POLL.with(|t| t.stop());
    sub.abort();
    PENDING.with(|p| p.borrow_mut().clear());
    SETTINGS_SHOWN.with(|s| s.set(false));
    let old = APP.with(|a| a.borrow_mut().take());
    drop(old);
    SETTINGS_RC.with(|p| *p.borrow_mut() = None);
    PANEL_RC.with(|p| *p.borrow_mut() = None);
    PANEL.with(|p| *p.borrow_mut() = None);
    drop(ui);
}

// ------------------------------------------------------------------ 设置页 ↔ 面板（同一个窗口）

/// SettingsCtl::new 里调用：记下设置页的组件，窗口先挂回面板（show_settings 时再换过去）
pub fn settings_created(win: &SettingsWindow) {
    // window() 让设置页拿到（共用的）窗口适配器，并把窗口的组件设成自己
    let rc = WindowInner::from_pub(win.window()).component();
    SETTINGS_RC.with(|p| *p.borrow_mut() = Some(rc));
    if !SETTINGS_SHOWN.with(|s| s.get()) {
        mount(&PANEL_RC);
    }
}

fn mount(which: &'static std::thread::LocalKey<RefCell<Option<ItemTreeRc>>>) {
    // 不经 with_app：这里常在 App 方法里被调用（with_app 不能嵌套）
    let Some(ui) = PANEL.with(|p| p.borrow().as_ref().and_then(|w| w.upgrade())) else { return };
    which.with(|p| {
        if let Some(rc) = p.borrow().as_ref() {
            WindowInner::from_pub(ui.window()).set_component(rc);
        }
    });
}

pub fn show_settings() {
    SETTINGS_SHOWN.with(|s| s.set(true));
    mount(&SETTINGS_RC);
}

pub fn show_panel() {
    if !SETTINGS_SHOWN.with(|s| s.replace(false)) {
        return;
    }
    mount(&PANEL_RC);
}

pub fn settings_shown() -> bool {
    SETTINGS_SHOWN.with(|s| s.get())
}

// ------------------------------------------------------------------ Java 事件

fn poll_java() {
    // 状态栏图标颜色跟主题
    if let Some(dark) = with_app(|app| app.ui().map(|u| u.global::<Theme>().get_dark())).flatten() {
        if LIGHT_BARS.with(|l| l.replace(Some(!dark))) != Some(!dark) {
            jvm::set_light_bars(!dark);
        }
    }
    for ev in jvm::poll() {
        let Some(kind) = ev.first() else { continue };
        let args: Vec<String> = ev[1..].to_vec();
        log::info!("java event {kind} ({} args)", args.len());
        with_app(|app| app.on_java(kind, args));
    }
    // 分享选择器开着时设备上下线：槽位跟着变
    let n = PENDING.with(|p| p.borrow().len());
    if n > 0 {
        with_app(|app| {
            if app.ui().is_some_and(|u| u.get_dragging()) {
                app.drag_hint(Some(n));
            }
        });
    }
}

impl App {
    fn on_java(&mut self, kind: &str, args: Vec<String>) {
        match kind {
            "share" => {
                let files: Vec<String> = args.into_iter().filter(|a| !a.is_empty()).collect();
                if files.is_empty() {
                    self.flash("没有读到要分享的文件");
                    return;
                }
                show_panel();
                if let Some(ui) = self.ui() {
                    ui.set_emoji_open(false);
                    ui.set_attach_open(false);
                    ui.set_menu_peer("".into());
                    ui.set_add_open(false);
                }
                let n = files.len();
                PENDING.with(|p| *p.borrow_mut() = files);
                self.drag_hint(Some(n));
            }
            "copying" => {
                show_panel();
                self.flash(format!("正在读取 {} 项…", args.first().map(|s| s.as_str()).unwrap_or("")));
            }
            "sharetext" => {
                show_panel();
                let text = args.join("\t");
                if let Some(ui) = self.ui() {
                    ui.set_draft(text.into());
                    ui.invoke_focus_input();
                }
                self.flash("已填入输入框：选好设备后点发送");
            }
            "open" => {
                let id = args.first().cloned().unwrap_or_default();
                if id == dock::DIAG_TOAST {
                    slint::Timer::single_shot(Duration::ZERO, || open_settings(true));
                    return;
                }
                show_panel();
                if self.peers.iter().any(|p| p.id == id) {
                    self.open(&id);
                }
            }
            "back" => self.back(),
            "resume" => {
                self.set_visible(true);
                // 首次授权存储权限后，接收目录可能从应用私有目录换成 Download/Lancat
                let rd = jvm::receive_dir();
                let mut s = self.node.settings();
                if !rd.is_empty() && s.receive_dir != rd {
                    s.receive_dir = rd;
                    let _ = self.node.set_settings(s);
                }
                let rt = self.rt.clone();
                rt.spawn(async {
                    let _ = diag::refresh().await;
                });
            }
            "pause" => {
                let sharing = PENDING.with(|p| !p.borrow().is_empty());
                if !sharing {
                    self.set_visible(false);
                }
            }
            _ => {}
        }
    }

    /// 返回键：设置页 → 面板；关掉分享选择器 / 弹层；否则回到桌面（节点继续在后台）
    fn back(&mut self) {
        if settings_shown() {
            show_panel();
            return;
        }
        let Some(ui) = self.ui() else { return };
        if ui.get_dragging() {
            self.cancel_share();
            return;
        }
        if ui.get_emoji_open() || ui.get_attach_open() || !ui.get_menu_peer().is_empty() || ui.get_add_open() {
            ui.set_emoji_open(false);
            ui.set_attach_open(false);
            ui.set_menu_peer("".into());
            ui.set_add_open(false);
            return;
        }
        jvm::move_to_back();
    }

    fn cancel_share(&mut self) {
        PENDING.with(|p| p.borrow_mut().clear());
        self.drag_hint(None);
        self.flash("已取消分享");
    }

    /// 分享选择器上的触摸（外壳坐标 = 窗口逻辑坐标，Android 上 frame 为 0）
    fn pick_touch(&mut self, x: f32, y: f32, up: bool) {
        if PENDING.with(|p| p.borrow().is_empty()) {
            if up {
                self.drag_hint(None);
            }
            return;
        }
        self.drag_over(x, y);
        if !up {
            return;
        }
        let key = self.drop_hit(x, y);
        if key.is_empty() {
            self.cancel_share();
            return;
        }
        let files = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
        self.drop_files(files, Some((x, y)));
        self.drag_hint(None);
    }
}

// ------------------------------------------------------------------ 权限与诊断（Android）

fn check(id: &str, title: &str, status: &str, summary: impl Into<String>) -> diag::Check {
    diag::Check { id: id.into(), title: title.into(), status: status.into(), summary: summary.into(), detail: vec![], help: None, fix: None, inbound: false }
}

fn fix_info(id: &str, label: &str) -> Option<diag::FixInfo> {
    Some(diag::FixInfo { id: id.into(), label: label.into(), admin: false, confirm: None })
}

/// 手机上要看的：Wi-Fi、组播锁（自动发现）、后台服务、通知权限、电池优化
pub fn diag_checks() -> Vec<diag::Check> {
    let info: HashMap<String, String> = jvm::diag_info().into_iter().collect();
    let on = |k: &str| info.get(k).is_some_and(|v| v == "1");
    let net = info.get("net").cloned().unwrap_or_default();
    let mut v = vec![];
    let mut c = match net.as_str() {
        "wifi" | "ethernet" => check("android_net", "网络", "ok", format!("已连接{}{}", if net == "wifi" { " Wi-Fi" } else { "有线网络" }, info.get("ssid").filter(|s| !s.is_empty()).map(|s| format!("「{s}」")).unwrap_or_default())),
        "cellular" => check("android_net", "网络", "warn", "当前用的是移动数据，局域网里的设备发现不了"),
        _ => check("android_net", "网络", "error", "没有连接网络"),
    };
    if c.status != "ok" {
        c.help = Some("连上和电脑同一个 Wi-Fi（或手机热点），然后点重新检查".into());
        c.fix = fix_info("wifi", "打开 Wi-Fi 设置");
    }
    c.inbound = true;
    v.push(c);
    let mut c = if on("service") && on("mlock") {
        check("android_service", "后台服务与组播", "ok", "后台服务运行中，已持有 Wi-Fi 组播锁和唤醒锁（后台也能被发现、能收文件）")
    } else {
        let mut c = check("android_service", "后台服务与组播", "warn", "后台服务没有运行：切到后台后可能发现不了设备、收不到文件");
        c.fix = fix_info("service", "启动后台服务");
        c
    };
    c.inbound = true;
    v.push(c);
    let mut c = if on("notif") {
        check("android_notif", "通知", "ok", "已允许通知（新消息、传输进度、完成提醒）")
    } else {
        let mut c = check("android_notif", "通知", "warn", "通知被关闭：看不到新消息和传输进度");
        c.fix = fix_info("notif", "允许通知");
        c
    };
    c.help = Some("在系统设置里允许 Lancat 发送通知".into());
    v.push(c);
    let mut c = if on("battery") {
        check("android_battery", "电池优化", "ok", "已允许在后台运行（不受电池优化限制）")
    } else {
        let mut c = check("android_battery", "电池优化", "unknown", "受电池优化限制：手机休眠一段时间后可能收不到文件");
        c.fix = fix_info("battery", "允许后台运行");
        c
    };
    c.help = Some("部分手机还要在系统的「应用启动管理 / 后台运行」里允许 Lancat".into());
    v.push(c);
    v
}

/// 诊断里的「修复」：打开对应的系统页面 / 申请权限。返回提示文字；不认识的 id 返回 None
pub fn run_fix(id: &str) -> Option<String> {
    let msg = match id {
        "wifi" => "已打开 Wi-Fi 设置，连好后回来点重新检查",
        "service" => "已启动后台服务",
        "notif" => "已请求通知权限",
        "battery" => "已打开电池优化设置，允许后回来点重新检查",
        _ => return None,
    };
    jvm::fix(id);
    Some(msg.into())
}
