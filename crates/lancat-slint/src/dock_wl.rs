//! 停靠逻辑（Linux Wayland，自写 layer-shell 后端 src/wl.rs）：与 Windows 版 dock/mod.rs 同一套对外接口。
//!
//! Wayland 没有全局光标位置，所以这里不轮询光标，全靠 surface 的指针进出事件：
//! - 停靠条 enter → hover_delay_ms 后打开（收起时光标还在条上 → 离开一次之前不再悬停打开）；按下 → 立即打开；
//! - 面板 / 停靠条都 leave → collapse_delay_ms 后收起；输入框聚焦且（有草稿或 8 s 内打过字）、按着鼠标、
//!   钉住时不收，400 ms 复查（与 Windows 版、旧版 Panel.svelte 相同）；
//! - 位置：按输出的逻辑尺寸算「贴边 + 沿边偏移」（与旧版 dock::compute 的 Linux 分支相同：停靠条贴真正的屏幕边，
//!   exclusive zone -1，不被状态栏挤开）；
//! - 单实例：$XDG_RUNTIME_DIR 下按数据目录哈希命名的 unix socket，第二个进程把命令行写过去后退出。
//!
//! - 拖放（wl.rs 的 wl_data_device）：文件拖到停靠条 → 打开面板并显示拖放选择器；拖拽进行中不自动收起。
//!
//! 尚未做：消息 toast、拖动停靠条换边、托盘（StatusNotifierItem）。

use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lancat_core::Node;

use crate::wl::{self, Cmd, Place, StripLook};

pub const STRIP: f64 = 6.0;
pub const PANEL_W: f64 = 360.0;
pub const PANEL_H: f64 = 520.0;
const RECHECK_MS: u64 = 400;
const TYPED_HOLD: Duration = Duration::from_secs(8);

/// 与 Windows 版相同的钩子。
pub struct Hooks {
    pub composer: Box<dyn Fn() -> (bool, bool)>,
    pub on_open: Box<dyn Fn(&str)>,
    pub on_close: Box<dyn Fn()>,
    pub on_drag_hint: Box<dyn Fn(Option<usize>)>,
    pub on_drag_over: Box<dyn Fn(f32, f32)>,
    pub on_drop: Box<dyn Fn(Vec<String>, Option<(f32, f32)>)>,
    pub on_settings: Box<dyn Fn()>,
    #[allow(dead_code)]
    pub on_pinned: Box<dyn Fn(bool)>,
}

struct Dock {
    node: Arc<Node>,
    hooks: Hooks,
    visible: bool,
    pinned: bool,
    strip_in: bool,
    panel_in: bool,
    pressed: bool,
    hover_disarmed: bool,
    entered: bool,
    last_typed: Option<Instant>,
    hover_timer: slint::Timer,
    leave_timer: slint::Timer,
    pulse: slint::Timer,
    pulse_t0: Instant,
    look: Option<StripLook>,
    listener: Option<UnixListener>,
    ipc_timer: slint::Timer,
    /// 文件拖拽进行中（在停靠条或面板上）：不收起
    dragging: bool,
    drag_count: usize,
    /// 拖出停靠条后没进面板：一会儿后结束这次拖拽
    drag_timer: slint::Timer,
}

thread_local! {
    static DOCK: RefCell<Option<Dock>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Dock) -> R) -> Option<R> {
    DOCK.with(|d| d.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

fn hook(f: impl FnOnce(&Hooks)) {
    DOCK.with(|x| {
        if let Ok(b) = x.try_borrow() {
            if let Some(d) = b.as_ref() {
                f(&d.hooks);
            }
        }
    });
}

/// 自写 Wayland 后端在用、且没有 LANCAT_SLINT_NODOCK=1。
pub fn enabled() -> bool {
    wl::active() && std::env::var("LANCAT_SLINT_NODOCK").as_deref() != Ok("1")
}

// ------------------------------------------------------------------ geometry

/// (停靠条, 面板)
fn compute(d: &Dock) -> Option<(Place, Place)> {
    let s = d.node.settings();
    let outs = wl::outputs();
    let idx = if s.dock_monitor < outs.len() { s.dock_monitor } else { 0 };
    let &(mw, mh, _) = outs.get(idx)?;
    let (mw, mh) = (mw as f64, mh as f64);
    let edge = s.dock_edge.clone();
    let horizontal = edge == "top" || edge == "bottom";
    let along_f = s.dock_offset.clamp(0.0, 1.0);
    let len = s.dock_length.clamp(40.0, 2000.0).min(if horizontal { mw } else { mh }).round();
    let span = if horizontal { mw } else { mh };
    // 面板窗口 = 面板 + 四周 8px 外框（旧版 Linux：MARGIN 8，窗口离边 STRIP + 2 − 8 = 0）
    let fr = wl::FRAME as f64;
    let (pw, ph) = (PANEL_W + 2.0 * fr, PANEL_H + 2.0 * fr);
    let pspan = if horizontal { pw } else { ph };
    let start = ((span - len) * along_f).round();
    let pstart = (start + len / 2.0 - pspan / 2.0).clamp(0.0, (span - pspan).max(0.0)).round();
    let (sw, sh) = if horizontal { (len as u32, STRIP as u32) } else { (STRIP as u32, len as u32) };
    let strip = Place { edge: edge.clone(), along: start as i32, inset: 0, w: sw, h: sh, output: idx };
    let panel = Place { edge, along: pstart as i32, inset: (STRIP + 2.0 - fr).max(0.0) as i32, w: pw as u32, h: ph as u32, output: idx };
    Some((strip, panel))
}

// ------------------------------------------------------------------ public API (same as Windows)

pub fn init(node: Arc<Node>, hooks: Hooks) {
    let d = Dock {
        node,
        hooks,
        visible: false,
        pinned: false,
        strip_in: false,
        panel_in: false,
        pressed: false,
        hover_disarmed: false,
        entered: false,
        last_typed: None,
        hover_timer: Default::default(),
        leave_timer: Default::default(),
        pulse: Default::default(),
        pulse_t0: Instant::now(),
        look: None,
        listener: None,
        ipc_timer: Default::default(),
        dragging: false,
        drag_count: 0,
        drag_timer: Default::default(),
    };
    DOCK.with(|x| *x.borrow_mut() = Some(d));
    ipc_listen();
    apply_layout();
    log::info!("dock ready (wayland): edge {}", with(|d| d.node.settings().dock_edge).unwrap_or_default());
}

pub fn apply_layout() {
    let Some(Some((strip, panel, visible))) = with(|d| compute(d).map(|(s, p)| (s, p, d.visible))) else { return };
    wl::push(Cmd::Strip(Some(strip)));
    if visible {
        wl::push(Cmd::Panel(Some(panel)));
    }
    refresh_visual();
}

pub fn refresh_visual() {
    let Some(look) = with(|d| {
        let unread = d.node.peers().iter().any(|p| !p.blocked && p.unread > 0);
        let edge = d.node.settings().dock_edge;
        StripLook { edge, unread, open: d.visible, hovering: d.strip_in, accent: (10.0 / 255.0, 132.0 / 255.0, 1.0), phase: 0.0 }
    }) else {
        return;
    };
    let pulse_on = look.unread;
    with(|d| {
        let changed = d.look.as_ref().map(|l| (l.unread, l.open, l.hovering, &l.edge)) != Some((look.unread, look.open, look.hovering, &look.edge));
        if !changed {
            return;
        }
        d.look = Some(look.clone());
        if pulse_on && !d.pulse.running() {
            d.pulse_t0 = Instant::now();
            // ~12 fps 足够 1.8 s 的呼吸脉冲（与旧版相同）
            d.pulse.start(slint::TimerMode::Repeated, Duration::from_millis(80), pulse_tick);
        } else if !pulse_on {
            d.pulse.stop();
        }
        wl::push(Cmd::StripLook(look));
    });
}

fn pulse_tick() {
    with(|d| {
        if let Some(l) = d.look.as_mut() {
            l.phase = d.pulse_t0.elapsed().as_secs_f64();
            wl::push(Cmd::StripLook(l.clone()));
        }
    });
}

pub fn note_typed() {
    with(|d| d.last_typed = Some(Instant::now()));
}

pub fn set_pinned(p: bool) {
    with(|d| d.pinned = p);
}

/// 设置页的「显示器」下拉（输出的逻辑尺寸）
pub fn monitor_labels() -> Vec<String> {
    wl::outputs().iter().enumerate().map(|(i, &(w, h, _))| format!("显示器 {}（{w}×{h}）{}", i + 1, if i == 0 { " · 主" } else { "" })).collect()
}

/// 开机自启（XDG autostart 的 .desktop，与旧版 tauri-plugin-autostart 的 Linux 实现相同）
pub mod tray {
    fn path() -> Option<std::path::PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
        Some(base.join("autostart").join("Lancat.desktop"))
    }

    /// 隔离的测试实例（LANCAT_DATA_DIR）不碰开机自启
    pub fn test_instance() -> bool {
        std::env::var_os("LANCAT_DATA_DIR").is_some()
    }

    pub fn autostart_enabled() -> bool {
        path().is_some_and(|p| p.exists())
    }

    pub fn set_autostart(on: bool) {
        if test_instance() {
            return;
        }
        let Some(p) = path() else { return };
        if on {
            let exe = std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_else(|_| "lancat".into());
            let body = format!("[Desktop Entry]\nType=Application\nVersion=1.0\nName=Lancat\nComment=Lancat startup script\nExec={exe} --autostart\nStartupNotify=false\nTerminal=false\n");
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(&p, body) {
                log::warn!("autostart: {e}");
            }
        } else {
            let _ = std::fs::remove_file(&p);
        }
    }
}

#[allow(dead_code)]
pub fn pinned() -> bool {
    with(|d| d.pinned).unwrap_or(false)
}

pub fn is_visible() -> bool {
    with(|d| d.visible).unwrap_or(false)
}

pub fn open_panel(reason: &str) {
    let focus = reason == "tray" || reason == "shortcut" || reason.starts_with("toast:");
    let first = with(|d| {
        d.hover_timer.stop();
        let was = d.visible;
        if !was {
            d.visible = true;
            d.entered = reason == "hover" || reason == "drag";
            d.leave_timer.stop();
        }
        (!was, compute(d))
    });
    let Some((first, geo)) = first else { return };
    if first {
        if let Some((_, panel)) = geo {
            wl::push(Cmd::Panel(Some(panel)));
        }
        refresh_visual();
    }
    log::debug!("open_panel({reason})");
    hook(|h| (h.on_open)(reason));
    if focus {
        wl::push(Cmd::PanelFocus);
    }
}

pub fn hide_panel() {
    let Some(true) = with(|d| {
        if !d.visible {
            return false;
        }
        d.visible = false;
        d.leave_timer.stop();
        d.hover_timer.stop();
        d.panel_in = false;
        d.hover_disarmed = d.strip_in;
        true
    }) else {
        return;
    };
    log::debug!("hide_panel");
    wl::push(Cmd::Panel(None));
    hook(|h| (h.on_close)());
    refresh_visual();
}

#[allow(dead_code)]
pub fn toggle_panel() {
    if is_visible() {
        hide_panel();
    } else {
        open_panel("tray");
    }
}

/// 收到消息：Wayland 版的 toast 在阶段 4 做；现在只让停靠条的未读脉冲跟上。
pub fn on_incoming(m: &lancat_core::Message) {
    if !m.outgoing {
        log::debug!("message from {} ({}): toast not implemented on wayland yet", m.peer_id, m.kind);
    }
}

/// 拖动停靠条换边：Wayland 下没有全局光标，阶段 4 再做（按停靠条 surface 的指针拖动）。
pub fn begin_move(_from_strip: bool) {}

// ------------------------------------------------------------------ events from wl.rs

pub fn wl_outputs_changed() {
    // 输出变化发生在事件分发中：下一轮再算（避免和 init 前的往返重入）
    slint::Timer::single_shot(Duration::ZERO, apply_layout);
}

pub fn wl_strip_pointer(inside: bool) {
    let probe = std::env::var_os("LANCAT_PROBE").is_some();
    let act = with(|d| {
        d.strip_in = inside;
        if !inside {
            d.hover_timer.stop();
            d.hover_disarmed = false;
            return false;
        }
        if d.visible {
            d.entered = true;
            d.leave_timer.stop();
            return false;
        }
        !d.hover_disarmed && !probe
    });
    if act == Some(true) {
        let delay = with(|d| d.node.settings().hover_delay_ms).unwrap_or(150);
        with(|d| d.hover_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(delay), || open_panel("hover")));
    }
    if !inside {
        schedule_leave();
    }
    refresh_visual();
}

pub fn wl_strip_press() {
    if std::env::var_os("LANCAT_PROBE").is_none() {
        open_panel("hover");
    }
}

pub fn wl_panel_pointer(inside: bool) {
    with(|d| {
        d.panel_in = inside;
        if inside {
            d.entered = true;
            d.leave_timer.stop();
        }
    });
    if !inside {
        schedule_leave();
    }
}

pub fn wl_panel_press(down: bool) {
    with(|d| d.pressed = down);
}

pub fn wl_panel_keyboard(_focused: bool) {}

// ---- 拖放（与 Windows 版 dock::on_drag 同一套语义）

pub fn wl_drag_enter(panel: bool, x: f32, y: f32) {
    let n = with(|d| {
        d.dragging = true;
        d.drag_timer.stop();
        d.drag_count
    })
    .unwrap_or(0);
    if !panel {
        // 文件碰到停靠条：立刻打开（旧版 open_panel("drag")）
        open_panel("drag");
    }
    hook(|h| (h.on_drag_hint)(Some(n)));
    if panel {
        hook(|h| (h.on_drag_over)(x, y));
    }
}

/// 预读到的项数（「拖到头像上发送 N 项」）
pub fn wl_drag_count(n: usize) {
    let on = with(|d| {
        d.drag_count = n;
        d.dragging
    })
    .unwrap_or(false);
    if on {
        hook(|h| (h.on_drag_hint)(Some(n)));
    }
}

pub fn wl_drag_motion(x: f32, y: f32) {
    hook(|h| (h.on_drag_over)(x, y));
}

fn end_drag() {
    with(|d| {
        d.dragging = false;
        d.drag_count = 0;
        d.drag_timer.stop();
    });
    hook(|h| (h.on_drag_hint)(None));
    schedule_leave();
}

pub fn wl_drag_leave(panel: bool) {
    if panel {
        end_drag();
    } else {
        // 从停靠条拖向正在弹出的面板：给 1.2 s 进入面板，否则算拖走了
        with(|d| d.drag_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(1200), end_drag));
    }
}

pub fn wl_drop(panel: bool, files: Vec<String>, pos: Option<(f32, f32)>) {
    with(|d| {
        d.dragging = false;
        d.drag_count = 0;
        d.drag_timer.stop();
    });
    hook(|h| (h.on_drag_hint)(None));
    if files.is_empty() {
        schedule_leave();
        return;
    }
    if !panel {
        open_panel("drag");
    }
    hook(move |h| (h.on_drop)(files, pos));
}

fn schedule_leave() {
    let delay = with(|d| {
        if !d.visible || d.panel_in || d.strip_in || !d.entered {
            return None;
        }
        Some(d.node.settings().collapse_delay_ms)
    })
    .flatten();
    if let Some(ms) = delay {
        with(|d| d.leave_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(ms), leave_check));
    }
}

fn leave_check() {
    let hold = with(|d| {
        if !d.visible || d.panel_in || d.strip_in {
            return None;
        }
        let (focused, draft) = (d.hooks.composer)();
        let typed = d.last_typed.is_some_and(|t| t.elapsed() < TYPED_HOLD);
        Some(d.pinned || (focused && (draft || typed)) || d.pressed || d.dragging)
    })
    .flatten();
    match hold {
        Some(true) => {
            with(|d| d.leave_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(RECHECK_MS), leave_check));
        }
        Some(false) => hide_panel(),
        None => {}
    }
}

// ------------------------------------------------------------------ single instance

fn socket_path() -> std::path::PathBuf {
    let dir = crate::data_dir();
    let h = dir.to_string_lossy().to_lowercase().bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join(format!("lancat-slint-{h:016x}.sock"))
}

/// 已有实例在跑：把命令行转给它并返回 true（调用方退出）。
pub fn forward_to_running() -> bool {
    let p = socket_path();
    let Ok(mut s) = UnixStream::connect(&p) else { return false };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let _ = s.write_all(args.join("\n").as_bytes());
    log::info!("forwarded {args:?} to the running instance");
    true
}

fn ipc_listen() {
    let p = socket_path();
    let _ = std::fs::remove_file(&p); // 上次崩溃留下的
    let Ok(l) = UnixListener::bind(&p) else {
        log::warn!("single-instance socket {p:?} could not be bound");
        return;
    };
    let _ = l.set_nonblocking(true);
    with(|d| {
        d.listener = Some(l);
        d.ipc_timer.start(slint::TimerMode::Repeated, Duration::from_millis(300), ipc_poll);
    });
}

fn ipc_poll() {
    let mut got: Vec<Vec<String>> = Vec::new();
    with(|d| {
        let Some(l) = d.listener.as_ref() else { return };
        while let Ok((mut s, _)) = l.accept() {
            let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            got.push(buf.split('\n').filter(|a| !a.is_empty()).map(String::from).collect());
        }
    });
    for args in got {
        handle_args(&args, true);
    }
}

pub fn handle_args(args: &[String], second: bool) {
    let has = |f: &str| args.iter().any(|a| a == f);
    // 测试：--script "cmd|peer|arg"（转给正在运行的实例，同 LANCAT_SLINT_SCRIPT 的一步）
    if let Some(i) = args.iter().position(|a| a == "--script") {
        if let Some(step) = args.get(i + 1) {
            crate::script_step(step);
        }
        return;
    }
    if has("--settings") {
        hook(|h| (h.on_settings)());
    } else if has("--hide-panel") {
        hide_panel();
    } else if has("--open-panel") {
        open_panel("probe");
    } else if second {
        open_panel("tray");
    }
}
