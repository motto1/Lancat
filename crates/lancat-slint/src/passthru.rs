//! Windows：GPU 窗口（DirectComposition 交换链，WS_EX_NOREDIRECTIONBITMAP）的点击穿透。
//!
//! CPU 版窗口是 layered（UpdateLayeredWindow），系统按像素 alpha 命中：完全透明的像素点下去落到下面的窗口。
//! DComp 窗口没有逐像素命中，整个窗口矩形都会挡住点击；WM_NCHITTEST 返回 HTTRANSPARENT 也只对本线程的
//! 窗口有效，挡不住别的进程。业界做法（Electron setIgnoreMouseEvents forward、各类桌面挂件）：
//! 光标落在透明处时给窗口加 WS_EX_LAYERED | WS_EX_TRANSPARENT（整窗穿透，LWA_ALPHA 保持 255 不影响画面），
//! 回到实心处再去掉。窗口穿透时收不到鼠标消息，所以由线程定时器（16 ms，只在有窗口登记时跑）读光标位置。
//! 按住鼠标键时不切换（拖拽 / 选择中途不丢捕获）。
//!
//! `LANCAT_PASSTHRU=0` 关掉（回到整窗挡点击），`LANCAT_PASSTHRU=force` 让登记的窗口一直穿透（测试画面不受影响）。

use std::cell::{Cell, RefCell};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
use windows::Win32::UI::WindowsAndMessaging::*;

/// 窗口内坐标（物理 px）→ 这里是实心的（要接住点击）
type Hit = Box<dyn Fn(i32, i32) -> bool>;

struct Entry {
    hwnd: isize,
    hit: Hit,
    /// 我们加上了穿透（只撤销自己加的）
    on: bool,
}

thread_local! {
    static REG: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
    static TIMER: Cell<usize> = const { Cell::new(0) };
}

fn mode() -> &'static str {
    static M: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    M.get_or_init(|| std::env::var("LANCAT_PASSTHRU").unwrap_or_default())
}

pub fn enabled() -> bool {
    mode() != "0"
}

/// 登记一个 GPU 窗口（已登记就换判定函数）。窗口显示后调用。
pub fn watch(h: HWND, hit: impl Fn(i32, i32) -> bool + 'static) {
    if !enabled() || h.0.is_null() {
        return;
    }
    REG.with(|r| {
        let mut r = r.borrow_mut();
        if let Some(e) = r.iter_mut().find(|e| e.hwnd == h.0 as isize) {
            e.hit = Box::new(hit);
        } else {
            r.push(Entry { hwnd: h.0 as isize, hit: Box::new(hit), on: false });
        }
    });
    ensure_timer();
    poll();
}

/// 取消登记并撤掉自己加的穿透。窗口隐藏 / 销毁 / 换成 CPU 版之前调用。
pub fn unwatch(h: HWND) {
    let e = REG.with(|r| {
        let mut r = r.borrow_mut();
        let i = r.iter().position(|e| e.hwnd == h.0 as isize)?;
        Some(r.remove(i))
    });
    if let Some(e) = e {
        if e.on {
            set_pass(HWND(e.hwnd as _), false);
        }
    }
    if REG.with(|r| r.borrow().is_empty()) {
        let t = TIMER.with(|t| t.replace(0));
        if t != 0 {
            unsafe {
                let _ = KillTimer(None, t);
            }
        }
    }
}

/// 这个窗口现在是不是被我们设成穿透（测试 / 日志）
pub fn is_passing(h: HWND) -> bool {
    REG.with(|r| r.borrow().iter().any(|e| e.hwnd == h.0 as isize && e.on))
}

fn ensure_timer() {
    if TIMER.with(|t| t.get()) != 0 {
        return;
    }
    unsafe extern "system" fn tick(_: HWND, _: u32, _: usize, _: u32) {
        poll();
    }
    let id = unsafe { SetTimer(None, 0, 16, Some(tick)) };
    TIMER.with(|t| t.set(id));
}

fn set_pass(h: HWND, on: bool) {
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        let bits = (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) as isize;
        let new = if on { ex | bits } else { ex & !bits };
        if new != ex {
            SetWindowLongPtrW(h, GWL_EXSTYLE, new);
            if on {
                // 刚变成 layered 的窗口要有属性才会显示；255 = 画面不变
                let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
            }
        }
    }
}

fn buttons_down() -> bool {
    unsafe { [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON].iter().any(|k| GetAsyncKeyState(k.0 as i32) as u16 & 0x8000 != 0) }
}

fn poll() {
    let mut p = POINT::default();
    if unsafe { GetCursorPos(&mut p) }.is_err() {
        return;
    }
    let force = mode() == "force";
    let held = buttons_down();
    // 判定函数可能借用各自窗口的状态：先收集决定，再在借用之外改样式
    let mut changes: Vec<(isize, bool)> = Vec::new();
    REG.with(|r| {
        let Ok(mut r) = r.try_borrow_mut() else { return };
        r.retain(|e| unsafe { IsWindow(Some(HWND(e.hwnd as _))).as_bool() });
        for e in r.iter_mut() {
            let h = HWND(e.hwnd as _);
            let want = if force {
                true
            } else {
                let mut wr = RECT::default();
                unsafe {
                    let _ = GetWindowRect(h, &mut wr);
                }
                let inside = p.x >= wr.left && p.x < wr.right && p.y >= wr.top && p.y < wr.bottom;
                if !inside {
                    // 光标不在窗口上：穿透与否没区别，保持现状（少改样式）
                    e.on
                } else if held {
                    e.on
                } else {
                    !(e.hit)(p.x - wr.left, p.y - wr.top)
                }
            };
            if want != e.on {
                e.on = want;
                changes.push((e.hwnd, want));
            }
        }
    });
    for (h, on) in changes {
        set_pass(HWND(h as _), on);
    }
}
