//! Message toast on Windows: when a peer sends something while the panel is closed, the dock strip
//! morphs into a small pill showing who sent what (Direct2D + DirectWrite on a layered window, so
//! no web renderer is woken for it). Short text, emoji and file names show in full; long text and
//! long names end in an ellipsis. It is a two-line card (name over message). Its glass is real:
//! the screen behind it is captured while the toast is still hidden, blurred, and bent
//! along the rim like a lens (a CPU "liquid glass" pass done once per toast), so it works even
//! with the system's transparency effects off. Hovering holds it, a click opens the panel on that conversation,
//! otherwise it shrinks back into the strip after a few seconds.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use tauri::AppHandle;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, D2D1_BITMAP_PROPERTIES, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, ID2D1Factory, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextLayout, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, CAPTUREBLT, SRCCOPY, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    DIB_RGB_COLORS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW,  KillTimer, LoadCursorW, RegisterClassW, SetTimer, SetWindowPos, ShowWindow, UpdateLayeredWindow, HWND_TOPMOST, IDC_HAND, MA_NOACTIVATE,
    SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::Rect;

const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_ANIM: usize = 11;
const TIMER_HIDE: usize = 12;
const ANIM_MS: f64 = 280.0;

/// Logical sizes.
const H: f64 = 48.0;
const PAD: f64 = 8.0;
const AVATAR: f64 = 32.0;
const GAP: f64 = 9.0;
const NAME_MAX: f64 = 196.0;
const TEXT_MAX: f64 = 196.0;
const RADIUS: f64 = 17.0;
const LINE_GAP: f64 = 1.0;
const SHADOW: f64 = 10.0;
/// distance of the pill from the screen edge
const INSET: f64 = 4.0;

pub struct ToastSpec {
    pub peer_id: String,
    pub name: String,
    pub text: String,
    pub edge: String,
    /// the strip's rect (physical px)
    pub strip: Rect,
    pub scale: f64,
    pub dark: bool,
    pub hold_ms: u32,
}

struct Gfx {
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    rt: ID2D1DCRenderTarget,
}

/// `EndDraw` result that means the render target's device is gone (display driver reset / update,
/// GPU removed, remote session switch): the target must be recreated, it never works again.
const D2DERR_RECREATE_TARGET: u32 = 0x8899_000C;

fn rt_props() -> D2D1_RENDER_TARGET_PROPERTIES {
    D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    }
}

fn create_gfx() -> Option<Gfx> {
    unsafe {
        (|| -> windows::core::Result<Gfx> {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let rt = d2d.CreateDCRenderTarget(&rt_props())?;
            Ok(Gfx { d2d, dw, rt })
        })()
        .map_err(|e| log::warn!("toast: Direct2D init failed: {e}"))
        .ok()
    }
}

/// The toast is on screen (read by the cursor poller, another thread).
static SHOWN: AtomicBool = AtomicBool::new(false);

pub fn shown() -> bool {
    SHOWN.load(Ordering::SeqCst)
}

struct State {
    hwnd: HWND,
    gfx: Option<Gfx>,
    spec: Option<ToastSpec>,
    name: Option<IDWriteTextLayout>,
    text: Option<IDWriteTextLayout>,
    initial: Option<IDWriteTextLayout>,
    name_w: f64,
    text_w: f64,
    /// window rect (physical)
    win: (i32, i32, i32, i32),
    /// 0 = strip, 1 = full pill
    t: f64,
    from: f64,
    to: f64,
    t0: Instant,
    hovering: bool,
    /// blurred + refracted screen behind the window (premultiplied BGRA, window size), if captured
    glass: Option<Vec<u8>>,
    /// the backdrop under the card is dark: tint and text follow it rather than the app theme
    glass_dark: Option<bool>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}
static APP: OnceLock<AppHandle> = OnceLock::new();

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

fn ensure_window(app: &AppHandle) -> bool {
    let _ = APP.set(app.clone());
    if STATE.with(|s| s.borrow().is_some()) {
        return true;
    }
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return false };
        let class = w!("LancatDockToast");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hmod.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd_res = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class,
            w!("Lancat Toast"),
            WS_POPUP,
            0,
            0,
            10,
            10,
            None,
            None,
            Some(hmod.into()),
            None,
        );
        let Ok(hwnd) = hwnd_res else {
            log::warn!("toast: CreateWindowExW failed");
            return false;
        };
        let gfx = create_gfx();
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                gfx,
                spec: None,
                name: None,
                text: None,
                initial: None,
                name_w: 0.0,
                text_w: 0.0,
                win: (0, 0, 10, 10),
                t: 0.0,
                from: 0.0,
                to: 0.0,
                t0: Instant::now(),
                hovering: false,
                glass: None,
                glass_dark: None,
            })
        });
    }
    true
}

fn layout(dw: &IDWriteFactory, s: &str, size: f32, weight: DWRITE_FONT_WEIGHT, max_w: f32) -> Option<(IDWriteTextLayout, f64)> {
    unsafe {
        let fmt = dw
            .CreateTextFormat(w!("Microsoft YaHei UI"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("zh-cn"))
            .ok()?;
        let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        if let Ok(sign) = dw.CreateEllipsisTrimmingSign(&fmt) {
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            let _ = fmt.SetTrimming(&trim, &sign);
        }
        let wide: Vec<u16> = s.encode_utf16().collect();
        let l = dw.CreateTextLayout(&wide, &fmt, max_w, size * 2.0).ok()?;
        let mut m = Default::default();
        l.GetMetrics(&mut m).ok()?;
        let width = (m.widthIncludingTrailingWhitespace as f64).min(max_w as f64);
        Some((l, width.ceil()))
    }
}

/// Shows (or replaces) the toast. Main thread only.
pub fn show(app: &AppHandle, spec: ToastSpec) {
    if !ensure_window(app) {
        return;
    }
    let hold = spec.hold_ms;
    with(|s| unsafe {
        if s.gfx.is_none() {
            // init failed earlier (e.g. during a driver update): try again
            s.gfx = create_gfx();
        }
        let Some(g) = &s.gfx else { return };
        let sc = spec.scale;
        let (name, name_w) = layout(&g.dw, &spec.name, (12.5 * sc) as f32, DWRITE_FONT_WEIGHT_SEMI_BOLD, (NAME_MAX * sc) as f32).unzip();
        let (text, text_w) = layout(&g.dw, &spec.text, (12.0 * sc) as f32, DWRITE_FONT_WEIGHT_NORMAL, (TEXT_MAX * sc) as f32).unzip();
        let init = initials(&spec.name);
        s.initial = layout(&g.dw, &init, (14.0 * sc) as f32, DWRITE_FONT_WEIGHT_SEMI_BOLD, (AVATAR * sc) as f32).map(|x| x.0);
        s.name = name;
        s.text = text;
        s.name_w = name_w.unwrap_or(0.0);
        s.text_w = text_w.unwrap_or(0.0);
        // window: the bounding box of the strip and the full pill, plus room for the shadow
        let (pw, ph) = pill_size(s, sc);
        let st = spec.strip;
        let m = SHADOW * sc;
        let inset = INSET * sc;
        let (cx, cy) = (st.x + st.w / 2.0, st.y + st.h / 2.0);
        let bw = pw.max(st.w) + inset + m;
        let bh = ph.max(st.h) + 2.0 * m;
        let (bw2, bh2) = (pw.max(st.w) + 2.0 * m, ph.max(st.h) + inset + m);
        let (x, y, w, h) = match spec.edge.as_str() {
            "left" => (st.x, cy - bh / 2.0, bw, bh),
            "top" => (cx - bw2 / 2.0, st.y, bw2, bh2),
            "bottom" => (cx - bw2 / 2.0, st.y + st.h - bh2, bw2, bh2),
            _ => (st.x + st.w - bw, cy - bh / 2.0, bw, bh),
        };
        s.win = (x.round() as i32, y.round() as i32, w.round() as i32, h.round() as i32);
        let replacing = s.spec.is_some() && s.to > 0.5;
        s.spec = Some(spec);
        let _ = SetWindowPos(s.hwnd, Some(HWND_TOPMOST), s.win.0, s.win.1, s.win.2, s.win.3, SWP_NOACTIVATE);
        if !replacing {
            s.t = 0.0;
        }
        // capture only while hidden; a replacement reuses the backdrop when the window kept its size
        let same = replacing && s.glass.as_ref().map(|g| g.len()) == Some((s.win.2 * s.win.3 * 4) as usize);
        if !replacing || !same {
            s.glass = if replacing { None } else { capture_glass(s) };
            s.glass_dark = s.glass.as_ref().and_then(|g| backdrop_dark(s, g));
        }
        animate(s, 1.0);
        draw(s);
        let _ = ShowWindow(s.hwnd, SW_SHOWNOACTIVATE);
        SHOWN.store(true, Ordering::SeqCst);
        super::windows::set_hidden(true);
        let _ = KillTimer(Some(s.hwnd), TIMER_HIDE);
        if !s.hovering {
            SetTimer(Some(s.hwnd), TIMER_HIDE, hold, None);
        }
    });
}

/// Hides the toast at once (the panel opened, the dock moved …). Main thread only.
pub fn dismiss() {
    with(|s| {
        if s.spec.is_some() {
            animate(s, 0.0);
        }
    });
}

fn pill_size(s: &State, sc: f64) -> (f64, f64) {
    let w = PAD * sc + (AVATAR + GAP) * sc + s.name_w.max(s.text_w) + PAD * sc * 1.7;
    (w.max(H * sc * 2.4), H * sc)
}

/// Final card rect inside the window (window-local), as `render` places it at t = 1.
fn card_rect(s: &State) -> Option<(f64, f64, f64, f64)> {
    let spec = s.spec.as_ref()?;
    let sc = spec.scale;
    let (pw, ph) = pill_size(s, sc);
    let (fw, fh) = (s.win.2 as f64, s.win.3 as f64);
    let inset = INSET * sc;
    Some(match spec.edge.as_str() {
        "left" => (inset, fh / 2.0 - ph / 2.0, pw, ph),
        "right" => (fw - pw - inset, fh / 2.0 - ph / 2.0, pw, ph),
        "top" => (fw / 2.0 - pw / 2.0, inset, pw, ph),
        _ => (fw / 2.0 - pw / 2.0, fh - ph - inset, pw, ph),
    })
}

/// Captures the screen under the window, blurs it and bends it along the card's rim.
fn capture_glass(s: &State) -> Option<Vec<u8>> {
    let spec = s.spec.as_ref()?;
    let (x, y, w, h) = s.win;
    if w <= 0 || h <= 0 || w > 4000 || h > 4000 {
        return None;
    }
    let mut px = vec![0u8; (w * h * 4) as usize];
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let ok = match CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bmp) => {
                let old = SelectObject(mem, bmp.into());
                let ok = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT).is_ok();
                if ok && !bits.is_null() {
                    std::ptr::copy_nonoverlapping(bits as *const u8, px.as_mut_ptr(), px.len());
                }
                SelectObject(mem, old);
                let _ = DeleteObject(bmp.into());
                ok
            }
            Err(_) => false,
        };
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !ok {
            return None;
        }
    }
    let sc = spec.scale;
    let (w, h) = (w as usize, h as usize);
    let r = (9.0 * sc).round().max(2.0) as usize;
    for _ in 0..3 {
        box_blur(&mut px, w, h, r);
    }
    let (cx0, cy0, cw, ch) = card_rect(s)?;
    let rad = (RADIUS * sc).min(ch / 2.0);
    let bevel = (13.0 * sc).min(ch / 2.0);
    let strength = 16.0 * sc;
    let src = px.clone();
    let (hx, hy) = (cw / 2.0, ch / 2.0);
    let (ccx, ccy) = (cx0 + hx, cy0 + hy);
    for yy in 0..h {
        for xx in 0..w {
            let (fx, fy) = (xx as f64 + 0.5 - ccx, yy as f64 + 0.5 - ccy);
            let (qx, qy) = (fx.abs() - (hx - rad), fy.abs() - (hy - rad));
            let outside = (qx.max(0.0).hypot(qy.max(0.0))) + qx.max(qy).min(0.0) - rad;
            let d = -outside;
            if d < 0.0 || d >= bevel {
                continue;
            }
            // outward normal of the rounded rect at this point
            let (mut nx, mut ny) = if qx > 0.0 && qy > 0.0 {
                let l = qx.hypot(qy).max(1e-6);
                (qx / l, qy / l)
            } else if qx > qy {
                (1.0, 0.0)
            } else {
                (0.0, 1.0)
            };
            nx *= fx.signum();
            ny *= fy.signum();
            let k = 1.0 - d / bevel;
            let off = k * k * strength;
            let sx = (xx as f64 - nx * off).clamp(0.0, (w - 1) as f64) as usize;
            let sy = (yy as f64 - ny * off).clamp(0.0, (h - 1) as f64) as usize;
            let (i, j) = ((yy * w + xx) * 4, (sy * w + sx) * 4);
            // the bent rim also catches a little light
            let lift = (k * k * 28.0) as u16;
            for c in 0..3 {
                px[i + c] = (src[j + c] as u16 + lift).min(255) as u8;
            }
        }
    }
    for a in px.chunks_exact_mut(4) {
        a[3] = 255;
    }
    Some(px)
}

/// Mean luminance of the backdrop inside the card rect < 0.42.
fn backdrop_dark(s: &State, g: &[u8]) -> Option<bool> {
    let (x0, y0, cw, ch) = card_rect(s)?;
    let w = s.win.2 as usize;
    let (mut sum, mut n) = (0f64, 0f64);
    let (xa, ya) = (x0.max(0.0) as usize, y0.max(0.0) as usize);
    let (xb, yb) = (((x0 + cw) as usize).min(w), ((y0 + ch) as usize).min(s.win.3 as usize));
    for y in (ya..yb).step_by(3) {
        for x in (xa..xb).step_by(3) {
            let i = (y * w + x) * 4;
            sum += 0.0722 * g[i] as f64 + 0.7152 * g[i + 1] as f64 + 0.2126 * g[i + 2] as f64;
            n += 1.0;
        }
    }
    (n > 0.0).then(|| sum / n / 255.0 < 0.42)
}

/// One horizontal + one vertical running-sum box blur pass (BGRA, alpha ignored).
fn box_blur(px: &mut [u8], w: usize, h: usize, r: usize) {
    let mut tmp = vec![0u8; px.len()];
    let win = (2 * r + 1) as u32;
    for y in 0..h {
        let row = y * w;
        for c in 0..3 {
            let mut acc: u32 = 0;
            for k in 0..=2 * r {
                let x = k.saturating_sub(r).min(w - 1);
                acc += px[(row + x) * 4 + c] as u32;
            }
            for x in 0..w {
                tmp[(row + x) * 4 + c] = (acc / win) as u8;
                let add = (x + r + 1).min(w - 1);
                let sub = x.saturating_sub(r);
                acc = acc + px[(row + add) * 4 + c] as u32 - px[(row + sub) * 4 + c] as u32;
            }
        }
    }
    for x in 0..w {
        for c in 0..3 {
            let mut acc: u32 = 0;
            for k in 0..=2 * r {
                let y = k.saturating_sub(r).min(h - 1);
                acc += tmp[(y * w + x) * 4 + c] as u32;
            }
            for y in 0..h {
                px[(y * w + x) * 4 + c] = (acc / win) as u8;
                let add = (y + r + 1).min(h - 1);
                let sub = y.saturating_sub(r);
                acc = acc + tmp[(add * w + x) * 4 + c] as u32 - tmp[(sub * w + x) * 4 + c] as u32;
            }
        }
    }
}

fn animate(s: &mut State, to: f64) {
    s.from = s.t;
    s.to = to;
    s.t0 = Instant::now();
    unsafe {
        // going away (dismissed, clicked, timed out): a pending hide timer would fire later and
        // re-run the shrink (or cut the next toast short); hovering may have armed a new one too
        if to <= 0.0 {
            let _ = KillTimer(Some(s.hwnd), TIMER_HIDE);
        }
        SetTimer(Some(s.hwnd), TIMER_ANIM, 16, None);
    }
}

/// Renders a frame; after a device loss recreates the render target and renders again. The toast
/// used to stay invisible from the first driver reset until the app was restarted (every
/// EndDraw failed with D2DERR_RECREATE_TARGET and nothing recreated the target).
fn draw(s: &mut State) {
    if render(s) {
        return;
    }
    let Some(g) = &s.gfx else { return };
    match unsafe { g.d2d.CreateDCRenderTarget(&rt_props()) } {
        Ok(rt) => {
            log::info!("toast: render target lost (display driver reset?); recreated");
            if let Some(g) = s.gfx.as_mut() {
                g.rt = rt;
            }
            render(s);
        }
        Err(e) => {
            log::warn!("toast: recreating the render target failed: {e}; reinitialising Direct2D");
            s.gfx = create_gfx();
        }
    }
}

fn ease(t: f64) -> f64 {
    // easeOutBack-ish: a little overshoot reads as "inflating" out of the strip
    let c1 = 1.2;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

fn step(s: &mut State) -> bool {
    let k = (s.t0.elapsed().as_secs_f64() * 1000.0 / ANIM_MS).min(1.0);
    let e = if s.to > s.from { ease(k) } else { k * k };
    s.t = s.from + (s.to - s.from) * e;
    k >= 1.0
}

fn pt(x: f32, y: f32) -> windows_numerics::Vector2 {
    windows_numerics::Vector2 { X: x, Y: y }
}

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

/// Same as the page's `initials` (api.ts).
fn initials(name: &str) -> String {
    let n = name.trim();
    if n.is_empty() {
        return "?".into();
    }
    if let Some(c) = n.chars().find(|c| ('\u{3400}'..='\u{9fff}').contains(c)) {
        return c.to_string();
    }
    let parts: Vec<&str> = n.split(|c: char| c.is_whitespace() || "-_.".contains(c)).filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2 {
        return parts[..2].iter().filter_map(|p| p.chars().next()).collect::<String>().to_uppercase();
    }
    n.chars().take(2).collect::<String>().to_uppercase()
}

/// Same colour as the page's `avatarColor` (api.ts): hsl(hue 70% 55%).
fn avatar_color(id: &str) -> D2D1_COLOR_F {
    const HUES: [f32; 9] = [211.0, 262.0, 330.0, 14.0, 32.0, 145.0, 175.0, 195.0, 290.0];
    let h = id.encode_utf16().fold(0u32, |a, c| a.wrapping_mul(31).wrapping_add(c as u32));
    let hue = HUES[(h % 9) as usize];
    let (s, l) = (0.70f32, 0.55f32);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    color(r + m, g + m, b + m, 1.0)
}

/// False when the render target must be recreated (device lost).
fn render(s: &State) -> bool {
    let (Some(g), Some(spec)) = (&s.gfx, &s.spec) else { return true };
    let (_, _, w, h) = s.win;
    if w <= 0 || h <= 0 {
        return true;
    }
    let mut ok = true;
    let sc = spec.scale;
    let (pw, ph) = pill_size(s, sc);
    let st = spec.strip;
    let (fw, fh) = (w as f64, h as f64);
    let m = SHADOW * sc;
    let inset = INSET * sc;
    let t = s.t.max(0.0);
    let vertical = spec.edge == "left" || spec.edge == "right";
    // strip shape (window-local) → pill shape, interpolated
    let (sw, sh) = (st.w, st.h);
    let dark = s.glass_dark.unwrap_or(spec.dark);
    let lerp = |a: f64, b: f64| a + (b - a) * t;
    let (cw, ch) = (lerp(sw, pw), lerp(sh, ph));
    let gap = lerp(0.0, inset);
    let (x0, y0) = if vertical {
        let x = if spec.edge == "right" { fw - cw - gap } else { gap };
        (x, fh / 2.0 - ch / 2.0)
    } else {
        let y = if spec.edge == "bottom" { fh - ch - gap } else { gap };
        (fw / 2.0 - cw / 2.0, y)
    };
    let radius = ((1.0 - t) * (cw.min(ch) / 2.0) + t * (RADIUS * sc).min(ch / 2.0)) as f32;
    let _ = m;
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        if let Ok(bmp) = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            let old = SelectObject(mem, bmp.into());
            let rc = RECT { left: 0, top: 0, right: w, bottom: h };
            let rt = &g.rt;
            if rt.BindDC(mem, &rc).is_ok() {
                rt.BeginDraw();
                rt.Clear(Some(&color(0.0, 0.0, 0.0, 0.0)));
                let content = ((t - 0.45) / 0.55).clamp(0.0, 1.0) as f32;
                // soft shadow: a few widening, fading rounded rects
                if let Ok(b) = rt.CreateSolidColorBrush(&color(0.0, 0.0, 0.0, 1.0), None) {
                    for i in 1..=4 {
                        let d = i as f64 * 2.0 * sc * t;
                        b.SetOpacity(0.05 * content.max(0.3 * t as f32) * (5 - i) as f32 / 4.0);
                        let r = D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F { left: (x0 - d) as f32, top: (y0 - d + sc) as f32, right: (x0 + cw + d) as f32, bottom: (y0 + ch + d + sc) as f32 },
                            radiusX: radius + d as f32,
                            radiusY: radius + d as f32,
                        };
                        rt.FillRoundedRectangle(&r, &b);
                    }
                }
                let pill = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F { left: x0 as f32, top: y0 as f32, right: (x0 + cw) as f32, bottom: (y0 + ch) as f32 },
                    radiusX: radius,
                    radiusY: radius,
                };
                // the captured, blurred, rim-bent backdrop
                let has_glass = if let Some(gl) = &s.glass {
                    let props = D2D1_BITMAP_PROPERTIES {
                        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                        dpiX: 96.0,
                        dpiY: 96.0,
                    };
                    match rt.CreateBitmap(D2D_SIZE_U { width: w as u32, height: h as u32 }, Some(gl.as_ptr() as *const _), (w * 4) as u32, &props) {
                        Ok(bmp) => match rt.CreateBitmapBrush(&bmp, None, None) {
                            Ok(b) => {
                                b.SetOpacity((t as f32 * 1.6).min(1.0));
                                rt.FillRoundedRectangle(&pill, &b);
                                true
                            }
                            Err(_) => false,
                        },
                        Err(_) => false,
                    }
                } else {
                    false
                };
                // body tint: accent while it is still the strip, a thin glass tint once inflated
                let body = match (dark, has_glass) {
                    (true, true) => (0.10, 0.10, 0.12, 0.42),
                    (false, true) => (0.98, 0.98, 0.99, 0.50),
                    (true, false) => (0.14, 0.14, 0.16, 0.80),
                    (false, false) => (0.97, 0.97, 0.98, 0.80),
                };
                let accent = (0.04f32, 0.52f32, 1.0f32);
                let k = (t as f32 * 2.0).min(1.0);
                let mix = |a: f32, b: f32| a + (b - a) * k;
                let fill = color(mix(accent.0, body.0), mix(accent.1, body.1), mix(accent.2, body.2), mix(0.8, body.3));
                if let Ok(b) = rt.CreateSolidColorBrush(&fill, None) {
                    rt.FillRoundedRectangle(&pill, &b);
                }
                // top sheen
                let stops = [
                    D2D1_GRADIENT_STOP { position: 0.0, color: color(1.0, 1.0, 1.0, if dark { 0.16 } else { 0.42 }) },
                    D2D1_GRADIENT_STOP { position: 0.55, color: color(1.0, 1.0, 1.0, 0.0) },
                ];
                if let Ok(coll) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) {
                    let lp = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(x0 as f32, y0 as f32), endPoint: pt(x0 as f32, (y0 + ch) as f32) };
                    if let Ok(b) = rt.CreateLinearGradientBrush(&lp, None, &coll) {
                        b.SetOpacity(content);
                        rt.FillRoundedRectangle(&pill, &b);
                    }
                }
                // specular rim + top sheen
                if let Ok(b) = rt.CreateSolidColorBrush(&color(1.0, 1.0, 1.0, if dark { 0.24 } else { 0.7 }), None) {
                    b.SetOpacity(content);
                    rt.DrawRoundedRectangle(&pill, &b, sc as f32 * 0.8, None);
                }
                if content > 0.0 {
                    let cx = x0 + PAD * sc + AVATAR * sc / 2.0;
                    let cy = y0 + ch / 2.0;
                    if let Ok(b) = rt.CreateSolidColorBrush(&avatar_color(&spec.peer_id), None) {
                        b.SetOpacity(content);
                        let r = (AVATAR * sc / 2.0) as f32;
                        rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx as f32, cy as f32), radiusX: r, radiusY: r }, &b);
                    }
                    let opts = D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT | D2D1_DRAW_TEXT_OPTIONS_CLIP;
                    if let (Some(l), Ok(b)) = (&s.initial, rt.CreateSolidColorBrush(&color(1.0, 1.0, 1.0, 1.0), None)) {
                        b.SetOpacity(content);
                        let mut mt = Default::default();
                        let _ = l.GetMetrics(&mut mt);
                        let tw = mt.width as f64;
                        let th = mt.height as f64;
                        rt.DrawTextLayout(pt((cx - tw / 2.0) as f32, (cy - th / 2.0) as f32), l, &b, opts);
                    }
                    let fg = if dark { color(1.0, 1.0, 1.0, 1.0) } else { color(0.07, 0.07, 0.08, 1.0) };
                    let sub = if dark { color(0.92, 0.92, 0.95, 0.82) } else { color(0.2, 0.2, 0.24, 0.85) };
                    let x = x0 + PAD * sc + AVATAR * sc + GAP * sc;
                    let lh = |l: &Option<IDWriteTextLayout>| {
                        l.as_ref()
                            .map(|l| {
                                let mut mt = Default::default();
                                let _ = l.GetMetrics(&mut mt);
                                mt.height as f64
                            })
                            .unwrap_or(0.0)
                    };
                    let (nh, th) = (lh(&s.name), lh(&s.text));
                    let top = cy - (nh + th + LINE_GAP * sc) / 2.0;
                    if let (Some(l), Ok(b)) = (&s.name, rt.CreateSolidColorBrush(&fg, None)) {
                        b.SetOpacity(content);
                        rt.DrawTextLayout(pt(x as f32, top as f32), l, &b, opts);
                    }
                    if let (Some(l), Ok(b)) = (&s.text, rt.CreateSolidColorBrush(&sub, None)) {
                        b.SetOpacity(content);
                        rt.DrawTextLayout(pt(x as f32, (top + nh + LINE_GAP * sc) as f32), l, &b, opts);
                    }
                }
                if let Err(e) = rt.EndDraw(None, None) {
                    if e.code().0 as u32 == D2DERR_RECREATE_TARGET {
                        ok = false;
                    } else {
                        log::debug!("toast: EndDraw failed: {e}");
                    }
                }
            }
            let size = SIZE { cx: w, cy: h };
            let src = POINT { x: 0, y: 0 };
            let pt = POINT { x: s.win.0, y: s.win.1 };
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            let _ = UpdateLayeredWindow(s.hwnd, Some(screen), Some(&pt), Some(&size), Some(mem), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
    ok
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            with(|s| {
                if !s.hovering {
                    s.hovering = true;
                    let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                    let _ = KillTimer(Some(hwnd), TIMER_HIDE);
                }
            });
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with(|s| {
                s.hovering = false;
                if s.to > 0.5 {
                    SetTimer(Some(hwnd), TIMER_HIDE, 1500, None);
                }
            });
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let peer = with(|s| {
                let p = s.spec.as_ref().map(|x| x.peer_id.clone());
                animate(s, 0.0);
                p
            })
            .flatten();
            if let (Some(p), Some(a)) = (peer, APP.get()) {
                crate::dock::open_panel(a, &format!("toast:{p}"));
            }
            return LRESULT(0);
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_HIDE => {
                    let _ = KillTimer(Some(hwnd), TIMER_HIDE);
                    with(|s| animate(s, 0.0));
                }
                TIMER_ANIM => {
                    with(|s| {
                        let done = step(s);
                        draw(s);
                        if done {
                            let _ = KillTimer(Some(hwnd), TIMER_ANIM);
                            if s.to <= 0.0 {
                                let _ = KillTimer(Some(hwnd), TIMER_HIDE);
                                let _ = ShowWindow(s.hwnd, SW_HIDE);
                                s.spec = None;
                                s.hovering = false;
                                s.glass = None; // window-sized backdrop copy: free it
                                SHOWN.store(false, Ordering::SeqCst);
                                super::windows::set_hidden(false);
                            }
                        }
                    });
                }
                _ => {}
            }
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
