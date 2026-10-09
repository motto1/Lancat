//! macOS 13+: NSVisualEffectView behind the panel (window-vibrancy), accessory activation
//! policy (no Dock icon), drag sensing through CoreGraphics' global button state.
//!
//! TODO-verify: not compiled or run on macOS yet.
//! TODO(macOS 26): use NSGlassEffectView (Liquid Glass) through objc2 when available at runtime,
//! falling back to NSVisualEffectView on 13–15.

use tauri::WebviewWindow;
use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};

/// NSVisualEffectView fills the window, so the shell has no CSS margin.
pub const NATIVE_GLASS: bool = true;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceButtonState(state_id: i32, button: u32) -> bool;
}

pub fn setup_panel(win: &WebviewWindow) {
    let _ = apply_vibrancy(win, NSVisualEffectMaterial::Popover, Some(NSVisualEffectState::Active), Some(14.0));
}

/// kCGEventSourceStateCombinedSessionState = 0, kCGMouseButtonLeft = 0
pub fn left_button_down() -> bool {
    unsafe { CGEventSourceButtonState(0, 0) }
}

pub fn first_run(_app: &tauri::AppHandle) {}

pub fn tune_webview(_win: &WebviewWindow) {}

pub fn init_app(app: &mut tauri::App) {
    // menu-bar style app: no Dock icon, no app switcher entry
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);
}

pub fn use_layer_shell() -> bool {
    false
}

pub fn place_layer(_win: &WebviewWindow, _x: f64, _y: f64, _w: f64, _h: f64) -> bool {
    false
}

pub fn trim_heap() {}

pub fn early_init() {}

/// WebKit (macOS) / WebView2 (Windows) shut their helper processes down by themselves once the
/// last web view is gone. TODO-verify with memprobe.ps1 / a macOS probe.
pub fn reap_web_helpers() {}
