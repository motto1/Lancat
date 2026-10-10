//! Windows dock strip: a Win32 layered, topmost, non-activating tool window (no WebView2).
//!
//! TODO-verify: written against the `windows` 0.61 API docs, not compiled or run on Windows yet.
//!
//! * hover: WM_MOUSEMOVE + TrackMouseEvent(TME_LEAVE) + a SetTimer for `hover_delay_ms`;
//! * file drags: OLE drags do not deliver WM_MOUSEMOVE to other windows, so they are sensed by the
//!   cursor poller in dock.rs (cursor inside the strip + `GetAsyncKeyState(VK_LBUTTON)`);
//! * look: solid brush, rounded window region, alpha via SetLayeredWindowAttributes; the unread
//!   pulse is a 80 ms WM_TIMER that only runs while there is something unread.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::Instant;

use tauri::AppHandle;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, SetWindowRgn, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, KillTimer, LoadCursorW, RegisterClassW, SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow, HWND_TOPMOST, IDC_ARROW,
    LWA_ALPHA, MA_NOACTIVATE, SWP_NOACTIVATE, SW_SHOWNOACTIVATE, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::{DockVisual, Rect};

/// Not exported by `windows` 0.61 without the Win32_UI_Controls feature.
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_HOVER: usize = 1;
const TIMER_PULSE: usize = 2;

static APP: OnceLock<AppHandle> = OnceLock::new();

struct State {
    hwnd: HWND,
    visual: DockVisual,
    hovering: bool,
    pulse_t0: Option<Instant>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.borrow_mut().as_mut().map(f))
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
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 191, LWA_ALPHA);
        STATE.with(|s| *s.borrow_mut() = Some(State { hwnd, visual: DockVisual::default(), hovering: false, pulse_t0: None }));
    }
}

pub fn place(r: Rect, _origin: (f64, f64), _scale: f64) {
    with(|s| unsafe {
        let (w, h) = (r.w.round() as i32, r.h.round() as i32);
        let _ = SetWindowPos(s.hwnd, Some(HWND_TOPMOST), r.x.round() as i32, r.y.round() as i32, w, h, SWP_NOACTIVATE);
        let rad = w.min(h);
        let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, rad, rad);
        SetWindowRgn(s.hwnd, Some(rgn), true);
    });
}

pub fn show() {
    with(|s| unsafe {
        let _ = ShowWindow(s.hwnd, SW_SHOWNOACTIVATE);
    });
}

pub fn set_visual(v: DockVisual) {
    with(|s| unsafe {
        if s.visual == v {
            return;
        }
        if v.unread && s.pulse_t0.is_none() {
            s.pulse_t0 = Some(Instant::now());
            SetTimer(Some(s.hwnd), TIMER_PULSE, 80, None);
        } else if !v.unread && s.pulse_t0.is_some() {
            s.pulse_t0 = None;
            let _ = KillTimer(Some(s.hwnd), TIMER_PULSE);
        }
        s.visual = v;
        apply_alpha(s);
        let _ = InvalidateRect(Some(s.hwnd), None, true);
    });
}

fn apply_alpha(s: &State) {
    let a = if s.visual.unread {
        let t = s.pulse_t0.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
        0.775 + 0.225 * (t * std::f64::consts::TAU / 1.8).cos()
    } else if s.hovering || s.visual.open {
        0.9
    } else {
        0.6
    };
    unsafe {
        let _ = SetLayeredWindowAttributes(s.hwnd, COLORREF(0), (a * 255.0) as u8, LWA_ALPHA);
    }
}

fn colorref(r: f64, g: f64, b: f64) -> COLORREF {
    COLORREF(((b * 255.0) as u32) << 16 | ((g * 255.0) as u32) << 8 | (r * 255.0) as u32)
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
                apply_alpha(s);
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
                apply_alpha(s);
            });
            let _ = KillTimer(Some(hwnd), TIMER_HOVER);
            if let Some(a) = app {
                crate::dock::on_hover_end(a);
            }
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            if let Some(a) = app {
                crate::dock::open_panel(a, "hover");
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
                TIMER_PULSE => {
                    with(|s| apply_alpha(s));
                }
                _ => {}
            }
            return LRESULT(0);
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let (r, g, b) = with(|s| if s.visual.unread { s.visual.accent } else { (128.0 / 255.0, 128.0 / 255.0, 140.0 / 255.0) }).unwrap_or((0.5, 0.5, 0.55));
            let brush = CreateSolidBrush(colorref(r, g, b));
            FillRect(hdc, &rc, brush);
            let _ = DeleteObject(brush.into());
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
