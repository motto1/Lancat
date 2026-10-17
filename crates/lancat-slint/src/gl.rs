//! Linux/Wayland GPU 路径（EGL + GLES 3）。占位：还没接上时一律走 wl_shm 的 CPU 路径。
#![allow(dead_code)]

/// 见 render::status
pub fn status() -> &'static str {
    if crate::render::pref_cpu() {
        "cpu"
    } else {
        "none"
    }
}
