// Prevents additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "linux")]
    {
        // Edge docking needs absolute window positioning + always-on-top, which Wayland does not
        // allow for regular clients. Run through XWayland unless the user opts out.
        if std::env::var_os("GDK_BACKEND").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("LANCAT_WAYLAND").is_none() {
            std::env::set_var("GDK_BACKEND", "x11");
        }
    }
    lancat_lib::run()
}
