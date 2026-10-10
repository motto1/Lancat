//! Windows 11: Acrylic material, DWM rounded corners, WebView2 tuning (browser arguments, memory
//! target level), OLE-drag sensing via the global mouse-button state, and a one-time inbound
//! firewall rule (elevated `netsh`).
//!
//! Acrylic: the panel is a never-activated popover (hover opens it without focus). Win11 system
//! backdrops (`DWMWA_SYSTEMBACKDROP_TYPE`, what `window_vibrancy::apply_acrylic` uses on 22H2+)
//! render their opaque fallback colour while the window is inactive, which is why the panel showed
//! a flat light-grey sheet. The legacy accent policy (`SetWindowCompositionAttribute` +
//! `ACCENT_ENABLE_ACRYLICBLURBEHIND`) blurs regardless of activation, so it is the default here.
//! With "transparency effects" switched off in Windows settings every variant (accent, system
//! backdrop) renders an opaque fallback; the accent one uses its tint colour for that, so the tint
//! follows the light/dark theme. `LANCAT_WIN_GLASS=accent|backdrop|mica|none` switches the
//! material for experiments.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::time::Duration;

use tauri::{Manager, Runtime, WebviewWindow, WebviewWindowBuilder};
use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL};
use windows::core::{s, w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DWM_WINDOW_CORNER_PREFERENCE};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetLayeredWindowAttributes, ShowWindow, LWA_ALPHA, SW_HIDE, SW_SHOWNOACTIVATE};
use windows_core::Interface;

/// Acrylic/Mica is applied to the whole window, so the shell fills it (no CSS margin).
pub const NATIVE_GLASS: bool = true;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn hwnd(win: &WebviewWindow) -> Option<HWND> {
    // tauri may link a different `windows` version: go through the raw pointer.
    win.hwnd().ok().map(|h| HWND(h.0 as _))
}

fn round_corners(h: HWND) {
    let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
    unsafe {
        let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as *const _, std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32);
    }
}

#[derive(Clone, Copy, Debug)]
enum Glass {
    Accent,
    Backdrop,
    Mica,
    None,
}

fn glass_mode() -> Glass {
    match env("LANCAT_WIN_GLASS").as_deref() {
        Some("backdrop") => Glass::Backdrop,
        Some("mica") => Glass::Mica,
        Some("none") => Glass::None,
        _ => Glass::Accent,
    }
}

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    gradient: u32,
    animation: u32,
}

#[repr(C)]
struct CompositionAttrData {
    attr: u32,
    data: *mut std::ffi::c_void,
    size: usize,
}

const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

/// Legacy (undocumented but stable since 1803) acrylic blur-behind; works for inactive windows.
fn accent(h: HWND, state: u32, dark: bool) -> bool {
    // 0xAABBGGRR. The page draws its own tint (--glass) on top, so the native one is nearly clear;
    // its RGB is also the opaque fallback when transparency effects are off, so match the theme.
    let tint = env("LANCAT_WIN_TINT")
        .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok())
        .unwrap_or(if dark { 0x0120_1E1E } else { 0x01F8_F6F6 });
    unsafe {
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return false };
        let Some(f) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else { return false };
        let f: unsafe extern "system" fn(HWND, *mut CompositionAttrData) -> i32 = std::mem::transmute(f);
        let mut policy = AccentPolicy { state, flags: 2, gradient: tint, animation: 0 };
        let mut data = CompositionAttrData { attr: WCA_ACCENT_POLICY, data: &mut policy as *mut _ as *mut _, size: std::mem::size_of::<AccentPolicy>() };
        f(h, &mut data) != 0
    }
}

/// Win11 22H2+ system backdrop (2 = Mica, 3 = Acrylic "transient window").
fn system_backdrop(h: HWND, kind: i32) -> bool {
    unsafe {
        let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(h, &m);
        DwmSetWindowAttribute(h, DWMWA_SYSTEMBACKDROP_TYPE, &kind as *const _ as *const _, 4).is_ok()
    }
}

/// The app's theme setting, or the system's app theme for "system".
fn dark_theme(win: &WebviewWindow) -> bool {
    theme_is_dark(&win.app_handle().state::<crate::commands::Core>().0.settings().theme)
}

fn reg_dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut v: u32 = 0;
    let mut size = 4u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key, value, RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut _), Some(&mut size)) };
    r.is_ok().then_some(v)
}

/// A theme setting ("dark" | "light" | "system") resolved against the system's app theme.
pub fn theme_is_dark(theme: &str) -> bool {
    match theme {
        "dark" => true,
        "light" => false,
        _ => reg_dword(w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), w!("AppsUseLightTheme")) == Some(0),
    }
}


pub fn setup_panel(win: &WebviewWindow) {
    let Some(h) = hwnd(win) else { return };
    let mode = glass_mode();
    let ok = match mode {
        Glass::Accent => accent(h, ACCENT_ENABLE_ACRYLICBLURBEHIND, dark_theme(win)),
        Glass::Backdrop => system_backdrop(h, 3),
        Glass::Mica => window_vibrancy::apply_mica(win, None).is_ok(),
        Glass::None => true,
    };
    if !ok {
        log::warn!("panel material {mode:?} failed; falling back to Mica");
        let _ = window_vibrancy::apply_mica(win, None);
    }
    round_corners(h);
    // Native drop shadow. The window keeps tao's frameless client area (`.shadow(false)`, so its
    // size and position stay exactly the computed rect, no invisible resize borders); a 1 px frame
    // extended into the client is enough for DWM to draw its shadow (what the `window-shadows`
    // crate did). The system backdrop path already extends the whole frame. Corners stay rounded
    // by the DWMWCP_ROUND preference above. `LANCAT_WIN_SHADOW=0` turns it off.
    if !matches!(mode, Glass::Backdrop) && env("LANCAT_WIN_SHADOW").as_deref() != Some("0") {
        let m = MARGINS { cxLeftWidth: 1, cxRightWidth: 1, cyTopHeight: 1, cyBottomHeight: 1 };
        unsafe {
            let _ = DwmExtendFrameIntoClientArea(h, &m);
        }
    }
    // The page animates the popover itself; DWM's own show/hide transition on top of that made the
    // first frames after a (warm) open show a half-revealed window. `LANCAT_WIN_DWM_ANIM=1` keeps it.
    if env("LANCAT_WIN_DWM_ANIM").as_deref() != Some("1") {
        let off: i32 = 1;
        unsafe {
            let _ = DwmSetWindowAttribute(h, DWMWA_TRANSITIONS_FORCEDISABLED, &off as *const _ as *const _, 4);
        }
    }
}

/// Theme setting or system app theme changed: re-apply the accent tint (its colour is also the
/// opaque fallback when transparency effects are off) without recreating the panel.
pub fn refresh_material(win: &WebviewWindow) {
    if !matches!(glass_mode(), Glass::Accent) {
        return;
    }
    let Some(h) = hwnd(win) else { return };
    let dark = dark_theme(win);
    if !accent(h, ACCENT_ENABLE_ACRYLICBLURBEHIND, dark) {
        log::warn!("re-applying the panel accent (dark={dark}) failed");
    }
}

// ------------------------------------------------------------------ panel park / reveal
//
// Hiding the panel (SW_HIDE) makes WebView2 treat its renderer as hidden; on the next (warm) open
// the first ~450 ms showed a black block and then only part of the page while it caught up. So a
// collapsed panel is *parked* instead: it stays shown (the renderer keeps a current frame) but is
// click-through (WS_EX_TRANSPARENT, so it never takes hover, clicks or OLE drops) and fully
// transparent (WS_EX_LAYERED, alpha 0). Both styles come from tao's own "ignore cursor events"
// flag, so tao's later style rewrites (show, focus, …) keep them. An open shows the window at
// alpha 0 and waits for the page to report a painted frame (`panel_ready`, with a timeout) before
// bringing it in; once fully in, the layered style is dropped again so the open panel is an
// ordinary window (shadow, rounded corners and Acrylic exactly as before).
// `LANCAT_WIN_PARK=0` restores the old show/hide behaviour.

/// Bumped on every open/close; a pending reveal or fade step only runs if nothing happened since.
static PANEL_GEN: AtomicU64 = AtomicU64::new(0);
/// The open waiting for `panel_ready` (0 = none).
static REVEAL_PENDING: AtomicU64 = AtomicU64::new(0);
/// Current layered alpha of the panel (255 = opaque / not layered).
static ALPHA: AtomicU8 = AtomicU8::new(255);
/// See `panel_revealed`.
static REVEALED: AtomicBool = AtomicBool::new(false);

fn park_enabled() -> bool {
    env("LANCAT_WIN_PARK").as_deref() != Some("0")
}

fn set_alpha(h: HWND, a: u8) {
    ALPHA.store(a, Ordering::SeqCst);
    unsafe {
        let _ = SetLayeredWindowAttributes(h, COLORREF(0), a, LWA_ALPHA);
    }
}

/// Makes the panel visible for an open. Main thread. `cold`: the page may still be loading.
pub fn panel_open(win: &WebviewWindow, cold: bool) {
    let Some(h) = hwnd(win).filter(|_| park_enabled()) else {
        let _ = win.show();
        return;
    };
    let gen = PANEL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    REVEALED.store(false, Ordering::SeqCst);
    if !win.is_visible().unwrap_or(false) {
        // first open of this webview: start invisible and click-through, like a parked panel
        let _ = win.set_ignore_cursor_events(true);
        set_alpha(h, 0);
        let _ = win.show();
    }
    REVEAL_PENDING.store(gen, Ordering::SeqCst);
    // never leave the panel invisible if the page does not answer (e.g. still loading, or crashed)
    let timeout = Duration::from_millis(if cold { 4000 } else { 350 });
    let w = win.clone();
    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        let w2 = w.clone();
        let _ = w.run_on_main_thread(move || {
            if REVEAL_PENDING.compare_exchange(gen, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                log::debug!("panel_ready timed out after {} ms; revealing anyway", timeout.as_millis());
                reveal(&w2, gen);
            }
        });
    });
}

/// The page painted its first frame after `panel://show`. Main thread.
pub fn panel_ready(win: &WebviewWindow) {
    let gen = REVEAL_PENDING.swap(0, Ordering::SeqCst);
    if gen != 0 && gen == PANEL_GEN.load(Ordering::SeqCst) {
        reveal(win, gen);
    }
}

/// The current open has started coming in (false from a close until the next reveal, including
/// while the webview of a cold open is still being created). Always true without parking.
pub fn panel_revealed() -> bool {
    !park_enabled() || REVEALED.load(Ordering::SeqCst)
}

fn reveal(win: &WebviewWindow, gen: u64) {
    if PANEL_GEN.load(Ordering::SeqCst) != gen {
        return;
    }
    REVEALED.store(true, Ordering::SeqCst);
    // the strip switches to its "open" look together with the window fade-in, not at the request
    crate::dock::refresh_visual(win.app_handle());
    // drop WS_EX_LAYERED | WS_EX_TRANSPARENT once fully in
    fade(win, gen, 255, fade_ms(true), |w| {
        let _ = w.set_ignore_cursor_events(false);
    });
}

/// Whole-window fade durations (the Acrylic material and the page fade together; the page itself
/// no longer slides on Windows). `LANCAT_WIN_FADE_MS` sets both, 0 = instant.
pub(crate) fn fade_ms(opening: bool) -> u64 {
    env("LANCAT_WIN_FADE_MS").and_then(|v| v.parse().ok()).unwrap_or(if opening { 120 } else { 140 })
}

/// Steps the layered alpha from its current value to `to` over `ms` (~60 Hz, eased), then runs
/// `done`. Every step runs on the main thread and is dropped once another open/close happened.
fn fade(win: &WebviewWindow, gen: u64, to: u8, ms: u64, done: fn(&WebviewWindow)) {
    let from = ALPHA.load(Ordering::SeqCst);
    if ms == 0 || from == to {
        if let Some(h) = hwnd(win) {
            set_alpha(h, to);
        }
        done(win);
        return;
    }
    let w = win.clone();
    std::thread::spawn(move || {
        let steps = (ms / 16).max(1);
        for i in 1..=steps {
            std::thread::sleep(Duration::from_millis(ms / steps));
            if PANEL_GEN.load(Ordering::SeqCst) != gen {
                return;
            }
            let t = i as f64 / steps as f64;
            // ease-out when coming in, ease-in when going away
            let e = if to > from { 1.0 - (1.0 - t) * (1.0 - t) } else { t * t };
            let a = (from as f64 + (to as f64 - from as f64) * e).round().clamp(0.0, 255.0) as u8;
            let last = i == steps;
            let w2 = w.clone();
            let _ = w.run_on_main_thread(move || {
                if PANEL_GEN.load(Ordering::SeqCst) != gen {
                    return;
                }
                if let Some(h) = hwnd(&w2) {
                    set_alpha(h, if last { to } else { a });
                }
                if last {
                    done(&w2);
                }
            });
        }
    });
}

/// Collapses the panel: park it (see above) instead of hiding it. Main thread.
pub fn panel_close(win: &WebviewWindow) {
    let Some(h) = hwnd(win).filter(|_| park_enabled()) else {
        let _ = win.hide();
        return;
    };
    let gen = PANEL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    REVEAL_PENDING.store(0, Ordering::SeqCst);
    REVEALED.store(false, Ordering::SeqCst);
    // click-through at once; (re)adding WS_EX_LAYERED needs the alpha set again right away
    let _ = win.set_ignore_cursor_events(true);
    set_alpha(h, ALPHA.load(Ordering::SeqCst));
    fade(win, gen, 0, fade_ms(false), |w| {
        // An invisible window must not keep the keyboard: hiding it lets Windows activate the
        // next window, showing it again without activation keeps the renderer warm.
        let Some(h) = hwnd(w) else { return };
        unsafe {
            if GetForegroundWindow() == h {
                let _ = ShowWindow(h, SW_HIDE);
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            }
        }
    });
}

// ------------------------------------------------------------------ WebView2

/// Tauri/wry's default arguments; `additional_browser_args` replaces them, so they are repeated.
const WV2_DEFAULT_FEATURES: &str = "msWebOOUI,msPdfOOUI,msSmartScreenProtection";

/// Browser arguments for every webview of the app. All webviews share one user-data folder, and
/// WebView2 refuses a second environment with different options there, so the settings window
/// must get exactly the same string as the panel. `LANCAT_WV2_ARGS` replaces it (experiments).
///
/// Measured on a Win11 laptop (memprobe, panel expanded, private bytes of the whole tree):
/// * `--disable-gpu`: the GPU process drops from ~88–98 MB (D3D/driver baseline; raster/tile
///   flags such as `--disable-gpu-rasterization` or `--force-gpu-mem-available-mb` change
///   nothing) to ~18 MB. The panel is one small page; the Acrylic material is drawn by DWM
///   behind the window, so it is unaffected, and dotLottie renders in WASM anyway. Cost: ~+6 %
///   of one core while a sticker animates; cold-open time is unchanged.
/// * `NetworkServiceInProcess`: the network utility process (~11 MB) moves into the browser
///   process (+~2 MB there).
/// * not used: `--in-process-gpu` (−40 MB with GPU on, but a GPU fault then takes the browser
///   process down), `--disable-gpu-compositing` (−25 MB), `--enable-low-end-device-mode` and
///   `--renderer-process-limit=1` (no measurable gain).
fn browser_args() -> String {
    env("LANCAT_WV2_ARGS").unwrap_or_else(|| {
        format!("--disable-features={WV2_DEFAULT_FEATURES} --enable-features=NetworkServiceInProcess,NetworkServiceInProcess2 --disable-gpu")
    })
}

/// Applies the Windows-only WebView2 options to a window builder (panel and settings).
pub fn webview_builder<'a, R: Runtime, M: Manager<R>>(b: WebviewWindowBuilder<'a, R, M>) -> WebviewWindowBuilder<'a, R, M> {
    b.additional_browser_args(&browser_args())
}

/// Panel shown (true) or collapsed but kept alive (false): steer WebView2's memory target level
/// (Low lets the renderer drop caches while hidden).
pub fn panel_visibility(win: &WebviewWindow, visible: bool) {
    // Measured while collapsed-but-alive: −17…21 MB with GPU on, but no difference (~125 MB
    // either way) with the default `--disable-gpu`, so it stays opt-in (`LANCAT_WV2_LOWMEM=1`).
    if env("LANCAT_WV2_LOWMEM").as_deref() != Some("1") {
        return;
    }
    let _ = win.with_webview(move |wv| unsafe {
        let level = if visible { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL } else { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW };
        if let Ok(core) = wv.controller().CoreWebView2() {
            if let Ok(c19) = core.cast::<ICoreWebView2_19>() {
                let _ = c19.SetMemoryUsageTargetLevel(level);
            }
        }
    });
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

pub fn tune_webview(_win: &WebviewWindow) {}

pub fn use_layer_shell() -> bool {
    false
}

pub fn place_layer(_win: &WebviewWindow, _x: f64, _y: f64, _w: f64, _h: f64) -> bool {
    false
}

pub fn trim_heap() {}

pub fn settle_after_start() {}

pub fn early_init() {}

/// WebView2 shuts its helper processes down by itself once the last web view (and environment
/// reference) is gone.
pub fn reap_web_helpers() {}
