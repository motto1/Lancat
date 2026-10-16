//! 粘贴文件 / 图片到输入框（Ctrl+V）：剪贴板里是文件（文件管理器里复制的）→ 把这些文件发给当前会话；
//! 只有图片（截图、浏览器「复制图片」）→ 存成 PNG 再发送；有文字 → 交给输入框照常粘贴文字。
//! 读剪贴板是平台的事（src/wl.rs、src/win.rs），这里只做「发出去」和临时文件。

use std::path::PathBuf;

/// 输入框有焦点（且没有添加设备之类的对话框）：只有这时 Ctrl+V 的文件 / 图片才当作发送
pub(crate) fn composer_focused() -> bool {
    crate::with_app(|app| {
        if app.dialog_open {
            return false;
        }
        app.ui().is_some_and(|u| u.get_composer_focused() && !u.get_add_open())
    })
    .unwrap_or(false)
}

/// 粘贴的文件：与拖到停靠条上松手一样，发给当前会话
pub(crate) fn send_files(files: Vec<String>) {
    if files.is_empty() {
        return;
    }
    log::info!("paste: {} file(s)", files.len());
    crate::with_app(move |app| app.paste_files(files));
}

/// 粘贴的图片放在数据目录（不放临时目录）：会话里要一直显示这张图，断点续传也要用到原文件
fn dir() -> PathBuf {
    crate::data_dir().join("pasted")
}

/// 图片字节（已是 PNG / JPEG 等编码）存成文件，返回路径。文件名同微信 / QQ 风格带时间，
/// 对方收到的就是这个名字。
pub(crate) fn save_image(bytes: &[u8], ext: &str) -> Option<String> {
    let d = dir();
    std::fs::create_dir_all(&d).ok()?;
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let mut p = d.join(format!("图片_{stamp}.{ext}"));
    let mut n = 2;
    while p.exists() {
        p = d.join(format!("图片_{stamp}_{n}.{ext}"));
        n += 1;
    }
    std::fs::write(&p, bytes).map_err(|e| log::warn!("paste: write image: {e}")).ok()?;
    Some(p.to_string_lossy().into_owned())
}

/// MIME → 扩展名（只认常见的位图；Wayland 用）
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn image_ext(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" | "image/x-bmp" => "bmp",
        "image/avif" => "avif",
        _ => return None,
    })
}

/// RGBA（不预乘）→ PNG
#[allow(dead_code)]
pub(crate) fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().ok()?;
        wr.write_image_data(rgba).ok()?;
    }
    Some(out)
}
