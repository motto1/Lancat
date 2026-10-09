//! Dock strip + panel windows.
//!
//! * `dock`  — 6 px transparent, always-on-top strip on a screen edge (no taskbar entry)
//! * `panel` — ~360×520 chat popover next to the strip
//!
//! Hover/drag sensing has two paths that both end in [`open_panel`]:
//! 1. the dock webview's own `mouseenter` / drag-enter events (works everywhere, incl. Wayland);
//! 2. a cursor poller (every 80 ms, cheap) that catches hovers and OS-level drags even when the
//!    6 px webview does not receive them (Windows OLE drags, macOS). While the panel is visible the
//!    poller also reports whether the pointer is inside it, as a fallback for `mouseleave`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, EventTarget, Manager, Monitor, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::commands::Core;
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
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
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
    Some(Geo { dock, panel })
}

fn place(w: &WebviewWindow, r: Rect) {
    let _ = w.set_size(PhysicalSize::new(r.w.round() as u32, r.h.round() as u32));
    let _ = w.set_position(PhysicalPosition::new(r.x.round() as i32, r.y.round() as i32));
}

/// Creates both windows (hidden panel) and starts the cursor poller.
pub fn create_windows(app: &AppHandle) -> tauri::Result<()> {
    let dock = WebviewWindowBuilder::new(app, "dock", WebviewUrl::App("index.html".into()))
        .title("Lancat Dock")
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .focused(false)
        .visible_on_all_workspaces(true)
        .inner_size(STRIP, 160.0)
        .visible(false)
        .build()?;
    platform::setup_dock(&dock);

    let panel = WebviewWindowBuilder::new(app, "panel", WebviewUrl::App("index.html".into()))
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
        .build()?;
    platform::setup_panel(&panel);

    apply_layout(app);
    let _ = dock.show();
    spawn_poller(app.clone());
    Ok(())
}

/// Re-applies edge / offset / length / monitor from settings.
pub fn apply_layout(app: &AppHandle) {
    let Some(geo) = compute(app) else { return };
    *app.state::<DockState>().geo.lock().unwrap() = Some(geo);
    if let Some(d) = app.get_webview_window("dock") {
        place(&d, geo.dock);
    }
    if let Some(p) = app.get_webview_window("panel") {
        place(&p, geo.panel);
    }
}

pub fn open_panel(app: &AppHandle, reason: &str) {
    let st = app.state::<DockState>();
    let Some(panel) = app.get_webview_window("panel") else { return };
    if !st.visible.swap(true, Ordering::SeqCst) {
        if let Some(geo) = compute(app) {
            *st.geo.lock().unwrap() = Some(geo);
            place(&panel, geo.panel);
        }
        let _ = panel.show();
        *st.last_inside.lock().unwrap() = Some(true);
        let _ = app.emit("panel://visible", true);
    }
    if reason == "tray" || reason == "shortcut" {
        let _ = panel.set_focus();
    }
    let edge = app.state::<Core>().0.settings().dock_edge;
    let _ = app.emit_to(EventTarget::webview_window("panel"), "panel://show", ShowPayload { edge, reason: reason.into() });
}

pub fn hide_panel(app: &AppHandle) {
    let st = app.state::<DockState>();
    if let Some(panel) = app.get_webview_window("panel") {
        let _ = panel.hide();
    }
    st.visible.store(false, Ordering::SeqCst);
    *st.hover_since.lock().unwrap() = None;
    let _ = app.emit("panel://visible", false);
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
    let w = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html".into()))
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
            std::thread::sleep(Duration::from_millis(80));
            tick(&app);
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
