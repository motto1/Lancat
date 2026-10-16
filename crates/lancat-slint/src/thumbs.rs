//! 会话里的图片：收发完成的图片文件直接显示成缩略图（点击用系统看图打开）。
//!
//! 后台一个线程解码（PNG / JPEG / GIF 首帧 / WebP / BMP，JPEG 按 EXIF 方向转正），按显示尺寸 × 缩放
//! 缩成正好 1:1 的像素并裁好圆角（软件渲染器的 clip 不带圆角），回到界面线程做成 slint::Image 缓存起来，
//! 刷新用到这个文件的消息行。解不开、太大、文件没了：照旧显示成文件卡片。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc;

/// 显示尺寸上限（逻辑 px）
pub const MAX_W: f32 = 220.0;
pub const MAX_H: f32 = 240.0;
const MIN_SIDE: f32 = 36.0;
/// 圆角（逻辑 px）
const RADIUS: f32 = 12.0;
/// 超过这么大的文件不解码（避免收起面板前卡住解码线程很久）
const MAX_FILE: u64 = 60 << 20;
/// 解码后像素上限（宽 × 高）
const MAX_PIXELS: u64 = 50_000_000;
const MAX_CACHE: usize = 96;

pub fn is_image_name(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    name.contains('.') && matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
}

#[derive(Clone)]
enum Entry {
    Pending,
    Ready(slint::Image, f32, f32),
    Failed,
}

struct Job {
    path: String,
    scale: f32,
}

struct Done {
    path: String,
    scale: f32,
    /// (rgba 像素缓冲, 逻辑宽, 逻辑高)
    out: Option<(slint::SharedPixelBuffer<slint::Rgba8Pixel>, f32, f32)>,
}

thread_local! {
    static CACHE: RefCell<HashMap<(String, u32), Entry>> = RefCell::new(HashMap::new());
    static TX: RefCell<Option<mpsc::Sender<Job>>> = const { RefCell::new(None) };
    /// 解码时让程序崩掉过的文件（release 是 panic = abort，catch_unwind 接不住）：不再解，免得每次打开会话都崩
    static BAD: RefCell<Option<std::collections::HashSet<String>>> = const { RefCell::new(None) };
}

/// 正在解码的文件写在这里，解完删掉；启动后还在 = 上次解它时崩了
fn marker() -> std::path::PathBuf {
    crate::data_dir().join("thumb-decoding")
}

fn bad_list() -> std::path::PathBuf {
    crate::data_dir().join("thumb-bad.txt")
}

fn is_bad(path: &str) -> bool {
    BAD.with(|b| {
        let mut b = b.borrow_mut();
        let set = b.get_or_insert_with(|| {
            let mut set: std::collections::HashSet<String> = std::fs::read_to_string(bad_list()).unwrap_or_default().lines().filter(|l| !l.is_empty()).map(String::from).collect();
            if let Ok(p) = std::fs::read_to_string(marker()) {
                let p = p.trim().to_string();
                if !p.is_empty() && set.insert(p.clone()) {
                    log::warn!("thumbs: {p} crashed the decoder last time; showing it as a file");
                    let mut all: Vec<&String> = set.iter().collect();
                    all.sort();
                    let _ = std::fs::write(bad_list(), all.iter().map(|s| format!("{s}\n")).collect::<String>());
                }
                let _ = std::fs::remove_file(marker());
            }
            set
        });
        set.contains(path)
    })
}

fn key(path: &str, scale: f32) -> (String, u32) {
    (path.to_string(), (scale * 100.0).round() as u32)
}

/// 缩略图：有就返回（图, 逻辑宽, 逻辑高）；没有就排队解码，好了以后刷新这些消息行，这次先显示文件卡片。
pub fn get(path: &str, scale: f32) -> Option<(slint::Image, f32, f32)> {
    let k = key(path, scale);
    let e = CACHE.with(|c| c.borrow().get(&k).cloned());
    match e {
        Some(Entry::Ready(img, w, h)) => Some((img, w, h)),
        Some(Entry::Pending) | Some(Entry::Failed) => None,
        None => {
            if !std::path::Path::new(path).is_file() || is_bad(path) {
                return None;
            }
            CACHE.with(|c| {
                let mut c = c.borrow_mut();
                if c.len() >= MAX_CACHE {
                    c.retain(|_, v| matches!(v, Entry::Pending));
                }
                c.insert(k, Entry::Pending);
            });
            send(Job { path: path.to_string(), scale });
            None
        }
    }
}

/// 收起面板一段时间后释放
pub fn release() {
    CACHE.with(|c| c.borrow_mut().retain(|_, v| matches!(v, Entry::Pending)));
}

fn send(job: Job) {
    TX.with(|t| {
        let mut t = t.borrow_mut();
        if t.is_none() {
            let (tx, rx) = mpsc::channel::<Job>();
            let ok = std::thread::Builder::new().name("thumbs".into()).spawn(move || worker(rx)).is_ok();
            if !ok {
                return;
            }
            *t = Some(tx);
        }
        if let Some(tx) = t.as_ref() {
            let _ = tx.send(job);
        }
    });
}

fn worker(rx: mpsc::Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        let _ = std::fs::write(marker(), &job.path);
        let r = std::panic::catch_unwind(|| decode(&job.path, job.scale));
        let _ = std::fs::remove_file(marker());
        let out = match r {
            Ok(Ok(o)) => Some(o),
            Ok(Err(e)) => {
                log::info!("thumbs: {}: {e}", job.path);
                None
            }
            Err(_) => {
                log::warn!("thumbs: decoder panicked on {}", job.path);
                None
            }
        };
        let done = Done { path: job.path, scale: job.scale, out };
        let _ = slint::invoke_from_event_loop(move || finish(done));
    }
}

fn finish(d: Done) {
    let k = key(&d.path, d.scale);
    let e = match d.out {
        Some((buf, w, h)) => Entry::Ready(slint::Image::from_rgba8(buf), w, h),
        None => Entry::Failed,
    };
    let ok = matches!(e, Entry::Ready(..));
    CACHE.with(|c| c.borrow_mut().insert(k, e));
    if ok {
        let p = d.path;
        crate::with_app(move |app| app.refresh_image(&p));
    }
}

/// 显示尺寸（逻辑 px）：原图像素当 CSS px，等比缩进 MAX_W × MAX_H，太细的边至少 MIN_SIDE
pub fn display_size(w: u32, h: u32) -> (f32, f32) {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let f = (MAX_W / w).min(MAX_H / h).min(1.0);
    ((w * f).max(MIN_SIDE).min(MAX_W).round(), (h * f).max(MIN_SIDE).min(MAX_H).round())
}

fn decode(path: &str, scale: f32) -> Result<(slint::SharedPixelBuffer<slint::Rgba8Pixel>, f32, f32), String> {
    use image::{DynamicImage, ImageDecoder, ImageReader};
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_FILE {
        return Err(format!("too large ({len} bytes)"));
    }
    let reader = ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    let mut dec = reader.into_decoder().map_err(|e| e.to_string())?;
    let (w0, h0) = dec.dimensions();
    if w0 == 0 || h0 == 0 || w0 as u64 * h0 as u64 > MAX_PIXELS {
        return Err(format!("unsupported size {w0}x{h0}"));
    }
    let ori = dec.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    img.apply_orientation(ori);
    let (lw, lh) = display_size(img.width(), img.height());
    let (pw, ph) = (((lw * scale).round() as u32).max(1), ((lh * scale).round() as u32).max(1));
    // 太细的边被撑到 MIN_SIDE：等比铺满再居中裁（同 object-fit: cover）
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let cover = (pw as f32 / iw).max(ph as f32 / ih);
    let (cw, ch) = (((iw * cover).round() as u32).max(pw), ((ih * cover).round() as u32).max(ph));
    let mut t = if cw == img.width() && ch == img.height() { img.to_rgba8() } else { img.resize_exact(cw, ch, image::imageops::FilterType::Triangle).to_rgba8() };
    if cw != pw || ch != ph {
        t = image::imageops::crop_imm(&t, (cw - pw) / 2, (ch - ph) / 2, pw, ph).to_image();
    }
    round_corners(&mut t, RADIUS * scale);
    let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(t.as_raw(), pw, ph);
    Ok((buf, lw, lh))
}

/// 四个角按半径 r 做抗锯齿圆角（改 alpha）
fn round_corners(img: &mut image::RgbaImage, r: f32) {
    let (w, h) = (img.width(), img.height());
    let r = r.min(w as f32 / 2.0).min(h as f32 / 2.0);
    if r < 0.5 {
        return;
    }
    let n = r.ceil() as u32;
    for y in 0..n.min(h) {
        for x in 0..n.min(w) {
            // 像素中心到圆心的距离 → 覆盖率
            let dx = r - (x as f32 + 0.5);
            let dy = r - (y as f32 + 0.5);
            let d = (dx * dx + dy * dy).sqrt();
            let cov = (r - d + 0.5).clamp(0.0, 1.0);
            if cov >= 1.0 {
                continue;
            }
            for (px, py) in [(x, y), (w - 1 - x, y), (x, h - 1 - y), (w - 1 - x, h - 1 - y)] {
                let p = img.get_pixel_mut(px, py);
                p[3] = (p[3] as f32 * cov).round() as u8;
            }
        }
    }
}
