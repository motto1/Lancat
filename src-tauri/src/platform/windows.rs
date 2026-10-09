//! Windows 11: Acrylic material (window-vibrancy), DWM rounded corners, OLE-drag sensing via
//! the global mouse-button state, and a one-time inbound firewall rule (elevated `netsh`).
//!
//! TODO-verify: everything in this file is written against the documented Win32 APIs but has
//! not been compiled or run on Windows yet (no Windows build machine available).

use tauri::{Manager, WebviewWindow};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DWM_WINDOW_CORNER_PREFERENCE};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

/// Acrylic/Mica is applied to the whole window, so the shell fills it (no CSS margin).
pub const NATIVE_GLASS: bool = true;

fn hwnd(win: &WebviewWindow) -> Option<HWND> {
    // tauri may link a different `windows` version: go through the raw pointer.
    win.hwnd().ok().map(|h| HWND(h.0 as _))
}

fn round_corners(win: &WebviewWindow) {
    if let Some(h) = hwnd(win) {
        let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
        unsafe {
            let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as *const _, std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32);
        }
    }
}

pub fn setup_dock(win: &WebviewWindow) {
    round_corners(win);
}

pub fn setup_panel(win: &WebviewWindow) {
    // Acrylic first (Win11 popover look), Mica as fallback.
    if window_vibrancy::apply_acrylic(win, Some((0, 0, 0, 0))).is_err() {
        let _ = window_vibrancy::apply_mica(win, None);
    }
    round_corners(win);
}

/// True while the left mouse button is held anywhere (i.e. an OLE drag is in progress when the
/// cursor reaches the strip). The 6 px WebView2 strip does not reliably receive DragEnter.
pub fn left_button_down() -> bool {
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

/// Ask once (UAC prompt) to add an inbound rule for TCP/UDP 47820 on private networks.
pub fn first_run(app: &tauri::AppHandle) {
    let core = app.state::<crate::commands::Core>();
    if core.0.store.kv_get("win_firewall_rule").ok().flatten().is_some() {
        return;
    }
    let _ = core.0.store.kv_set("win_firewall_rule", "requested");
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    let args = format!(
        "/c netsh advfirewall firewall add rule name=\"Lancat\" dir=in action=allow program=\"{exe}\" enable=yes profile=private,domain"
    );
    unsafe {
        let _ = ShellExecuteW(None, w!("runas"), w!("cmd.exe"), &HSTRING::from(args), PCWSTR::null(), SW_HIDE);
    }
}

pub fn init_app(_app: &mut tauri::App) {}
