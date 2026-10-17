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
use std::rc::Rc;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CAPTUREBLT,
    DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassW, SetWindowDisplayAffinity, SetWindowPos, ShowWindow, UpdateLayeredWindow, HWND_TOPMOST, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA,
    WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOREDIRECTIONBITMAP, WM_NCHITTEST, HTTRANSPARENT, SetLayeredWindowAttributes, LWA_ALPHA, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
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

/// 帧统计（LANCAT_FRAME_STATS）
struct Stats {
    t0: Instant,
    last: Option<Instant>,
    n: u32,
    max_gap: f32,
    slow: u32,
    work_max: f32,
    work_sum: f32,
}

impl Stats {
    fn new() -> Stats {
        Stats { t0: Instant::now(), last: None, n: 0, max_gap: 0.0, slow: 0, work_max: 0.0, work_sum: 0.0 }
    }
}

struct State {
    hwnd: HWND,
    /// GPU 版的变形窗口（WS_EX_NOREDIRECTIONBITMAP + DComp 交换链），用到时才建
    ghwnd: HWND,
    /// 这一次动画在 GPU 上画
    gpu: bool,
    surf: Option<crate::gpu::Surface>,
    gglass: Option<Rc<crate::gpu::Glass>>,
    gcontent: Option<crate::gpu::GTex>,
    stats: Stats,
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
                ghwnd: HWND::default(),
                gpu: false,
                surf: None,
                gglass: None,
                gcontent: None,
                stats: Stats::new(),
                on_done: None,
                fade: None,
                timer: slint::Timer::default(),
            })
        });
    }
    true
}

/// 变形走 GPU：面板已经接上硬件 D3D 设备，渲染方式是 GPU（LANCAT_MORPH_GPU=0 强制 CPU 版）
pub fn gpu_mode() -> bool {
    crate::gpu::wanted() && crate::gpu::active() && crate::gpu::hardware() && std::env::var("LANCAT_MORPH_GPU").as_deref() != Ok("0")
}

fn ensure_gwin(s: &mut State) -> bool {
    if !s.ghwnd.0.is_null() {
        return true;
    }
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return false };
        let class = w!("LancatPanelMorphGpu");
        unsafe extern "system" fn wndproc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
            if m == WM_NCHITTEST {
                // 点击穿透（同线程的停靠条 / 面板收到；变形只有几百毫秒）
                return LRESULT(HTTRANSPARENT as isize);
            }
            DefWindowProcW(h, m, wp, lp)
        }
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hmod.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        // WS_EX_LAYERED | WS_EX_TRANSPARENT（LWA_ALPHA 255）：整窗点击穿透，跨进程也有效（只靠 HTTRANSPARENT
        // 挡不住别的进程的窗口）。LANCAT_MORPH_LAYERED=0 退回不带 layered。
        let layered = std::env::var("LANCAT_MORPH_LAYERED").as_deref() != Ok("0");
        let mut ex = WS_EX_NOREDIRECTIONBITMAP | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;
        if layered {
            ex |= WS_EX_LAYERED;
        }
        let Ok(h) = CreateWindowExW(ex, class, w!("Lancat Morph"), WS_POPUP, 0, 0, 10, 10, None, None, Some(hmod.into()), None) else {
            log::warn!("morph: CreateWindowExW (gpu) failed");
            return false;
        };
        if layered {
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
        }
        s.ghwnd = h;
    }
    true
}

/// 当前这次动画用的窗口
fn cur_hwnd(s: &State) -> HWND {
    if s.gpu {
        s.ghwnd
    } else {
        s.hwnd
    }
}

/// 换渲染方式（或设备没了）：丢掉 GPU 资源，正在跑的动画直接收尾
pub fn reset_gpu() {
    let cb = with(|s| {
        let cb = if s.gpu && (s.anim.is_some() || s.fade.is_some()) {
            finish(s);
            s.on_done.take()
        } else {
            None
        };
        s.surf = None;
        s.gglass = None;
        s.gcontent = None;
        s.gpu = false;
        if !s.ghwnd.0.is_null() {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(s.ghwnd);
            }
            s.ghwnd = HWND::default();
        }
        cb
    })
    .flatten();
    if let Some(cb) = cb {
        cb();
    }
}

/// GPU 动画进行中：事件循环等这个句柄（交换链可以收下一帧 = 显示器刷新节拍），有信号就 gpu_tick
pub fn gpu_wait() -> Option<HANDLE> {
    with(|s| if s.gpu && (s.anim.is_some() || s.fade.is_some()) { s.surf.as_ref().map(|x| x.handle()) } else { None }).flatten()
}

thread_local! {
    /// 这一帧之前事件循环在交换链句柄上等了多久（累计，帧统计用）
    static WAITED: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

pub fn note_wait(ms: f32) {
    WAITED.with(|w| w.set(w.get() + ms));
}

pub fn gpu_tick() {
    tick();
}

/// 打开动画最后的模糊背景（GPU）：真面板接着当液态玻璃底，不读回 CPU
pub fn gpu_glass() -> Option<Rc<crate::gpu::Glass>> {
    with(|s| if s.gpu { s.gglass.clone() } else { None }).flatten()
}

/// 预取的背景截图：光标靠近停靠条（还没打开）时在后台线程先截好，打开时省掉 12–25 ms 的同步截屏。
/// 只在窗口矩形一致、截了不到 PREFETCH_TTL 时用（期间屏幕变了也只差一点点模糊后的颜色）。
static PREFETCH: std::sync::Mutex<Option<((i32, i32, i32, i32), Instant, Vec<Bgra>)>> = std::sync::Mutex::new(None);
static PREFETCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const PREFETCH_TTL: std::time::Duration = std::time::Duration::from_millis(1500);

/// 面板收着、光标靠近 / 悬停在停靠条上：后台先截下这次展开要用的那块屏幕（GPU 模式才有意义：模糊在 GPU 上几乎不花时间）
pub fn prefetch(strip: Rect, panel: Rect, scale: f64) {
    if !gpu_mode() || running() || std::env::var("LANCAT_MORPH_PREFETCH").as_deref() == Ok("0") {
        return;
    }
    let win = union(strip, panel, 18.0 * scale);
    if PREFETCH.lock().unwrap().as_ref().is_some_and(|(w, t, _)| *w == win && t.elapsed() < PREFETCH_TTL / 3) {
        return;
    }
    if PREFETCHING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        let px = capture_raw(win);
        if let Some(px) = px {
            *PREFETCH.lock().unwrap() = Some((win, Instant::now(), px));
        }
        PREFETCHING.store(false, std::sync::atomic::Ordering::Release);
    });
}

fn take_prefetch(win: (i32, i32, i32, i32)) -> Option<Vec<Bgra>> {
    let (w, t, px) = PREFETCH.lock().unwrap().take()?;
    (w == win && t.elapsed() < PREFETCH_TTL).then_some(px)
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
    let px = capture_raw(win)?;
    Some(morph::blur_half(&px, win.2 as usize, win.3 as usize, blur_radius(scale)))
}

pub fn blur_radius(scale: f64) -> f64 {
    (11.0 * scale).max(2.0)
}

/// 截屏幕（不模糊；GPU 版在着色器里模糊）
pub fn capture_raw(win: (i32, i32, i32, i32)) -> Option<Vec<Bgra>> {
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
    Some(px)
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
    let px = capture_excluding_raw(exclude, win)?;
    Some(morph::blur_half(&px, win.2 as usize, win.3 as usize, blur_radius(scale)))
}

/// 同上，不模糊（GPU 版）
pub fn capture_excluding_raw(exclude: HWND, win: (i32, i32, i32, i32)) -> Option<Vec<Bgra>> {
    unsafe {
        if let Err(e) = SetWindowDisplayAffinity(exclude, WDA_EXCLUDEFROMCAPTURE) {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| log::warn!("liquid glass refresh: SetWindowDisplayAffinity failed: {e}"));
            return None;
        }
        let _ = DwmFlush();
    }
    let g = capture_raw(win);
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
    let t = Instant::now();
    if s.gpu {
        present_gpu(s, f, alpha);
    } else {
        present_cpu(s, f, alpha);
    }
    if crate::compose::stats_on() {
        let work = t.elapsed().as_secs_f32() * 1000.0;
        let st = &mut s.stats;
        let gap = st.last.map(|l| t.duration_since(l).as_secs_f32() * 1000.0).unwrap_or(0.0);
        st.last = Some(t);
        st.n += 1;
        st.work_sum += work;
        st.work_max = st.work_max.max(work);
        if st.n > 1 {
            st.max_gap = st.max_gap.max(gap);
            if gap > crate::gpu::frame_ms() as f32 * 1.5 + 1.0 {
                st.slow += 1;
            }
        }
        let waited = WAITED.with(|w| w.replace(0.0));
        log::info!("morph frame {work:.1} ms (gap {gap:.1} ms, waited {waited:.1} ms) {}", if s.gpu { "gpu" } else { "cpu" });
    }
}

fn present_gpu(s: &mut State, f: &Frame, alpha: u8) {
    let Some(surf) = s.surf.as_ref() else { return };
    let content = s.gcontent.as_ref().map(|c| (c, (0.0, 0.0)));
    let d = crate::gpu::MorphDraw { frame: f, look: &s.look, org: (s.win.0 as f64, s.win.1 as f64), glass: s.gglass.as_deref(), content, alpha: alpha as f32 / 255.0 };
    crate::gpu::draw_morph(surf, &d);
    surf.present();
}

fn present_cpu(s: &mut State, f: &Frame, alpha: u8) {
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
    if crate::compose::stats_on() && s.stats.n > 0 {
        let st = &s.stats;
        let ms = st.t0.elapsed().as_secs_f32() * 1000.0;
        log::info!(
            "morph done {}: {} frames in {:.0} ms (avg gap {:.1} ms, max gap {:.1} ms, {} slow > 1.5 frame), work avg {:.2} max {:.2} ms",
            if s.gpu { "gpu" } else { "cpu" },
            st.n,
            ms,
            ms / st.n.max(1) as f32,
            st.max_gap,
            st.slow,
            st.work_sum / st.n as f32,
            st.work_max
        );
    }
    if s.gpu {
        // 下次显示前别闪出上一次的最后一帧
        if let Some(surf) = s.surf.as_ref() {
            surf.wait(0);
            surf.clear();
            surf.present();
        }
        s.gcontent = None;
    }
    s.anim = None;
    s.fade = None;
    s.last = None;
    s.glass = None;
    s.content = Vec::new();
    s.dib = None;
    unsafe {
        let _ = ShowWindow(cur_hwnd(s), SW_HIDE);
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
    if s.gpu {
        // GPU：事件循环按交换链的节拍调 gpu_tick（gpu_wait），不用定时器
        s.timer.stop();
        return;
    }
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
    /// GPU 快照（有就走 GPU 版，content 可以是空的）
    pub gcontent: Option<crate::gpu::GTex>,
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
    if let Some(g) = p.gcontent {
        let (w, h) = g.size();
        s.cw = w;
        s.ch = h;
        s.gcontent = Some(g);
    } else {
        s.gcontent = None;
    }
    s.look = Look { dark: p.dark, scale: p.scale, accent: p.accent };
    let r_strip = ((strip[2] - strip[0]).min(strip[3] - strip[1]) / 2.0).max(1.0);
    let r_panel = 8.0 * p.scale;
    (strip, panel, r_strip, r_panel)
}

/// 这一次动画用 GPU 还是 CPU：换了的话先把正在跑的另一种收掉；GPU 准备交换链（窗口尺寸）
fn choose(s: &mut State, want_gpu: bool) -> bool {
    let running = s.anim.is_some() || s.fade.is_some();
    if s.gpu != want_gpu && running {
        unsafe {
            let _ = ShowWindow(cur_hwnd(s), SW_HIDE);
        }
        s.timer.stop();
        s.anim = None;
        s.fade = None;
        s.last = None;
        s.glass = None;
        s.gglass = None;
    }
    let mut gpu = want_gpu;
    if gpu && !ensure_gwin(s) {
        gpu = false;
    }
    s.gpu = gpu;
    gpu
}

/// GPU 设备刚接上（启动 / 切换 / 设备丢失后重建）：先把变形窗口和它的交换链按这次布局的尺寸建好、清一帧，
/// 第一次展开就不用再现建 DComp 目标和交换链（首帧前少 5–8 ms，首次展开不掉帧）
pub fn prewarm() {
    if !gpu_mode() || running() {
        return;
    }
    let Some(g) = super::geo() else { return };
    if !ensure() {
        return;
    }
    let t = Instant::now();
    let ok = with(|s| {
        if !ensure_gwin(s) {
            return false;
        }
        s.win = union(g.dock, g.panel, 18.0 * g.scale);
        let ok = prep_surface(s);
        if let Some(x) = s.surf.as_ref() {
            x.clear();
            x.present();
        }
        ok
    })
    .unwrap_or(false);
    log::info!("morph: prewarmed GPU surface ({ok}) in {:.1} ms", t.elapsed().as_secs_f32() * 1000.0);
}

fn prep_surface(s: &mut State) -> bool {
    let (w, h) = (s.win.2.max(1) as u32, s.win.3.max(1) as u32);
    match s.surf.as_mut() {
        Some(x) => x.resize(w, h),
        None => {
            s.surf = crate::gpu::Surface::new(s.ghwnd, w, h);
            s.surf.is_some()
        }
    }
}

fn gglass_fits(s: &State) -> bool {
    s.gglass.as_ref().is_some_and(|g| g.org == (s.win.0, s.win.1) && g.w == s.win.2 as u32 && g.h == s.win.3 as u32)
}

/// 第一帧：贴上并显示窗口（GPU 先等交换链空出来）
fn first_frame(s: &mut State) {
    unsafe {
        let (x, y, w, h) = s.win;
        let _ = SetWindowPos(cur_hwnd(s), Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
    }
    if s.gpu {
        if let Some(x) = s.surf.as_ref() {
            x.wait(crate::gpu::frame_ms() + 2);
        }
    }
    let f = s.anim.as_ref().map(|a| a.frame());
    if let Some(f) = f {
        present(s, &f, 255);
        s.last = Some(f);
    }
    unsafe {
        let _ = ShowWindow(cur_hwnd(s), SW_SHOWNOACTIVATE);
    }
}

/// 面板打开：从停靠条长出来。`on_done`：形状停稳时（该把真面板显示出来了）。
pub fn open(p: Params, on_done: impl FnOnce() + 'static) {
    if !ensure() {
        on_done();
        return;
    }
    let want_gpu = p.gcontent.is_some() && gpu_mode();
    let edge = p.edge.clone();
    let scale = p.scale;
    let mut on_done: Option<Box<dyn FnOnce()>> = Some(Box::new(on_done));
    let ok = with(|s| {
        let t0 = Instant::now();
        let gpu = choose(s, want_gpu);
        if want_gpu && !gpu {
            return false;
        }
        let reuse = s.anim.is_some() && if gpu { s.gglass.is_some() } else { s.glass.is_some() };
        let prev = s.last;
        let (strip, panel, rs, rp) = setup(s, p);
        if gpu && !prep_surface(s) {
            s.gpu = false;
            return false;
        }
        let fits = if gpu { gglass_fits(s) } else { s.glass.as_ref().is_some_and(|g| g.len() == (s.win.2 * s.win.3) as usize) };
        let mut t_cap = 0.0;
        if !reuse || !fits {
            // 窗口还没显示：截到的就是面板背后的屏幕（面板此时 alpha 0）
            unsafe {
                let was = s.fade.is_some() || s.anim.is_some();
                let _ = ShowWindow(cur_hwnd(s), SW_HIDE);
                if was {
                    let _ = DwmFlush();
                }
            }
            let tc = Instant::now();
            if gpu {
                let pre = take_prefetch(s.win);
                if pre.is_some() && crate::compose::stats_on() {
                    log::info!("morph open: using prefetched backdrop");
                }
                let px = pre.or_else(|| capture_raw(s.win));
                s.gglass = px.and_then(|px| crate::gpu::glass_from_pixels(&px, s.win.2 as usize, s.win.3 as usize, (s.win.0, s.win.1), blur_radius(scale)));
            } else {
                s.glass = capture(s.win, scale);
            }
            t_cap = tc.elapsed().as_secs_f32() * 1000.0;
        }
        let (from, from_r, from_c, from_g) = match (reuse, prev) {
            (true, Some(f)) => (f.rect, f.radius, f.content, f.grow),
            _ => (strip, rs, 0.0, 0.0),
        };
        if !reuse {
            s.stats = Stats::new();
        }
        s.anim = Some(Anim::new(true, from, panel, from_r, rp, &edge, from_c, from_g));
        s.fade = None;
        s.on_done = on_done.take();
        first_frame(s);
        super::strip::set_hidden(true);
        start_timer(s);
        if crate::compose::stats_on() {
            log::info!("morph open {} setup {:.1} ms (capture+blur {:.1} ms) {}x{}", if s.gpu { "gpu" } else { "cpu" }, t0.elapsed().as_secs_f32() * 1000.0, t_cap, s.win.2, s.win.3);
        }
        true
    });
    match ok {
        Some(true) => {}
        Some(false) => {
            log::warn!("morph: GPU surface unavailable, showing the panel directly");
            if let Some(cb) = on_done.take() {
                cb();
            }
        }
        None => log::warn!("morph: open skipped (busy)"),
    }
}

/// 面板收起：缩回停靠条。`panel_hwnd`：面板还在屏幕上时用来把它排除出截图；`park`：变形窗口
/// 已经盖上之后立刻停放真面板；`on_done`：缩完了。`panel_gglass`：面板液态玻璃底的 GPU 模糊图。
pub fn close(p: Params, panel_glass: Option<Vec<Bgra>>, panel_gglass: Option<Rc<crate::gpu::Glass>>, panel_hwnd: Option<HWND>, park: impl FnOnce(), on_done: impl FnOnce() + 'static) {
    if !ensure() {
        park();
        on_done();
        return;
    }
    let want_gpu = p.gcontent.is_some() && gpu_mode();
    let edge = p.edge.clone();
    let scale = p.scale;
    let mut park = Some(park);
    let mut on_done: Option<Box<dyn FnOnce()>> = Some(Box::new(on_done));
    with(|s| {
        let t0 = Instant::now();
        let gpu = choose(s, want_gpu);
        if want_gpu && !gpu {
            return;
        }
        let running = s.anim.is_some() || s.fade.is_some();
        let prev = s.last;
        let (strip, panel, rs, rp) = setup(s, p);
        if gpu && !prep_surface(s) {
            s.gpu = false;
            return;
        }
        let size_ok = if gpu { gglass_fits(s) } else { s.glass.as_ref().is_some_and(|g| g.len() == (s.win.2 * s.win.3) as usize) };
        let tc = Instant::now();
        if running && size_ok {
            // 中途反向：沿用
        } else if gpu && panel_gglass.is_some() {
            // 面板的玻璃底那张（屏幕坐标取样，超出部分夹到边上 = extend_glass）
            s.gglass = panel_gglass;
        } else if !gpu && panel_glass.is_some() {
            s.glass = panel_glass.as_ref().map(|gp| extend_glass(gp, s.cw, s.ch, s.at, s.win));
        } else {
            unsafe {
                let _ = ShowWindow(cur_hwnd(s), SW_HIDE);
                if let Some(h) = panel_hwnd {
                    let _ = SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE);
                    let _ = DwmFlush();
                }
            }
            if gpu {
                let px = capture_raw(s.win);
                s.gglass = px.and_then(|px| crate::gpu::glass_from_pixels(&px, s.win.2 as usize, s.win.3 as usize, (s.win.0, s.win.1), blur_radius(scale)));
            } else {
                s.glass = capture(s.win, scale);
            }
            if let Some(h) = panel_hwnd {
                unsafe {
                    let _ = SetWindowDisplayAffinity(h, WDA_NONE);
                }
            }
        }
        let t_cap = tc.elapsed().as_secs_f32() * 1000.0;
        let (from, from_r, from_c, from_g) = match (running, prev) {
            (true, Some(f)) if s.anim.as_ref().is_some_and(|a| a.opening) => (f.rect, f.radius, f.content, f.grow),
            _ => (panel, rp, 1.0, 1.0),
        };
        if !running {
            s.stats = Stats::new();
        }
        s.anim = Some(Anim::new(false, from, strip, from_r, rs, &edge, from_c, from_g));
        s.fade = None;
        s.on_done = on_done.take();
        first_frame(s);
        super::strip::set_hidden(true);
        if let Some(pk) = park.take() {
            pk();
        }
        start_timer(s);
        if crate::compose::stats_on() {
            log::info!("morph close {} setup {:.1} ms (glass {:.1} ms) {}x{}", if s.gpu { "gpu" } else { "cpu" }, t0.elapsed().as_secs_f32() * 1000.0, t_cap, s.win.2, s.win.3);
        }
    });
    if let Some(pk) = park.take() {
        pk();
    }
    if let Some(cb) = on_done.take() {
        cb();
    }
}
