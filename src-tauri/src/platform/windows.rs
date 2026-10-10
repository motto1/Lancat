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

use tauri::{Manager, Runtime, WebviewWindow, WebviewWindowBuilder};
use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL};
use windows::core::{s, w, HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DWM_WINDOW_CORNER_PREFERENCE};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
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
    match win.app_handle().state::<crate::commands::Core>().0.settings().theme.as_str() {
        "dark" => true,
        "light" => false,
        _ => {
            let mut v: u32 = 1;
            let mut size = 4u32;
            let r = unsafe {
                RegGetValueW(
                    HKEY_CURRENT_USER,
                    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
                    w!("AppsUseLightTheme"),
                    RRF_RT_REG_DWORD,
                    None,
                    Some(&mut v as *mut u32 as *mut _),
                    Some(&mut size),
                )
            };
            r.is_ok() && v == 0
        }
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
