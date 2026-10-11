//! OLE drop target for the Windows dock strip.
//!
//! Without one, Windows shows the "no entry" cursor while a file is dragged over the strip, and a
//! file released there is lost. The strip now accepts file drags (copy cursor); the cursor poller
//! in dock.rs still opens the panel (it already watches cursor + left button), and a file
//! released on the strip itself is handed to the panel (`panel://strip-drop`), which sends it to
//! the current conversation.
//!
//! The panel cannot take OLE drops itself: the window under the cursor is WebView2's
//! `Chrome_RenderWidgetHostHWND`, owned by the msedgewebview2 browser process, and depending on
//! the moment (cold page, parked, GPU on/off) the drag either reaches wry's target or a WebView2
//! target that refuses it ("no entry" cursor, file lost) — registering our own target on the
//! panel's windows in this process did not help, OLE never got past the browser-process windows.
//! So while an outside drag is over the panel, an invisible (alpha 1) native window of ours covers
//! it (`overlay_show` / `overlay_hide`, driven by the cursor poller in dock.rs) and takes the
//! drop; it forwards the drag as the usual `tauri://drag-*` events, so the page's
//! `onDragDropEvent` code (avatar highlight, hit test, send) is unchanged.
//!
//! Hand-written COM object (one static instance, never freed): the `windows` crate's `implement`
//! macro would need a matching `windows-core`, and this crate links two versions of it.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use tauri::{AppHandle, Emitter, EventTarget};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetWindowRect, RegisterClassW, SetLayeredWindowAttributes, SetWindowPos, ShowWindow, HWND_TOPMOST, LWA_ALPHA, MA_NOACTIVATE, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SW_HIDE, WM_MOUSEACTIVATE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

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
    /// 0: the dock strip; else the panel window drag positions are relative to
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
static mut APP: Option<AppHandle> = None;

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

unsafe extern "system" fn drag_enter(this: *mut Target, data: *mut c_void, _keys: u32, _pt: PointL, effect: *mut u32) -> HRESULT {
    let files = paths(data);
    let has = !files.is_empty();
    if (*this).panel == 0 {
        log::debug!("dock strip: drag enter (files: {has})");
    } else {
        log::debug!("panel: drag enter (files: {has})");
    }
    if (*this).panel != 0 && has {
        panel_event(this, "tauri://drag-enter", Some(files.clone()), _pt);
    }
    (*this).files.store(has as u32, Ordering::SeqCst);
    if !effect.is_null() {
        *effect = if has { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
    }
    S_OK
}

unsafe extern "system" fn drag_over(this: *mut Target, _keys: u32, _pt: PointL, effect: *mut u32) -> HRESULT {
    if (*this).panel != 0 && (*this).files.load(Ordering::SeqCst) != 0 {
        panel_event(this, "tauri://drag-over", None, _pt);
    }
    if !effect.is_null() {
        *effect = if (*this).files.load(Ordering::SeqCst) != 0 { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
    }
    S_OK
}

unsafe extern "system" fn drag_leave(this: *mut Target) -> HRESULT {
    if (*this).panel != 0 && (*this).files.load(Ordering::SeqCst) != 0 {
        #[allow(static_mut_refs)]
        if let Some(app) = APP.as_ref() {
            let _ = app.emit_to(EventTarget::labeled("panel"), "tauri://drag-leave", ());
        }
    }
    (*this).files.store(0, Ordering::SeqCst);
    S_OK
}

unsafe extern "system" fn drop(this: *mut Target, data: *mut c_void, _keys: u32, _pt: PointL, effect: *mut u32) -> HRESULT {
    (*this).files.store(0, Ordering::SeqCst);
    let files = paths(data);
    if !effect.is_null() {
        *effect = if files.is_empty() { DROPEFFECT_NONE } else { DROPEFFECT_COPY };
    }
    if (*this).panel != 0 {
        log::debug!("panel: drop ({} files)", files.len());
        if !files.is_empty() {
            panel_event(this, "tauri://drag-drop", Some(files), _pt);
        }
        overlay_hide();
        return S_OK;
    }
    log::debug!("dock strip: drop ({} files)", files.len());
    #[allow(static_mut_refs)]
    if let (false, Some(app)) = (files.is_empty(), APP.clone()) {
        log::info!("{} file(s) dropped on the dock strip", files.len());
        // this runs inside an incoming COM call: leave it before touching the panel
        crate::timers::after(std::time::Duration::ZERO, move || {
            let a = app.clone();
            let _ = app.run_on_main_thread(move || {
                crate::dock::open_panel(&a, "drag");
                let _ = a.emit_to(EventTarget::webview_window("panel"), "panel://strip-drop", files);
            });
        });
    }
    S_OK
}

/// Sends a drag event to the panel page in tauri's own format (position: physical pixels in the
/// webview, which fills the panel window's client area).
unsafe fn panel_event(this: *mut Target, event: &str, paths: Option<Vec<String>>, pt: PointL) {
    #[derive(serde::Serialize, Clone)]
    struct Pos {
        x: f64,
        y: f64,
    }
    #[derive(serde::Serialize, Clone)]
    struct Payload {
        #[serde(skip_serializing_if = "Option::is_none")]
        paths: Option<Vec<String>>,
        position: Pos,
    }
    let mut p = pt;
    ScreenToClient((*this).panel, &mut p);
    #[allow(static_mut_refs)]
    if let Some(app) = APP.as_ref() {
        let _ = app.emit_to(EventTarget::labeled("panel"), event, Payload { paths, position: Pos { x: p.x as f64, y: p.y as f64 } });
    }
}

static OVERLAY: AtomicIsize = AtomicIsize::new(0);
static OVERLAY_TARGET: AtomicIsize = AtomicIsize::new(0);
static OVERLAY_SHOWN: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize);
    }
    DefWindowProcW(hwnd, msg, w, l)
}

/// Covers the panel window (`panel`, whose client area the drag positions are relative to) with
/// the invisible drop window. Main thread.
pub fn overlay_show(panel: HWND) {
    unsafe {
        let mut h = OVERLAY.load(Ordering::SeqCst);
        if h == 0 {
            let Ok(hmod) = GetModuleHandleW(None) else { return };
            let class = w!("LancatDropOverlay");
            let wc = WNDCLASSW { lpfnWndProc: Some(overlay_proc), hInstance: hmod.into(), lpszClassName: class, ..Default::default() };
            RegisterClassW(&wc);
            let Ok(hw) = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                class,
                w!("Lancat Drop"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(hmod.into()),
                None,
            ) else {
                log::warn!("drop overlay: CreateWindowExW failed");
                return;
            };
            // alpha 1: invisible, but still hit-tested (alpha 0 would be click-through)
            let _ = SetLayeredWindowAttributes(hw, COLORREF(0), 1, LWA_ALPHA);
            let t: &'static mut Target = Box::leak(Box::new(Target { vtbl: &VTBL, refs: AtomicU32::new(1), files: AtomicU32::new(0), panel: 0 }));
            let hr = RegisterDragDrop(hw.0, t as *mut Target as *mut c_void);
            if hr != S_OK {
                log::warn!("drop overlay: RegisterDragDrop failed: {hr:#x}");
            }
            OVERLAY_TARGET.store(t as *mut Target as isize, Ordering::SeqCst);
            h = hw.0 as isize;
            OVERLAY.store(h, Ordering::SeqCst);
        }
        let t = OVERLAY_TARGET.load(Ordering::SeqCst) as *mut Target;
        if !t.is_null() {
            (*t).panel = panel.0 as isize;
        }
        let mut r = RECT::default();
        if GetWindowRect(panel, &mut r).is_err() {
            return;
        }
        let _ = SetWindowPos(HWND(h as _), Some(HWND_TOPMOST), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        if !OVERLAY_SHOWN.swap(true, Ordering::SeqCst) {
            log::debug!("drop overlay shown");
        }
    }
}

/// Main thread.
pub fn overlay_hide() {
    let h = OVERLAY.load(Ordering::SeqCst);
    if h != 0 && OVERLAY_SHOWN.swap(false, Ordering::SeqCst) {
        unsafe {
            let _ = ShowWindow(HWND(h as _), SW_HIDE);
        }
        log::debug!("drop overlay hidden");
    }
}

pub fn overlay_shown() -> bool {
    OVERLAY_SHOWN.load(Ordering::SeqCst)
}

/// Registers the strip as a drop target. Main thread, once.
pub fn register(app: &AppHandle, hwnd: HWND) {
    unsafe {
        APP = Some(app.clone());
        let _ = OleInitialize(std::ptr::null_mut()); // tao already did; S_FALSE then
        let t: &'static mut Target = Box::leak(Box::new(Target { vtbl: &VTBL, refs: AtomicU32::new(1), files: AtomicU32::new(0), panel: 0 }));
        let hr = RegisterDragDrop(hwnd.0, t as *mut Target as *mut c_void);
        if hr != S_OK {
            log::warn!("native dock: RegisterDragDrop failed: {hr:#x}");
        } else {
            log::debug!("native dock: registered as drop target");
        }
    }
}
