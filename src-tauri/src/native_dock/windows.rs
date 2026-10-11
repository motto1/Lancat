//! Windows dock strip: a Win32 layered, topmost, non-activating tool window (no WebView2).
//!
//! * hover: WM_MOUSEMOVE + TrackMouseEvent(TME_LEAVE) + a SetTimer for `hover_delay_ms`;
//! * file drags: OLE drags do not deliver WM_MOUSEMOVE to other windows, so they are sensed by the
//!   cursor poller in dock.rs (cursor inside the strip + `GetAsyncKeyState(VK_LBUTTON)`); the strip
//!   is also an OLE drop target (`win_drop`) so the drag shows a copy cursor, not "no entry";
//! * look: drawn into a premultiplied 32-bit DIB and pushed with `UpdateLayeredWindow` (per-pixel
//!   alpha), so the rounded ends are anti-aliased at any DPI (a window region is 1-bit: stair-
//!   stepped corners at 150 %) and fully transparent pixels stay click-through. Same shape as the
//!   Linux strip: idle it is 87 % thick on the screen-edge side and fainter; hover / open it grows
//!   to the full thickness. Colours follow the light/dark theme, unread uses the app accent;
//! * motion: hover and open/close changes ease over the same durations as the panel's window fade
//!   (in 120 ms ease-out, out 140 ms ease-in), on a 16 ms timer that only runs while something
//!   moves; the unread pulse only changes the window's constant alpha (the bitmap is drawn once),
//!   at 20 fps, only while something is unread and the strip is on screen;
//! * layout: display, work-area (taskbar moved / resized / auto-hide), DPI and theme / accent
//!   changes reach this top-level window as broadcasts and re-apply the layout or the visual.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::Instant;

use tauri::AppHandle;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    DIB_RGB_COLORS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, KillTimer, LoadCursorW, RegisterClassW, SetTimer, SetWindowPos, ShowWindow, UpdateLayeredWindow, HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE,
    SPI_SETWORKAREA, SWP_NOACTIVATE, SW_SHOWNOACTIVATE, ULW_ALPHA, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_SETTINGCHANGE, WM_TIMER,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::{DockVisual, Rect};

/// Not exported by `windows` 0.61 without the Win32_UI_Controls feature.
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;
const TIMER_HOVER: usize = 1;
const TIMER_ANIM: usize = 2;
const TIMER_LAYOUT: usize = 3;
const ANIM_TICK_MS: u32 = 16;
/// The pulse is a slow 1.8 s breath: 20 steps/s is smooth, and each step is now a single
/// UpdateLayeredWindow call with the same bitmap (it used to redraw every pixel into a new DIB
/// ~30 times/s, also while the strip was hidden behind the message toast).
const PULSE_TICK_MS: u32 = 50;

/// Idle strip: thickness share (towards the screen interior) and opacity factor, as on Linux.
const IDLE_THICK: f64 = 0.87;
const IDLE_ALPHA: f64 = 0.7;
/// Fill opacity when hovered / open.
const FILL_ALPHA: f64 = 0.8;

static APP: OnceLock<AppHandle> = OnceLock::new();

#[derive(Clone, Copy, PartialEq)]
struct Look {
    /// share of the full thickness, 0..1
    thick: f64,
    /// opacity factor, 0..1
    alpha: f64,
}

struct Anim {
    from: Look,
    to: Look,
    t0: Instant,
    ms: f64,
    ease_in: bool,
}

struct State {
    hwnd: HWND,
    visual: DockVisual,
    hovering: bool,
    pulse_t0: Option<Instant>,
    /// physical size of the window
    w: i32,
    h: i32,
    scale: f64,
    look: Look,
    anim: Option<Anim>,
    timer_on: bool,
    /// Hidden (the message toast stands in for it): no timer at all.
    hidden: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// `try_borrow_mut`: Win32 calls made while the state is borrowed (SetWindowPos, ShowWindow …)
/// can re-enter the window procedure synchronously; those nested messages are simply skipped.
fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

pub fn create(app: &AppHandle) {
    let _ = APP.set(app.clone());
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return };
        let class = w!("LancatDockStrip");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hmod.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class,
            w!("Lancat Dock"),
            WS_POPUP,
            0,
            0,
            6,
            160,
            None,
            None,
            Some(hmod.into()),
            None,
        ) else {
            log::warn!("native dock: CreateWindowExW failed");
            return;
        };
        super::win_drop::register(app, hwnd);
        let idle = Look { thick: IDLE_THICK, alpha: IDLE_ALPHA };
        STATE.with(|s| {
            *s.borrow_mut() =
                Some(State { hwnd, visual: DockVisual::default(), hovering: false, pulse_t0: None, w: 6, h: 160, scale: 1.0, look: idle, anim: None, timer_on: false, hidden: false })
        });
    }
}

/// `r` is in physical pixels (virtual-desktop coordinates); the process is per-monitor DPI aware.
pub fn place(r: Rect, _origin: (f64, f64), scale: f64) {
    with(|s| unsafe {
        let (w, h) = ((r.w.round() as i32).max(1), (r.h.round() as i32).max(1));
        s.w = w;
        s.h = h;
        s.scale = scale;
        let _ = SetWindowPos(s.hwnd, Some(HWND_TOPMOST), r.x.round() as i32, r.y.round() as i32, w, h, SWP_NOACTIVATE);
        render(s);
    });
}

pub fn show() {
    with(|s| unsafe {
        render(s);
        let _ = ShowWindow(s.hwnd, SW_SHOWNOACTIVATE);
    });
}

/// Hidden while the message toast stands in for the strip.
pub fn set_hidden(hidden: bool) {
    with(|s| unsafe {
        s.hidden = hidden;
        sync_timer(s);
        if hidden {
            let _ = ShowWindow(s.hwnd, windows::Win32::UI::WindowsAndMessaging::SW_HIDE);
        } else {
            render(s);
            let _ = ShowWindow(s.hwnd, SW_SHOWNOACTIVATE);
        }
    });
}

pub fn set_visual(v: DockVisual) {
    with(|s| {
        if s.visual == v {
            return;
        }
        let was_open = s.visual.open;
        if v.unread && s.pulse_t0.is_none() {
            s.pulse_t0 = Some(Instant::now());
        } else if !v.unread {
            s.pulse_t0 = None;
        }
        s.visual = v;
        // closing: same duration/easing as the window fade-out; otherwise the fade-in's
        let closing = was_open && !s.visual.open;
        retarget(s, closing);
        render(s);
    });
}

fn target(s: &State) -> Look {
    if s.hovering || s.visual.open {
        Look { thick: 1.0, alpha: 1.0 }
    } else {
        Look { thick: IDLE_THICK, alpha: IDLE_ALPHA }
    }
}

fn retarget(s: &mut State, ease_in: bool) {
    let to = target(s);
    let cur_to = s.anim.as_ref().map(|a| a.to).unwrap_or(s.look);
    if to != cur_to {
        let ms = crate::platform::fade_ms(!ease_in) as f64;
        if ms <= 0.0 {
            s.look = to;
            s.anim = None;
        } else {
            s.anim = Some(Anim { from: s.look, to, t0: Instant::now(), ms, ease_in });
        }
    }
    sync_timer(s);
}

/// One timer for both: 16 ms while easing, 33 ms while only the unread pulse runs, none otherwise.
fn sync_timer(s: &mut State) {
    let want = if s.hidden {
        None
    } else if s.anim.is_some() {
        Some(ANIM_TICK_MS)
    } else if s.pulse_t0.is_some() {
        Some(PULSE_TICK_MS)
    } else {
        None
    };
    unsafe {
        match want {
            Some(ms) => {
                SetTimer(Some(s.hwnd), TIMER_ANIM, ms, None);
                s.timer_on = true;
            }
            None if s.timer_on => {
                let _ = KillTimer(Some(s.hwnd), TIMER_ANIM);
                s.timer_on = false;
            }
            None => {}
        }
    }
}

fn step(s: &mut State) {
    if s.anim.is_none() {
        // only the unread pulse runs: same pixels, new constant alpha
        pulse_only(s);
        return;
    }
    if let Some(a) = &s.anim {
        let t = (a.t0.elapsed().as_secs_f64() * 1000.0 / a.ms).min(1.0);
        let e = if a.ease_in { t * t } else { 1.0 - (1.0 - t) * (1.0 - t) };
        s.look = Look { thick: a.from.thick + (a.to.thick - a.from.thick) * e, alpha: a.from.alpha + (a.to.alpha - a.from.alpha) * e };
        if t >= 1.0 {
            s.look = a.to;
            s.anim = None;
            sync_timer(s);
        }
    }
    render(s);
}

/// Base colour, outline colour (both 0..1 sRGB) for the theme.
fn palette(v: &DockVisual) -> ((f64, f64, f64), (f64, f64, f64, f64)) {
    if v.unread {
        (v.accent, (1.0, 1.0, 1.0, 0.35))
    } else if v.dark {
        // light grey on dark surroundings, a dark hairline keeps it visible over light windows
        ((176.0 / 255.0, 176.0 / 255.0, 188.0 / 255.0), (0.0, 0.0, 0.0, 0.30))
    } else {
        // mid grey (the Linux strip colour), a light hairline keeps it visible over dark windows
        ((128.0 / 255.0, 128.0 / 255.0, 140.0 / 255.0), (1.0, 1.0, 1.0, 0.45))
    }
}

/// Signed distance from (px, py) to a rounded rectangle (negative inside).
fn sd_round_rect(px: f64, py: f64, x: f64, y: f64, w: f64, h: f64, r: f64) -> f64 {
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let (qx, qy) = ((px - cx).abs() - (w / 2.0 - r), (py - cy).abs() - (h / 2.0 - r));
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

/// Draws the strip into a premultiplied BGRA DIB and hands it to DWM (per-pixel alpha).
fn render(s: &State) {
    let (w, h) = (s.w, s.h);
    let v = &s.visual;
    let vertical = v.edge == "left" || v.edge == "right";
    let (fw, fh) = (w as f64, h as f64);
    // the shape hugs the screen edge and grows towards the interior
    let (mut x, mut y, mut rw, mut rh) = (0.0, 0.0, fw, fh);
    if vertical {
        rw = (fw * s.look.thick).max(1.0);
        if v.edge == "right" {
            x = fw - rw;
        }
    } else {
        rh = (fh * s.look.thick).max(1.0);
        if v.edge == "bottom" {
            y = fh - rh;
        }
    }
    let r = (rw.min(rh) / 2.0).min(3.0 * s.scale);
    // unread: drawn at the pulse's peak; the breathing itself is the window's constant alpha
    let alpha = if v.unread { s.look.alpha.max(0.85) } else { FILL_ALPHA * s.look.alpha };
    let ((br, bg, bb), (or, og, ob, oa)) = palette(v);
    let line = s.scale.max(1.0); // one logical pixel of hairline
    let mut px = vec![0u32; (w * h) as usize];
    for j in 0..h {
        for i in 0..w {
            let (cx, cy) = (i as f64 + 0.5, j as f64 + 0.5);
            let d = sd_round_rect(cx, cy, x, y, rw, rh, r);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            // hairline: the outermost `line` pixels of the shape
            let ring = ((d + line) + 0.5).clamp(0.0, 1.0);
            let fa = alpha * cov;
            let la = oa * alpha * ring * cov;
            // hairline over fill (premultiplied "over")
            let a = la + fa * (1.0 - la);
            let cr = or * la + br * fa * (1.0 - la);
            let cg = og * la + bg * fa * (1.0 - la);
            let cb = ob * la + bb * fa * (1.0 - la);
            let q = |c: f64| (c * 255.0).round().clamp(0.0, 255.0) as u32;
            px[(j * w + i) as usize] = q(a) << 24 | q(cr) << 16 | q(cg) << 8 | q(cb);
        }
    }
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        if let Ok(bmp) = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            if !bits.is_null() {
                std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u32, px.len());
            }
            let old = SelectObject(mem, bmp.into());
            let size = SIZE { cx: w, cy: h };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: pulse_alpha(s), AlphaFormat: AC_SRC_ALPHA as u8 };
            let _ = UpdateLayeredWindow(s.hwnd, Some(screen), None, Some(&size), Some(mem), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
}

/// Constant alpha of the unread pulse: 0.55 ↔ 1.0 of the peak over 1.8 s, like the Linux strip.
fn pulse_alpha(s: &State) -> u8 {
    if !s.visual.unread {
        return 255;
    }
    let t = s.pulse_t0.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
    let k = 0.775 + 0.225 * (t * std::f64::consts::TAU / 1.8).cos();
    (k * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Re-blends the last bitmap with the current pulse alpha (no redraw).
fn pulse_only(s: &State) {
    let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: pulse_alpha(s), AlphaFormat: AC_SRC_ALPHA as u8 };
    unsafe {
        let _ = UpdateLayeredWindow(s.hwnd, None, None, None, None, None, COLORREF(0), Some(&blend), ULW_ALPHA);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let app = APP.get();
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let start = with(|s| {
                if s.hovering {
                    return false;
                }
                s.hovering = true;
                let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut tme);
                retarget(s, false);
                render(s);
                true
            })
            .unwrap_or(false);
            if start {
                if let Some(a) = app {
                    SetTimer(Some(hwnd), TIMER_HOVER, super::hover_delay(a) as u32, None);
                    crate::dock::on_hover_start(a);
                }
            }
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with(|s| {
                s.hovering = false;
                retarget(s, true);
                render(s);
            });
            let _ = KillTimer(Some(hwnd), TIMER_HOVER);
            if let Some(a) = app {
                crate::dock::on_hover_end(a);
            }
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            // a click opens the panel, a drag moves the dock (decided by dock::move_tick)
            if let Some(a) = app {
                crate::dock::begin_move(a, true);
            }
            return LRESULT(0);
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_HOVER => {
                    let _ = KillTimer(Some(hwnd), TIMER_HOVER);
                    if let Some(a) = app {
                        crate::dock::open_panel(a, "hover");
                    }
                }
                TIMER_ANIM => {
                    with(step);
                }
                TIMER_LAYOUT => {
                    let _ = KillTimer(Some(hwnd), TIMER_LAYOUT);
                    if let Some(a) = app {
                        crate::dock::apply_layout(a);
                    }
                }
                _ => {}
            }
            return LRESULT(0);
        }
        // monitors added/removed/re-arranged, resolution or scale changed, taskbar moved/resized/
        // auto-hide toggled: re-place strip and panel (debounced; these come in bursts)
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            SetTimer(Some(hwnd), TIMER_LAYOUT, 250, None);
            if msg == WM_DPICHANGED {
                return LRESULT(0); // keep our own size; apply_layout computes it
            }
        }
        WM_SETTINGCHANGE => {
            if wparam.0 as u32 == SPI_SETWORKAREA.0 {
                SetTimer(Some(hwnd), TIMER_LAYOUT, 250, None);
            } else if lparam.0 != 0 {
                // "ImmersiveColorSet": light/dark app theme or accent colour changed
                let p = windows::core::PCWSTR(lparam.0 as *const u16);
                if p.to_string().map(|s| s == "ImmersiveColorSet").unwrap_or(false) {
                    if let Some(a) = app {
                        crate::dock::refresh_visual(a);
                    }
                }
            }
        }
        WM_DWMCOLORIZATIONCOLORCHANGED => {
            if let Some(a) = app {
                crate::dock::refresh_visual(a);
            }
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
