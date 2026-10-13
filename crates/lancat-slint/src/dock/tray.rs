//! 系统托盘（Windows，Shell_NotifyIconW）：与旧版 src-tauri/src/tray.rs 相同的菜单和行为。
//!
//! - 左键单击：切换面板（打开时拿键盘焦点，与旧版 "tray" 原因相同）；
//! - 右键：打开 Lancat / 钉住面板 / 刷新设备 / 打开接收文件夹 / 设置… / 开机自动启动 / 退出 Lancat；
//! - 提示文字显示未读数（「Lancat — N 条未读」）；
//! - Explorer 重启（TaskbarCreated 广播）后重新加图标。
//!
//! 开机自启动与旧版 tauri-plugin-autostart（auto-launch crate）用同一个注册表位置：
//! HKCU\Software\Microsoft\Windows\CurrentVersion\Run 的 "Lancat" 值，并认 StartupApproved
//! （任务管理器里禁用 = 首字节为奇数）。测试实例（设了 LANCAT_DATA_DIR）这一项置灰，不碰注册表。
//!
//! 托盘窗口必须是普通顶层窗口（隐藏）：消息窗口（HWND_MESSAGE）收不到 TaskbarCreated 广播。

use std::cell::Cell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::{ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 0x31;
const ICON_ID: u32 = 1;
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const APPROVED_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run");
const APP_NAME: PCWSTR = w!("Lancat");

const ID_OPEN: usize = 1;
const ID_PIN: usize = 2;
const ID_REFRESH: usize = 3;
const ID_RECV: usize = 4;
const ID_SETTINGS: usize = 5;
const ID_AUTOSTART: usize = 6;
const ID_QUIT: usize = 7;

static ICO: &[u8] = include_bytes!("../../assets/icon.ico");

thread_local! {
    static HWND_TRAY: Cell<isize> = const { Cell::new(0) };
    static HICON_TRAY: Cell<isize> = const { Cell::new(0) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static LAST_UNREAD: Cell<i64> = const { Cell::new(-1) };
}

fn hwnd() -> Option<HWND> {
    let h = HWND_TRAY.with(|c| c.get());
    (h != 0).then_some(HWND(h as *mut _))
}

/// 从 .ico 里挑最接近 `want` 像素的那一张建 HICON（PNG 压缩的条目 CreateIconFromResourceEx 也认）。
pub(crate) fn load_icon(want: u32) -> Option<HICON> {
    let d = ICO;
    if d.len() < 6 {
        return None;
    }
    let n = u16::from_le_bytes([d[4], d[5]]) as usize;
    let mut best: Option<(u32, usize, usize)> = None; // (score, offset, size)
    for i in 0..n {
        let e = 6 + i * 16;
        if e + 16 > d.len() {
            break;
        }
        let w = if d[e] == 0 { 256 } else { d[e] as u32 };
        let size = u32::from_le_bytes([d[e + 8], d[e + 9], d[e + 10], d[e + 11]]) as usize;
        let off = u32::from_le_bytes([d[e + 12], d[e + 13], d[e + 14], d[e + 15]]) as usize;
        if off + size > d.len() {
            continue;
        }
        // 优先不小于目标的最小一张，其次最大的
        let score = if w >= want { w - want } else { 10_000 + (want - w) };
        if best.is_none_or(|b| score < b.0) {
            best = Some((score, off, size));
        }
    }
    let (_, off, size) = best?;
    unsafe { CreateIconFromResourceEx(&d[off..off + size], true, 0x0003_0000, want as i32, want as i32, LR_DEFAULTCOLOR).ok() }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn nid(h: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: h, uID: ICON_ID, ..Default::default() }
}

fn set_tip(n: &mut NOTIFYICONDATAW, tip: &str) {
    let t: Vec<u16> = tip.encode_utf16().take(n.szTip.len() - 1).collect();
    n.szTip = [0; 128];
    n.szTip[..t.len()].copy_from_slice(&t);
}

fn add_icon() {
    let Some(h) = hwnd() else { return };
    let mut n = nid(h);
    n.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    n.uCallbackMessage = WM_TRAY;
    n.hIcon = HICON(HICON_TRAY.with(|c| c.get()) as *mut _);
    let unread = LAST_UNREAD.with(|c| c.get()).max(0);
    set_tip(&mut n, &tip_text(unread));
    unsafe {
        if !Shell_NotifyIconW(NIM_ADD, &n).as_bool() {
            log::warn!("tray: Shell_NotifyIconW(NIM_ADD) failed");
        }
    }
}

fn tip_text(n: i64) -> String {
    if n > 0 {
        format!("Lancat — {n} 条未读")
    } else {
        "Lancat".to_string()
    }
}

pub fn create() {
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return };
        let class = w!("LancatSlintTray");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hmod.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let Ok(h) = CreateWindowExW(WS_EX_TOOLWINDOW, class, w!("Lancat Tray"), WS_POPUP, 0, 0, 0, 0, None, None, Some(hmod.into()), None) else {
            log::warn!("tray: window creation failed");
            return;
        };
        HWND_TRAY.with(|c| c.set(h.0 as isize));
        TASKBAR_CREATED.with(|c| c.set(RegisterWindowMessageW(w!("TaskbarCreated"))));
        // 托盘图标的逻辑尺寸是 SM_CXSMICON（16 @96 DPI），按系统 DPI 取
        let px = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()).max(16) as u32;
        if let Some(i) = load_icon(px) {
            HICON_TRAY.with(|c| c.set(i.0 as isize));
        }
        add_icon();
    }
}

/// 退出前去掉图标（否则托盘里留个“幽灵”图标直到鼠标划过）。
pub fn remove() {
    let Some(h) = hwnd() else { return };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid(h));
    }
}

/// 未读数变了才更新提示文字。
pub fn set_unread(n: i64) {
    if LAST_UNREAD.with(|c| c.replace(n)) == n {
        return;
    }
    let Some(h) = hwnd() else { return };
    let mut d = nid(h);
    d.uFlags = NIF_TIP;
    set_tip(&mut d, &tip_text(n));
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
    }
}

pub fn test_instance() -> bool {
    std::env::var_os("LANCAT_DATA_DIR").is_some()
}

fn reg_sz_exists(key: PCWSTR, name: PCWSTR) -> bool {
    let mut size = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, key, name, RRF_RT_REG_SZ, None, None, Some(&mut size)).is_ok() }
}

pub fn autostart_enabled() -> bool {
    if !reg_sz_exists(RUN_KEY, APP_NAME) {
        return false;
    }
    let mut buf = [0u8; 12];
    let mut size = buf.len() as u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, APPROVED_KEY, APP_NAME, RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)) };
    // 没有 StartupApproved 记录 = 启用；有记录时首字节偶数（2/6）= 启用，奇数（3）= 被任务管理器禁用
    !r.is_ok() || size == 0 || buf[0] % 2 == 0
}

pub fn set_autostart(on: bool) {
    if test_instance() {
        return;
    }
    unsafe {
        if on {
            let Ok(exe) = std::env::current_exe() else { return };
            let v = wide(&format!("\"{}\"", exe.display()));
            let r = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, APP_NAME, REG_SZ.0, Some(v.as_ptr() as *const _), (v.len() * 2) as u32);
            // 曾在任务管理器里禁用过：把 StartupApproved 的记录去掉，否则 Run 值写了也不生效
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, APPROVED_KEY, APP_NAME);
            log::info!("autostart on: {r:?}");
        } else {
            let r = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, APP_NAME);
            log::info!("autostart off: {r:?}");
        }
    }
}

fn open_receive_dir() {
    let Some(dir) = super::with(|d| d.node.receive_dir()) else { return };
    let _ = std::fs::create_dir_all(&dir);
    // 打开的是目录，绝不是收到的文件：不会执行任何东西
    let p = wide(&dir.to_string_lossy());
    unsafe {
        ShellExecuteW(None, w!("open"), windows::core::PCWSTR(p.as_ptr()), None, None, SW_SHOWNORMAL);
    }
}

fn show_menu(h: HWND) {
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        let pinned = super::pinned();
        let auto = autostart_enabled();
        let chk = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        let _ = AppendMenuW(m, MF_STRING, ID_OPEN, w!("打开 Lancat"));
        let _ = AppendMenuW(m, MF_STRING | chk(pinned), ID_PIN, w!("钉住面板（不自动收起）"));
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(m, MF_STRING, ID_REFRESH, w!("刷新设备"));
        let _ = AppendMenuW(m, MF_STRING, ID_RECV, w!("打开接收文件夹"));
        let _ = AppendMenuW(m, MF_STRING, ID_SETTINGS, w!("设置…"));
        let gray = if test_instance() { MF_GRAYED } else { MF_ENABLED };
        let _ = AppendMenuW(m, MF_STRING | chk(auto) | gray, ID_AUTOSTART, w!("开机自动启动"));
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(m, MF_STRING, ID_QUIT, w!("退出 Lancat"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // 文档要求：弹菜单前把拥有窗口设为前台，否则点菜单外面菜单不消失；之后发 WM_NULL
        let _ = SetForegroundWindow(h);
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, Some(0), h, None);
        let _ = PostMessageW(Some(h), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(m);
        run(cmd.0 as usize);
    }
}

fn run(cmd: usize) {
    match cmd {
        ID_OPEN => super::open_panel("tray"),
        ID_PIN => {
            let v = !super::pinned();
            super::set_pinned(v);
            if v {
                super::open_panel("tray");
            }
        }
        ID_REFRESH => {
            super::with(|d| d.node.refresh_peers());
        }
        ID_RECV => open_receive_dir(),
        ID_SETTINGS => super::hook(|h| (h.on_settings)()),
        ID_AUTOSTART => set_autostart(!autostart_enabled()),
        ID_QUIT => {
            remove();
            let _ = slint::quit_event_loop();
        }
        _ => {}
    }
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_TRAY {
        match lp.0 as u32 {
            WM_LBUTTONUP => {
                // 离开窗口过程再动面板（toggle 会 SetWindowPos / 改焦点）
                slint::Timer::single_shot(std::time::Duration::ZERO, super::toggle_panel);
            }
            WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(h),
            _ => {}
        }
        return LRESULT(0);
    }
    let tc = TASKBAR_CREATED.with(|c| c.get());
    if tc != 0 && msg == tc {
        add_icon();
        return LRESULT(0);
    }
    DefWindowProcW(h, msg, wp, lp)
}
