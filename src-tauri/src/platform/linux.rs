//! Linux: no native blur for arbitrary windows, so the panel uses the CSS translucent
//! fallback (`.glass`).
//!
//! Window placement:
//! * X11 / XWayland (default): absolute positions + GTK "dock" / "utility" type hints.
//! * Native Wayland with the `layer-shell` feature: dock and panel become wlr-layer-shell
//!   overlay surfaces anchored to the output (positions expressed as margins), which is the
//!   only way to dock to a screen edge on tiling compositors such as niri or Sway.

use tauri::WebviewWindow;

/// No native material: the frontend draws its own translucent shell with a margin.
pub const NATIVE_GLASS: bool = false;

/// True when this build can and will use wlr-layer-shell (decided once, before windows exist).
#[allow(dead_code)]
pub fn use_layer_shell() -> bool {
    #[cfg(feature = "layer-shell")]
    {
        std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("LANCAT_X11").is_none()
    }
    #[cfg(not(feature = "layer-shell"))]
    {
        false
    }
}

#[cfg(feature = "layer-shell")]
fn init_layer(win: &WebviewWindow, keyboard: bool) {
    use gtk::prelude::*;
    use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
    let Ok(gw) = win.gtk_window() else { return };
    if !gtk_layer_shell::is_supported() {
        log::warn!("compositor has no wlr-layer-shell; falling back to normal windows");
        return;
    }
    // layer-shell must be set up before the surface is realized
    gw.hide();
    if gw.is_realized() {
        gw.unrealize();
    }
    gw.init_layer_shell();
    gw.set_namespace("lancat");
    gw.set_layer(Layer::Overlay);
    gw.set_anchor(Edge::Top, true);
    gw.set_anchor(Edge::Left, true);
    gw.set_exclusive_zone(-1);
    gw.set_keyboard_mode(if keyboard { KeyboardMode::OnDemand } else { KeyboardMode::None });
}

pub fn setup_dock(win: &WebviewWindow) {
    use gtk::prelude::*;
    #[cfg(feature = "layer-shell")]
    if use_layer_shell() {
        init_layer(win, false);
        return;
    }
    if let Ok(gw) = win.gtk_window() {
        gw.set_type_hint(gtk::gdk::WindowTypeHint::Dock);
        gw.set_accept_focus(false);
        gw.set_keep_above(true);
    }
}

pub fn setup_panel(win: &WebviewWindow) {
    use gtk::prelude::*;
    #[cfg(feature = "layer-shell")]
    if use_layer_shell() {
        init_layer(win, true);
        return;
    }
    if let Ok(gw) = win.gtk_window() {
        gw.set_type_hint(gtk::gdk::WindowTypeHint::Utility);
        gw.set_keep_above(true);
        gw.set_skip_pager_hint(true);
    }
}

/// Layer-shell placement: logical offset from the output's top-left corner + logical size.
/// Returns false when the window is a normal window (caller uses set_position/set_size).
pub fn place_layer(win: &WebviewWindow, x: f64, y: f64, w: f64, h: f64) -> bool {
    #[cfg(feature = "layer-shell")]
    if use_layer_shell() {
        let win2 = win.clone();
        let _ = win.run_on_main_thread(move || {
            use gtk::prelude::*;
            use gtk_layer_shell::{Edge, LayerShell};
            if let Ok(gw) = win2.gtk_window() {
                if !gw.is_layer_window() {
                    return;
                }
                gw.set_layer_shell_margin(Edge::Left, x.round() as i32);
                gw.set_layer_shell_margin(Edge::Top, y.round() as i32);
                gw.set_size_request(w.round() as i32, h.round() as i32);
                gw.resize(w.round() as i32, h.round() as i32);
            }
        });
        return true;
    }
    let _ = (win, x, y, w, h);
    false
}

/// GTK delivers drag events to the dock webview itself; the poller does not need button state.
pub fn left_button_down() -> bool {
    false
}

pub fn first_run(_app: &tauri::AppHandle) {}

pub fn init_app(_app: &mut tauri::App) {}
