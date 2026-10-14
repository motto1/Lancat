//! 停靠逻辑（Windows）：把 Slint 面板挂到与旧版相同的停靠条 / toast / 拖放 / 单实例上。
//!
//! 与 src-tauri/src/dock.rs 的对应关系（行为保持一致，实现更直接）：
//! - 停靠条 `strip`、消息 toast `toast`、拖放 `drop` 是旧版 native_dock 的 Win32 代码原样移植；
//! - 面板不是 WebView：没有创建/预热/销毁/挂起，收起就是整窗淡出 + 停放（WS_EX_TRANSPARENT，alpha 0），
//!   打开是淡入，见 `win::panel_show / panel_hide`；
//! - 光标轮询不再开线程：在 UI 线程上用 slint::Timer（附近/打开时 80 ms，远离时 300 ms，拖动时 16 ms）；
//! - 旧版前端 Panel.svelte 的收起判断（离开后 collapse_delay_ms；草稿 / 输入框聚焦且 8 s 内打过字 /
//!   固定 / 拖拽时不收，400 ms 复查）搬到这里；
//! - 单实例：命名互斥量 + 消息窗口（WM_COPYDATA 传参），`--open-panel` / `--hide-panel` / `--settings`。
//!   互斥量名带数据目录的哈希，所以测试实例（LANCAT_DATA_DIR 不同）互不影响，也不碰正式版 Lancat。
//!
//! 全部函数只在 UI 线程调用（Win32 窗口过程和 Slint 定时器都在同一个消息循环里）。

pub mod drop;
pub mod strip;
pub mod toast;
pub mod tray;

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lancat_core::Node;
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const STRIP: f64 = 6.0;
pub const PANEL_W: f64 = 360.0;
pub const PANEL_H: f64 = 520.0;
/// 光标在停靠条这个范围内（逻辑 px）算“接近”，轮询提到 80 ms。
const NEAR_PX: f64 = 240.0;
const POLL_FAR_MS: u64 = 300;
const POLL_NEAR_MS: u64 = 80;
const POLL_MOVE_MS: u64 = 16;
/// 收起被挡住（草稿、刚打过字…）时的复查间隔。
const RECHECK_MS: u64 = 400;
/// 输入框最近打过字的保持时间（旧版 ui.recentlyTyped）。
const TYPED_HOLD: Duration = Duration::from_secs(8);

/// 物理像素、虚拟桌面坐标。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn contains(&self, px: f64, py: f64, slop: f64) -> bool {
        px >= self.x - slop && px < self.x + self.w + slop && py >= self.y - slop && py < self.y + self.h + slop
    }
}

/// 停靠条外观（与旧版 native_dock::DockVisual 相同）。
#[derive(Clone, Debug, PartialEq)]
pub struct DockVisual {
    pub edge: String,
    pub unread: bool,
    pub open: bool,
    pub accent: (f64, f64, f64),
    pub dark: bool,
}

impl Default for DockVisual {
    fn default() -> Self {
        Self { edge: "right".into(), unread: false, open: false, accent: (10.0 / 255.0, 132.0 / 255.0, 1.0), dark: false }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Geo {
    pub dock: Rect,
    pub panel: Rect,
    pub scale: f64,
}

#[derive(Clone)]
struct Monitor {
    full: RECT,
    work: RECT,
    scale: f64,
}

/// 拖动停靠条（按下停靠条或面板标题栏后移动）。
struct Move {
    from_strip: bool,
    start: (f64, f64),
    moved: bool,
    monitors: Vec<Monitor>,
    settings: lancat_core::Settings,
}

/// 拖放事件（drop.rs → 这里，已离开 COM 调用）。坐标：面板客户区物理像素（拖到头像上发给该设备：阶段 4）。
#[allow(dead_code)]
pub enum DragEvent {
    Enter { panel: bool, n: usize, x: i32, y: i32 },
    Over { x: i32, y: i32 },
    Leave { panel: bool },
    Drop { panel: bool, files: Vec<String>, x: i32, y: i32 },
}

/// 面板这一侧（main.rs 的 App）提供的钩子：收起前问它能不能收、拖放结果交给它。
pub struct Hooks {
    /// 草稿非空 / 输入框聚焦 等“别收起”的界面状态：(输入框聚焦, 草稿非空)
    pub composer: Box<dyn Fn() -> (bool, bool)>,
    /// 打开面板：reason 为 "toast:<peer>" 时切到该会话
    pub on_open: Box<dyn Fn(&str)>,
    pub on_close: Box<dyn Fn()>,
    /// 拖放：Some(n) 进入（n 个文件）/ None 离开
    pub on_drag_hint: Box<dyn Fn(Option<usize>)>,
    /// 松手：文件 + 面板客户区逻辑坐标（停靠条上松手时为 None）
    pub on_drop: Box<dyn Fn(Vec<String>, Option<(f32, f32)>)>,
    /// 拖动中：面板客户区逻辑坐标（命中测试 DropPicker 槽位）
    pub on_drag_over: Box<dyn Fn(f32, f32)>,
    /// `--settings`
    pub on_settings: Box<dyn Fn()>,
    /// 钉住状态变化（标题栏图钉、托盘菜单）
    pub on_pinned: Box<dyn Fn(bool)>,
}

struct Dock {
    node: Arc<Node>,
    hooks: Hooks,
    visible: bool,
    pinned: bool,
    hovering: bool,
    /// 收起时光标还停在停靠条上：离开一次之前不再悬停弹出（“进入触发”）
    hover_disarmed: bool,
    hover_since: Option<Instant>,
    geo: Option<Geo>,
    /// 本次打开后光标进过面板（hover/drag 打开时一开始就算进过）
    entered: bool,
    inside: bool,
    leave_since: Option<Instant>,
    last_typed: Option<Instant>,
    left_down: bool,
    press_outside: bool,
    dragging_files: bool,
    mv: Option<Move>,
    move_place: Option<(String, f64, usize)>,
    poll_ms: u64,
    /// 打开前的前台窗口（只有主动拿焦点的打开才记），收起时还给它
    prev_fg: Option<isize>,
}

thread_local! {
    static DOCK: RefCell<Option<Dock>> = const { RefCell::new(None) };
    static POLL: slint::Timer = slint::Timer::default();
}

/// `try_borrow_mut`：Win32 调用可能同步重入窗口过程，嵌套的那次直接跳过（与旧版 strip 相同的约定）。
fn with<R>(f: impl FnOnce(&mut Dock) -> R) -> Option<R> {
    DOCK.with(|d| d.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

pub fn left_button_down() -> bool {
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

fn cursor() -> Option<(f64, f64)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some((p.x as f64, p.y as f64))
}

// ------------------------------------------------------------------ monitors / geometry

unsafe extern "system" fn enum_mon(h: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
    let out = &mut *(lp.0 as *mut Vec<(bool, Monitor)>);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if GetMonitorInfoW(h, &mut mi).as_bool() {
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        out.push((mi.dwFlags & 1 != 0, // MONITORINFOF_PRIMARY
            Monitor { full: mi.rcMonitor, work: mi.rcWork, scale: dx as f64 / 96.0 }));
    }
    BOOL(1)
}

/// 应用的显示器编号：0 = 主显示器，其余按系统顺序（与旧版 monitors_ordered 相同）。
fn monitors() -> Vec<Monitor> {
    let mut v: Vec<(bool, Monitor)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_mon), LPARAM(&mut v as *mut _ as isize));
    }
    v.sort_by_key(|(primary, _)| !*primary);
    v.into_iter().map(|(_, m)| m).collect()
}

/// 设置页「显示器」下拉：名称（宽×高）· 主（0 = 主显示器）
pub fn monitor_labels() -> Vec<String> {
    monitors()
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let (w, h) = (m.full.right - m.full.left, m.full.bottom - m.full.top);
            format!("显示器 {}（{w}×{h}）{}", i + 1, if i == 0 { " · 主" } else { "" })
        })
        .collect()
}

fn hover_delay() -> u64 {
    with(|d| d.node.settings().hover_delay_ms).unwrap_or(150)
}

fn current_edge(d: &Dock) -> String {
    d.move_place.as_ref().map(|p| p.0.clone()).unwrap_or_else(|| settings_now(d).dock_edge)
}

fn settings_now(d: &Dock) -> lancat_core::Settings {
    d.mv.as_ref().map(|m| m.settings.clone()).unwrap_or_else(|| d.node.settings())
}

/// 与旧版 dock::compute 的 Windows 分支相同：停靠条放在工作区内（不压任务栏）。
fn compute(d: &Dock) -> Option<Geo> {
    let s = settings_now(d);
    let (edge_now, offset_now, monitor_now) = d.move_place.clone().unwrap_or((s.dock_edge.clone(), s.dock_offset, s.dock_monitor));
    let list = d.mv.as_ref().map(|m| m.monitors.clone()).unwrap_or_else(monitors);
    let m = list.get(monitor_now).or(list.first())?.clone();
    let sc = m.scale;
    let (mx, my, mw, mh) = (m.work.left as f64, m.work.top as f64, (m.work.right - m.work.left) as f64, (m.work.bottom - m.work.top) as f64);
    let (ax, ay, aw, ah) = (mx, my, mw, mh);
    let t = (STRIP * sc).round();
    let along = offset_now.clamp(0.0, 1.0);
    let pw = (PANEL_W * sc).round();
    let ph = (PANEL_H * sc).round();
    let gap = (2.0 * sc).round();
    let horizontal = edge_now == "top" || edge_now == "bottom";
    let len = (s.dock_length.clamp(40.0, 2000.0) * sc).round().min(if horizontal { mw } else { mh });
    let clamp = |v: f64, lo: f64, hi: f64| if hi < lo { lo } else { v.clamp(lo, hi) };
    let (dock, panel) = match edge_now.as_str() {
        "left" => {
            let dy = my + (mh - len) * along;
            let py = clamp(dy + len / 2.0 - ph / 2.0, ay, ay + ah - ph);
            (Rect { x: mx, y: dy, w: t, h: len }, Rect { x: mx + t + gap, y: py, w: pw, h: ph })
        }
        "top" => {
            let dx = mx + (mw - len) * along;
            let px = clamp(dx + len / 2.0 - pw / 2.0, ax, ax + aw - pw);
            (Rect { x: dx, y: my, w: len, h: t }, Rect { x: px, y: my + t + gap, w: pw, h: ph })
        }
        "bottom" => {
            let dx = mx + (mw - len) * along;
            let px = clamp(dx + len / 2.0 - pw / 2.0, ax, ax + aw - pw);
            (Rect { x: dx, y: my + mh - t, w: len, h: t }, Rect { x: px, y: my + mh - t - gap - ph, w: pw, h: ph })
        }
        _ => {
            let dy = my + (mh - len) * along;
            let py = clamp(dy + len / 2.0 - ph / 2.0, ay, ay + ah - ph);
            (Rect { x: mx + mw - t, y: dy, w: t, h: len }, Rect { x: mx + mw - t - gap - pw, y: py, w: pw, h: ph })
        }
    };
    Some(Geo { dock, panel, scale: sc })
}

// ------------------------------------------------------------------ public API

/// 建停靠条、注册拖放、单实例消息窗口、启动轮询。面板窗口要先 show 过（停放状态）。
pub fn init(node: Arc<Node>, hooks: Hooks) {
    let d = Dock {
        node,
        hooks,
        visible: false,
        pinned: false,
        hovering: false,
        hover_disarmed: false,
        hover_since: None,
        geo: None,
        entered: false,
        inside: false,
        leave_since: None,
        last_typed: None,
        left_down: false,
        press_outside: false,
        dragging_files: false,
        mv: None,
        move_place: None,
        poll_ms: 0,
        prev_fg: None,
    };
    DOCK.with(|x| *x.borrow_mut() = Some(d));
    strip::create();
    if let Some(h) = crate::win::panel_hwnd() {
        drop::register_panel(h);
    }
    ipc_window();
    tray::create();
    apply_layout();
    strip::show();
    schedule_poll(POLL_FAR_MS);
    log::info!("dock ready: edge {}", with(|d| current_edge(d)).unwrap_or_default());
}

pub fn apply_layout() {
    let Some(geo) = with(|d| {
        let g = compute(d)?;
        d.geo = Some(g);
        Some(g)
    })
    .flatten() else {
        return;
    };
    strip::place(geo.dock, (0.0, 0.0), geo.scale);
    if with(|d| d.visible).unwrap_or(false) {
        crate::win::panel_place(geo.panel);
    }
    refresh_visual();
}

pub fn refresh_visual() {
    let Some(v) = with(|d| {
        let n: i64 = d.node.peers().iter().filter(|p| !p.blocked).map(|p| p.unread as i64).sum();
        let dark = crate::theme_is_dark(&d.node.settings().theme);
        (DockVisual { edge: current_edge(d), unread: n > 0, open: d.visible, dark, ..Default::default() }, n)
    }) else {
        return;
    };
    let (v, n) = v;
    tray::set_unread(n);
    strip::set_visual(v);
}

pub fn on_hover_start() {
    with(|d| {
        d.hovering = true;
        if d.visible {
            d.inside = true;
            d.entered = true;
            d.leave_since = None;
        }
    });
    schedule_poll(POLL_NEAR_MS);
}

pub fn on_hover_end() {
    with(|d| d.hovering = false);
}

#[allow(dead_code)] // 托盘（下一步）用
pub fn is_visible() -> bool {
    with(|d| d.visible).unwrap_or(false)
}

/// 面板这一侧报告：输入框打了字（旧版 recentlyTyped）。
pub fn note_typed() {
    with(|d| d.last_typed = Some(Instant::now()));
}

pub fn set_pinned(p: bool) {
    with(|d| {
        d.pinned = p;
        // 取消钉住时光标已在外面：从现在起重新计时收起
        d.leave_since = None;
    });
    hook(|h| (h.on_pinned)(p));
}

pub fn pinned() -> bool {
    with(|d| d.pinned).unwrap_or(false)
}

pub fn open_panel(reason: &str) {
    if reason == "hover" && with(|d| d.mv.is_some()).unwrap_or(false) {
        return;
    }
    toast::dismiss();
    log::debug!("open_panel({reason})");
    let focus = reason == "tray" || reason == "shortcut" || reason.starts_with("toast:");
    let first = with(|d| {
        let was = d.visible;
        if !was {
            d.visible = true;
            d.entered = reason == "hover" || reason == "drag";
            d.inside = d.entered;
            d.leave_since = None;
            if let Some(g) = compute(d) {
                d.geo = Some(g);
            }
        }
        (!was, d.geo)
    });
    let Some((first, geo)) = first else { return };
    if first {
        if let Some(g) = geo {
            crate::win::panel_show(g.panel);
        }
        refresh_visual();
        schedule_poll(POLL_NEAR_MS);
    }
    // 回调到 App 时不能持有 DOCK 的借用
    DOCK.with(|x| {
        if let Ok(b) = x.try_borrow() {
            if let Some(d) = b.as_ref() {
                (d.hooks.on_open)(reason);
            }
        }
    });
    // 主动打开（toast 点击、托盘、快捷键）才拿键盘焦点；悬停 / 拖拽 / 命令行开关从不抢前台
    if focus {
        let prev = crate::win::panel_focus();
        with(|d| d.prev_fg = prev);
    }
}

pub fn hide_panel() {
    let Some(prev) = with(|d| {
        if !d.visible {
            return None;
        }
        d.visible = false;
        d.leave_since = None;
        d.hover_since = None;
        d.hover_disarmed = d.geo.zip(cursor()).is_some_and(|(g, (x, y))| g.dock.contains(x, y, 1.0));
        Some(d.prev_fg.take())
    })
    .flatten() else {
        return;
    };
    log::debug!("hide_panel");
    crate::win::panel_hide(prev);
    DOCK.with(|x| {
        if let Ok(b) = x.try_borrow() {
            if let Some(d) = b.as_ref() {
                (d.hooks.on_close)();
                (d.hooks.on_drag_hint)(None);
            }
        }
    });
    refresh_visual();
}

pub fn toggle_panel() {
    if is_visible() {
        hide_panel();
    } else {
        open_panel("tray");
    }
}

/// 收到消息：面板收起时在停靠条位置弹 toast（与旧版 dock::on_incoming 相同的判定和文案）。
pub fn on_incoming(m: &lancat_core::Message) {
    if m.outgoing {
        return;
    }
    let skip = |why: &str| log::debug!("message toast for {} ({}) not shown: {why}", m.peer_id, m.kind);
    let Some(spec) = with(|d| -> Result<toast::ToastSpec, &'static str> {
        if d.visible {
            return Err("panel open");
        }
        if d.mv.is_some() {
            return Err("dock being dragged");
        }
        let peer = d.node.peers().into_iter().find(|p| p.id == m.peer_id).ok_or("unknown peer")?;
        if peer.blocked {
            return Err("peer blocked");
        }
        let s = |k: &str| m.body.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let text = match m.kind.as_str() {
            "text" => s("text").split_whitespace().collect::<Vec<_>>().join(" "),
            "emoji" => {
                let alt = s("alt");
                if s("kind") == "sticker" {
                    format!("[贴纸] {alt}")
                } else if alt.is_empty() {
                    "[表情]".into()
                } else {
                    alt
                }
            }
            "file" => {
                let size = m.body.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
                format!("📄 {} · {}", s("name"), crate::human(size))
            }
            _ => return Err("kind has no toast"),
        };
        let geo = d.geo.ok_or("no dock geometry yet")?;
        log::info!("message toast: {} from {} ({})", m.kind, peer.name, m.peer_id);
        let dark = crate::theme_is_dark(&d.node.settings().theme);
        let hold_ms = 4000 + (text.chars().count() as u32 * 60).min(3000);
        Ok(toast::ToastSpec { peer_id: peer.id.clone(), name: peer.name.clone(), text, edge: current_edge(d), strip: geo.dock, scale: geo.scale, dark, hold_ms })
    }) else {
        return;
    };
    match spec {
        Ok(spec) => toast::show(spec),
        Err(why) => skip(why),
    }
}

/// 诊断提醒（其他设备可能连不进本机……）用的 toast 标识：点它打开「权限与诊断」。
pub const DIAG_TOAST: &str = "__diag__";

/// 新的诊断提醒：面板收起时横幅看不到，停靠条鼓出提醒 + 托盘气泡通知，点哪个都打开诊断页一键修复。
pub fn on_diag_alert(title: &str, detail: &str) {
    let spec = with(|d| -> Result<toast::ToastSpec, &'static str> {
        if d.visible {
            return Err("panel open");
        }
        if d.mv.is_some() {
            return Err("dock being dragged");
        }
        let geo = d.geo.ok_or("no dock geometry yet")?;
        let dark = crate::theme_is_dark(&d.node.settings().theme);
        Ok(toast::ToastSpec { peer_id: DIAG_TOAST.into(), name: "Lancat".into(), text: format!("{title}，点此修复"), edge: current_edge(d), strip: geo.dock, scale: geo.scale, dark, hold_ms: 9000 })
    });
    match spec {
        Some(Ok(spec)) => {
            log::info!("diag alert toast: {title}");
            toast::show(spec);
            tray::balloon(title, &format!("{detail}\n点此打开「权限与诊断」一键修复"));
        }
        Some(Err(why)) => log::info!("diag alert toast not shown: {why}"),
        None => {}
    }
}

/// 新版本提醒的 toast 标识：点它打开设置的「更新」。
pub const UPDATE_TOAST: &str = "__update__";

/// 发现新版本：面板关着时停靠条鼓出提醒（Windows 另弹托盘气泡）。
pub fn on_update_alert(title: &str) {
    let spec = with(|d| -> Result<toast::ToastSpec, &'static str> {
        if d.visible {
            return Err("panel open");
        }
        if d.mv.is_some() {
            return Err("dock being dragged");
        }
        let geo = d.geo.ok_or("no dock geometry yet")?;
        let dark = crate::theme_is_dark(&d.node.settings().theme);
        Ok(toast::ToastSpec { peer_id: UPDATE_TOAST.into(), name: "Lancat".into(), text: format!("{title}，点此更新"), edge: current_edge(d), strip: geo.dock, scale: geo.scale, dark, hold_ms: 9000 })
    });
    match spec {
        Some(Ok(spec)) => {
            log::info!("update alert toast: {title}");
            toast::show(spec);
            tray::balloon_update(title, "点此打开 Lancat 设置，一键更新到新版本");
        }
        Some(Err(why)) => log::info!("update alert toast not shown: {why}"),
        None => {}
    }
}

/// 拖放（drop.rs 已离开 COM 调用）。
pub fn on_drag(ev: DragEvent) {
    match ev {
        DragEvent::Enter { panel, n, x, y } => {
            with(|d| d.dragging_files = true);
            if !panel {
                // 文件碰到停靠条：立刻打开（旧版同样 open_panel("drag")）
                open_panel("drag");
            }
            hook(|h| (h.on_drag_hint)(Some(n)));
            if panel {
                if let Some(s) = crate::win::panel_scale() {
                    hook(|h| (h.on_drag_over)(x as f32 / s, y as f32 / s));
                }
            }
        }
        DragEvent::Over { x, y } => {
            if let Some(s) = crate::win::panel_scale() {
                hook(|h| (h.on_drag_over)(x as f32 / s, y as f32 / s));
            }
        }
        DragEvent::Leave { panel } => {
            if panel {
                with(|d| d.dragging_files = false);
                hook(|h| (h.on_drag_hint)(None));
            }
        }
        DragEvent::Drop { panel, files, x, y } => {
            with(|d| d.dragging_files = false);
            hook(|h| (h.on_drag_hint)(None));
            if files.is_empty() {
                return;
            }
            if !panel {
                open_panel("drag");
            }
            let pos = if panel { crate::win::panel_scale().map(|s| (x as f32 / s, y as f32 / s)) } else { None };
            hook(move |h| (h.on_drop)(files, pos));
        }
    }
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

// ------------------------------------------------------------------ moving the dock

pub fn begin_move(from_strip: bool) {
    toast::dismiss();
    let started = with(|d| {
        let c = cursor()?;
        d.mv = Some(Move { from_strip, start: c, moved: false, monitors: monitors(), settings: d.node.settings() });
        Some(())
    })
    .flatten();
    if started.is_none() && from_strip {
        open_panel("hover");
    }
    schedule_poll(POLL_MOVE_MS);
}

/// 与旧版 place_for 相同：光标所在显示器、离得最近的工作区边、停靠条中心对准光标。
fn place_for(d: &Dock, x: f64, y: f64) -> Option<(String, f64, usize)> {
    let mv = d.mv.as_ref()?;
    let idx = mv.monitors.iter().position(|m| x >= m.full.left as f64 && x < m.full.right as f64 && y >= m.full.top as f64 && y < m.full.bottom as f64)?;
    let m = &mv.monitors[idx];
    let (ax, ay, aw, ah) = (m.work.left as f64, m.work.top as f64, (m.work.right - m.work.left) as f64, (m.work.bottom - m.work.top) as f64);
    let len = mv.settings.dock_length.clamp(40.0, 2000.0) * m.scale;
    let dist = [("left", x - ax), ("right", ax + aw - x), ("top", y - ay), ("bottom", ay + ah - y)];
    let edge = dist.iter().min_by(|a, b| a.1.total_cmp(&b.1))?.0;
    let along = |c: f64, start: f64, span: f64| if span <= len { 0.5 } else { ((c - start - len / 2.0) / (span - len)).clamp(0.0, 1.0) };
    let offset = if edge == "left" || edge == "right" { along(y, ay, ah) } else { along(x, ax, aw) };
    Some((edge.to_string(), (offset * 1000.0).round() / 1000.0, idx))
}

/// 拖动中的一步；没有拖动时返回 false。
fn move_tick(x: f64, y: f64) -> bool {
    enum Out {
        None,
        Busy,
        Relayout,
        End { moved: bool, from_strip: bool, place: Option<(String, f64, usize)> },
    }
    let out = with(|d| {
        let Some(m) = d.mv.as_mut() else { return Out::None };
        if !left_button_down() {
            let m = d.mv.take().unwrap();
            return Out::End { moved: m.moved, from_strip: m.from_strip, place: d.move_place.take() };
        }
        if !m.moved {
            let scale = d.geo.map(|g| g.scale).unwrap_or(1.0);
            let (dx, dy) = (x - m.start.0, y - m.start.1);
            if (dx * dx + dy * dy).sqrt() < 6.0 * scale {
                return Out::Busy;
            }
            m.moved = true;
        }
        let Some(p) = place_for(d, x, y) else { return Out::Busy };
        if d.move_place.as_ref() != Some(&p) {
            d.move_place = Some(p);
            return Out::Relayout;
        }
        Out::Busy
    })
    .unwrap_or(Out::None);
    match out {
        Out::None => false,
        Out::Busy => true,
        Out::Relayout => {
            apply_layout();
            true
        }
        Out::End { moved, from_strip, place } => {
            if let (true, Some((edge, offset, monitor))) = (moved, place) {
                log::info!("dock moved to {edge} {offset:.3} on monitor {monitor}");
                with(|d| {
                    let mut s = d.node.settings();
                    s.dock_edge = edge;
                    s.dock_offset = offset;
                    s.dock_monitor = monitor;
                    if let Err(e) = d.node.set_settings(s) {
                        log::warn!("saving the dock position failed: {e}");
                    }
                });
                apply_layout();
            } else if from_strip && !moved {
                open_panel("hover");
            }
            true
        }
    }
}

// ------------------------------------------------------------------ poller

fn schedule_poll(ms: u64) {
    let changed = with(|d| {
        if d.poll_ms == ms {
            return false;
        }
        d.poll_ms = ms;
        true
    })
    .unwrap_or(false);
    if changed {
        POLL.with(|t| t.start(slint::TimerMode::Repeated, Duration::from_millis(ms), tick));
    }
}

/// 旧版 dock::tick（光标轮询）+ Panel.svelte 的收起判断。
fn tick() {
    let Some((x, y)) = cursor() else { return };
    if move_tick(x, y) {
        schedule_poll(if with(|d| d.mv.is_some()).unwrap_or(false) { POLL_MOVE_MS } else { POLL_NEAR_MS });
        return;
    }
    let down = left_button_down();
    enum Act {
        Nothing,
        Open(&'static str),
        Hide,
    }
    let (act, next) = with(|d| {
        let Some(geo) = d.geo else { return (Act::Nothing, POLL_FAR_MS) };
        let in_dock = geo.dock.contains(x, y, 1.0);
        let in_panel = d.visible && geo.panel.contains(x, y, 0.0);
        let near = down || geo.dock.contains(x, y, NEAR_PX * geo.scale);
        let was_down = std::mem::replace(&mut d.left_down, down);
        if down && !was_down {
            d.press_outside = !in_dock && !in_panel;
        }
        if !d.visible {
            let next = if near { POLL_NEAR_MS } else { POLL_FAR_MS };
            if !in_dock {
                d.hover_since = None;
                d.hover_disarmed = false;
                return (Act::Nothing, next);
            }
            if down && d.press_outside {
                // 从别处按下拖过来（文件拖拽，OLE 拖拽不给停靠条发 WM_MOUSEMOVE）：立刻打开
                return (Act::Open("drag"), POLL_NEAR_MS);
            }
            if toast::shown() || d.hover_disarmed {
                d.hover_since = None;
                return (Act::Nothing, next);
            }
            let delay = Duration::from_millis(d.node.settings().hover_delay_ms);
            match d.hover_since {
                None => d.hover_since = Some(Instant::now()),
                Some(t) if t.elapsed() >= delay => {
                    d.hover_since = None;
                    return (Act::Open("hover"), POLL_NEAR_MS);
                }
                _ => {}
            }
            return (Act::Nothing, next);
        }
        // 面板打开：光标在面板或停靠条里算“在里面”（面板左缘多算 1px 防抖）
        let mut p = geo.panel;
        p.x -= 1.0;
        let inside = in_dock || p.contains(x, y, 0.0);
        if inside {
            d.entered = true;
            d.inside = true;
            d.leave_since = None;
            return (Act::Nothing, POLL_NEAR_MS);
        }
        d.inside = false;
        if !d.entered || d.pinned {
            return (Act::Nothing, POLL_NEAR_MS);
        }
        let since = *d.leave_since.get_or_insert_with(Instant::now);
        let delay = Duration::from_millis(d.node.settings().collapse_delay_ms);
        if since.elapsed() < delay {
            return (Act::Nothing, POLL_NEAR_MS);
        }
        // holding()：输入框聚焦且（有草稿或 8 s 内打过字）、拖拽中 → 400 ms 后再看
        let (focused, draft) = (d.hooks.composer)();
        let typed = d.last_typed.is_some_and(|t| t.elapsed() < TYPED_HOLD);
        if (focused && (draft || typed)) || d.dragging_files || (down && !d.press_outside) {
            d.leave_since = Some(Instant::now() - delay + Duration::from_millis(RECHECK_MS));
            return (Act::Nothing, POLL_NEAR_MS);
        }
        (Act::Hide, POLL_NEAR_MS)
    })
    .unwrap_or((Act::Nothing, POLL_FAR_MS));
    match act {
        Act::Open(r) => open_panel(r),
        Act::Hide => hide_panel(),
        Act::Nothing => {}
    }
    schedule_poll(next);
}

// ------------------------------------------------------------------ single instance

const IPC_CLASS: PCWSTR = w!("LancatSlintIpc");

fn instance_key() -> String {
    let dir = crate::data_dir();
    let h = dir.to_string_lossy().to_lowercase().bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    format!("LancatSlint-{h:016x}")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 已有实例在跑：把命令行转给它并返回 true（调用方退出）。
pub fn forward_to_running() -> bool {
    let key = instance_key();
    unsafe {
        let name = wide(&format!("Local\\{key}"));
        let m = CreateMutexW(None, true, PCWSTR(name.as_ptr()));
        let exists = windows::Win32::Foundation::GetLastError() == windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
        if m.is_ok() && !exists {
            // 句柄保持到进程结束
            std::mem::forget(m);
            return false;
        }
        let title = wide(&key);
        let args: Vec<String> = std::env::args().skip(1).collect();
        for _ in 0..20 {
            if let Ok(h) = FindWindowExW(Some(HWND_MESSAGE), None, IPC_CLASS, PCWSTR(title.as_ptr())) {
                let payload = args.join("\n");
                let data: Vec<u16> = payload.encode_utf16().collect();
                let cds = COPYDATASTRUCT { dwData: 0x4c43, cbData: (data.len() * 2) as u32, lpData: data.as_ptr() as *mut _ };
                let mut res = 0usize;
                let _ = SendMessageTimeoutW(h, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize), SMTO_ABORTIFHUNG, 3000, Some(&mut res));
                log::info!("forwarded {args:?} to the running instance");
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        log::warn!("another instance holds the lock but its window was not found; exiting");
        true
    }
}

unsafe extern "system" fn ipc_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA {
        let cds = &*(lp.0 as *const COPYDATASTRUCT);
        if cds.dwData == 0x4c43 {
            let s = std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2);
            let args: Vec<String> = String::from_utf16_lossy(s).split('\n').filter(|a| !a.is_empty()).map(String::from).collect();
            // 离开 SendMessage 再处理（发送方在等返回）
            slint::Timer::single_shot(Duration::ZERO, move || handle_args(&args, true));
            return LRESULT(1);
        }
    }
    DefWindowProcW(h, msg, wp, lp)
}

fn ipc_window() {
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSW { lpfnWndProc: Some(ipc_proc), hInstance: hmod.into(), lpszClassName: IPC_CLASS, ..Default::default() };
        RegisterClassW(&wc);
        let title = wide(&instance_key());
        if CreateWindowExW(WINDOW_EX_STYLE(0), IPC_CLASS, PCWSTR(title.as_ptr()), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hmod.into()), None).is_err() {
            log::warn!("single-instance window could not be created");
        }
    }
}

/// 命令行开关（启动时 `second == false`；第二次启动转发过来时 true）。
/// `--open-panel`：打开（不抢焦点）；`--hide-panel`：收起；`--settings`：设置；第二次无参数启动：打开并拿焦点。
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
