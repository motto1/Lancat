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
    /// The panel collapsed while the pointer was on the strip: the poller must not hover-open it
    /// again until the pointer has left the strip once (open on entering, not on resting there).
    hover_disarmed: AtomicBool,
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

/// Monitors in the app's numbering: index 0 = primary, then the others in OS order.
fn monitors_ordered(app: &AppHandle) -> Vec<Monitor> {
    if let Some(c) = MOVE_CACHE.lock().unwrap().as_ref() {
        return c.monitors.clone();
    }
    let list = app.available_monitors().unwrap_or_default();
    let Some(primary) = app.primary_monitor().ok().flatten() else { return list };
    let mut out = vec![primary.clone()];
    out.extend(list.into_iter().filter(|m| m.position() != primary.position() || m.size() != primary.size()));
    out
}

fn pick_monitor(app: &AppHandle, index: usize) -> Option<Monitor> {
    let list = monitors_ordered(app);
    list.get(index).or(list.first()).cloned()
}

/// What a dock drag reads on every 16 ms step, snapshotted when the drag starts: it used to query
/// the settings (SQLite) and the monitor list several times per step (place_for, compute,
/// refresh_visual's peer list for the unread flag).
struct MoveCache {
    monitors: Vec<Monitor>,
    settings: lancat_core::Settings,
    unread: bool,
}
static MOVE_CACHE: Mutex<Option<MoveCache>> = Mutex::new(None);

/// The settings, from the drag snapshot while the dock is being dragged.
fn settings_now(app: &AppHandle) -> lancat_core::Settings {
    if let Some(c) = MOVE_CACHE.lock().unwrap().as_ref() {
        return c.settings.clone();
    }
    app.state::<Core>().0.settings()
}

/// Edge, offset and monitor in effect: the live ones while the dock is being dragged, else the
/// saved settings.
fn placement(app: &AppHandle) -> (String, f64, usize) {
    if let Some(p) = MOVE_PLACE.lock().unwrap().clone() {
        return p;
    }
    let s = settings_now(app);
    (s.dock_edge, s.dock_offset, s.dock_monitor)
}

pub(crate) fn current_edge(app: &AppHandle) -> String {
    placement(app).0
}

fn compute(app: &AppHandle) -> Option<Geo> {
    let s = settings_now(app);
    let (edge_now, offset_now, monitor_now) = placement(app);
    let m = pick_monitor(app, monitor_now)?;
    let sc = m.scale_factor();
    let wa = m.work_area();
    let (ax, ay, aw, ah) = (wa.position.x as f64, wa.position.y as f64, wa.size.width as f64, wa.size.height as f64);
    // Windows: the strip lives in the work area. On the full monitor rect it sat on (and fought
    // the z-order with) a taskbar on that edge, and a bottom/top panel opened over the taskbar;
    // along the edge it could also run into a taskbar on the adjacent edge. Linux keeps the
    // monitor edge (layer-shell overlay; bars there are the compositor's business).
    #[cfg(windows)]
    let (mx, my, mw, mh) = (ax, ay, aw, ah);
    #[cfg(not(windows))]
    let (mx, my, mw, mh) = (m.position().x as f64, m.position().y as f64, m.size().width as f64, m.size().height as f64);

    let t = (STRIP * sc).round();
    let along = offset_now.clamp(0.0, 1.0);
    let pw = ((PANEL_W + 2.0 * MARGIN) * sc).round();
    let ph = ((PANEL_H + 2.0 * MARGIN) * sc).round();
    let gap = (2.0 * sc).round();
    let edge = edge_now.as_str();
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
    // one-time: the collapsed panel is now suspended (no CPU, working set trimmed) instead of only
    // parked, so keeping it for 30 min instead of the old default 60 s costs little and spares the
    // slow cold start after a pause (Windows only: elsewhere a kept panel stays fully resident)
    #[cfg(windows)]
    {
        let core = &app.state::<Core>().0;
        if core.store.kv_get("keep_alive_v2").ok().flatten().is_none() {
            let _ = core.store.kv_set("keep_alive_v2", "1");
            let mut s = core.settings();
            if s.panel_keep_alive_s == 60 {
                s.panel_keep_alive_s = 1800;
                let _ = core.set_settings(s);
            }
        }
    }
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
    crate::timers::after(Duration::from_secs(8), move || {
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

/// Set while the panel webview is being built. On Windows, WebView2 creation pumps the message
/// loop (`wait_with_pump`), so dock messages (hover, click, drag) are dispatched *inside*
/// `ensure_panel`. Re-entering the builder there locks tauri-runtime-wry's web-context mutex a
/// second time on the same thread and deadlocks the UI thread for good.
static PANEL_CREATING: AtomicBool = AtomicBool::new(false);
/// An open requested while the panel was being created; replayed once creation finishes.
static PENDING_OPEN: Mutex<Option<String>> = Mutex::new(None);

struct CreatingGuard(AppHandle);
impl Drop for CreatingGuard {
    fn drop(&mut self) {
        PANEL_CREATING.store(false, Ordering::SeqCst);
        if let Some(reason) = PENDING_OPEN.lock().unwrap().take() {
            let a = self.0.clone();
            // queued behind the current message, never run inside the creation pump
            let _ = self.0.run_on_main_thread(move || open_panel(&a, &reason));
        }
    }
}

/// Returns the panel window, creating it (hidden) if it does not exist yet.
fn ensure_panel(app: &AppHandle) -> Option<WebviewWindow> {
    if let Some(p) = app.get_webview_window("panel") {
        return Some(p);
    }
    if PANEL_CREATING.swap(true, Ordering::SeqCst) {
        log::debug!("ensure_panel re-entered while the panel is being created; skipped");
        return None;
    }
    let _guard = CreatingGuard(app.clone());
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
        // tao's undecorated shadow adds invisible resize borders around the client area (the
        // panel would land off its computed rect); Windows gets a DWM shadow in setup_panel instead
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
            "edge": current_edge(app),
            "reason": reason_now(&st),
        },
    });
    // JSON is valid JS; escape "</" just in case the payload ever ends up inside markup
    format!("window.__LANCAT_BOOT__ = {};", boot.to_string().replace("</", "<\\/"))
}

/// Pointer entered the strip: pre-warm the panel webview so the hover delay hides its start-up.
pub fn on_hover_start(app: &AppHandle) {
    wake_poller();
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
        // Windows: on_hover_start bumped panel_gen, which cancelled the suspend a collapse had
        // scheduled; touching the strip within 4 s of a collapse left the parked panel rendering
        // (unsuspended) until the 30 min reaper. Re-arm it.
        #[cfg(windows)]
        schedule_suspend(app);
    }
}

/// Generic hover-delay timer for backends without a native one (macOS).
#[allow(dead_code)]
pub fn schedule_hover_open(app: &AppHandle) {
    let st = app.state::<DockState>();
    let gen = st.hover_gen.load(Ordering::SeqCst);
    let delay = Duration::from_millis(app.state::<Core>().0.settings().hover_delay_ms);
    let a = app.clone();
    crate::timers::after(delay, move || {
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
    crate::timers::after(Duration::from_secs(secs), move || {
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
                crate::timers::after(Duration::from_secs(2), || {
                    platform::reap_web_helpers();
                    platform::trim_heap();
                });
            }
        });
    });
}

/// Pushes edge / unread / open state to the native strip. Main thread only.
pub fn refresh_visual(app: &AppHandle) {
    let cached = MOVE_CACHE.lock().unwrap().as_ref().map(|c| (c.unread, c.settings.clone()));
    let (unread, settings) = cached.unwrap_or_else(|| {
        let core = &app.state::<Core>().0;
        (core.peers().iter().any(|p| !p.blocked && p.unread > 0), core.settings())
    });
    #[cfg(not(windows))]
    let _ = &settings;
    // Windows: "open" once the parked panel actually starts fading in (platform::reveal calls back)
    let open = app.state::<DockState>().visible.load(Ordering::SeqCst) && platform_revealed();
    #[allow(unused_mut)]
    let mut visual = DockVisual { edge: current_edge(app), unread, open, ..Default::default() };
    // accent stays the default #0a84ff: the page does not use the system accent on Windows
    // either (WebView2's CSS `AccentColor` is not the Windows accent), so both show the same blue
    #[cfg(windows)]
    {
        visual.dark = platform::theme_is_dark(&settings.theme);
    }
    native_dock::set_visual(visual);
}

fn platform_revealed() -> bool {
    #[cfg(windows)]
    return platform::panel_revealed();
    #[cfg(not(windows))]
    true
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
    if reason == "hover" && is_moving() {
        return;
    }
    #[cfg(windows)]
    native_dock::toast_dismiss();
    log::debug!("open_panel({reason})");
    if PANEL_CREATING.load(Ordering::SeqCst) {
        // dispatched from inside the WebView2 creation pump: defer until the panel exists
        log::debug!("open_panel({reason}) deferred: panel is being created");
        *PENDING_OPEN.lock().unwrap() = Some(reason.to_string());
        return;
    }
    let st = app.state::<DockState>();
    st.panel_gen.fetch_add(1, Ordering::SeqCst); // cancel a pending destroy
    *st.reason.lock().unwrap() = reason.to_string();
    let existed = app.get_webview_window("panel").is_some();
    // mark visible before a cold create so the boot script tells the page to animate in at once
    let was_visible = st.visible.swap(true, Ordering::SeqCst);
    wake_poller();
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
    // Windows: creating the WebView2 pumps messages, so a hide queued meanwhile (--hide-panel right
    // after a cold open) may already have run inside ensure_panel; do not show the panel after it.
    #[cfg(windows)]
    if !was_visible && !st.visible.load(Ordering::SeqCst) {
        log::debug!("panel hidden while it was being created; not showing it");
        return;
    }
    if !was_visible {
        if let Some(geo) = compute(app) {
            *st.geo.lock().unwrap() = Some(geo);
            place(&panel, geo.panel, &geo);
        }
        #[cfg(not(windows))]
        let _ = panel.show();
        // Windows: shown (or un-parked) at alpha 0, brought in once the page reports a frame
        #[cfg(windows)]
        platform::panel_resume(&panel);
        #[cfg(windows)]
        platform::panel_open(&panel, st.cold_open.load(Ordering::SeqCst));
        #[cfg(windows)]
        platform::panel_visibility(&panel, true);
        *st.last_inside.lock().unwrap() = Some(true);
        let _ = app.emit("panel://visible", true);
        refresh_visual(app);
    }
    // opened on purpose (tray, shortcut, a click on the message toast): take the keyboard so the
    // user can type the reply at once. Hover / drag opens never steal focus.
    if reason == "tray" || reason == "shortcut" || reason.starts_with("toast:") {
        let _ = panel.set_focus();
    }
    let edge = current_edge(app);
    let _ = app.emit_to(EventTarget::webview_window("panel"), "panel://show", ShowPayload { edge, reason: reason.into() });
}

pub fn hide_panel(app: &AppHandle) {
    let st = app.state::<DockState>();
    // A collapse can reach here up to three times (the page's own hidePanel after its animation,
    // request_collapse's "not revealed yet" path and its 1 s fallback): each run restarted the
    // fade-out from the current alpha and queued another suspend and destroy. Only the first one
    // of a close does the work.
    if !st.visible.swap(false, Ordering::SeqCst) {
        log::debug!("hide_panel: already hidden");
        return;
    }
    log::debug!("hide_panel");
    if let Some(panel) = app.get_webview_window("panel") {
        #[cfg(not(windows))]
        let _ = panel.hide();
        // Windows: parked (shown, transparent, click-through) so the next open needs no resume
        #[cfg(windows)]
        platform::panel_close(&panel);
        #[cfg(windows)]
        platform::panel_visibility(&panel, false);
    }
    st.cold_open.store(false, Ordering::SeqCst);
    *st.hover_since.lock().unwrap() = None;
    st.hover_disarmed.store(cursor_in_dock(app), Ordering::SeqCst);
    let _ = app.emit("panel://visible", false);
    refresh_visual(app);
    schedule_destroy(app);
    #[cfg(windows)]
    schedule_suspend(app);
}

/// A few seconds after collapsing (unless reopened), suspend the parked panel page.
#[cfg(windows)]
fn schedule_suspend(app: &AppHandle) {
    let gen = app.state::<DockState>().panel_gen.load(Ordering::SeqCst);
    let a = app.clone();
    crate::timers::after(Duration::from_secs(4), move || {
        let a2 = a.clone();
        let _ = a.run_on_main_thread(move || {
            let st = a2.state::<DockState>();
            if st.visible.load(Ordering::SeqCst) || st.hovering.load(Ordering::SeqCst) || st.panel_gen.load(Ordering::SeqCst) != gen || is_moving() {
                return;
            }
            if let Some(p) = a2.get_webview_window("panel") {
                platform::panel_suspend(&p);
            }
        });
    });
}

/// Asks the panel to play its collapse animation (it hides itself afterwards).
pub fn request_collapse(app: &AppHandle) {
    let _ = app.emit_to(EventTarget::webview_window("panel"), "panel://collapse", ());
    // The page may not act on it (still loading, listeners not attached yet, or its own state
    // already says hidden), which left the window on screen after `--hide-panel`. If nothing else
    // happened meanwhile (no re-open, no hide), hide it from here.
    let st = app.state::<DockState>();
    if !st.visible.load(Ordering::SeqCst) {
        return;
    }
    // Windows: an open that has not been revealed yet (cold page still loading, or the webview
    // still being created: this may run off the main thread meanwhile) has nothing to animate out;
    // park it as soon as the main thread is free instead of letting it fade in and hiding it 1 s
    // later. The fallback below still covers a reveal that wins the race.
    #[cfg(windows)]
    if !platform::panel_revealed() {
        let a = app.clone();
        let _ = app.run_on_main_thread(move || {
            if a.state::<DockState>().visible.load(Ordering::SeqCst) && !platform::panel_revealed() {
                log::debug!("collapse requested before the panel was revealed; hiding at once");
                hide_panel(&a);
            }
        });
    }
    let gen = st.panel_gen.load(Ordering::SeqCst);
    let a = app.clone();
    crate::timers::after(Duration::from_millis(1000), move || {
        let a2 = a.clone();
        let _ = a.run_on_main_thread(move || {
            let st = a2.state::<DockState>();
            if st.visible.load(Ordering::SeqCst) && st.panel_gen.load(Ordering::SeqCst) == gen {
                log::info!("panel did not collapse itself within 1 s; hiding it");
                hide_panel(&a2);
            }
        });
    });
}

/// Pointer currently over the strip (false where the cursor position is unknown, e.g. Wayland).
fn cursor_in_dock(app: &AppHandle) -> bool {
    let Ok(cur) = app.cursor_position() else { return false };
    let geo = *app.state::<DockState>().geo.lock().unwrap();
    geo.is_some_and(|g| g.dock.contains(cur.x, cur.y, 1.0))
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

/// The pointer is near the strip, a button is held or the panel is open: poll at full rate.
static POLL_NEAR: AtomicBool = AtomicBool::new(true);
/// Wakes the poller early (pointer entered the strip, panel opened).
static POLL_WAKE: (Mutex<bool>, std::sync::Condvar) = (Mutex::new(false), std::sync::Condvar::new());

pub(crate) fn wake_poller() {
    *POLL_WAKE.0.lock().unwrap() = true;
    POLL_WAKE.1.notify_one();
}

/// Poll period while nothing is near: the strip senses plain hovers itself (WM_MOUSEMOVE + its
/// own hover timer), the poller is only needed for OLE drags (which deliver no mouse messages)
/// and the pointer-inside fallback of the open panel. It used to tick every 80 ms (12.5
/// wake-ups/s) for the whole life of the app; far from the strip it now ticks every 300 ms and
/// switches to 80 ms as soon as the pointer is within reach, a button is held or the panel is open.
const POLL_FAR_MS: u64 = 300;
/// "Near": within this many logical px of the strip.
const NEAR_PX: f64 = 240.0;

fn spawn_poller(app: AppHandle) {
    std::thread::Builder::new()
        .name("lancat-dock-poller".into())
        .spawn(move || loop {
            // Linux: the native strip gets enter/leave/drag itself, so the poller only matters while
            // the panel is visible (pointer-inside fallback). Elsewhere it also senses OLE drags.
            let visible = app.state::<DockState>().visible.load(Ordering::SeqCst);
            let idle = cfg!(target_os = "linux") && !visible;
            let ms = if is_moving() {
                16
            } else if idle {
                500
            } else if visible || POLL_NEAR.load(Ordering::SeqCst) {
                80
            } else {
                POLL_FAR_MS
            };
            {
                let g = POLL_WAKE.0.lock().unwrap();
                let (mut g, _) = POLL_WAKE.1.wait_timeout_while(g, Duration::from_millis(ms), |woken| !*woken).unwrap();
                *g = false;
            }
            if !idle {
                tick(&app);
            }
        })
        .ok();
}

/// Left button state at the last poller tick.
static LEFT_DOWN: AtomicBool = AtomicBool::new(false);
/// The current press started outside the strip and the panel.
static PRESS_OUTSIDE: AtomicBool = AtomicBool::new(false);

// ------------------------------------------------------------------ message toast
//
// A message from a peer while the panel is closed: the strip turns into a pill saying who sent
// what for a few seconds (Windows: native, see native_dock/win_toast.rs). Main thread only.
pub fn on_incoming(app: &AppHandle, m: &lancat_core::Message) {
    if m.outgoing || app.state::<DockState>().visible.load(Ordering::SeqCst) || is_moving() {
        return;
    }
    let core = &app.state::<Core>().0;
    let Some(peer) = core.peers().into_iter().find(|p| p.id == m.peer_id) else { return };
    if peer.blocked {
        return;
    }
    let s = |k: &str| m.body.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let text = match m.kind.as_str() {
        "text" => s("text").split_whitespace().collect::<Vec<_>>().join(" "),
        "emoji" => {
            let alt = s("alt");
            if s("kind") == "sticker" { format!("[贴纸] {alt}") } else if alt.is_empty() { "[表情]".into() } else { alt }
        }
        "file" => {
            let size = m.body.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            format!("📄 {} · {}", s("name"), human_size(size))
        }
        _ => return,
    };
    #[cfg(windows)]
    {
        let Some(geo) = *app.state::<DockState>().geo.lock().unwrap() else { return };
        let dark = platform::theme_is_dark(&core.settings().theme);
        let hold_ms = 4000 + (text.chars().count() as u32 * 60).min(3000);
        native_dock::toast_show(
            app,
            native_dock::ToastSpec { peer_id: peer.id.clone(), name: peer.name.clone(), text, edge: current_edge(app), strip: geo.dock, scale: geo.scale, dark, hold_ms },
        );
    }
    #[cfg(not(windows))]
    let _ = text;
}

fn human_size(n: u64) -> String {
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

// ------------------------------------------------------------------ moving the dock
//
// Press on the strip (or the panel header) and drag: the strip follows the pointer to the nearest
// edge of the monitor under it, at the pointer's position along that edge; the panel, if open,
// moves with it. Releasing saves edge / offset / monitor. A press on the strip that does not
// move is a click (opens the panel). Driven by the cursor poller (16 ms while moving) and the
// global button state, so it needs no mouse capture on any window.

struct Move {
    from_strip: bool,
    start: (f64, f64),
    moved: bool,
}

static MOVE: Mutex<Option<Move>> = Mutex::new(None);
/// (edge, offset, monitor) while dragging; `compute` prefers it over the settings.
static MOVE_PLACE: Mutex<Option<(String, f64, usize)>> = Mutex::new(None);

pub fn is_moving() -> bool {
    MOVE.lock().unwrap().is_some()
}

/// The left button went down on the strip (`from_strip`) or the panel header.
pub fn begin_move(app: &AppHandle, from_strip: bool) {
    #[cfg(windows)]
    native_dock::toast_dismiss();
    match app.cursor_position() {
        Ok(c) => {
            let core = &app.state::<Core>().0;
            let unread = core.peers().iter().any(|p| !p.blocked && p.unread > 0);
            let monitors = monitors_ordered(app);
            *MOVE_CACHE.lock().unwrap() = Some(MoveCache { monitors, settings: core.settings(), unread });
            *MOVE.lock().unwrap() = Some(Move { from_strip, start: (c.x, c.y), moved: false });
        }
        Err(_) if from_strip => open_panel(app, "hover"),
        Err(_) => {}
    }
}

/// Where the dock goes for a pointer at (x, y): monitor index (pick_monitor's numbering), nearest
/// edge of its work area, centre of the strip at the pointer.
fn place_for(app: &AppHandle, x: f64, y: f64) -> Option<(String, f64, usize)> {
    let list = monitors_ordered(app);
    let inside = |m: &Monitor| {
        let (p, s) = (m.position(), m.size());
        x >= p.x as f64 && x < (p.x + s.width as i32) as f64 && y >= p.y as f64 && y < (p.y + s.height as i32) as f64
    };
    let idx = list.iter().position(inside)?;
    let m = &list[idx];
    let sc = m.scale_factor();
    #[cfg(windows)]
    let (ax, ay, aw, ah) = {
        let wa = m.work_area();
        (wa.position.x as f64, wa.position.y as f64, wa.size.width as f64, wa.size.height as f64)
    };
    #[cfg(not(windows))]
    let (ax, ay, aw, ah) = (m.position().x as f64, m.position().y as f64, m.size().width as f64, m.size().height as f64);
    let len = settings_now(app).dock_length.clamp(40.0, 2000.0) * sc;
    let d = [("left", x - ax), ("right", ax + aw - x), ("top", y - ay), ("bottom", ay + ah - y)];
    let edge = d.iter().min_by(|a, b| a.1.total_cmp(&b.1))?.0;
    let along = |c: f64, start: f64, span: f64| if span <= len { 0.5 } else { ((c - start - len / 2.0) / (span - len)).clamp(0.0, 1.0) };
    let offset = if edge == "left" || edge == "right" { along(y, ay, ah) } else { along(x, ax, aw) };
    Some((edge.to_string(), (offset * 1000.0).round() / 1000.0, idx))
}

/// One poller step while the dock is being dragged; false when no drag is in progress.
fn move_tick(app: &AppHandle, x: f64, y: f64, scale: f64) -> bool {
    let mut g = MOVE.lock().unwrap();
    let Some(m) = g.as_mut() else { return false };
    if !platform::left_button_down() {
        let m = g.take().unwrap();
        drop(g);
        let place = MOVE_PLACE.lock().unwrap().take();
        *MOVE_CACHE.lock().unwrap() = None;
        let a = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let (true, Some((edge, offset, monitor))) = (m.moved, place) {
                log::info!("dock moved to {edge} {offset:.3} on monitor {monitor}");
                let core = a.state::<Core>();
                let mut s = core.0.settings();
                s.dock_edge = edge;
                s.dock_offset = offset;
                s.dock_monitor = monitor;
                if let Err(e) = core.0.set_settings(s) {
                    log::warn!("saving the dock position failed: {e}");
                }
                apply_layout(&a);
            } else if m.from_strip && !m.moved {
                open_panel(&a, "hover");
            }
        });
        return true;
    }
    if !m.moved {
        let (dx, dy) = (x - m.start.0, y - m.start.1);
        if (dx * dx + dy * dy).sqrt() < 6.0 * scale {
            return true;
        }
        m.moved = true;
    }
    drop(g);
    let Some(p) = place_for(app, x, y) else { return true };
    let prev = MOVE_PLACE.lock().unwrap().replace(p.clone());
    if prev.as_ref() != Some(&p) {
        let edge_changed = prev.map(|q| q.0).unwrap_or_else(|| settings_now(app).dock_edge) != p.0;
        let a = app.clone();
        let _ = app.run_on_main_thread(move || {
            apply_layout(&a);
            if edge_changed {
                let _ = a.emit_to(EventTarget::webview_window("panel"), "panel://edge", p.0.clone());
            }
        });
    }
    true
}

fn tick(app: &AppHandle) {
    let Ok(cur) = app.cursor_position() else { return }; // e.g. Wayland: rely on webview events
    {
        let scale = app.state::<DockState>().geo.lock().unwrap().map(|g| g.scale).unwrap_or(1.0);
        if move_tick(app, cur.x, cur.y, scale) {
            return;
        }
    }
    let st = app.state::<DockState>();
    let Some(geo) = *st.geo.lock().unwrap() else { return };
    let visible = st.visible.load(Ordering::SeqCst);
    let in_dock = geo.dock.contains(cur.x, cur.y, 1.0);
    let in_panel = visible && geo.panel.contains(cur.x, cur.y, 0.0);
    // Mouse button: a press that started outside our windows and is still held is (likely) a file
    // drag from another app.
    let down = platform::left_button_down();
    POLL_NEAR.store(down || geo.dock.contains(cur.x, cur.y, NEAR_PX * geo.scale), Ordering::SeqCst);
    let was_down = LEFT_DOWN.swap(down, Ordering::SeqCst);
    if down && !was_down {
        PRESS_OUTSIDE.store(!in_dock && !in_panel, Ordering::SeqCst);
    }
    if was_down && !down && visible {
        // A drag that opened the panel may end anywhere (on the strip, outside, Esc): the page
        // only hears about drags that reach it, and its "dragging" flag holds the panel open.
        let _ = app.emit_to(EventTarget::webview_window("panel"), "panel://button-up", ());
    }
    // Windows: WebView2 does not reliably take OLE drops, an invisible native window over the
    // panel does (native_dock::win_drop).
    #[cfg(windows)]
    {
        let want = in_panel && down && PRESS_OUTSIDE.load(Ordering::SeqCst);
        if want != platform::drop_overlay_shown() {
            let a = app.clone();
            let _ = app.run_on_main_thread(move || {
                let w = if want { a.get_webview_window("panel") } else { None };
                platform::drop_overlay(w.as_ref());
            });
        }
    }
    if !visible {
        if !in_dock {
            *st.hover_since.lock().unwrap() = None;
            st.hover_disarmed.store(false, Ordering::SeqCst);
            return;
        }
        if platform::left_button_down() {
            // a drag (file being dragged toward the edge): open at once
            let a = app.clone();
            let _ = app.run_on_main_thread(move || open_panel(&a, "drag"));
            return;
        }
        // The message toast stands in for the strip (same place): resting on it must not
        // hover-open the panel behind the user's back; a click on the toast opens it.
        #[cfg(windows)]
        if native_dock::toast_shown() {
            *st.hover_since.lock().unwrap() = None;
            return;
        }
        if st.hover_disarmed.load(Ordering::SeqCst) {
            // collapsed with the pointer resting here: wait until it leaves and comes back
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
pub async fn cmd_open_panel(app: AppHandle, reason: String) {
    // may create the panel webview: async + queued, see cmd_open_settings
    let a = app.clone();
    let _ = app.run_on_main_thread(move || open_panel(&a, &reason));
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
    // The page calls this once its listeners are attached. `panel://cursor` is only sent on a
    // change, and a cold page misses the ones sent while it loads (pointer already gone): it then
    // believed the pointer was still inside and never collapsed. Forget what was sent so the
    // poller re-sends the real state on its next tick.
    *st.last_inside.lock().unwrap() = None;
    PanelState {
        visible: st.visible.load(Ordering::SeqCst),
        pinned: st.pinned.load(Ordering::SeqCst),
        edge: current_edge(&app),
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

/// Windows: the page painted a frame after `panel://show`, so the parked panel can come in.
#[tauri::command]
pub fn panel_ready(app: AppHandle) {
    #[cfg(windows)]
    if let Some(p) = app.get_webview_window("panel") {
        platform::panel_ready(&p);
    }
    #[cfg(not(windows))]
    let _ = app;
}

#[tauri::command]
pub fn set_hold(app: AppHandle, hold: bool) {
    app.state::<DockState>().hold.store(hold, Ordering::SeqCst);
}

// Synchronous commands run on the main thread *inside* WebView2's WebMessageReceived callback.
// Creating another WebView2 controller there never completes (WebView2 does not deliver the
// creation callback while one of its own event handlers is still on the stack): the nested
// `wait_with_pump` spins forever, the new window stays blank and cannot be closed. Window-creating
// commands are therefore async (run off the main thread), and `run_on_main_thread` called from
// there posts the work to the event loop instead of running it inline.
#[tauri::command]
pub async fn cmd_open_settings(app: AppHandle) -> Result<(), String> {
    let a = app.clone();
    app.run_on_main_thread(move || {
        if let Err(e) = open_settings(&a) {
            log::warn!("opening settings failed: {e}");
        }
    })
    .map_err(|e| e.to_string())
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
