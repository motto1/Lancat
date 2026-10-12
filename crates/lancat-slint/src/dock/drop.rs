//! OLE 拖放目标（移植自 src-tauri/src/native_dock/win_drop.rs 的 IDropTarget 手写 vtable）。
//! Slint 版没有 WebView，面板窗口自己就是 drop target，不再需要旧版的透明覆盖窗口：
//! - 停靠条：拖着文件碰到停靠条 → 打开面板；松手 → 发给当前会话（dock::on_drop）。
//! - 面板：拖入/移动/离开/松手 → dock::on_drag(...)，由界面显示提示并在松手时发送。
//! 回调都在 UI 线程（OLE STA），但处在一次进入的 COM 调用里：碰界面前先用 0ms 定时器出栈。
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::HWND;

type HRESULT = i32;
const S_OK: HRESULT = 0;
const E_NOINTERFACE: HRESULT = 0x8000_4002_u32 as i32;
const DROPEFFECT_NONE: u32 = 0;
const DROPEFFECT_COPY: u32 = 1;
const CF_HDROP: u16 = 15;
const DVASPECT_CONTENT: u32 = 1;
const TYMED_HGLOBAL: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid(u32, u16, u16, [u8; 8]);
const IID_IUNKNOWN: Guid = Guid(0, 0, 0, [0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_IDROPTARGET: Guid = Guid(0x0000_0122, 0, 0, [0xC0, 0, 0, 0, 0, 0, 0, 0x46]);

fn same(a: &Guid, b: &Guid) -> bool {
    a.0 == b.0 && a.1 == b.1 && a.2 == b.2 && a.3 == b.3
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PointL {
    x: i32,
    y: i32,
}

#[repr(C)]
struct FormatEtc {
    cf_format: u16,
    ptd: *mut c_void,
    aspect: u32,
    lindex: i32,
    tymed: u32,
}

#[repr(C)]
struct StgMedium {
    tymed: u32,
    data: *mut c_void,
    unk_for_release: *mut c_void,
}

/// IDataObject vtable prefix up to GetData.
#[repr(C)]
struct DataObjectVtbl {
    _qi: usize,
    _add_ref: usize,
    _release: usize,
    get_data: unsafe extern "system" fn(*mut c_void, *const FormatEtc, *mut StgMedium) -> HRESULT,
}

#[repr(C)]
struct Vtbl {
    qi: unsafe extern "system" fn(*mut Target, *const Guid, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut Target) -> u32,
    release: unsafe extern "system" fn(*mut Target) -> u32,
    drag_enter: unsafe extern "system" fn(*mut Target, *mut c_void, u32, PointL, *mut u32) -> HRESULT,
    drag_over: unsafe extern "system" fn(*mut Target, u32, PointL, *mut u32) -> HRESULT,
    drag_leave: unsafe extern "system" fn(*mut Target) -> HRESULT,
    drop: unsafe extern "system" fn(*mut Target, *mut c_void, u32, PointL, *mut u32) -> HRESULT,
}

#[repr(C)]
struct Target {
    vtbl: *const Vtbl,
    refs: AtomicU32,
    /// the current drag carries files
    files: AtomicU32,
    /// 0: the dock strip; else the panel window (drag positions are relative to its client area)
    panel: isize,
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn OleInitialize(reserved: *mut c_void) -> HRESULT;
    fn RegisterDragDrop(hwnd: *mut c_void, target: *mut c_void) -> HRESULT;
    fn ReleaseStgMedium(medium: *mut StgMedium);
}

#[link(name = "user32")]
unsafe extern "system" {
    fn ScreenToClient(hwnd: isize, pt: *mut PointL) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn DragQueryFileW(hdrop: *mut c_void, file: u32, buf: *mut u16, cch: u32) -> u32;
}

static VTBL: Vtbl = Vtbl { qi, add_ref, release, drag_enter, drag_over, drag_leave, drop };

unsafe extern "system" fn qi(this: *mut Target, iid: *const Guid, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() || iid.is_null() {
        return E_NOINTERFACE;
    }
    if same(&*iid, &IID_IUNKNOWN) || same(&*iid, &IID_IDROPTARGET) {
        add_ref(this);
        *out = this as *mut c_void;
        S_OK
    } else {
        *out = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn add_ref(this: *mut Target) -> u32 {
    (*this).refs.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn release(this: *mut Target) -> u32 {
    // static object: never freed
    (*this).refs.fetch_sub(1, Ordering::SeqCst).saturating_sub(1).max(1)
}

/// Paths of a CF_HDROP data object (empty when it carries no files).
unsafe fn paths(data: *mut c_void) -> Vec<String> {
    if data.is_null() {
        return vec![];
    }
    let vtbl = *(data as *const *const DataObjectVtbl);
    let fmt = FormatEtc { cf_format: CF_HDROP, ptd: std::ptr::null_mut(), aspect: DVASPECT_CONTENT, lindex: -1, tymed: TYMED_HGLOBAL };
    let mut med = StgMedium { tymed: 0, data: std::ptr::null_mut(), unk_for_release: std::ptr::null_mut() };
    if ((*vtbl).get_data)(data, &fmt, &mut med) != S_OK {
        return vec![];
    }
    let mut out = vec![];
    if med.tymed == TYMED_HGLOBAL && !med.data.is_null() {
        let n = DragQueryFileW(med.data, u32::MAX, std::ptr::null_mut(), 0);
        for i in 0..n {
            let len = DragQueryFileW(med.data, i, std::ptr::null_mut(), 0);
            let mut buf = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(med.data, i, buf.as_mut_ptr(), buf.len() as u32);
            out.push(String::from_utf16_lossy(&buf[..got as usize]));
        }
    }
    ReleaseStgMedium(&mut med);
    out
}

unsafe fn client(this: *mut Target, pt: PointL) -> (i32, i32) {
    let mut p = pt;
    if (*this).panel != 0 {
        ScreenToClient((*this).panel, &mut p);
    }
    (p.x, p.y)
}

fn later(f: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::ZERO, f);
}

unsafe extern "system" fn drag_enter(this: *mut Target, data: *mut c_void, _keys: u32, pt: PointL, effect: *mut u32) -> HRESULT {
    let files = paths(data);
    let has = !files.is_empty();
    let panel = (*this).panel != 0;
    log::debug!("{}: drag enter (files: {has})", if panel { "panel" } else { "dock strip" });
    (*this).files.store(has as u32, Ordering::SeqCst);
    if has {
        let (x, y) = client(this, pt);
        later(move || super::on_drag(super::DragEvent::Enter { panel, n: files.len(), x, y }));
    }
    if !effect.is_null() {
        *effect = if has { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
    }
    S_OK
}

unsafe extern "system" fn drag_over(this: *mut Target, _keys: u32, pt: PointL, effect: *mut u32) -> HRESULT {
    let has = (*this).files.load(Ordering::SeqCst) != 0;
    if has && (*this).panel != 0 {
        let (x, y) = client(this, pt);
        later(move || super::on_drag(super::DragEvent::Over { x, y }));
    }
    if !effect.is_null() {
        *effect = if has { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
    }
    S_OK
}

unsafe extern "system" fn drag_leave(this: *mut Target) -> HRESULT {
    if (*this).files.swap(0, Ordering::SeqCst) != 0 {
        let panel = (*this).panel != 0;
        later(move || super::on_drag(super::DragEvent::Leave { panel }));
    }
    S_OK
}

unsafe extern "system" fn drop(this: *mut Target, data: *mut c_void, _keys: u32, pt: PointL, effect: *mut u32) -> HRESULT {
    (*this).files.store(0, Ordering::SeqCst);
    let files = paths(data);
    if !effect.is_null() {
        *effect = if files.is_empty() { DROPEFFECT_NONE } else { DROPEFFECT_COPY };
    }
    let panel = (*this).panel != 0;
    let (x, y) = client(this, pt);
    log::info!("{} file(s) dropped on the {}", files.len(), if panel { "panel" } else { "dock strip" });
    later(move || super::on_drag(super::DragEvent::Drop { panel, files, x, y }));
    S_OK
}

/// 注册为拖放目标（UI 线程；先确保 OLE 已初始化）。`panel`：面板窗口（坐标换算到它的客户区）。
pub fn register(hwnd: HWND) {
    register_as(hwnd, false)
}

pub fn register_panel(hwnd: HWND) {
    register_as(hwnd, true)
}

fn register_as(hwnd: HWND, panel: bool) {
    unsafe {
        let _ = OleInitialize(std::ptr::null_mut()); // S_FALSE when already initialised
        let t: &'static mut Target = Box::leak(Box::new(Target { vtbl: &VTBL, refs: AtomicU32::new(1), files: AtomicU32::new(0), panel: if panel { hwnd.0 as isize } else { 0 } }));
        let hr = RegisterDragDrop(hwnd.0, t as *mut Target as *mut c_void);
        if hr != S_OK {
            log::warn!("RegisterDragDrop failed: {hr:#x}");
        }
    }
}
