// Prevents additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "linux")]
    {
        // Edge docking needs absolute window positioning + always-on-top, which Wayland does not
        // allow for regular clients. Builds with the `layer-shell` feature dock natively through
        // wlr-layer-shell; other builds run through XWayland (LANCAT_X11=1 forces XWayland).
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let layer = cfg!(feature = "layer-shell") && std::env::var_os("LANCAT_X11").is_none();
        if wayland && !layer && std::env::var_os("GDK_BACKEND").is_none() {
            std::env::set_var("GDK_BACKEND", "x11");
        }
    }
    lancat_lib::run()
}
