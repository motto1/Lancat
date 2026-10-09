//! Linux: no native blur for arbitrary windows, so the panel uses the CSS translucent
//! fallback (`.glass`).
//!
//! The dock strip is a native GTK window (see `native_dock/linux.rs`); this file only deals
//! with the panel webview.
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

/// WebKitGTK knobs that tauri does not expose: the panel only ever shows one local page, so the
/// in-memory page/resource caches are useless (`DocumentViewer` disables them).
pub fn tune_webview(win: &WebviewWindow) {
    let _ = win.with_webview(|wv| {
        use webkit2gtk::{CacheModel, WebContextExt, WebViewExt};
        if let Some(ctx) = wv.inner().context() {
            if ctx.cache_model() != CacheModel::DocumentViewer {
                ctx.set_cache_model(CacheModel::DocumentViewer);
            }
        }
    });
}

pub fn init_app(_app: &mut tauri::App) {}

/// Returns freed heap pages to the OS (glibc keeps them in its arenas otherwise). Called after the
/// panel webview is destroyed, when a few MB of UI-process WebKit state have just been freed.
pub fn trim_heap() {
    #[cfg(target_env = "gnu")]
    {
        extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: plain libc call without pointers
        unsafe {
            malloc_trim(0);
        }
    }
}

/// Process-wide graphics defaults. Must run before GTK, EGL or WebKit initialise (first thing in
/// `run()`, still single-threaded). Every variable is only set when the user has not set it, and
/// `LANCAT_NO_GL_DEFAULTS=1` skips all of them.
pub fn early_init() {
    if std::env::var_os("LANCAT_NO_GL_DEFAULTS").is_some() {
        return;
    }
    // libglvnd's EGL loads *every* installed vendor library up front and asks each one for a
    // display. On a hybrid laptop whose screen is driven by Intel/AMD that maps both Mesa
    // (libgallium + libLLVM) and the whole NVIDIA EGL stack into the UI process and every web
    // process. Point glvnd at Mesa only when no connected output is driven by the NVIDIA driver.
    if let Some(json) = mesa_only_vendor() {
        set_default("__EGL_VENDOR_LIBRARY_FILENAMES", &json);
    }
}

fn set_default(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        log::info!("graphics default: {key}={value}");
        std::env::set_var(key, value);
    }
}

/// The Mesa EGL vendor JSON, if every connected display is driven by a non-NVIDIA DRM driver.
fn mesa_only_vendor() -> Option<String> {
    let mut connected = 0;
    for ent in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        // connectors are named like card1-eDP-1
        let Some((card, _)) = name.split_once('-') else { continue };
        if !card.starts_with("card") {
            continue;
        }
        if std::fs::read_to_string(ent.path().join("status")).map(|s| s.trim() != "connected").unwrap_or(true) {
            continue;
        }
        let driver = std::fs::read_link(format!("/sys/class/drm/{card}/device/driver")).ok()?;
        if driver.file_name()?.to_string_lossy().starts_with("nvidia") {
            return None;
        }
        connected += 1;
    }
    if connected == 0 {
        return None;
    }
    for dir in ["/etc/glvnd/egl_vendor.d", "/usr/share/glvnd/egl_vendor.d", "/usr/local/share/glvnd/egl_vendor.d"] {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        files.sort();
        for f in files {
            if std::fs::read_to_string(&f).is_ok_and(|s| s.contains("libEGL_mesa")) {
                return Some(f.to_string_lossy().to_string());
            }
        }
    }
    None
}

/// After the last webview is gone: stop WebKit's network process too. WebKitGTK keeps it running
/// even with no web views (tauri keeps the WebContext alive on Linux for that reason, see
/// tauri-apps/tauri#14626); WebKit launches a new one on the next page load, exactly as it does
/// after a network-process crash. `LANCAT_KEEP_NETWORK_PROCESS=1` disables this.
pub fn reap_web_helpers() {
    if std::env::var_os("LANCAT_KEEP_NETWORK_PROCESS").is_some() {
        return;
    }
    let me = std::process::id();
    let Ok(rd) = std::fs::read_dir("/proc") else { return };
    // children, and grandchildren (bubblewrap sandbox puts one process in between)
    let mut parents = vec![me];
    let mut procs = vec![];
    for ent in rd.flatten() {
        let Ok(pid) = ent.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
        let Some(close) = stat.rfind(')') else { continue };
        let comm = stat.get(stat.find('(').map(|i| i + 1).unwrap_or(0)..close).unwrap_or("").to_string();
        let ppid: u32 = stat[close + 2..].split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
        procs.push((pid, ppid, comm));
    }
    for _ in 0..2 {
        let more: Vec<u32> = procs.iter().filter(|p| parents.contains(&p.1) && !parents.contains(&p.0)).map(|p| p.0).collect();
        parents.extend(more);
    }
    for (pid, ppid, comm) in &procs {
        if comm.starts_with("WebKitNetworkPr") && parents.contains(ppid) {
            log::info!("stopping idle WebKitNetworkProcess {pid}");
            // SAFETY: plain kill(2) on our own descendant
            unsafe {
                kill(*pid as i32, 15);
            }
        }
    }
}

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}
