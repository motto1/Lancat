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

/// Reliable pointer enter/leave for the panel straight from GDK (wl_pointer.enter/leave on
/// Wayland). WebKitGTK does not deliver DOM `mouseleave` when the pointer leaves a layer-shell
/// surface, so the frontend never learned the pointer had gone and the panel never collapsed.
fn watch_crossing(win: &WebviewWindow) {
    use gtk::prelude::*;
    use tauri::Manager;
    let Ok(gw) = win.gtk_window() else { return };
    gw.add_events(gtk::gdk::EventMask::ENTER_NOTIFY_MASK | gtk::gdk::EventMask::LEAVE_NOTIFY_MASK);
    let app = win.app_handle().clone();
    gw.connect_enter_notify_event(move |_, ev| {
        if ev.detail() != gtk::gdk::NotifyType::Inferior {
            crate::dock::emit_cursor(&app, true);
        }
        gtk::glib::Propagation::Proceed
    });
    let app = win.app_handle().clone();
    gw.connect_leave_notify_event(move |_, ev| {
        log::debug!("panel leave-notify detail={:?}", ev.detail());
        if ev.detail() != gtk::gdk::NotifyType::Inferior {
            crate::dock::emit_cursor(&app, false);
        }
        gtk::glib::Propagation::Proceed
    });
}

pub fn setup_panel(win: &WebviewWindow) {
    use gtk::prelude::*;
    watch_crossing(win);
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

/// Whether the pointer is over one of our surfaces (panel or strip). On Wayland GDK only knows the
/// pointer while it is over one of ours, which is exactly the question. Main thread.
pub fn pointer_in_app() -> Option<bool> {
    use gtk::gdk::prelude::*;
    let seat = gtk::gdk::Display::default()?.default_seat()?;
    let ptr = seat.pointer()?;
    let (w, _, _) = ptr.window_at_position();
    Some(w.is_some())
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
    if std::env::var_os("LANCAT_KEEP_LIB_PAGES").is_none() {
        drop_clean_lib_pages();
    }
}

/// After the web view is gone, most of what the UI process still holds are *clean* pages of the
/// libraries WebKit pulled in: libwebkit2gtk / libjavascriptcoregtk code, and Mesa's libgallium +
/// libLLVM that GTK loaded to composite the GL output (≈ 48 MB on the niri box, see
/// docs/postmortem-memory.md). They are identical to the files on disk, but because no other
/// process maps them they count as this process's private memory (USS) until the kernel happens
/// to reclaim them under pressure.
///
/// `MADV_DONTNEED` on a read-only, never-written private file mapping only drops our page-table
/// entries: the pages stay in the page cache (reclaimable, no longer charged to us) and the next
/// access is a minor fault. It must never touch a mapping that holds copy-on-write data (RELRO,
/// .data), because there DONTNEED would revert it to the file's contents, so only mappings that
/// are not writable and have `Anonymous: 0 kB` are released. (`MADV_PAGEOUT` would be the obvious
/// call, but since Linux 6.2 it skips file pages of files the caller cannot write, i.e. /usr/lib.)
/// `LANCAT_KEEP_LIB_PAGES=1` disables it.
fn drop_clean_lib_pages() {
    const MADV_DONTNEED: i32 = 4;
    extern "C" {
        fn madvise(addr: *mut std::ffi::c_void, len: usize, advice: i32) -> i32;
    }
    let Ok(smaps) = std::fs::read_to_string("/proc/self/smaps") else { return };
    let exe = std::fs::read_link("/proc/self/exe").ok();
    let mut released = 0usize;
    let mut cur: Option<(usize, usize, bool)> = None; // start, end, candidate
    let mut flush = |cur: &mut Option<(usize, usize, bool)>, anon_kb: Option<u64>| {
        if let (Some((s, e, true)), Some(0)) = (*cur, anon_kb) {
            // SAFETY: the range is one whole VMA of a read-only, unmodified file mapping; dropping
            // its pages only makes the next access re-read them from the page cache.
            if unsafe { madvise(s as *mut _, e - s, MADV_DONTNEED) } == 0 {
                released += e - s;
            }
        }
        *cur = None;
    };
    for line in smaps.lines() {
        let mut it = line.split_whitespace();
        let Some(first) = it.next() else { continue };
        if let Some((a, b)) = first.split_once('-').filter(|_| !first.ends_with(':')) {
            // mapping header: "start-end perms offset dev inode path"
            let (Ok(s), Ok(e)) = (usize::from_str_radix(a, 16), usize::from_str_radix(b, 16)) else { continue };
            let perms = it.next().unwrap_or("");
            let path = it.nth(3).unwrap_or("");
            let candidate = path.starts_with('/')
                && path.contains(".so")
                && !perms.contains('w')
                && perms.starts_with('r')
                && exe.as_deref().is_none_or(|x| x.as_os_str() != path);
            cur = Some((s, e, candidate));
        } else if first == "Anonymous:" {
            let kb = it.next().and_then(|v| v.parse::<u64>().ok());
            flush(&mut cur, kb);
        }
    }
    log::info!("dropped the resident pages of {} MB of read-only library mappings", released >> 20);
}

/// Process-wide graphics defaults. Must run before GTK, EGL or WebKit initialise (first thing in
/// `run()`, still single-threaded). Every variable is only set when the user has not set it, and
/// `LANCAT_NO_GL_DEFAULTS=1` skips all of them.
pub fn early_init() {
    limit_malloc_arenas();
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

/// glibc gives every thread that ever calls malloc its own arena (up to 8 × cores), and WebKit's
/// UI-side threads leave a few MB of partly used arenas behind after the web view is gone. Two
/// arenas are plenty for this app (measured: −1.5 MB after a panel open/close cycle). An explicit
/// `MALLOC_ARENA_MAX` from the environment wins.
fn limit_malloc_arenas() {
    #[cfg(target_env = "gnu")]
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        const M_ARENA_MAX: i32 = -8;
        extern "C" {
            fn mallopt(param: i32, value: i32) -> i32;
        }
        // SAFETY: plain libc call, runs before any other thread exists
        unsafe {
            mallopt(M_ARENA_MAX, 2);
        }
    }
}

/// Called once the app has settled after start (collapsed, no web view yet): the WebKit/GTK
/// libraries were loaded and relocated but most of their code pages are never needed again until
/// the panel opens, so release them like after a panel is destroyed.
pub fn settle_after_start() {
    trim_heap();
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
// ------------------------------------------------------------------ frozen panel
//
// Linux counterpart of WebView2's TrySuspend + EmptyWorkingSet: a few seconds after the panel
// collapses its WebKit web process is stopped (SIGSTOP: no timers, no GC, 0 CPU) and, when the
// app has a cgroup of its own, the kernel is asked to page out about as much as that renderer
// holds (cgroup v2 memory.reclaim; the stopped renderer's pages are the coldest there). Opening
// or hovering the strip continues it (SIGCONT) before the panel is shown. Events sent to the
// page meanwhile are queued by the UI process and delivered on resume. LANCAT_FREEZE=0 disables.

static FROZEN: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn getpid() -> i32;
}
const SIGCONT: i32 = 18;
const SIGSTOP: i32 = 19;

fn ppid_comm(pid: i32) -> Option<(i32, String)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (l, r) = (s.find('(')?, s.rfind(')')?);
    let ppid = s.get(r + 2..)?.split_whitespace().nth(1)?.parse().ok()?;
    Some((ppid, s[l + 1..r].to_string()))
}

fn rss_kb(pid: i32) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)?.parse().ok()))
        .unwrap_or(0)
}

/// Our WebKit renderers (direct children named WebKitWebProcess; not the network process).
fn web_processes() -> Vec<i32> {
    let me = unsafe { getpid() };
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|&p| matches!(ppid_comm(p), Some((pp, c)) if pp == me && c.starts_with("WebKitWebProc")))
        .collect()
}

pub fn panel_suspend(win: &WebviewWindow) {
    use tauri::Manager;
    if std::env::var("LANCAT_FREEZE").as_deref() == Ok("0") {
        return;
    }
    // the settings window has a renderer of its own, which must keep running: while it is open
    // the collapsed panel is only hidden
    if win.app_handle().get_webview_window("settings").is_some() {
        log::debug!("panel not frozen: settings window open");
        return;
    }
    let mut frozen = FROZEN.lock().unwrap();
    if !frozen.is_empty() {
        return;
    }
    let pids = web_processes();
    if pids.is_empty() {
        return;
    }
    for &p in &pids {
        // SAFETY: plain syscall on our own child
        unsafe { kill(p, SIGSTOP) };
    }
    let kb: u64 = pids.iter().map(|&p| rss_kb(p)).sum();
    *frozen = pids.clone();
    drop(frozen);
    log::info!("panel web process frozen ({pids:?}, {} MB resident)", kb >> 10);
    let _ = std::thread::Builder::new().name("lancat-reclaim".into()).spawn(move || reclaim(kb * 1024 * 3 / 4));
}

/// Pages out `bytes` of our cgroup's coldest memory, only when the cgroup holds nothing but this
/// app (a systemd unit, or the per-app scope niri/GNOME/KDE start apps in), never a shared one.
fn reclaim(bytes: u64) {
    let me = unsafe { getpid() };
    let Some(path) = std::fs::read_to_string("/proc/self/cgroup").ok().and_then(|s| s.lines().find_map(|l| l.strip_prefix("0::").map(str::to_string))) else { return };
    let dir = format!("/sys/fs/cgroup{path}");
    let procs = std::fs::read_to_string(format!("{dir}/cgroup.procs")).unwrap_or_default();
    let ours = |p: i32| p == me || ppid_comm(p).is_some_and(|(pp, _)| pp == me);
    if procs.is_empty() || !procs.lines().filter_map(|l| l.trim().parse::<i32>().ok()).all(ours) {
        log::debug!("not reclaiming: cgroup {path} is shared with other processes");
        return;
    }
    let cur = || std::fs::read_to_string(format!("{dir}/memory.current")).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
    let before = cur();
    // EAGAIN = reclaimed less than asked, which is fine
    let r = std::fs::write(format!("{dir}/memory.reclaim"), bytes.to_string());
    let after = cur();
    match r {
        Err(e) if e.raw_os_error() != Some(11) => log::debug!("memory.reclaim unavailable: {e}"),
        _ => log::info!("paged out {} MB of the frozen panel (cgroup {} → {} MB)", before.saturating_sub(after) >> 20, before >> 20, after >> 20),
    }
}

pub fn panel_resume(_win: &WebviewWindow) {
    if thaw_all() {
        log::debug!("panel web process resumed");
    }
}

/// Continues every renderer we stopped; true if there was one.
pub fn thaw_all() -> bool {
    let pids = std::mem::take(&mut *FROZEN.lock().unwrap());
    for &p in &pids {
        // SAFETY: plain syscall
        unsafe { kill(p, SIGCONT) };
    }
    !pids.is_empty()
}

/// Hover on the strip: continue the renderer during the hover delay, before the open.
pub fn panel_prewarm(win: &WebviewWindow) {
    panel_resume(win);
}

/// Blocks until the system is short of memory: MemAvailable under 6 % of RAM, or the kernel's
/// pressure stall (PSI "some" avg10) above 25 %. Polled every 15 s; returns at once while low.
pub fn wait_low_memory() {
    fn low() -> bool {
        let mi = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let kb = |k: &str| mi.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<f64>().ok());
        if let (Some(t), Some(a)) = (kb("MemTotal:"), kb("MemAvailable:")) {
            if t > 0.0 && a / t < 0.06 {
                return true;
            }
        }
        let psi = std::fs::read_to_string("/proc/pressure/memory").unwrap_or_default();
        psi.lines()
            .find(|l| l.starts_with("some"))
            .and_then(|l| l.split_whitespace().find_map(|f| f.strip_prefix("avg10=")))
            .and_then(|v| v.parse::<f64>().ok())
            .is_some_and(|v| v > 25.0)
    }
    while !low() {
        std::thread::sleep(std::time::Duration::from_secs(15));
    }
}

/// Opt-in only (LANCAT_REAP_NETWORK=1): WebKitGTK does not bring a terminated network process
/// back for the web context, so the next panel webview never loaded its page and the panel could
/// not be opened any more (seen 2026-10-12 after a low-memory release).
pub fn reap_web_helpers() {
    if std::env::var_os("LANCAT_REAP_NETWORK").is_none() {
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

