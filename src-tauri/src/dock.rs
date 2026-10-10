//! Dock strip + panel windows.
//!
//! * dock  — 6 px always-on-top strip on a screen edge, a **native** window (see `native_dock`),
//!   never a WebView, so the collapsed app runs no web renderer at all;
//! * panel — ~360×520 chat popover next to the strip, a WebView created **on demand**: it is
//!   pre-warmed when the pointer starts hovering the strip, shown after `hover_delay_ms`, and
//!   destroyed `panel_keep_alive_s` seconds after it collapses (messages live in the core/SQLite,
//!   the frontend re-reads them on mount).
//!
//! Hover/drag sensing ends in [`open_panel`] from two paths:
//! 1. the native strip's own enter/leave/drag events (works everywhere, incl. Wayland);
//! 2. a cursor poller (80 ms while relevant) that catches OS-level drags the strip may not get
//!    (Windows OLE drags) and, while the panel is visible, reports whether the pointer is inside
//!    it as a fallback for `mouseleave`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, EventTarget, Manager, Monitor, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::commands::Core;
use crate::native_dock::{self, DockVisual};
use crate::platform;

pub const STRIP: f64 = 6.0;
pub const PANEL_W: f64 = 360.0;
pub const PANEL_H: f64 = 520.0;
/// Transparent margin around the CSS shell (shadow + spring overshoot) where there is no native material.
pub const MARGIN: f64 = if platform::NATIVE_GLASS { 0.0 } else { 8.0 };

#[derive(Default)]
pub struct DockState {
    pub pinned: AtomicBool,
    /// Frontend says "don't collapse" (composer focus, emoji panel, transfer, dialog).
    pub hold: AtomicBool,
    pub visible: AtomicBool,
    geo: Mutex<Option<Geo>>,
    hover_since: Mutex<Option<Instant>>,
    last_inside: Mutex<Option<bool>>,
    /// When the current open was requested (for the "panel shown after N ms" log line).
    open_requested: Mutex<Option<Instant>>,
    /// The panel webview had to be created for this open (cold) rather than re-shown (warm).
    cold_open: AtomicBool,
    /// Why the panel was last opened ("hover", "drag", "tray", …), for a freshly mounted webview.
    reason: Mutex<String>,
    /// Bumped on every open/close; a pending destroy only runs if nothing happened since.
    panel_gen: AtomicU64,
    /// Bumped on every hover start/end (generic hover-delay timer, macOS).
    hover_gen: AtomicU64,
    hovering: AtomicBool,
}

/// Physical pixels, virtual-desktop coordinates (top-left origin).
#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    fn contains(&self, px: f64, py: f64, slop: f64) -> bool {
        px >= self.x - slop && px < self.x + self.w + slop && py >= self.y - slop && py < self.y + self.h + slop
    }
}

#[derive(Clone, Copy, Debug)]
struct Geo {
    dock: Rect,
    panel: Rect,
    /// monitor origin + scale, for layer-shell (logical, output-relative) placement
    origin: (f64, f64),
    scale: f64,
}

#[derive(Serialize, Clone)]
struct ShowPayload {
    edge: String,
    reason: String,
}

fn pick_monitor(app: &AppHandle, index: usize) -> Option<Monitor> {
    let list = app.available_monitors().unwrap_or_default();
    if let Some(primary) = app.primary_monitor().ok().flatten() {
        if index == 0 {
            return Some(primary);
        }
        // index 0 = primary, then the others in OS order
        let others: Vec<Monitor> = list.into_iter().filter(|m| m.position() != primary.position() || m.size() != primary.size()).collect();
        return others.into_iter().nth(index - 1).or(Some(primary));
    }
    list.into_iter().nth(index)
}

fn compute(app: &AppHandle) -> Option<Geo> {
    let s = app.state::<Core>().0.settings();
    let m = pick_monitor(app, s.dock_monitor)?;
    let sc = m.scale_factor();
    let (mx, my) = (m.position().x as f64, m.position().y as f64);
    let (mw, mh) = (m.size().width as f64, m.size().height as f64);
    let wa = m.work_area();
    let (ax, ay, aw, ah) = (wa.position.x as f64, wa.position.y as f64, wa.size.width as f64, wa.size.height as f64);

    let t = (STRIP * sc).round();
    let along = s.dock_offset.clamp(0.0, 1.0);
    let pw = ((PANEL_W + 2.0 * MARGIN) * sc).round();
    let ph = ((PANEL_H + 2.0 * MARGIN) * sc).round();
    let gap = (2.0 * sc).round();
    let edge = s.dock_edge.as_str();
    let horizontal = edge == "top" || edge == "bottom";
    let len = (s.dock_length.clamp(40.0, 2000.0) * sc).round().min(if horizontal { mw } else { mh });

    let clamp = |v: f64, lo: f64, hi: f64| if hi < lo { lo } else { v.clamp(lo, hi) };
    let (dock, panel) = match edge {
        "left" => {
            let dy = my + (mh - len) * along;
            let d = Rect { x: mx, y: dy, w: t, h: len };
            let py = clamp(dy + len / 2.0 - ph / 2.0, ay, ay + ah - ph);
            (d, Rect { x: mx + t + gap - MARGIN * sc, y: py, w: pw, h: ph })
        }
        "top" => {
            let dx = mx + (mw - len) * along;
            let d = Rect { x: dx, y: my, w: len, h: t };
            let px = clamp(dx + len / 2.0 - pw / 2.0, ax, ax + aw - pw);
            (d, Rect { x: px, y: my + t + gap - MARGIN * sc, w: pw, h: ph })
        }
        "bottom" => {
            let dx = mx + (mw - len) * along;
            let d = Rect { x: dx, y: my + mh - t, w: len, h: t };
            let px = clamp(dx + len / 2.0 - pw / 2.0, ax, ax + aw - pw);
            (d, Rect { x: px, y: my + mh - t - gap - ph + MARGIN * sc, w: pw, h: ph })
        }
        _ => {
            let dy = my + (mh - len) * along;
            let d = Rect { x: mx + mw - t, y: dy, w: t, h: len };
            let py = clamp(dy + len / 2.0 - ph / 2.0, ay, ay + ah - ph);
            (d, Rect { x: mx + mw - t - gap - pw + MARGIN * sc, y: py, w: pw, h: ph })
        }
    };
    Some(Geo { dock, panel, origin: (mx, my), scale: sc })
}

fn place(w: &WebviewWindow, r: Rect, geo: &Geo) {
    let (ox, oy) = geo.origin;
    if platform::place_layer(w, (r.x - ox) / geo.scale, (r.y - oy) / geo.scale, r.w / geo.scale, r.h / geo.scale) {
        return;
    }
    let _ = w.set_size(PhysicalSize::new(r.w.round() as u32, r.h.round() as u32));
    let _ = w.set_position(PhysicalPosition::new(r.x.round() as i32, r.y.round() as i32));
}

/// Creates the native dock strip and starts the cursor poller. The panel is created on demand.
pub fn create_windows(app: &AppHandle) -> tauri::Result<()> {
    native_dock::create(app);
    apply_layout(app);
    native_dock::show();
    refresh_visual(app);
    if keep_alive_s(app) == 0 {
        // "always resident" mode: instant opens, at the cost of a live web renderer
        let _ = ensure_panel(app);
    }
    spawn_poller(app.clone());
    // the collapsed app has settled: give back start-up heap and library pages (Linux)
    let a = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(8));
        let st = a.state::<DockState>();
        if a.get_webview_window("panel").is_none() && !st.hovering.load(Ordering::SeqCst) {
            platform::settle_after_start();
        }
    });
    Ok(())
}

/// Seconds a collapsed panel webview is kept before it is destroyed; 0 = never (created at start).
/// `LANCAT_PANEL_KEEP_ALIVE` overrides the setting (tests, memprobe).
fn keep_alive_s(app: &AppHandle) -> u64 {
    std::env::var("LANCAT_PANEL_KEEP_ALIVE").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| app.state::<Core>().0.settings().panel_keep_alive_s)
}

/// Returns the panel window, creating it (hidden) if it does not exist yet.
fn ensure_panel(app: &AppHandle) -> Option<WebviewWindow> {
    if let Some(p) = app.get_webview_window("panel") {
        return Some(p);
    }
    let t = Instant::now();
    let builder = WebviewWindowBuilder::new(app, "panel", WebviewUrl::App("index.html".into()));
    #[cfg(windows)]
    let builder = platform::webview_builder(builder);
    let panel = builder
        .initialization_script(boot_script(app))
        .title("Lancat")
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .focused(false)
        .visible_on_all_workspaces(true)
        .inner_size(PANEL_W + 2.0 * MARGIN, PANEL_H + 2.0 * MARGIN)
        .visible(false)
        .build()
        .map_err(|e| log::warn!("creating the panel webview failed: {e}"))
        .ok()?;
    platform::setup_panel(&panel);
    platform::tune_webview(&panel);
    if let Some(geo) = compute(app) {
        *app.state::<DockState>().geo.lock().unwrap() = Some(geo);
        place(&panel, geo.panel, &geo);
    }
    log::info!("panel webview created in {} ms", t.elapsed().as_millis());
    Some(panel)
}

/// Everything the panel needs for its first frame, injected as `window.__LANCAT_BOOT__` so a cold
/// panel can start its open animation without waiting for IPC round-trips (each one goes
/// web process → network process → UI process and back on WebKitGTK). The frontend re-syncs over
/// IPC once its event listeners are attached.
fn boot_script(app: &AppHandle) -> String {
    let core = &app.state::<Core>().0;
    let st = app.state::<DockState>();
    let mut peers = core.peers();
    // same order as the frontend (state.svelte.ts sortPeers): online first, newest, name
    peers.sort_by(|a, b| b.online.cmp(&a.online).then(b.last_ts.cmp(&a.last_ts)).then(a.name.cmp(&b.name)));
    let first = peers.first().map(|p| p.id.clone());
    let mut messages = serde_json::Map::new();
    if let Some(id) = first {
        if let Ok(m) = core.messages(&id, None, 80) {
            messages.insert(id, serde_json::to_value(m).unwrap_or_default());
        }
    }
    let settings = core.settings();
    let boot = serde_json::json!({
        "me": core.me(),
        "peers": peers,
        "settings": settings,
        "messages": messages,
        "panel": {
            "visible": st.visible.load(Ordering::SeqCst),
            "pinned": st.pinned.load(Ordering::SeqCst),
            "edge": settings.dock_edge,
            "reason": reason_now(&st),
        },
    });
    // JSON is valid JS; escape "</" just in case the payload ever ends up inside markup
    format!("window.__LANCAT_BOOT__ = {};", boot.to_string().replace("</", "<\\/"))
}

/// Pointer entered the strip: pre-warm the panel webview so the hover delay hides its start-up.
pub fn on_hover_start(app: &AppHandle) {
    let st = app.state::<DockState>();
    st.hovering.store(true, Ordering::SeqCst);
    st.hover_gen.fetch_add(1, Ordering::SeqCst);
    if st.visible.load(Ordering::SeqCst) {
        emit_cursor(app, true);
        return;
    }
    st.panel_gen.fetch_add(1, Ordering::SeqCst); // cancel a pending destroy
    if app.get_webview_window("panel").is_none() {
        st.cold_open.store(true, Ordering::SeqCst);
        let _ = ensure_panel(app);
    }
}

/// Pointer left the strip.
pub fn on_hover_end(app: &AppHandle) {
    let st = app.state::<DockState>();
    st.hovering.store(false, Ordering::SeqCst);
    st.hover_gen.fetch_add(1, Ordering::SeqCst);
    if st.visible.load(Ordering::SeqCst) {
        emit_cursor(app, false);
    } else {
        // pre-warmed but never opened: let it go after the keep-alive like a collapsed panel
        schedule_destroy(app);
    }
}

/// Generic hover-delay timer for backends without a native one (macOS).
#[allow(dead_code)]
pub fn schedule_hover_open(app: &AppHandle) {
    let st = app.state::<DockState>();
    let gen = st.hover_gen.load(Ordering::SeqCst);
    let delay = Duration::from_millis(app.state::<Core>().0.settings().hover_delay_ms);
    let a = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        let a2 = a.clone();
        let _ = a.run_on_main_thread(move || {
            let st = a2.state::<DockState>();
            if st.hovering.load(Ordering::SeqCst) && st.hover_gen.load(Ordering::SeqCst) == gen {
                open_panel(&a2, "hover");
            }
        });
    });
}

pub(crate) fn emit_cursor(app: &AppHandle, inside: bool) {
    #[derive(Serialize, Clone)]
    struct Inside {
        inside: bool,
    }
    *app.state::<DockState>().last_inside.lock().unwrap() = Some(inside);
    let _ = app.emit_to(EventTarget::webview_window("panel"), "panel://cursor", Inside { inside });
}

/// Destroys the panel webview after `panel_keep_alive_s` unless it is opened again meanwhile.
fn schedule_destroy(app: &AppHandle) {
    let st = app.state::<DockState>();
    let gen = st.panel_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let secs = keep_alive_s(app);
    if secs == 0 {
        return;
    }
    let a = app.clone();
    std::thread::Builder::new()
        .name("lancat-panel-reaper".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(secs));
            let a2 = a.clone();
            let _ = a.run_on_main_thread(move || {
                let st = a2.state::<DockState>();
                if st.visible.load(Ordering::SeqCst) || st.hovering.load(Ordering::SeqCst) || st.panel_gen.load(Ordering::SeqCst) != gen {
                    return;
                }
                if let Some(p) = a2.get_webview_window("panel") {
                    log::info!("destroying the idle panel webview (collapsed for {secs}s)");
                    let _ = p.destroy();
                    // give the freed UI-process side of WebKit back to the OS a moment later
                    std::thread::spawn(|| {
                        std::thread::sleep(Duration::from_secs(2));
                        platform::reap_web_helpers();
                        platform::trim_heap();
                    });
                }
            });
        })
        .ok();
}

/// Pushes edge / unread / open state to the native strip. Main thread only.
pub fn refresh_visual(app: &AppHandle) {
    let core = &app.state::<Core>().0;
    let unread = core.peers().iter().any(|p| !p.blocked && p.unread > 0);
    let visual = DockVisual { edge: core.settings().dock_edge, unread, open: app.state::<DockState>().visible.load(Ordering::SeqCst), ..Default::default() };
    native_dock::set_visual(visual);
}

/// Re-applies edge / offset / length / monitor from settings.
pub fn apply_layout(app: &AppHandle) {
    let Some(geo) = compute(app) else { return };
    *app.state::<DockState>().geo.lock().unwrap() = Some(geo);
    native_dock::place(geo.dock, geo.origin, geo.scale);
    if let Some(p) = app.get_webview_window("panel") {
        place(&p, geo.panel, &geo);
    }
    refresh_visual(app);
}

pub fn open_panel(app: &AppHandle, reason: &str) {
    log::debug!("open_panel({reason})");
    let st = app.state::<DockState>();
    st.panel_gen.fetch_add(1, Ordering::SeqCst); // cancel a pending destroy
    *st.reason.lock().unwrap() = reason.to_string();
    let existed = app.get_webview_window("panel").is_some();
    // mark visible before a cold create so the boot script tells the page to animate in at once
    let was_visible = st.visible.swap(true, Ordering::SeqCst);
    if !was_visible {
        *st.open_requested.lock().unwrap() = Some(Instant::now());
        if !existed {
            st.cold_open.store(true, Ordering::SeqCst); // else: keep what the pre-warm recorded
        }
    }
    let Some(panel) = ensure_panel(app) else {
        st.visible.store(false, Ordering::SeqCst);
        return;
    };
    if !was_visible {
        if let Some(geo) = compute(app) {
            *st.geo.lock().unwrap() = Some(geo);
            place(&panel, geo.panel, &geo);
        }
        let _ = panel.show();
        #[cfg(windows)]
        platform::panel_visibility(&panel, true);
        *st.last_inside.lock().unwrap() = Some(true);
        let _ = app.emit("panel://visible", true);
        refresh_visual(app);
    }
    if reason == "tray" || reason == "shortcut" {
        let _ = panel.set_focus();
    }
    let edge = app.state::<Core>().0.settings().dock_edge;
    let _ = app.emit_to(EventTarget::webview_window("panel"), "panel://show", ShowPayload { edge, reason: reason.into() });
}

pub fn hide_panel(app: &AppHandle) {
    log::debug!("hide_panel");
    let st = app.state::<DockState>();
    if let Some(panel) = app.get_webview_window("panel") {
        let _ = panel.hide();
        #[cfg(windows)]
        platform::panel_visibility(&panel, false);
    }
    st.visible.store(false, Ordering::SeqCst);
    st.cold_open.store(false, Ordering::SeqCst);
    *st.hover_since.lock().unwrap() = None;
    let _ = app.emit("panel://visible", false);
    refresh_visual(app);
    schedule_destroy(app);
}

/// Asks the panel to play its collapse animation (it hides itself afterwards).
pub fn request_collapse(app: &AppHandle) {
    let _ = app.emit_to(EventTarget::webview_window("panel"), "panel://collapse", ());
}

pub fn toggle_panel(app: &AppHandle) {
    if app.state::<DockState>().visible.load(Ordering::SeqCst) {
        request_collapse(app);
    } else {
        open_panel(app, "tray");
    }
}

pub fn open_settings(app: &AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    let builder = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html".into()));
    #[cfg(windows)]
    let builder = platform::webview_builder(builder);
    let w = builder
        .title("Lancat 设置")
        .inner_size(480.0, 620.0)
        .min_inner_size(420.0, 480.0)
        .resizable(true)
        .center()
        .build()?;
    let _ = w.set_focus();
    Ok(())
}

fn spawn_poller(app: AppHandle) {
    std::thread::Builder::new()
        .name("lancat-dock-poller".into())
        .spawn(move || loop {
            // Linux: the native strip gets enter/leave/drag itself, so the poller only matters while
            // the panel is visible (pointer-inside fallback). Elsewhere it also senses OLE drags.
            let idle = cfg!(target_os = "linux") && !app.state::<DockState>().visible.load(Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(if idle { 500 } else { 80 }));
            if !idle {
                tick(&app);
            }
        })
        .ok();
}

fn tick(app: &AppHandle) {
    let Ok(cur) = app.cursor_position() else { return }; // e.g. Wayland: rely on webview events
    let st = app.state::<DockState>();
    let Some(geo) = *st.geo.lock().unwrap() else { return };
    let visible = st.visible.load(Ordering::SeqCst);
    let in_dock = geo.dock.contains(cur.x, cur.y, 1.0);
    if !visible {
        if !in_dock {
            *st.hover_since.lock().unwrap() = None;
            return;
        }
        if platform::left_button_down() {
            // a drag (file being dragged toward the edge): open at once
            let a = app.clone();
            let _ = app.run_on_main_thread(move || open_panel(&a, "drag"));
            return;
        }
        let delay = Duration::from_millis(app.state::<Core>().0.settings().hover_delay_ms);
        let mut since = st.hover_since.lock().unwrap();
        match *since {
            None => *since = Some(Instant::now()),
            Some(t) if t.elapsed() >= delay => {
                *since = None;
                let a = app.clone();
                let _ = app.run_on_main_thread(move || open_panel(&a, "hover"));
            }
            _ => {}
        }
        return;
    }
    if st.pinned.load(Ordering::SeqCst) {
        return;
    }
    let mut panel = geo.panel;
    // the shell is inset by MARGIN; count the margin as inside to avoid flicker
    panel.x -= 1.0;
    let inside = in_dock || panel.contains(cur.x, cur.y, 0.0);
    let mut last = st.last_inside.lock().unwrap();
    if *last != Some(inside) {
        *last = Some(inside);
        #[derive(Serialize, Clone)]
        struct Inside {
            inside: bool,
        }
        let _ = app.emit_to(EventTarget::webview_window("panel"), "panel://cursor", Inside { inside });
    }
}

// ------------------------------------------------------------------ commands

#[tauri::command]
pub fn cmd_open_panel(app: AppHandle, reason: String) {
    open_panel(&app, &reason);
}

#[tauri::command]
pub fn cmd_hide_panel(app: AppHandle) {
    hide_panel(&app);
}

#[tauri::command]
pub fn set_pinned(app: AppHandle, pinned: bool) {
    app.state::<DockState>().pinned.store(pinned, Ordering::SeqCst);
    let _ = app.emit("panel://pinned", pinned);
    crate::tray::sync(&app);
}

#[derive(Serialize)]
pub struct PanelState {
    visible: bool,
    pinned: bool,
    edge: String,
    reason: String,
}

/// Lets a freshly loaded panel webview catch up if `panel://show` fired before it listened.
#[tauri::command]
pub fn panel_state(app: AppHandle) -> PanelState {
    let st = app.state::<DockState>();
    PanelState {
        visible: st.visible.load(Ordering::SeqCst),
        pinned: st.pinned.load(Ordering::SeqCst),
        edge: app.state::<Core>().0.settings().dock_edge,
        reason: reason_now(&st),
    }
}

fn reason_now(st: &DockState) -> String {
    st.reason.lock().unwrap().clone()
}

/// The panel calls this when its open animation starts; logs the open latency.
/// scripts/memprobe.sh greps for "panel shown".
#[tauri::command]
pub fn panel_shown(app: AppHandle, page_ms: Option<f64>, marks: Option<String>) {
    let st = app.state::<DockState>();
    let requested = st.open_requested.lock().unwrap().take();
    if let Some(t) = requested {
        let kind = if st.cold_open.load(Ordering::SeqCst) { "cold" } else { "warm" };
        log::info!(
            "panel shown {} ms after open request ({kind}; page {:.0} ms since navigation start: {})",
            t.elapsed().as_millis(),
            page_ms.unwrap_or(-1.0),
            marks.unwrap_or_default()
        );
    }
}

#[tauri::command]
pub fn set_hold(app: AppHandle, hold: bool) {
    app.state::<DockState>().hold.store(hold, Ordering::SeqCst);
}

#[tauri::command]
pub fn cmd_open_settings(app: AppHandle) -> Result<(), String> {
    open_settings(&app).map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct MonitorInfo {
    index: usize,
    name: String,
    width: u32,
    height: u32,
    primary: bool,
}

#[tauri::command]
pub fn list_monitors(app: AppHandle) -> Vec<MonitorInfo> {
    let primary = app.primary_monitor().ok().flatten();
    let mut out = vec![];
    if let Some(p) = &primary {
        out.push(MonitorInfo { index: 0, name: p.name().cloned().unwrap_or_else(|| "主显示器".into()), width: p.size().width, height: p.size().height, primary: true });
    }
    for m in app.available_monitors().unwrap_or_default() {
        if let Some(p) = &primary {
            if m.position() == p.position() && m.size() == p.size() {
                continue;
            }
        }
        out.push(MonitorInfo { index: out.len(), name: m.name().cloned().unwrap_or_else(|| format!("显示器 {}", out.len() + 1)), width: m.size().width, height: m.size().height, primary: false });
    }
    out
}
