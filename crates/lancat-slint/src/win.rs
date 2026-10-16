//! Windows：自写 Slint Platform。
//! - 一个 WS_POPUP 工具窗口；Slint SoftwareRenderer 渲染到预乘 BGRA 缓冲，WM_PAINT 里 SetDIBitsToDevice
//!   贴上去。窗口开了 Acrylic（accent policy，tint 几乎全透明）后 DWM 会按像素 alpha 合成，所以
//!   面板自己画的半透明玻璃底色下面就是系统模糊（与旧版 WebView 透明页面同一套做法）。
//! - 材质/圆角/阴影/淡入/停放沿用 src-tauri/src/platform/windows.rs 的逻辑（直接作用于 HWND）。
//! - 事件循环：MsgWaitForMultipleObjectsEx + Slint 定时器；invoke_from_event_loop 用 PostThreadMessage 唤醒。
//! - 输入法：IMM32（WM_IME_COMPOSITION）→ Slint 内部 InternalKeyEvent（UpdateComposition / CommitComposition），
//!   候选框位置来自 WindowAdapterInternal::input_method_request。

use crate::PanelWindow;
use i_slint_core::input::{InternalKeyEvent, KeyEventType};
use i_slint_core::window::{InputMethodRequest, WindowAdapterInternal};
use slint::platform::software_renderer::{RepaintBufferType, SoftwareRenderer};
use slint::platform::{Key, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalSize};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::core::{s, w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{GetCurrentThreadId, INFINITE};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::Ime::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

// ---------- pixel ----------

pub use crate::compose::Bgra;

// ---------- event loop ----------

const WM_WAKE: u32 = WM_APP + 1;
const WM_MOUSELEAVE_: u32 = 0x02A3;
type Job = Box<dyn FnOnce() + Send>;

struct Proxy {
    thread: u32,
    queue: Arc<Mutex<VecDeque<Job>>>,
    quit: Arc<std::sync::atomic::AtomicBool>,
}

impl slint::platform::EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        self.quit.store(true, std::sync::atomic::Ordering::SeqCst);
        unsafe {
            let _ = PostThreadMessageW(self.thread, WM_WAKE, WPARAM(0), LPARAM(0));
        }
        Ok(())
    }
    fn invoke_from_event_loop(&self, event: Job) -> Result<(), slint::EventLoopError> {
        self.queue.lock().unwrap().push_back(event);
        unsafe { PostThreadMessageW(self.thread, WM_WAKE, WPARAM(0), LPARAM(0)) }.map_err(|_| slint::EventLoopError::EventLoopTerminated)
    }
}

pub struct Win32Platform {
    thread: u32,
    queue: Arc<Mutex<VecDeque<Job>>>,
    quit: Arc<std::sync::atomic::AtomicBool>,
}

impl Win32Platform {
    pub fn new() -> Self {
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            // make sure the thread has a message queue before anyone posts to it
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        }
        Win32Platform { thread: unsafe { GetCurrentThreadId() }, queue: Default::default(), quit: Default::default() }
    }
}

thread_local! {
    /// 下一个创建的窗口是普通窗口（设置页）：`with_normal_window(|| SettingsWindow::new())`
    static NEXT_NORMAL: Cell<bool> = const { Cell::new(false) };
    static WINDOWS: RefCell<Vec<(isize, Weak<Win32Window>)>> = const { RefCell::new(Vec::new()) };
    /// 面板窗口（第一个创建的窗口）。
    static PANEL: RefCell<Weak<Win32Window>> = const { RefCell::new(Weak::new()) };
}

/// 停靠模式（默认开）：面板由 crate::dock 显示/收起；`LANCAT_SLINT_NODOCK=1` 退回阶段 2 的独立窗口。
pub fn dock_mode() -> bool {
    env("LANCAT_SLINT_NODOCK").as_deref() != Some("1")
}

fn panel() -> Option<Rc<Win32Window>> {
    PANEL.with(|p| p.borrow().upgrade())
}

/// 剪贴板的所有者窗口（EmptyClipboard 要一个窗口，NULL 时 SetClipboardData 会失败）
fn clip_owner() -> isize {
    WINDOWS.with(|w| w.borrow().iter().find(|(_, w)| w.strong_count() > 0).map(|(k, _)| *k).unwrap_or(0))
}

thread_local! {
    /// 上一次 Ctrl+V 是否当作文件 / 图片发送了（按住重复时沿用，不重复发送）
    static PASTE_SPECIAL: Cell<bool> = const { Cell::new(false) };
}

fn window_for(h: HWND) -> Option<Rc<Win32Window>> {
    WINDOWS.with(|w| w.borrow().iter().find(|(k, _)| *k == h.0 as isize).and_then(|(_, w)| w.upgrade()))
}

impl slint::platform::Platform for Win32Platform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(Win32Window::new()?)
    }

    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        loop {
            slint::platform::update_timers_and_animations();
            // render windows whose contents changed
            let wins: Vec<Rc<Win32Window>> = WINDOWS.with(|w| w.borrow().iter().filter_map(|(_, w)| w.upgrade()).collect());
            for w in &wins {
                if w.needs_redraw.get() {
                    // 先渲染（ReusedBuffer 只重画脏区），再只让脏区失效：WM_PAINT 只拷贝这一块，
                    // 不再每帧整窗 SetDIBitsToDevice（无设备时的雷达动画从 ~40% 单核降下来）
                    if let Some(r) = w.render() {
                        let full = std::env::var("LANCAT_FULL_BLIT").as_deref() == Ok("1");
                        unsafe {
                            let _ = InvalidateRect(Some(w.hwnd.get()), if full { None } else { Some(&r) }, false);
                        }
                    }
                }
            }
            let wait = slint::platform::duration_until_next_timer_update();
            let anim = wins.iter().any(|w| w.window.has_active_animations() || w.settling()) || crate::compose::animating();
            let ms = if anim { 16 } else { wait.map(|d| d.as_millis().min(u32::MAX as u128 - 1) as u32).unwrap_or(INFINITE) };
            unsafe {
                MsgWaitForMultipleObjectsEx(None, ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return Ok(());
                    }
                    if msg.hwnd.0.is_null() && msg.message == WM_WAKE {
                        continue;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                    // WM_PAINT 之后回到外层：paint 里的补渲染会再失效一块，动画进行中会一直有新的
                    // WM_PAINT，留在这里就永远跑不到 update_timers_and_animations（动画时间不走、
                    // 定时器和 invoke_from_event_loop 全饿死，界面卡死在那一帧）
                    if msg.message == WM_PAINT {
                        break;
                    }
                }
            }
            loop {
                let job = self.queue.lock().unwrap().pop_front();
                match job {
                    Some(j) => j(),
                    None => break,
                }
            }
            if self.quit.swap(false, std::sync::atomic::Ordering::SeqCst) {
                return Ok(());
            }
        }
    }

    fn set_clipboard_text(&self, text: &str, clipboard: slint::platform::Clipboard) {
        if clipboard == slint::platform::Clipboard::DefaultClipboard {
            crate::clip_win::set_text(clip_owner(), text);
        }
    }

    fn clipboard_text(&self, clipboard: slint::platform::Clipboard) -> Option<String> {
        if clipboard != slint::platform::Clipboard::DefaultClipboard {
            return None;
        }
        crate::clip_win::text(clip_owner())
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn slint::platform::EventLoopProxy>> {
        Some(Box::new(Proxy { thread: self.thread, queue: self.queue.clone(), quit: self.quit.clone() }))
    }
}

// ---------- window ----------

pub struct Win32Window {
    window: slint::Window,
    renderer: SoftwareRenderer,
    hwnd: Cell<HWND>,
    size: Cell<PhysicalSize>,
    scale: Cell<f32>,
    buf: RefCell<Vec<Bgra>>,
    needs_redraw: Cell<bool>,
    visible: Cell<bool>,
    tracking: Cell<bool>,
    surrogate: Cell<u16>,
    ime_rect: Cell<(i32, i32, i32, i32)>,
    first_paint_logged: Cell<bool>,
    /// 普通窗口（设置页）：有标题栏、可调大小、可激活；面板/停靠逻辑不作用于它
    normal: bool,
    shown_once: Cell<bool>,
    alpha: Cell<u8>,
    fade_gen: Cell<u64>,
    /// 弹出时页面的 scale(.985→1) 160 ms ease（旧版 Panel.svelte 的 Windows 分支）
    settle: Cell<Option<std::time::Instant>>,
    /// 变换后的帧（settle 期间 WM_PAINT 从这里拷）
    xbuf: RefCell<Vec<Bgra>>,
    xformed: Cell<bool>,
    /// 液态玻璃底（面板打开时）：背后屏幕的模糊图 + 边缘透镜 + 色调 + 高光，已按窗口尺寸合好；
    /// WM_PAINT 把 Slint 的帧叠在它上面。空 = 用窗口自己的 Acrylic。
    lq_base: RefCell<Vec<Bgra>>,
    /// 合成 lq_base 用的那张模糊背景（收起动画直接拿它当玻璃，不用再截屏）
    lq_glass: RefCell<Vec<Bgra>>,
    lq_dark: Cell<bool>,
}

const CLASS: PCWSTR = w!("LancatSlintPanel");

impl Win32Window {
    fn new() -> Result<Rc<Self>, slint::PlatformError> {
        let rc = Rc::<Self>::new_cyclic(|weak: &Weak<Self>| Win32Window {
            window: slint::Window::new(weak.clone() as Weak<dyn WindowAdapter>),
            renderer: SoftwareRenderer::new_with_repaint_buffer_type(RepaintBufferType::ReusedBuffer),
            hwnd: Cell::new(HWND::default()),
            size: Cell::new(PhysicalSize::new(1, 1)),
            scale: Cell::new(1.0),
            buf: RefCell::new(Vec::new()),
            needs_redraw: Cell::new(true),
            visible: Cell::new(false),
            tracking: Cell::new(false),
            surrogate: Cell::new(0),
            ime_rect: Cell::new((0, 0, 1, 16)),
            first_paint_logged: Cell::new(false),
            alpha: Cell::new(0),
            fade_gen: Cell::new(0),
            settle: Cell::new(None),
            xbuf: RefCell::new(Vec::new()),
            xformed: Cell::new(false),
            lq_base: RefCell::new(Vec::new()),
            lq_glass: RefCell::new(Vec::new()),
            lq_dark: Cell::new(false),
            normal: NEXT_NORMAL.with(|n| n.replace(false)),
            shown_once: Cell::new(false),
        });
        unsafe {
            let inst: HINSTANCE = GetModuleHandleW(None).map_err(|e| e.to_string())?.into();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: inst,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let (ex, style) = if rc.normal { (WINDOW_EX_STYLE(0), WS_OVERLAPPEDWINDOW) } else { (WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED, WS_POPUP) };
            let h = CreateWindowExW(ex, CLASS, w!("Lancat"), style, 0, 0, 480, 620, None, None, Some(inst), None).map_err(|e| e.to_string())?;
            rc.hwnd.set(h);
            WINDOWS.with(|w| w.borrow_mut().push((h.0 as isize, Rc::downgrade(&rc))));
            PANEL.with(|p| {
                if !rc.normal && p.borrow().upgrade().is_none() {
                    *p.borrow_mut() = Rc::downgrade(&rc);
                }
            });
            if !rc.normal {
                let _ = SetLayeredWindowAttributes(h, COLORREF(0), 0, LWA_ALPHA);
            }
            let scale = GetDpiForWindow(h) as f32 / 96.0;
            rc.scale.set(scale);
            rc.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
            if !rc.normal {
                setup_material(h, crate::theme_is_dark("system"));
            } else {
                let dark: i32 = crate::theme_is_dark("system") as i32;
                let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
                // 标题栏 / 任务栏图标：旧版（Tauri）窗口带应用图标，不设就是系统默认空白图标
                for (kind, base) in [(ICON_SMALL, 16.0f32), (ICON_BIG, 32.0f32)] {
                    if let Some(ic) = crate::dock::tray::load_icon((base * scale).round() as u32) {
                        SendMessageW(h, WM_SETICON, Some(WPARAM(kind as usize)), Some(LPARAM(ic.0 as isize)));
                    }
                }
            }
        }
        Ok(rc)
    }

    fn logical(&self, lp: LPARAM) -> LogicalPosition {
        let x = (lp.0 & 0xffff) as i16 as f32;
        let y = ((lp.0 >> 16) & 0xffff) as i16 as f32;
        let s = self.scale.get();
        LogicalPosition::new(x / s, y / s)
    }

    /// 渲染到缓冲（只重画脏区），返回要拷贝到窗口的矩形；没有变化返回 None。
    fn render(&self) -> Option<RECT> {
        let size = self.size.get();
        let (w, hgt) = (size.width as usize, size.height as usize);
        if w == 0 || hgt == 0 {
            return None;
        }
        let mut buf = self.buf.borrow_mut();
        let mut full = false;
        if buf.len() != w * hgt {
            buf.clear();
            buf.resize(w * hgt, Bgra::default());
            self.renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
            self.renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
            full = true;
        }
        self.needs_redraw.set(false);
        let t = std::time::Instant::now();
        let out = if self.normal {
            let r = self.renderer.render(&mut buf, w);
            let (o, sz) = (r.bounding_box_origin(), r.bounding_box_size());
            if sz.width == 0 || sz.height == 0 {
                crate::compose::Out::Partial(None)
            } else {
                crate::compose::Out::Partial(Some((o.x, o.y, sz.width, sz.height)))
            }
        } else {
            let o = crate::compose::render(&self.renderer, &mut buf, w, hgt, self.scale.get(), true);
            if o.composed() {
                // 合成时切换 Fx.pass 引起的重绘请求不算数；层里的动画还在跑就接着画
                self.needs_redraw.set(crate::compose::layer_anim());
            }
            o
        };
        if !self.first_paint_logged.replace(true) {
            log::info!("first frame {}x{} rendered in {} ms ({} ms after start)", w, hgt, t.elapsed().as_millis(), crate::t0().elapsed().as_millis());
        }
        // 弹出 settle：整帧以中心缩放（页面还在 .985 → 1）
        let mut full = full || matches!(out, crate::compose::Out::Full);
        let was = self.xformed.replace(false);
        if let Some(t0) = self.settle.get() {
            let k = (t0.elapsed().as_secs_f32() * 1000.0 / 160.0).min(1.0);
            if k < 1.0 {
                let e = crate::fx::bezier(0.2, 0.8, 0.3, 1.0, k);
                let sc = 0.985 + 0.015 * e;
                let mut xb = self.xbuf.borrow_mut();
                if xb.len() != buf.len() {
                    *xb = vec![Bgra::default(); buf.len()];
                }
                crate::compose::xform_frame(&buf, &mut xb, w, hgt, sc, 0.0, 0.0, 1.0);
                self.xformed.set(true);
                self.needs_redraw.set(true);
                return Some(RECT { left: 0, top: 0, right: w as i32, bottom: hgt as i32 });
            }
            self.settle.set(None);
        }
        if was {
            full = true;
        }
        if full {
            return Some(RECT { left: 0, top: 0, right: w as i32, bottom: hgt as i32 });
        }
        match out {
            crate::compose::Out::Partial(Some((x, y, rw, rh))) | crate::compose::Out::Inc((x, y, rw, rh)) => Some(RECT { left: x.max(0), top: y.max(0), right: (x + rw as i32).min(w as i32), bottom: (y + rh as i32).min(hgt as i32) }),
            _ => None,
        }
    }

    fn settling(&self) -> bool {
        self.settle.get().is_some()
    }

    fn paint(&self) {
        let h = self.hwnd.get();
        // 在 WM_PAINT 里才发现有新变化（两次循环之间又改了东西）：先渲染，拷完本次失效区后
        // 再把这次的脏区单独失效，否则落在 rcPaint 外的那部分新内容就永远不会被拷到窗口上。
        // 动画进行中的脏区交给外层循环按 16 ms 节奏画，这里补画会和 WM_PAINT 互相触发
        let late = if self.needs_redraw.get() && !self.window.has_active_animations() { self.render() } else { None };
        let size = self.size.get();
        let (w, hgt) = (size.width as usize, size.height as usize);
        let buf0 = self.buf.borrow();
        let xb = self.xbuf.borrow();
        let buf: &Vec<Bgra> = if self.xformed.get() && xb.len() == buf0.len() { &xb } else { &buf0 };
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(h, &mut ps);
            if buf.len() == w * hgt && w > 0 {
                // 只拷贝失效区：取覆盖它的整行作为一张小的 top-down DIB（源 y 恒为 0，避开 top-down 的 ySrc 歧义）
                let r = ps.rcPaint;
                let (x0, y0) = (r.left.max(0), r.top.max(0));
                let (x1, y1) = (r.right.min(w as i32), r.bottom.min(hgt as i32));
                if x1 > x0 && y1 > y0 {
                    let rows = (y1 - y0) as u32;
                    let bi = BITMAPINFO {
                        bmiHeader: BITMAPINFOHEADER {
                            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                            biWidth: w as i32,
                            biHeight: -(rows as i32),
                            biPlanes: 1,
                            biBitCount: 32,
                            biCompression: BI_RGB.0,
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    let lq = self.lq_base.borrow();
                    let mut tmp: Vec<Bgra> = Vec::new();
                    let p = if lq.len() == buf.len() {
                        // 帧叠在液态玻璃底上（预乘 over）
                        tmp.extend_from_slice(&buf[y0 as usize * w..y1 as usize * w]);
                        let base = &lq[y0 as usize * w..y1 as usize * w];
                        for (o, b) in tmp.iter_mut().zip(base) {
                            let inv = 255 - o.a as u32;
                            if inv > 0 {
                                o.b = (o.b as u32 + (b.b as u32 * inv + 127) / 255).min(255) as u8;
                                o.g = (o.g as u32 + (b.g as u32 * inv + 127) / 255).min(255) as u8;
                                o.r = (o.r as u32 + (b.r as u32 * inv + 127) / 255).min(255) as u8;
                                o.a = (o.a as u32 + (b.a as u32 * inv + 127) / 255).min(255) as u8;
                            }
                        }
                        tmp.as_ptr()
                    } else {
                        buf[y0 as usize * w..].as_ptr()
                    };
                    SetDIBitsToDevice(dc, x0, y0, (x1 - x0) as u32, rows, x0, 0, 0, rows, p as *const _, &bi, DIB_RGB_COLORS);
                }
            }
            let _ = EndPaint(h, &ps);
            if let Some(r) = late {
                let _ = InvalidateRect(Some(h), Some(&r), false);
            }
        }
    }

    fn key(&self, text: slint::SharedString, down: bool, repeat: bool) {
        let ev = if !down {
            WindowEvent::KeyReleased { text }
        } else if repeat {
            WindowEvent::KeyPressRepeated { text }
        } else {
            WindowEvent::KeyPressed { text }
        };
        self.window.dispatch_event(ev);
    }

    fn ime_dispatch(&self, ev: InternalKeyEvent) {
        self.window.dispatch_event(WindowEvent::internal(ev));
    }

    fn place_candidate(&self) {
        let (x, y, _w, hgt) = self.ime_rect.get();
        unsafe {
            let h = self.hwnd.get();
            let imc = ImmGetContext(h);
            if imc.is_invalid() {
                return;
            }
            let cf = COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: POINT { x, y }, ..Default::default() };
            let _ = ImmSetCompositionWindow(imc, &cf);
            let cand = CANDIDATEFORM { dwIndex: 0, dwStyle: CFS_EXCLUDE, ptCurrentPos: POINT { x, y: y + hgt }, rcArea: RECT { left: x, top: y, right: x + 1, bottom: y + hgt } };
            let _ = ImmSetCandidateWindow(imc, &cand);
            let _ = ImmReleaseContext(h, imc);
        }
    }
}

impl WindowAdapter for Win32Window {
    fn window(&self) -> &slint::Window {
        &self.window
    }
    fn size(&self) -> PhysicalSize {
        self.size.get()
    }
    fn renderer(&self) -> &dyn slint::platform::Renderer {
        &self.renderer
    }
    fn request_redraw(&self) {
        self.needs_redraw.set(true);
    }
    fn set_visible(&self, visible: bool) -> Result<(), slint::PlatformError> {
        self.visible.set(visible);
        let h = self.hwnd.get();
        if self.normal {
            unsafe {
                if visible {
                    if !self.shown_once.replace(true) {
                        center_normal(self);
                    }
                    let _ = ShowWindow(h, SW_SHOW);
                    let _ = SetForegroundWindow(h);
                } else {
                    let _ = ShowWindow(h, SW_HIDE);
                }
            }
            self.needs_redraw.set(true);
            return Ok(());
        }
        if visible && dock_mode() {
            // 停放：已映射、alpha 0、点击穿透；dock::open_panel 时再定位并淡入
            set_layered(h, true);
            set_alpha(h, 0, self);
            unsafe {
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            }
            self.needs_redraw.set(true);
        } else if visible {
            place_default(self);
            unsafe {
                // never take the foreground (a full-screen game may be in front)
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            }
            self.needs_redraw.set(true);
            self.settle.set(Some(std::time::Instant::now()));
            fade_to(self, 255, fade_ms(true));
        } else {
            unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
        Ok(())
    }
    fn position(&self) -> Option<slint::PhysicalPosition> {
        let mut r = RECT::default();
        unsafe { GetWindowRect(self.hwnd.get(), &mut r).ok()? };
        Some(slint::PhysicalPosition::new(r.left, r.top))
    }
    fn set_position(&self, p: slint::WindowPosition) {
        let p = p.to_physical(self.scale.get());
        unsafe {
            let _ = SetWindowPos(self.hwnd.get(), None, p.x, p.y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
    fn set_size(&self, s: slint::WindowSize) {
        let s = s.to_physical(self.scale.get());
        unsafe {
            let _ = SetWindowPos(self.hwnd.get(), None, 0, 0, s.width as i32, s.height as i32, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
    fn update_window_properties(&self, props: slint::platform::WindowProperties<'_>) {
        if self.normal {
            let t: Vec<u16> = props.title().encode_utf16().chain(Some(0)).collect();
            unsafe {
                let _ = SetWindowTextW(self.hwnd.get(), PCWSTR(t.as_ptr()));
            }
            return;
        }
        // fixed-size panel: take the preferred size from the layout
        let c = props.layout_constraints();
        let s = self.scale.get();
        let (w, h) = ((c.preferred.width * s).round() as i32, (c.preferred.height * s).round() as i32);
        let cur = self.size.get();
        if w > 0 && h > 0 && (w as u32 != cur.width || h as u32 != cur.height) {
            unsafe {
                let _ = SetWindowPos(self.hwnd.get(), None, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
    }
    fn internal(&self, _: i_slint_core::InternalToken) -> Option<&dyn WindowAdapterInternal> {
        Some(self)
    }
}

impl WindowAdapterInternal for Win32Window {
    fn input_method_request(&self, req: InputMethodRequest) {
        if let InputMethodRequest::Enable(p) | InputMethodRequest::Update(p) = req {
            let s = self.scale.get();
            self.ime_rect.set((
                (p.cursor_rect_origin.x * s) as i32,
                (p.cursor_rect_origin.y * s) as i32,
                (p.cursor_rect_size.width * s) as i32,
                (p.cursor_rect_size.height * s).max(1.0) as i32,
            ));
            self.place_candidate();
        }
    }
}

fn place_default(w: &Win32Window) {
    // skeleton: right edge of the primary work area, vertically centred (the dock places it later)
    unsafe {
        let mut wa = RECT::default();
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let s = w.scale.get();
        let (pw, ph) = ((360.0 * s).round() as i32, (520.0 * s).round() as i32);
        let x = env("LANCAT_SLINT_X").and_then(|v| v.parse().ok()).unwrap_or(wa.right - pw - (12.0 * s) as i32);
        let y = env("LANCAT_SLINT_Y").and_then(|v| v.parse().ok()).unwrap_or(wa.top + (wa.bottom - wa.top - ph) / 2);
        let _ = SetWindowPos(w.hwnd.get(), Some(HWND_TOPMOST), x, y, pw, ph, SWP_NOACTIVATE);
    }
}

fn modifier_key(vk: VIRTUAL_KEY) -> Option<Key> {
    Some(match vk {
        VK_SHIFT | VK_LSHIFT | VK_RSHIFT => Key::Shift,
        VK_CONTROL | VK_LCONTROL | VK_RCONTROL => Key::Control,
        VK_MENU | VK_LMENU | VK_RMENU => Key::Alt,
        VK_LWIN | VK_RWIN => Key::Meta,
        _ => return None,
    })
}

fn special_key(vk: VIRTUAL_KEY) -> Option<Key> {
    Some(match vk {
        VK_RETURN => Key::Return,
        VK_BACK => Key::Backspace,
        VK_TAB => Key::Tab,
        VK_ESCAPE => Key::Escape,
        VK_DELETE => Key::Delete,
        VK_LEFT => Key::LeftArrow,
        VK_RIGHT => Key::RightArrow,
        VK_UP => Key::UpArrow,
        VK_DOWN => Key::DownArrow,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_INSERT => Key::Insert,
        _ => return None,
    })
}

fn ime_string(h: HWND, kind: IME_COMPOSITION_STRING) -> Option<String> {
    unsafe {
        let imc = ImmGetContext(h);
        if imc.is_invalid() {
            return None;
        }
        let n = ImmGetCompositionStringW(imc, kind, None, 0);
        let out = if n > 0 {
            let mut v = vec![0u16; n as usize / 2];
            ImmGetCompositionStringW(imc, kind, Some(v.as_mut_ptr() as *mut _), n as u32);
            Some(String::from_utf16_lossy(&v))
        } else {
            Some(String::new())
        };
        let _ = ImmReleaseContext(h, imc);
        out
    }
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(w) = window_for(h) else { return DefWindowProcW(h, msg, wp, lp) };
    match msg {
        WM_PAINT => {
            w.paint();
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            let (pw, ph) = ((lp.0 & 0xffff) as u32, ((lp.0 >> 16) & 0xffff) as u32);
            if pw > 0 && ph > 0 {
                w.size.set(PhysicalSize::new(pw, ph));
                let s = w.scale.get();
                w.window.dispatch_event(WindowEvent::Resized { size: slint::LogicalSize::new(pw as f32 / s, ph as f32 / s) });
                w.needs_redraw.set(true);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let s = (wp.0 & 0xffff) as f32 / 96.0;
            w.scale.set(s);
            w.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: s });
            crate::dpr_changed();
            let r = &*(lp.0 as *const RECT);
            let _ = SetWindowPos(h, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_ACTIVATE as isize),
        WM_GETMINMAXINFO if w.normal => {
            let mi = &mut *(lp.0 as *mut MINMAXINFO);
            let s = w.scale.get();
            mi.ptMinTrackSize = POINT { x: (420.0 * s) as i32, y: (480.0 * s) as i32 };
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if !w.tracking.replace(true) {
                let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: h, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut t);
            }
            let pos = w.logical(lp);
            crate::note_pointer(w.normal, pos.x, pos.y, true);
            w.window.dispatch_event(WindowEvent::PointerMoved { position: pos });
            LRESULT(0)
        }
        WM_MOUSELEAVE_ => {
            w.tracking.set(false);
            crate::note_pointer(w.normal, -1000.0, -1000.0, false);
            w.window.dispatch_event(WindowEvent::PointerExited);
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let button = match msg {
                WM_LBUTTONDOWN => PointerEventButton::Left,
                WM_RBUTTONDOWN => PointerEventButton::Right,
                _ => PointerEventButton::Middle,
            };
            SetCapture(h);
            if !w.normal {
                // 点在弹层外：先关弹层（旧版 window click），这次点击照常分发
                let p = w.logical(lp);
                crate::pointer_down(p.x, p.y);
            }
            w.window.dispatch_event(WindowEvent::PointerPressed { position: w.logical(lp), button });
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            let button = match msg {
                WM_LBUTTONUP => PointerEventButton::Left,
                WM_RBUTTONUP => PointerEventButton::Right,
                _ => PointerEventButton::Middle,
            };
            let _ = ReleaseCapture();
            w.window.dispatch_event(WindowEvent::PointerReleased { position: w.logical(lp), button });
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let mut p = POINT { x: (lp.0 & 0xffff) as i16 as i32, y: ((lp.0 >> 16) & 0xffff) as i16 as i32 };
            let _ = ScreenToClient(h, &mut p);
            let s = w.scale.get();
            // Chromium（WebView2）滚轮一格 100px
            let delta = ((wp.0 >> 16) & 0xffff) as i16 as f32 / 120.0 * 100.0;
            w.window.dispatch_event(WindowEvent::PointerScrolled { position: LogicalPosition::new(p.x as f32 / s, p.y as f32 / s), delta_x: 0.0, delta_y: delta });
            LRESULT(0)
        }
        WM_SETFOCUS => {
            w.window.dispatch_event(WindowEvent::WindowActiveChanged(true));
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            w.window.dispatch_event(WindowEvent::WindowActiveChanged(false));
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let repeat = down && (lp.0 >> 30) & 1 == 1;
            let vk = VIRTUAL_KEY(wp.0 as u16);
            if let Some(k) = modifier_key(vk) {
                if !repeat {
                    w.key(k.into(), down, false);
                }
                return LRESULT(0);
            }
            if vk == VK_ESCAPE && !w.normal {
                // 窗口级 Esc（旧版 Panel.svelte onKey + 表情面板的捕获）
                if down && !repeat {
                    crate::escape_key();
                }
                return LRESULT(0);
            }
            if let Some(k) = special_key(vk) {
                w.key(k.into(), down, repeat);
                return LRESULT(0);
            }
            // Ctrl+letter (copy/paste/select-all): WM_CHAR delivers control codes, send the letter
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            if ctrl && (0x41..=0x5a).contains(&vk.0) {
                // Ctrl+V 在输入框里：剪贴板是文件 / 图片就直接发送（文字照常交给输入框）；按住不放的重复不再发一遍
                if down && vk.0 == 0x56 && !w.normal && crate::paste::composer_focused() {
                    let special = if repeat { PASTE_SPECIAL.with(|c| c.get()) } else { crate::clip_win::paste_special(h.0 as isize) };
                    PASTE_SPECIAL.with(|c| c.set(special));
                    if special {
                        return LRESULT(0);
                    }
                }
                let c = char::from_u32(vk.0 as u32 + 32).unwrap_or('a');
                w.key(c.to_string().into(), down, repeat);
                return LRESULT(0);
            }
            DefWindowProcW(h, msg, wp, lp)
        }
        WM_CHAR => {
            let u = wp.0 as u16;
            if (0xD800..0xDC00).contains(&u) {
                w.surrogate.set(u);
                return LRESULT(0);
            }
            let s = if (0xDC00..0xE000).contains(&u) {
                String::from_utf16_lossy(&[w.surrogate.replace(0), u])
            } else {
                String::from_utf16_lossy(&[u])
            };
            if s.chars().all(|c| !c.is_control()) {
                w.key(s.clone().into(), true, false);
                w.key(s.into(), false, false);
            }
            LRESULT(0)
        }
        WM_IME_STARTCOMPOSITION => {
            w.place_candidate();
            LRESULT(0)
        }
        WM_IME_COMPOSITION => {
            let flags = lp.0 as u32;
            if flags & GCS_RESULTSTR.0 != 0 {
                if let Some(text) = ime_string(h, GCS_RESULTSTR) {
                    let mut key_event = i_slint_core::items::KeyEvent::default();
                    key_event.text = text.into();
                    w.ime_dispatch(InternalKeyEvent { event_type: KeyEventType::CommitComposition, key_event, ..Default::default() });
                }
            }
            if flags & GCS_COMPSTR.0 != 0 {
                if let Some(pre) = ime_string(h, GCS_COMPSTR) {
                    w.ime_dispatch(InternalKeyEvent { event_type: KeyEventType::UpdateComposition, preedit_text: pre.into(), ..Default::default() });
                }
            }
            w.place_candidate();
            LRESULT(0)
        }
        WM_IME_ENDCOMPOSITION => {
            w.ime_dispatch(InternalKeyEvent { event_type: KeyEventType::UpdateComposition, preedit_text: "".into(), ..Default::default() });
            LRESULT(0)
        }
        WM_SETTINGCHANGE if w.normal => DefWindowProcW(h, msg, wp, lp),
        WM_SETTINGCHANGE => {
            // ImmersiveColorSet: system light/dark or accent changed
            refresh_material_hwnd(h, crate::theme_is_dark("system"));
            DefWindowProcW(h, msg, wp, lp)
        }
        WM_CLOSE => {
            w.window.dispatch_event(WindowEvent::CloseRequested);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

// ---------- material (ported from src-tauri/src/platform/windows.rs) ----------

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

fn accent(h: HWND, state: u32, dark: bool) -> bool {
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

fn setup_material(h: HWND, dark: bool) {
    if env("LANCAT_WIN_GLASS").as_deref() != Some("none") && !accent(h, ACCENT_ENABLE_ACRYLICBLURBEHIND, dark) {
        log::warn!("acrylic accent failed");
    }
    unsafe {
        let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as *const _, 4);
        if env("LANCAT_WIN_SHADOW").as_deref() != Some("0") {
            let m = MARGINS { cxLeftWidth: 1, cxRightWidth: 1, cyTopHeight: 1, cyBottomHeight: 1 };
            let _ = DwmExtendFrameIntoClientArea(h, &m);
        }
        let off: i32 = 1;
        let _ = DwmSetWindowAttribute(h, DWMWA_TRANSITIONS_FORCEDISABLED, &off as *const _ as *const _, 4);
    }
}

fn refresh_material_hwnd(h: HWND, dark: bool) {
    if env("LANCAT_WIN_GLASS").as_deref() != Some("none") {
        accent(h, ACCENT_ENABLE_ACRYLICBLURBEHIND, dark);
    }
}

pub fn refresh_material(ui: &PanelWindow, dark: bool) {
    let _ = ui;
    WINDOWS.with(|w| {
        for (h, _) in w.borrow().iter() {
            refresh_material_hwnd(HWND(*h as _), dark);
        }
    });
    let _ = dark;
}

fn reg_dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut v: u32 = 0;
    let mut size = 4u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key, value, RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut _), Some(&mut size)) };
    r.is_ok().then_some(v)
}

pub fn theme_is_dark(theme: &str) -> bool {
    match theme {
        "dark" => true,
        "light" => false,
        _ => reg_dword(w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), w!("AppsUseLightTheme")) == Some(0),
    }
}

/// 把本进程的工作集修剪掉（只是换出到备用列表，下次用到时软缺页取回）：toast 收回后调用。
pub fn trim_working_set() {
    unsafe {
        let _ = windows::Win32::System::ProcessStatus::EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
    }
}

// ---------- fade / park ----------

pub fn fade_ms(opening: bool) -> u64 {
    env("LANCAT_WIN_FADE_MS").and_then(|v| v.parse().ok()).unwrap_or(if opening { 120 } else { 140 })
}

fn set_alpha(h: HWND, a: u8, wnd: &Win32Window) {
    wnd.alpha.set(a);
    unsafe {
        let _ = SetLayeredWindowAttributes(h, COLORREF(0), a, LWA_ALPHA);
    }
}

fn set_layered(h: HWND, on: bool) {
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        let bits = (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) as isize;
        let new = if on { ex | bits } else { ex & !bits };
        if new != ex {
            SetWindowLongPtrW(h, GWL_EXSTYLE, new);
        }
    }
}

/// Whole-window alpha fade on the UI thread (ease-out in, ease-in out); fully in drops
/// WS_EX_LAYERED | WS_EX_TRANSPARENT, fully out parks the window (click-through, alpha 0).
fn fade_to(wnd: &Win32Window, to: u8, ms: u64) {
    let h = wnd.hwnd.get();
    let gen = wnd.fade_gen.get() + 1;
    wnd.fade_gen.set(gen);
    set_layered(h, true);
    let from = wnd.alpha.get();
    set_alpha(h, from, wnd);
    let start = std::time::Instant::now();
    let weak = window_for(h).map(|w| Rc::downgrade(&w));
    let Some(weak) = weak else { return };
    let timer = Rc::new(slint::Timer::default());
    let t2 = timer.clone();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(8), move || {
        let Some(w) = weak.upgrade() else { return t2.stop() };
        if w.fade_gen.get() != gen {
            return t2.stop();
        }
        let t = if ms == 0 { 1.0 } else { (start.elapsed().as_millis() as f64 / ms as f64).min(1.0) };
        let e = if to > from { 1.0 - (1.0 - t) * (1.0 - t) } else { t * t };
        let a = (from as f64 + (to as f64 - from as f64) * e).round().clamp(0.0, 255.0) as u8;
        set_alpha(h, a, &w);
        if t >= 1.0 {
            t2.stop();
            if to == 255 {
                set_layered(h, false);
                log::info!("panel fully shown {} ms after start", crate::t0().elapsed().as_millis());
            }
        }
    });
}

/// Esc / collapse: fade out and park (stays mapped, click-through, alpha 0).
pub fn request_hide() {
    if dock_mode() {
        crate::dock::hide_panel();
        return;
    }
    let wins: Vec<Rc<Win32Window>> = WINDOWS.with(|w| w.borrow().iter().filter_map(|(_, w)| w.upgrade()).collect());
    for w in wins {
        fade_to(&w, 0, fade_ms(false));
    }
}


// ---------- dock API（crate::dock 调用，UI 线程） ----------

pub fn panel_hwnd() -> Option<HWND> {
    panel().map(|w| w.hwnd.get())
}

pub fn panel_scale() -> Option<f32> {
    panel().map(|w| w.scale.get())
}

pub fn panel_place(r: crate::dock::Rect) {
    if let Some(w) = panel() {
        unsafe {
            let _ = SetWindowPos(w.hwnd.get(), Some(HWND_TOPMOST), r.x.round() as i32, r.y.round() as i32, r.w.round() as i32, r.h.round() as i32, SWP_NOACTIVATE);
        }
    }
}

/// 定位到停靠条旁并淡入（不激活：悬停打开时前台仍是用户原来的窗口）。
pub fn panel_show(r: crate::dock::Rect) {
    let Some(w) = panel() else { return };
    panel_place(r);
    w.needs_redraw.set(true);
    if w.alpha.get() == 0 {
        w.settle.set(Some(std::time::Instant::now()));
    }
    unsafe {
        let _ = ShowWindow(w.hwnd.get(), SW_SHOWNOACTIVATE);
    }
    fade_to(&w, 255, fade_ms(true));
}

/// 面板拿着前台时把前台交还给打开前的窗口（`prev`），否则交给 Z 序里下一个可见窗口。
fn give_back_foreground(h: HWND, prev: Option<isize>) {
    unsafe {
        if GetForegroundWindow() == h {
            let back = prev.map(|p| HWND(p as _)).filter(|p| IsWindow(Some(*p)).as_bool() && IsWindowVisible(*p).as_bool());
            let next = back.or_else(|| {
                let mut n = GetWindow(h, GW_HWNDNEXT).ok();
                while let Some(c) = n {
                    if IsWindowVisible(c).as_bool() && GetWindowLongPtrW(c, GWL_EXSTYLE) & WS_EX_TOOLWINDOW.0 as isize == 0 {
                        return Some(c);
                    }
                    n = GetWindow(c, GW_HWNDNEXT).ok();
                }
                None
            });
            if let Some(n) = next {
                let _ = SetForegroundWindow(n);
            }
        }
    }
}

/// 淡出并停放。
pub fn panel_hide(prev: Option<isize>) {
    let Some(w) = panel() else { return };
    give_back_foreground(w.hwnd.get(), prev);
    fade_to(&w, 0, fade_ms(false));
}

/// 面板最后一帧（先把还没画的变化画进缓冲）：(预乘 BGRA, 宽, 高)
fn panel_snapshot() -> Option<(Vec<Bgra>, usize, usize)> {
    let w = panel()?;
    let size = w.size.get();
    let n = (size.width * size.height) as usize;
    if w.needs_redraw.get() || w.buf.borrow().len() != n {
        let _ = w.render();
    }
    let b = w.buf.borrow();
    (n > 0 && b.len() == n).then(|| (b.clone(), size.width as usize, size.height as usize))
}

/// 用一张背后屏幕的模糊图（窗口尺寸）重建面板的液态玻璃底，整窗重画
fn set_liquid(w: &Win32Window, glass: Option<Vec<Bgra>>, dark: bool) {
    let size = w.size.get();
    let (pw, ph) = (size.width as usize, size.height as usize);
    let Some(glass) = glass.filter(|g| g.len() == pw * ph && pw > 0) else {
        w.lq_base.borrow_mut().clear();
        w.lq_glass.borrow_mut().clear();
        return;
    };
    let sc = w.scale.get() as f64;
    let mut base = vec![Bgra::default(); pw * ph];
    let none = [Bgra::default()];
    let content = crate::morph::Content { px: &none, w: 1, h: 1, at: (0.0, 0.0) };
    // 和变形最后一帧一样的参数（content = 1 只影响顶部高光强度）
    let f = crate::morph::Frame { rect: [0.0, 0.0, pw as f64, ph as f64], radius: 8.0 * sc, content: 1.0, grow: 1.0, done: true };
    crate::morph::render(&mut base, pw, ph, &f, Some(&glass), &content, &crate::morph::Look { dark, scale: sc, accent: crate::dock::ACCENT });
    *w.lq_base.borrow_mut() = base;
    *w.lq_glass.borrow_mut() = glass;
    w.lq_dark.set(dark);
    unsafe {
        let _ = InvalidateRect(Some(w.hwnd.get()), None, false);
    }
}

thread_local! {
    static LQ_TIMER: slint::Timer = slint::Timer::default();
}
static LQ_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 面板开着时每秒重截一次背后的屏幕（截的那一瞬间把面板排除出截图），玻璃里的背景跟着变
thread_local! {
    static LQ_SIG: Cell<u64> = const { Cell::new(0) };
}

/// 面板背后、与面板相交的可见顶层窗口（句柄 + 位置 + 标题长度）的指纹；不截屏，几微秒
fn behind_signature(panel: HWND) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    let mut pr = RECT::default();
    unsafe {
        let _ = GetWindowRect(panel, &mut pr);
        let mut h = GetWindow(panel, GW_HWNDNEXT).unwrap_or_default();
        let mut n = 0;
        while !h.0.is_null() && n < 400 {
            n += 1;
            if IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() {
                let mut r = RECT::default();
                let _ = GetWindowRect(h, &mut r);
                if r.left < pr.right && r.right > pr.left && r.top < pr.bottom && r.bottom > pr.top {
                    (h.0 as usize, r.left, r.top, r.right, r.bottom, GetWindowTextLengthW(h)).hash(&mut hs);
                }
            }
            h = GetWindow(h, GW_HWNDNEXT).unwrap_or_default();
        }
    }
    hs.finish() | 1
}

fn start_liquid_refresh() {
    LQ_TIMER.with(|t| {
        // 不再定时截屏（每截一次面板要从录屏/截图里消失一帧）：只在面板背后的窗口变了（切换、挪动、开关）时才重截
        t.start(slint::TimerMode::Repeated, Duration::from_millis(350), || {
            let Some(w) = panel() else { return };
            if w.alpha.get() != 255 || !crate::dock::is_visible() || crate::dock::morph::running() || w.lq_base.borrow().is_empty() {
                LQ_SIG.with(|s| s.set(0));
                return;
            }
            let sig = behind_signature(w.hwnd.get());
            let old = LQ_SIG.with(|s| s.replace(sig));
            if old == 0 || old == sig {
                return;
            }
            if LQ_BUSY.swap(true, std::sync::atomic::Ordering::AcqRel) {
                return;
            }
            let h = w.hwnd.get().0 as isize;
            let mut r = RECT::default();
            unsafe {
                let _ = GetWindowRect(w.hwnd.get(), &mut r);
            }
            let rect = (r.left, r.top, r.right - r.left, r.bottom - r.top);
            let sc = w.scale.get() as f64;
            let dark = w.lq_dark.get();
            std::thread::spawn(move || {
                let g = crate::dock::morph::capture_excluding(HWND(h as _), rect, sc);
                let _ = slint::invoke_from_event_loop(move || {
                    LQ_BUSY.store(false, std::sync::atomic::Ordering::Release);
                    let Some(w) = panel() else { return };
                    if w.alpha.get() == 255 && !w.lq_base.borrow().is_empty() && g.is_some() {
                        set_liquid(&w, g, dark);
                    }
                });
            });
        })
    });
}

/// 立刻停放（alpha 0、点击穿透），取消正在进行的淡入淡出
fn park_now(w: &Win32Window) {
    let h = w.hwnd.get();
    w.lq_base.borrow_mut().clear();
    w.lq_glass.borrow_mut().clear();
    w.fade_gen.set(w.fade_gen.get() + 1);
    set_layered(h, true);
    set_alpha(h, 0, w);
}

/// 打开：面板先停在原位（不可见），变形窗口从停靠条长出面板的样子，停稳后真面板接手。
pub fn panel_show_morph(g: crate::dock::Geo, edge: String, dark: bool, accent: (f64, f64, f64)) {
    if !crate::dock::morph::enabled() {
        return panel_show(g.panel);
    }
    let Some(w) = panel() else { return };
    panel_place(g.panel);
    park_now(&w);
    unsafe {
        let _ = ShowWindow(w.hwnd.get(), SW_SHOWNOACTIVATE);
    }
    // 等打开的钩子（切会话等）跑完、界面变化进了下一帧再截面板
    slint::Timer::single_shot(Duration::ZERO, move || {
        if !crate::dock::is_visible() {
            return;
        }
        let Some((px, cw, ch)) = panel_snapshot() else { return panel_show(g.panel) };
        let p = crate::dock::morph::Params { strip: g.dock, panel: g.panel, edge, scale: g.scale, dark, accent, content: px, cw, ch };
        crate::dock::morph::open(p, move || {
            if !crate::dock::is_visible() {
                return;
            }
            let Some(w) = panel() else { return };
            let h = w.hwnd.get();
            // 变形用的那张背景图接着给真面板当玻璃底，玻璃效果不会在停稳后消失
            if std::env::var("LANCAT_WIN_LIQUID").as_deref() != Ok("0") {
                set_liquid(&w, crate::dock::morph::crop_glass(g.panel), dark);
                start_liquid_refresh();
            }
            w.fade_gen.set(w.fade_gen.get() + 1);
            set_alpha(h, 255, &w);
            set_layered(h, false);
            w.needs_redraw.set(true);
            unsafe {
                let _ = InvalidateRect(Some(h), None, false);
            }
            log::info!("panel shown after morph, {} ms after start", crate::t0().elapsed().as_millis());
        });
    });
}

/// 收起：变形窗口以面板的样子盖上，面板立刻停放，形状缩回停靠条。
pub fn panel_hide_morph(prev: Option<isize>, g: crate::dock::Geo, edge: String, dark: bool, accent: (f64, f64, f64)) {
    let Some(w) = panel() else { return };
    let shown = w.alpha.get() == 255;
    if !crate::dock::morph::enabled() || !(shown || crate::dock::morph::running()) {
        return panel_hide(prev);
    }
    let h = w.hwnd.get();
    give_back_foreground(h, prev);
    let Some((px, cw, ch)) = panel_snapshot() else { return panel_hide(None) };
    let p = crate::dock::morph::Params { strip: g.dock, panel: g.panel, edge, scale: g.scale, dark, accent, content: px, cw, ch };
    // 面板现成的玻璃背景（≤1 s 前截的）：收起不用再截屏，动画立刻开始
    let glass = std::mem::take(&mut *w.lq_glass.borrow_mut());
    let glass = (glass.len() == cw * ch).then_some(glass);
    crate::dock::morph::close(p, glass, shown.then_some(h), || park_now(&w), || {});
}

/// 主动打开（点 toast、托盘、第二次启动）：拿键盘焦点；返回之前的前台窗口。
pub fn panel_focus() -> Option<isize> {
    let w = panel()?;
    let h = w.hwnd.get();
    unsafe {
        let prev = GetForegroundWindow();
        if prev == h {
            return None;
        }
        if !SetForegroundWindow(h).as_bool() {
            log::debug!("SetForegroundWindow refused (foreground lock)");
        }
        let _ = SetFocus(Some(h));
        (!prev.0.is_null()).then_some(prev.0 as isize)
    }
}

/// 在 f 里创建的 Slint 窗口是普通窗口（标题栏、可调大小、显示时激活），用于设置页。
pub fn with_normal_window<R>(f: impl FnOnce() -> R) -> R {
    NEXT_NORMAL.with(|n| n.set(true));
    let r = f();
    NEXT_NORMAL.with(|n| n.set(false));
    r
}

fn center_normal(w: &Win32Window) {
    unsafe {
        let mut wa = RECT::default();
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let s = w.scale.get();
        // 客户区 480×620（旧版设置窗口 inner_size），加上边框
        let mut r = RECT { left: 0, top: 0, right: (480.0 * s) as i32, bottom: (620.0 * s) as i32 };
        let _ = AdjustWindowRectExForDpi(&mut r, WS_OVERLAPPEDWINDOW, false, WINDOW_EX_STYLE(0), GetDpiForWindow(w.hwnd.get()));
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
        let x = wa.left + (wa.right - wa.left - ww) / 2;
        let y = wa.top + (wa.bottom - wa.top - wh) / 2;
        let _ = SetWindowPos(w.hwnd.get(), None, x, y, ww, wh, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// 设置窗口（所有普通窗口）的标题栏深浅色跟随应用主题
pub fn set_normal_dark(dark: bool) {
    let wins: Vec<Rc<Win32Window>> = WINDOWS.with(|w| w.borrow().iter().filter_map(|(_, w)| w.upgrade()).filter(|w| w.normal).collect());
    for w in wins {
        let d: i32 = dark as i32;
        unsafe {
            let _ = DwmSetWindowAttribute(w.hwnd.get(), DWMWA_USE_IMMERSIVE_DARK_MODE, &d as *const _ as *const _, 4);
        }
    }
}
