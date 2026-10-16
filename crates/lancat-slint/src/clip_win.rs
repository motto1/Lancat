//! Windows 剪贴板：Slint 的 Ctrl+C / Ctrl+V 文字（Platform::set_clipboard_text / clipboard_text），
//! 以及输入框里 Ctrl+V 粘贴文件（CF_HDROP，资源管理器里复制的）和图片（"PNG" / CF_DIB，截图、浏览器复制图片）。
//! 直接声明 user32 / kernel32 / shell32 的几个函数（与 dock/drop.rs 一样手写 FFI，不随 windows crate 版本变）。

use std::ffi::c_void;

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const CF_DIB: u32 = 8;
const GMEM_MOVEABLE: u32 = 0x0002;

#[link(name = "user32")]
extern "system" {
    fn OpenClipboard(hwnd: isize) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn GetClipboardData(format: u32) -> isize;
    fn SetClipboardData(format: u32, mem: isize) -> isize;
    fn IsClipboardFormatAvailable(format: u32) -> i32;
    fn RegisterClipboardFormatW(name: *const u16) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> isize;
    fn GlobalLock(mem: isize) -> *mut c_void;
    fn GlobalUnlock(mem: isize) -> i32;
    fn GlobalSize(mem: isize) -> usize;
    fn GlobalFree(mem: isize) -> isize;
}

#[link(name = "shell32")]
extern "system" {
    fn DragQueryFileW(hdrop: *mut c_void, file: u32, buf: *mut u16, cch: u32) -> u32;
}

/// 打开剪贴板（别的程序正占着时重试一会儿），离开作用域时关闭
struct Open;

impl Open {
    fn new(owner: isize) -> Option<Open> {
        for _ in 0..20 {
            if unsafe { OpenClipboard(owner) } != 0 {
                return Some(Open);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        log::warn!("clipboard: OpenClipboard failed");
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

fn has(format: u32) -> bool {
    format != 0 && unsafe { IsClipboardFormatAvailable(format) } != 0
}

fn png_format() -> u32 {
    let name: Vec<u16> = "PNG\0".encode_utf16().collect();
    unsafe { RegisterClipboardFormatW(name.as_ptr()) }
}

/// 剪贴板里某个格式的全部字节（须已 Open）
fn bytes(format: u32) -> Option<Vec<u8>> {
    unsafe {
        let h = GetClipboardData(format);
        if h == 0 {
            return None;
        }
        let n = GlobalSize(h);
        let p = GlobalLock(h) as *const u8;
        if p.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(p, n).to_vec();
        GlobalUnlock(h);
        Some(v)
    }
}

pub(crate) fn set_text(owner: isize, text: &str) {
    let w: Vec<u16> = text.replace("\r\n", "\n").replace('\n', "\r\n").encode_utf16().chain(std::iter::once(0)).collect();
    let Some(_c) = Open::new(owner) else { return };
    unsafe {
        if EmptyClipboard() == 0 {
            return;
        }
        let h = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2);
        if h == 0 {
            return;
        }
        let p = GlobalLock(h) as *mut u16;
        if p.is_null() {
            GlobalFree(h);
            return;
        }
        std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
        GlobalUnlock(h);
        if SetClipboardData(CF_UNICODETEXT, h) == 0 {
            GlobalFree(h);
        }
    }
}

pub(crate) fn text(owner: isize) -> Option<String> {
    if !has(CF_UNICODETEXT) {
        return None;
    }
    let _c = Open::new(owner)?;
    let b = bytes(CF_UNICODETEXT)?;
    let u: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    Some(String::from_utf16_lossy(&u).replace("\r\n", "\n"))
}

/// 剪贴板里复制的文件（须已 Open）
fn files() -> Vec<String> {
    unsafe {
        let h = GetClipboardData(CF_HDROP);
        if h == 0 {
            return Vec::new();
        }
        // HDROP 是 HGLOBAL：DragQueryFileW 自己处理锁定
        let n = DragQueryFileW(h as *mut c_void, u32::MAX, std::ptr::null_mut(), 0);
        let mut out = Vec::new();
        for i in 0..n {
            let len = DragQueryFileW(h as *mut c_void, i, std::ptr::null_mut(), 0);
            let mut buf = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(h as *mut c_void, i, buf.as_mut_ptr(), buf.len() as u32);
            buf.truncate(got as usize);
            out.push(String::from_utf16_lossy(&buf));
        }
        out
    }
}

/// CF_DIB（BITMAPINFOHEADER + 像素）→ RGBA。只认未压缩的 24 / 32 位（截图、浏览器、Office 都是这两种）
fn dib_rgba(d: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let rd_u32 = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let rd_i32 = |o: usize| rd_u32(o).map(|v| v as i32);
    let hsize = rd_u32(0)? as usize;
    let w = rd_i32(4)?;
    let h = rd_i32(8)?;
    let bpp = u16::from_le_bytes([*d.get(14)?, *d.get(15)?]);
    let comp = rd_u32(16)?;
    let used = rd_u32(32)? as usize;
    if w <= 0 || h == 0 || !(bpp == 24 || bpp == 32) || !(comp == 0 || comp == 3) {
        return None;
    }
    // BI_BITFIELDS 且是 40 字节的头：三个掩码跟在头后面；24/32 位一般没有调色板，但 biClrUsed 可能不为 0
    let masks = if comp == 3 && hsize == 40 { 12 } else { 0 };
    let off = hsize + masks + used * 4;
    let (w, top_down, h) = (w as usize, h < 0, h.unsigned_abs() as usize);
    let bypp = bpp as usize / 8;
    let stride = (w * bypp + 3) & !3;
    if d.len() < off + stride * h {
        return None;
    }
    let mut rgba = vec![0u8; w * h * 4];
    let mut any_alpha = false;
    for y in 0..h {
        let sy = if top_down { y } else { h - 1 - y };
        let row = &d[off + sy * stride..];
        for x in 0..w {
            let s = &row[x * bypp..];
            let o = &mut rgba[(y * w + x) * 4..];
            o[0] = s[2];
            o[1] = s[1];
            o[2] = s[0];
            o[3] = if bypp == 4 { s[3] } else { 255 };
            any_alpha |= bypp == 4 && s[3] != 0;
        }
    }
    // 32 位但 alpha 全 0（很多程序不填 alpha）：当作不透明
    if bypp == 4 && !any_alpha {
        for p in rgba.chunks_exact_mut(4) {
            p[3] = 255;
        }
    }
    Some((w as u32, h as u32, rgba))
}

/// Ctrl+V 在输入框里：剪贴板是文件 → 发文件；没有文字只有图片 → 存 PNG 发送。返回 true = 已处理（不再交给 Slint 粘贴文字）
pub(crate) fn paste_special(owner: isize) -> bool {
    let png = png_format();
    let (f, t, p, dib) = (has(CF_HDROP), has(CF_UNICODETEXT), has(png), has(CF_DIB));
    log::debug!("paste: clipboard files={f} text={t} png={p} dib={dib}");
    if !f && (t || !(p || dib)) {
        return false;
    }
    let Some(c) = Open::new(owner) else { return false };
    if f {
        let list: Vec<String> = files().into_iter().filter(|p| std::path::Path::new(p).exists()).collect();
        drop(c);
        if list.is_empty() {
            return false;
        }
        crate::paste::send_files(list);
        return true;
    }
    // Office 复制文字时也带位图：有文字就粘贴文字（上面已返回）
    let encoded = if p { bytes(png).filter(|b| b.starts_with(b"\x89PNG")) } else { None };
    let encoded = encoded.or_else(|| {
        let d = bytes(CF_DIB)?;
        let (w, h, rgba) = dib_rgba(&d)?;
        crate::paste::encode_png(w, h, &rgba)
    });
    drop(c);
    match encoded {
        Some(b) => {
            if let Some(path) = crate::paste::save_image(&b, "png") {
                crate::paste::send_files(vec![path]);
            }
        }
        None => log::warn!("paste: unsupported clipboard image"),
    }
    true
}
