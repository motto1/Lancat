//! Windows：面板展开 / 收起的液态玻璃变形窗口（几何、弹簧和合成在 crate::morph）。
//!
//! 一个分层窗口（点击穿透、不激活、置顶），盖住停靠条和面板的并集再加阴影外沿。
//! - 打开：先把面板停在原位（alpha 0），截一张面板背后的屏幕并模糊（窗口还没显示时截），
//!   拿面板最后一帧做内容，形状从停靠条长成面板；弹簧停稳后真面板一下子显示出来，变形窗口再
//!   用 90 ms 淡出（两者最后一帧几乎一样，看不出切换）。
//! - 收起：面板还在屏幕上时把它设成「不进截图」（WDA_EXCLUDEFROMCAPTURE）再截背景，这样截到的是
//!   面板后面的东西；变形窗口以面板的样子出现，面板立刻停放，然后形状缩回停靠条。
//! - 中途反向：从当前形状接着走，背景沿用。
//! 只在 UI 线程上用。LANCAT_WIN_MORPH=0 关掉（退回整窗淡入淡出）。

use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CAPTUREBLT,
    DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassW, SetWindowDisplayAffinity, SetWindowPos, ShowWindow, UpdateLayeredWindow, HWND_TOPMOST, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA,
    WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use super::Rect;
use crate::compose::Bgra;
use crate::morph::{self, Anim, Content, Frame, Look, R};

const FADE_MS: f64 = 90.0;

pub fn enabled() -> bool {
    std::env::var("LANCAT_WIN_MORPH").as_deref() != Ok("0")
}

struct Dib {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut Bgra,
    len: usize,
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

struct State {
    hwnd: HWND,
    /// 窗口矩形（屏幕物理像素）
    win: (i32, i32, i32, i32),
    anim: Option<Anim>,
    last: Option<Frame>,
    glass: Option<Vec<Bgra>>,
    content: Vec<Bgra>,
    cw: usize,
    ch: usize,
    at: (f64, f64),
    look: Look,
    dib: Option<Dib>,
    on_done: Option<Box<dyn FnOnce()>>,
    fade: Option<Instant>,
    timer: slint::Timer,
}

thread_local! {
    static S: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R2>(f: impl FnOnce(&mut State) -> R2) -> Option<R2> {
    S.with(|s| s.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

/// 变形正在进行（或正在淡出）
pub fn running() -> bool {
    with(|s| s.anim.is_some() || s.fade.is_some()).unwrap_or(false)
}

fn ensure() -> bool {
    if S.with(|s| s.borrow().is_some()) {
        return true;
    }
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return false };
        let class = w!("LancatPanelMorph");
        unsafe extern "system" fn wndproc(h: HWND, m: u32, wp: windows::Win32::Foundation::WPARAM, lp: windows::Win32::Foundation::LPARAM) -> windows::Win32::Foundation::LRESULT {
            DefWindowProcW(h, m, wp, lp)
        }
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hmod.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, class, w!("Lancat Morph"), WS_POPUP, 0, 0, 10, 10, None, None, Some(hmod.into()), None) else {
            log::warn!("morph: CreateWindowExW failed");
            return false;
        };
        S.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                win: (0, 0, 0, 0),
                anim: None,
                last: None,
                glass: None,
                content: Vec::new(),
                cw: 0,
                ch: 0,
                at: (0.0, 0.0),
                look: Look { dark: false, scale: 1.0, accent: (0.04, 0.52, 1.0) },
                dib: None,
                on_done: None,
                fade: None,
                timer: slint::Timer::default(),
            })
        });
    }
    true
}

fn union(strip: Rect, panel: Rect, m: f64) -> (i32, i32, i32, i32) {
    let x0 = strip.x.min(panel.x) - m;
    let y0 = strip.y.min(panel.y) - m;
    let x1 = (strip.x + strip.w).max(panel.x + panel.w) + m;
    let y1 = (strip.y + strip.h).max(panel.y + panel.h) + m;
    (x0.floor() as i32, y0.floor() as i32, (x1 - x0).ceil() as i32, (y1 - y0).ceil() as i32)
}

fn local(r: Rect, win: (i32, i32, i32, i32)) -> R {
    let (ox, oy) = (win.0 as f64, win.1 as f64);
    [r.x - ox, r.y - oy, r.x + r.w - ox, r.y + r.h - oy]
}

/// 截屏幕上窗口矩形里的内容并模糊（模糊半径 ≈ 亚克力）
fn capture(win: (i32, i32, i32, i32), scale: f64) -> Option<Vec<Bgra>> {
    let (x, y, w, h) = win;
    if w <= 0 || h <= 0 || w > 6000 || h > 6000 {
        return None;
    }
    let n = (w * h) as usize;
    let mut px = vec![Bgra::default(); n];
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
                    std::ptr::copy_nonoverlapping(bits as *const Bgra, px.as_mut_ptr(), n);
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
    Some(morph::blur_half(&px, w as usize, h as usize, (11.0 * scale).max(2.0)))
}

/// 变形窗口当前背景图里面板那一块（屏幕物理坐标），给真面板接着当玻璃底
pub fn crop_glass(panel: Rect) -> Option<Vec<Bgra>> {
    with(|s| {
        let g = s.glass.as_ref()?;
        let (wx, wy, ww, wh) = s.win;
        let (x0, y0) = (panel.x.round() as i32 - wx, panel.y.round() as i32 - wy);
        let (pw, ph) = (panel.w.round() as i32, panel.h.round() as i32);
        if x0 < 0 || y0 < 0 || x0 + pw > ww || y0 + ph > wh || pw <= 0 || ph <= 0 {
            return None;
        }
        let mut out = Vec::with_capacity((pw * ph) as usize);
        for y in 0..ph {
            let row = ((y0 + y) * ww + x0) as usize;
            out.extend_from_slice(&g[row..row + pw as usize]);
        }
        Some(out)
    })
    .flatten()
}

/// 截 `win` 矩形里的屏幕（截图那一瞬间把 `exclude` 窗口排除）并模糊；可在任意线程调用
pub fn capture_excluding(exclude: HWND, win: (i32, i32, i32, i32), scale: f64) -> Option<Vec<Bgra>> {
    unsafe {
        if let Err(e) = SetWindowDisplayAffinity(exclude, WDA_EXCLUDEFROMCAPTURE) {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| log::warn!("liquid glass refresh: SetWindowDisplayAffinity failed: {e}"));
            return None;
        }
        let _ = DwmFlush();
    }
    let g = capture(win, scale);
    unsafe {
        let _ = SetWindowDisplayAffinity(exclude, WDA_NONE);
    }
    g
}

/// 把面板那块背景图铺成变形窗口大小：面板外面（停靠条、阴影外沿）用最近的边缘像素延伸（本来就是模糊的，看不出来）
fn extend_glass(gp: &[Bgra], pw: usize, ph: usize, at: (f64, f64), win: (i32, i32, i32, i32)) -> Vec<Bgra> {
    let (ww, wh) = (win.2.max(1) as usize, win.3.max(1) as usize);
    let (ox, oy) = (at.0.round() as i64, at.1.round() as i64);
    let mut out = vec![Bgra::default(); ww * wh];
    for y in 0..wh {
        let sy = (y as i64 - oy).clamp(0, ph as i64 - 1) as usize;
        for x in 0..ww {
            let sx = (x as i64 - ox).clamp(0, pw as i64 - 1) as usize;
            out[y * ww + x] = gp[sy * pw + sx];
        }
    }
    out
}

fn make_dib(w: i32, h: i32) -> Option<Dib> {
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        ReleaseDC(None, screen);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() {
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(dc);
            return None;
        }
        let old = SelectObject(dc, bmp.into());
        Some(Dib { dc, bmp, old, bits: bits as *mut Bgra, len: (w * h) as usize })
    }
}

/// 画当前帧并贴到分层窗口；`alpha`：整窗不透明度（淡出用）
fn present(s: &mut State, f: &Frame, alpha: u8) {
    let (x, y, w, h) = s.win;
    if s.dib.as_ref().is_none_or(|d| d.len != (w * h) as usize) {
        s.dib = make_dib(w, h);
    }
    let Some(dib) = &s.dib else { return };
    // SAFETY: DIB 的像素区是 w*h*4 字节，只在这个线程上写
    let out = unsafe { std::slice::from_raw_parts_mut(dib.bits, dib.len) };
    // 内容 = 面板窗口本身（就是卡片，没有外框），按形状矩形拉伸叠在玻璃底上；停稳时 = 真面板（帧叠 lq_base）
    let content = Content { px: &s.content, w: s.cw, h: s.ch, at: (0.0, 0.0) };
    morph::render_stretch(out, w as usize, h as usize, f, s.glass.as_deref(), &content, &s.look);
    unsafe {
        let screen = GetDC(None);
        let size = SIZE { cx: w, cy: h };
        let src = POINT { x: 0, y: 0 };
        let pt = POINT { x, y };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: AC_SRC_ALPHA as u8 };
        let _ = UpdateLayeredWindow(s.hwnd, Some(screen), Some(&pt), Some(&size), Some(dib.dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
        ReleaseDC(None, screen);
    }
}

fn finish(s: &mut State) {
    s.timer.stop();
    s.anim = None;
    s.fade = None;
    s.last = None;
    s.glass = None;
    s.content = Vec::new();
    s.dib = None;
    unsafe {
        let _ = ShowWindow(s.hwnd, SW_HIDE);
    }
    super::strip::set_hidden(false);
}

fn tick() {
    let done_cb = with(|s| {
        if let Some(t0) = s.fade {
            let k = (t0.elapsed().as_secs_f64() * 1000.0 / FADE_MS).min(1.0);
            if let Some(f) = s.last {
                present(s, &f, ((1.0 - k) * 255.0).round() as u8);
            }
            if k >= 1.0 {
                finish(s);
            }
            return None;
        }
        let a = s.anim.as_ref()?;
        let opening = a.opening;
        let f = a.frame();
        present(s, &f, 255);
        s.last = Some(f);
        if !f.done {
            return None;
        }
        s.anim = None;
        let cb = s.on_done.take();
        if opening {
            // 真面板接手后淡出
            s.fade = Some(Instant::now());
        } else {
            finish(s);
        }
        cb
    })
    .flatten();
    if let Some(cb) = done_cb {
        cb();
    }
}

fn start_timer(s: &mut State) {
    if !s.timer.running() {
        s.timer.start(slint::TimerMode::Repeated, Duration::from_millis(6), tick);
    }
}

pub struct Params {
    pub strip: Rect,
    pub panel: Rect,
    pub edge: String,
    pub scale: f64,
    pub dark: bool,
    pub accent: (f64, f64, f64),
    /// 面板最后一帧（预乘 BGRA，面板物理尺寸）
    pub content: Vec<Bgra>,
    pub cw: usize,
    pub ch: usize,
}

fn setup(s: &mut State, p: Params) -> (R, R, f64, f64) {
    let win = union(p.strip, p.panel, 18.0 * p.scale);
    s.win = win;
    let strip = local(p.strip, win);
    let panel = local(p.panel, win);
    s.at = (panel[0], panel[1]);
    s.content = p.content;
    s.cw = p.cw;
    s.ch = p.ch;
    s.look = Look { dark: p.dark, scale: p.scale, accent: p.accent };
    let r_strip = ((strip[2] - strip[0]).min(strip[3] - strip[1]) / 2.0).max(1.0);
    let r_panel = 8.0 * p.scale;
    (strip, panel, r_strip, r_panel)
}

/// 面板打开：从停靠条长出来。`on_done`：形状停稳时（该把真面板显示出来了）。
pub fn open(p: Params, on_done: impl FnOnce() + 'static) {
    if !ensure() {
        on_done();
        return;
    }
    let edge = p.edge.clone();
    let scale = p.scale;
    let ok = with(|s| {
        let reuse = s.anim.is_some() && s.glass.is_some();
        let prev = s.last;
        let (strip, panel, rs, rp) = setup(s, p);
        if !reuse || s.glass.as_ref().is_some_and(|g| g.len() != (s.win.2 * s.win.3) as usize) {
            // 窗口还没显示：截到的就是面板背后的屏幕（面板此时 alpha 0）
            unsafe {
                let was = s.fade.is_some() || s.anim.is_some();
                let _ = ShowWindow(s.hwnd, SW_HIDE);
                if was {
                    let _ = DwmFlush();
                }
            }
            s.glass = capture(s.win, scale);
        }
        let (from, from_r, from_c, from_g) = match (reuse, prev) {
            (true, Some(f)) => (f.rect, f.radius, f.content, f.grow),
            _ => (strip, rs, 0.0, 0.0),
        };
        s.anim = Some(Anim::new(true, from, panel, from_r, rp, &edge, from_c, from_g));
        s.fade = None;
        s.on_done = Some(Box::new(on_done));
        unsafe {
            let (x, y, w, h) = s.win;
            let _ = SetWindowPos(s.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
        }
        let f = s.anim.as_ref().map(|a| a.frame());
        if let Some(f) = f {
            present(s, &f, 255);
            s.last = Some(f);
        }
        unsafe {
            let _ = ShowWindow(s.hwnd, SW_SHOWNOACTIVATE);
        }
        super::strip::set_hidden(true);
        start_timer(s);
        true
    });
    if ok.is_none() {
        log::warn!("morph: open skipped (busy)");
    }
}

/// 面板收起：缩回停靠条。`panel_hwnd`：面板还在屏幕上时用来把它排除出截图；`park`：变形窗口
/// 已经盖上之后立刻停放真面板；`on_done`：缩完了。
pub fn close(p: Params, panel_glass: Option<Vec<Bgra>>, panel_hwnd: Option<HWND>, park: impl FnOnce(), on_done: impl FnOnce() + 'static) {
    if !ensure() {
        park();
        on_done();
        return;
    }
    let edge = p.edge.clone();
    let scale = p.scale;
    let mut park = Some(park);
    with(|s| {
        let running = s.anim.is_some() || s.fade.is_some();
        let prev = s.last;
        let (strip, panel, rs, rp) = setup(s, p);
        let size_ok = s.glass.as_ref().is_some_and(|g| g.len() == (s.win.2 * s.win.3) as usize);
        if running && size_ok {
            // 中途反向：沿用
        } else if let Some(gp) = panel_glass.as_ref() {
            s.glass = Some(extend_glass(gp, s.cw, s.ch, s.at, s.win));
        } else {
            unsafe {
                let _ = ShowWindow(s.hwnd, SW_HIDE);
                if let Some(h) = panel_hwnd {
                    let _ = SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE);
                    let _ = DwmFlush();
                }
            }
            s.glass = capture(s.win, scale);
            if let Some(h) = panel_hwnd {
                unsafe {
                    let _ = SetWindowDisplayAffinity(h, WDA_NONE);
                }
            }
        }
        let (from, from_r, from_c, from_g) = match (running, prev) {
            (true, Some(f)) if s.anim.as_ref().is_some_and(|a| a.opening) => (f.rect, f.radius, f.content, f.grow),
            _ => (panel, rp, 1.0, 1.0),
        };
        s.anim = Some(Anim::new(false, from, strip, from_r, rs, &edge, from_c, from_g));
        s.fade = None;
        s.on_done = Some(Box::new(on_done));
        unsafe {
            let (x, y, w, h) = s.win;
            let _ = SetWindowPos(s.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
        }
        let f = s.anim.as_ref().map(|a| a.frame());
        if let Some(f) = f {
            present(s, &f, 255);
            s.last = Some(f);
        }
        unsafe {
            let _ = ShowWindow(s.hwnd, SW_SHOWNOACTIVATE);
        }
        super::strip::set_hidden(true);
        if let Some(pk) = park.take() {
            pk();
        }
        start_timer(s);
    });
    if let Some(pk) = park.take() {
        pk();
    }
}
