//! Linux: no native blur for arbitrary windows, so the panel uses the CSS translucent
//! fallback (`.glass`). The dock strip gets the GTK "dock" type hint so window managers
//! keep it undecorated, unfocusable and above normal windows.

use tauri::WebviewWindow;

/// No native material: the frontend draws its own translucent shell with a margin.
pub const NATIVE_GLASS: bool = false;

pub fn setup_dock(win: &WebviewWindow) {
    use gtk::prelude::*;
    if let Ok(gw) = win.gtk_window() {
        gw.set_type_hint(gtk::gdk::WindowTypeHint::Dock);
        gw.set_accept_focus(false);
        gw.set_keep_above(true);
    }
}

pub fn setup_panel(win: &WebviewWindow) {
    use gtk::prelude::*;
    if let Ok(gw) = win.gtk_window() {
        gw.set_type_hint(gtk::gdk::WindowTypeHint::Utility);
        gw.set_keep_above(true);
        gw.set_skip_pager_hint(true);
    }
}

/// GTK delivers drag events to the dock webview itself; the poller does not need button state.
pub fn left_button_down() -> bool {
    false
}

pub fn first_run(_app: &tauri::AppHandle) {}

pub fn init_app(_app: &mut tauri::App) {}
