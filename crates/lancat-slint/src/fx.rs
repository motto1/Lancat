//! 软件渲染器做不到的视觉效果，按旧版 CSS 的参数在 Rust 里画好交给 Slint（ui/common.slint 的 `Fx` 全局）：
//! - `shadow`：box-shadow 图。外阴影 = 形状（偏移 + 扩散）高斯模糊（σ = blur/2，三次盒式模糊）后**挖掉元素本身**
//!   （CSS 的外阴影不画在元素下面）；inset = 元素内部减去偏移/收缩后的形状再模糊。按尺寸/颜色/主题/缩放缓存。
//! - `rotate-path`：24×24 图标路径绕 (12,12) 旋转（刷新转圈、诊断展开箭头），转成绝对坐标再旋转。
//! - `dash-rect`：CSS `2px dashed` 圆角矩形的虚线段路径。
//! - `paint`：径向/线性渐变一类的填充（Windows 玻璃的 sheen + rim、拖放选择器的头像高光）。
//! - `ease`：命名的 cubic-bezier 曲线 y(x)。

use slint::{Color, Image, Rgba8Pixel, SharedPixelBuffer};
use std::cell::RefCell;
use std::collections::HashMap;

// ------------------------------------------------------------------ cubic-bezier

/// CSS cubic-bezier(x1,y1,x2,y2) 的 y(x)（牛顿迭代 + 二分兜底，与浏览器同法）
pub fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let cx = 3.0 * x1;
    let bx = 3.0 * (x2 - x1) - cx;
    let ax = 1.0 - cx - bx;
    let cy = 3.0 * y1;
    let by = 3.0 * (y2 - y1) - cy;
    let ay = 1.0 - cy - by;
    let sx = |t: f32| ((ax * t + bx) * t + cx) * t;
    let dsx = |t: f32| (3.0 * ax * t + 2.0 * bx) * t + cx;
    let mut t = x;
    for _ in 0..8 {
        let e = sx(t) - x;
        if e.abs() < 1e-5 {
            return ((ay * t + by) * t + cy) * t;
        }
        let d = dsx(t);
        if d.abs() < 1e-6 {
            break;
        }
        t -= e / d;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    t = x;
    for _ in 0..30 {
        let v = sx(t);
        if (v - x).abs() < 1e-5 {
            break;
        }
        if v < x {
            lo = t;
        } else {
            hi = t;
        }
        t = (lo + hi) / 2.0;
    }
    ((ay * t + by) * t + cy) * t
}

pub fn ease(name: &str, x: f32) -> f32 {
    match name {
        "ping" => bezier(0.2, 0.6, 0.4, 1.0, x),
        "ease" => bezier(0.2, 0.8, 0.3, 1.0, x),
        "ease-spring" => bezier(0.2, 0.9, 0.3, 1.4, x),
        "css-ease" => bezier(0.25, 0.1, 0.25, 1.0, x),
        "ease-in-out" => bezier(0.42, 0.0, 0.58, 1.0, x),
        _ => x,
    }
}

// ------------------------------------------------------------------ geometry helpers

/// 圆角矩形的有符号距离（物理 px）
fn rrect_sd(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;
    let qx = (px - cx).abs() - (w / 2.0 - r);
    let qy = (py - cy).abs() - (h / 2.0 - r);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

/// 像素中心的覆盖率（抗锯齿）
fn cov(sd: f32) -> f32 {
    (0.5 - sd).clamp(0.0, 1.0)
}

fn mask_rrect(buf: &mut [f32], bw: usize, bh: usize, x: f32, y: f32, w: f32, h: f32, r: f32) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    for j in 0..bh {
        let py = j as f32 + 0.5;
        for i in 0..bw {
            let px = i as f32 + 0.5;
            buf[j * bw + i] = cov(rrect_sd(px, py, x, y, w, h, r));
        }
    }
}

/// 三次盒式模糊近似高斯（σ 物理 px），原地
pub fn blur_f32(buf: &mut [f32], w: usize, h: usize, sigma: f32) {
    if sigma < 0.3 || w == 0 || h == 0 {
        return;
    }
    let boxes = boxes_for_gauss(sigma, 3);
    let mut tmp = vec![0f32; buf.len()];
    for b in boxes {
        let r = (b - 1) / 2;
        box_h(buf, &mut tmp, w, h, r);
        box_v(&tmp, buf, w, h, r);
    }
}

pub fn boxes_for_gauss(sigma: f32, n: usize) -> Vec<usize> {
    let w_ideal = ((12.0 * sigma * sigma / n as f32) + 1.0).sqrt();
    let mut wl = w_ideal.floor() as i64;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m_ideal = (12.0 * sigma * sigma - (n as f32) * (wl * wl) as f32 - 4.0 * n as f32 * wl as f32 - 3.0 * n as f32) / (-4.0 * wl as f32 - 4.0);
    let m = m_ideal.round() as i64;
    (0..n as i64).map(|i| if i < m { wl.max(1) as usize } else { wu.max(1) as usize }).collect()
}

fn box_h(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    let iarr = 1.0 / (2 * r + 1) as f32;
    for j in 0..h {
        let row = &src[j * w..(j + 1) * w];
        let out = &mut dst[j * w..(j + 1) * w];
        // 边缘外按 0（阴影图四周留了足够的空白）
        let mut acc = 0f32;
        for k in 0..=r.min(w - 1) {
            acc += row[k];
        }
        for i in 0..w {
            out[i] = acc * iarr;
            let add = i + r + 1;
            if add < w {
                acc += row[add];
            }
            if i >= r {
                acc -= row[i - r];
            }
        }
    }
}

fn box_v(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    let iarr = 1.0 / (2 * r + 1) as f32;
    for i in 0..w {
        let mut acc = 0f32;
        for k in 0..=r.min(h - 1) {
            acc += src[k * w + i];
        }
        for j in 0..h {
            dst[j * w + i] = acc * iarr;
            let add = j + r + 1;
            if add < h {
                acc += src[add * w + i];
            }
            if j >= r {
                acc -= src[(j - r) * w + i];
            }
        }
    }
}

// ------------------------------------------------------------------ shadows

#[derive(Clone, Copy)]
struct Sh {
    inset: bool,
    dx: f32,
    dy: f32,
    blur: f32,
    spread: f32,
    rgba: [f32; 4],
}

const fn sh(inset: bool, dx: f32, dy: f32, blur: f32, spread: f32, r: f32, g: f32, b: f32, a: f32) -> Sh {
    Sh { inset, dx, dy, blur, spread, rgba: [r, g, b, a] }
}

/// 旧版 CSS 的 box-shadow（按 CSS 顺序：第一个在最上面）。`c` = 参数色（头像色 / accent）
fn layers(kind: &str, dark: bool, c: Color) -> Vec<Sh> {
    let (cr, cg, cb) = (c.red() as f32 / 255.0, c.green() as f32 / 255.0, c.blue() as f32 / 255.0);
    match kind {
        // --shadow（.glass：外壳/表情面板/菜单）
        "glass" => {
            if dark {
                vec![sh(false, 0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0, 0.14), sh(false, 0.0, 14.0, 44.0, 0.0, 0.0, 0.0, 0.0, 0.55)]
            } else {
                vec![sh(false, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.12), sh(false, 0.0, 12.0, 40.0, 0.0, 0.0, 0.0, 0.0, 0.22), sh(false, 0.0, 2.0, 8.0, 0.0, 0.0, 0.0, 0.0, 0.08)]
            }
        }
        // inset 0 1px 0 var(--glass-highlight)
        "glass-inset" => vec![sh(true, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, if dark { 0.14 } else { 0.75 })],
        "avatar" => vec![sh(false, 0.0, 1.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.15)],
        "avatar-inset" => vec![sh(true, 0.0, 0.5, 0.0, 0.0, 1.0, 1.0, 1.0, 0.4)],
        "avatar-hot" => vec![sh(false, 0.0, 6.0, 18.0, 0.0, cr, cg, cb, 0.45)],
        "core" => vec![sh(false, 0.0, 4.0, 14.0, 0.0, cr, cg, cb, 0.45)],
        "toast" => vec![sh(false, 0.0, 6.0, 18.0, 0.0, 0.0, 0.0, 0.0, 0.18)],
        "toast-inset" => vec![sh(true, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.28)],
        "hint" => vec![sh(false, 0.0, 6.0, 18.0, 0.0, 0.0, 0.0, 0.0, 0.12)],
        "hint-inset" => vec![sh(true, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.7), sh(true, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.06)],
        "pick" => vec![sh(false, 0.0, 6.0, 16.0, -4.0, cr, cg, cb, 0.45)],
        "pick-focus" => vec![sh(false, 0.0, 0.0, 22.0, 4.0, cr, cg, cb, 0.55), sh(false, 0.0, 0.0, 46.0, 14.0, cr, cg, cb, 0.22)],
        "btn" => vec![sh(false, 0.0, 0.5, 1.0, 0.0, 0.0, 0.0, 0.0, 0.08)],
        "knob" => vec![sh(false, 0.0, 1.0, 3.0, 0.0, 0.0, 0.0, 0.0, 0.3)],
        "seg-on" => {
            if dark {
                vec![sh(false, 0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0, 0.12), sh(true, 0.0, 0.5, 0.0, 0.0, 1.0, 1.0, 1.0, 0.14), sh(false, 0.0, 1.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.45)]
            } else {
                vec![sh(false, 0.0, 1.0, 3.0, 0.0, 0.0, 0.0, 0.0, 0.15), sh(false, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.05)]
            }
        }
        _ => vec![],
    }
}

pub fn margin(kind: &str) -> f32 {
    let mut m = 0f32;
    for s in layers(kind, false, Color::default()).into_iter().chain(layers(kind, true, Color::default())) {
        if !s.inset {
            m = m.max(s.dx.abs().max(s.dy.abs()) + s.blur + s.spread.max(0.0));
        }
    }
    if m > 0.0 {
        m.ceil() + 1.0
    } else {
        0.0
    }
}

struct Cache {
    map: HashMap<String, Image>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache { map: HashMap::new() });
}

fn cached(key: String, f: impl FnOnce() -> Image) -> Image {
    if let Some(i) = CACHE.with(|c| c.borrow().map.get(&key).cloned()) {
        return i;
    }
    let img = f();
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.map.len() > 256 {
            c.map.clear();
        }
        c.map.insert(key, img.clone());
    });
    img
}

/// 预乘 RGBA f32 → slint 图
fn to_image(px: &[[f32; 4]], w: usize, h: usize) -> Image {
    let mut pb = SharedPixelBuffer::<Rgba8Pixel>::new(w as u32, h as u32);
    for (d, s) in pb.make_mut_slice().iter_mut().zip(px) {
        let a = s[3].clamp(0.0, 1.0);
        *d = Rgba8Pixel {
            r: (s[0].clamp(0.0, a) * 255.0 + 0.5) as u8,
            g: (s[1].clamp(0.0, a) * 255.0 + 0.5) as u8,
            b: (s[2].clamp(0.0, a) * 255.0 + 0.5) as u8,
            a: (a * 255.0 + 0.5) as u8,
        };
    }
    Image::from_rgba8_premultiplied(pb)
}

fn over(dst: &mut [f32; 4], c: [f32; 3], a: f32) {
    // c 非预乘，a = 覆盖率 × 颜色 alpha
    let ia = 1.0 - a;
    dst[0] = c[0] * a + dst[0] * ia;
    dst[1] = c[1] * a + dst[1] * ia;
    dst[2] = c[2] * a + dst[2] * ia;
    dst[3] = a + dst[3] * ia;
}

pub fn shadow(kind: &str, w: f32, h: f32, r: f32, c: Color, dark: bool, dpr: f32) -> Image {
    if w <= 0.5 || h <= 0.5 {
        return Image::default();
    }
    let dpr = dpr.max(0.5);
    let key = format!("{kind}|{:.2}|{:.2}|{:.2}|{:08x}|{dark}|{dpr:.2}", w, h, r, c.as_argb_encoded());
    cached(key, || {
        let m = margin(kind);
        let bw = ((w + 2.0 * m) * dpr).ceil().max(1.0) as usize;
        let bh = ((h + 2.0 * m) * dpr).ceil().max(1.0) as usize;
        let mut out = vec![[0f32; 4]; bw * bh];
        let (ex, ey, ew, eh, er) = (m * dpr, m * dpr, w * dpr, h * dpr, r * dpr);
        let mut elem = vec![0f32; bw * bh];
        mask_rrect(&mut elem, bw, bh, ex, ey, ew, eh, er);
        let mut tmp = vec![0f32; bw * bh];
        // CSS：第一个阴影在最上面 → 从最后一个画起
        for s in layers(kind, dark, c).iter().rev() {
            let sigma = s.blur / 2.0 * dpr;
            if !s.inset {
                let sp = s.spread * dpr;
                mask_rrect(&mut tmp, bw, bh, ex + s.dx * dpr - sp, ey + s.dy * dpr - sp, ew + 2.0 * sp, eh + 2.0 * sp, (er + sp).max(0.0));
                blur_f32(&mut tmp, bw, bh, sigma);
                for k in 0..bw * bh {
                    let a = tmp[k] * (1.0 - elem[k]) * s.rgba[3];
                    if a > 0.0 {
                        over(&mut out[k], [s.rgba[0], s.rgba[1], s.rgba[2]], a);
                    }
                }
            } else {
                let sp = s.spread * dpr;
                mask_rrect(&mut tmp, bw, bh, ex + s.dx * dpr + sp, ey + s.dy * dpr + sp, ew - 2.0 * sp, eh - 2.0 * sp, (er - sp).max(0.0));
                blur_f32(&mut tmp, bw, bh, sigma);
                for k in 0..bw * bh {
                    let a = elem[k] * (1.0 - tmp[k]) * s.rgba[3];
                    if a > 0.0 {
                        over(&mut out[k], [s.rgba[0], s.rgba[1], s.rgba[2]], a);
                    }
                }
            }
        }
        to_image(&out, bw, bh)
    })
}

// ------------------------------------------------------------------ paints

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    // 预乘空间插值（浏览器渐变的做法）
    let pa = [a[0] * a[3], a[1] * a[3], a[2] * a[3], a[3]];
    let pb = [b[0] * b[3], b[1] * b[3], b[2] * b[3], b[3]];
    [pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t, pa[2] + (pb[2] - pa[2]) * t, pa[3] + (pb[3] - pa[3]) * t]
}

fn over_pm(dst: &mut [f32; 4], s: [f32; 4]) {
    let ia = 1.0 - s[3];
    for i in 0..4 {
        dst[i] = s[i] + dst[i] * ia;
    }
}

/// inset 阴影叠到预乘缓冲上（元素形状 elem，元素几何 ex..）
fn inset_into(out: &mut [[f32; 4]], elem: &[f32], bw: usize, bh: usize, g: (f32, f32, f32, f32, f32), s: Sh, dpr: f32) {
    let (ex, ey, ew, eh, er) = g;
    let mut tmp = vec![0f32; bw * bh];
    let sp = s.spread * dpr;
    mask_rrect(&mut tmp, bw, bh, ex + s.dx * dpr + sp, ey + s.dy * dpr + sp, ew - 2.0 * sp, eh - 2.0 * sp, (er - sp).max(0.0));
    blur_f32(&mut tmp, bw, bh, s.blur / 2.0 * dpr);
    for k in 0..bw * bh {
        let a = elem[k] * (1.0 - tmp[k]) * s.rgba[3];
        if a > 0.0 {
            over(&mut out[k], [s.rgba[0], s.rgba[1], s.rgba[2]], a);
        }
    }
}

pub fn paint(kind: &str, w: f32, h: f32, c: Color, dark: bool, dpr: f32) -> Image {
    if w <= 0.5 || h <= 0.5 {
        return Image::default();
    }
    let dpr = dpr.max(0.5);
    let key = format!("paint|{kind}|{w:.2}|{h:.2}|{:08x}|{dark}|{dpr:.2}", c.as_argb_encoded());
    cached(key, || {
        let bw = (w * dpr).ceil().max(1.0) as usize;
        let bh = (h * dpr).ceil().max(1.0) as usize;
        let mut out = vec![[0f32; 4]; bw * bh];
        match kind {
            // :root.native-glass .glass::before —— radial(90% 55% at 12% 0, sheen, transparent 70%)
            // + linear(180deg, sheen 0, transparent 22%) + inset rim-top / rim-side / rim-low + 0 -18px 30px -24px rim-low；
            // border-radius 8（外壳 overflow:hidden 再裁成 14；弹出层不裁）
            "winglass" | "winglass-pop" => {
                let sheen = if dark { 0.07 } else { 0.2 };
                let rim_top = if dark { 0.22 } else { 0.85 };
                let rim_side = if dark { 0.09 } else { 0.35 };
                let rim_low = if dark { 0.35 } else { 0.07 };
                let g = (0.0, 0.0, w * dpr, h * dpr, 8.0 * dpr);
                let mut elem = vec![0f32; bw * bh];
                mask_rrect(&mut elem, bw, bh, g.0, g.1, g.2, g.3, g.4);
                let (rx, ry, cx) = (0.9 * w * dpr, 0.55 * h * dpr, 0.12 * w * dpr);
                for j in 0..bh {
                    let py = j as f32 + 0.5;
                    for i in 0..bw {
                        let px = i as f32 + 0.5;
                        let k = j * bw + i;
                        let mut p = [0f32; 4];
                        // background 的第二层（线性）在下，第一层（径向）在上
                        let tl = py / (0.22 * h * dpr);
                        if tl < 1.0 {
                            over_pm(&mut p, lerp4([1.0, 1.0, 1.0, sheen], [1.0, 1.0, 1.0, 0.0], tl));
                        }
                        let tr = (((px - cx) / rx).powi(2) + (py / ry).powi(2)).sqrt() / 0.7;
                        if tr < 1.0 {
                            over_pm(&mut p, lerp4([1.0, 1.0, 1.0, sheen], [1.0, 1.0, 1.0, 0.0], tr));
                        }
                        let e = elem[k];
                        out[k] = [p[0] * e, p[1] * e, p[2] * e, p[3] * e];
                    }
                }
                // box-shadow：第一个在最上 → 倒序
                let rims = [
                    sh(true, 0.0, -18.0, 30.0, -24.0, 0.0, 0.0, 0.0, rim_low),
                    sh(true, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, rim_low),
                    sh(true, -1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, rim_side),
                    sh(true, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, rim_side),
                    sh(true, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, rim_top),
                ];
                for s in rims {
                    inset_into(&mut out, &elem, bw, bh, g, s, dpr);
                }
                if kind == "winglass" {
                    let mut clip = vec![0f32; bw * bh];
                    mask_rrect(&mut clip, bw, bh, 0.0, 0.0, w * dpr, h * dpr, 14.0 * dpr);
                    for k in 0..bw * bh {
                        for ch in 0..4 {
                            out[k][ch] *= clip[k];
                        }
                    }
                }
            }
            // 侧栏头像：linear-gradient(180deg, color-mix(c 82%, white), c)，圆形（软件渲染器画渐变不裁圆角）
            "side-avatar" => {
                let mut elem = vec![0f32; bw * bh];
                let r = w.min(h) * dpr / 2.0;
                mask_rrect(&mut elem, bw, bh, 0.0, 0.0, w * dpr, h * dpr, r);
                let c2 = [c.red() as f32 / 255.0, c.green() as f32 / 255.0, c.blue() as f32 / 255.0, 1.0];
                let c1 = [c2[0] * 0.82 + 0.18, c2[1] * 0.82 + 0.18, c2[2] * 0.82 + 0.18, 1.0];
                for j in 0..bh {
                    let t = ((j as f32 + 0.5) / (h * dpr)).clamp(0.0, 1.0);
                    let p = lerp4(c1, c2, t);
                    for i in 0..bw {
                        let k = j * bw + i;
                        let e = elem[k];
                        out[k] = [p[0] * e, p[1] * e, p[2] * e, p[3] * e];
                    }
                }
            }
            // 拖放选择器头像：radial(120% 90% at 30% 20%, rgba(255,255,255,.45), transparent 55%) + 底色（或「全部」的
            // linear(135deg, #5ac8fa, #0a84ff 55%, #5e5ce6)）；inset 0 1px 1px rgba(255,255,255,.55)、inset 0 -2px 6px rgba(0,0,0,.12)
            "pick-avatar" | "pick-all" => {
                let mut elem = vec![0f32; bw * bh];
                let r = w.min(h) * dpr / 2.0;
                mask_rrect(&mut elem, bw, bh, 0.0, 0.0, w * dpr, h * dpr, r);
                let base = [c.red() as f32 / 255.0, c.green() as f32 / 255.0, c.blue() as f32 / 255.0, 1.0];
                let (rx, ry, cx, cy) = (1.2 * w * dpr, 0.9 * h * dpr, 0.3 * w * dpr, 0.2 * h * dpr);
                // 135deg：从左上到右下；渐变线长 = w·|sin|+h·|cos|
                let glen = (w + h) * dpr * std::f32::consts::FRAC_1_SQRT_2;
                let stops = [(0.0f32, [90.0 / 255.0, 200.0 / 255.0, 250.0 / 255.0, 1.0]), (0.55, [10.0 / 255.0, 132.0 / 255.0, 1.0, 1.0]), (1.0, [94.0 / 255.0, 92.0 / 255.0, 230.0 / 255.0, 1.0])];
                for j in 0..bh {
                    let py = j as f32 + 0.5;
                    for i in 0..bw {
                        let px = i as f32 + 0.5;
                        let k = j * bw + i;
                        let mut p = if kind == "pick-all" {
                            let t = (((px - w * dpr / 2.0) + (py - h * dpr / 2.0)) * std::f32::consts::FRAC_1_SQRT_2 / glen + 0.5).clamp(0.0, 1.0);
                            let (a, b) = if t <= stops[1].0 { (stops[0], stops[1]) } else { (stops[1], stops[2]) };
                            let u = ((t - a.0) / (b.0 - a.0)).clamp(0.0, 1.0);
                            lerp4(a.1, b.1, u)
                        } else {
                            base
                        };
                        let tr = (((px - cx) / rx).powi(2) + ((py - cy) / ry).powi(2)).sqrt() / 0.55;
                        if tr < 1.0 {
                            over_pm(&mut p, lerp4([1.0, 1.0, 1.0, 0.45], [1.0, 1.0, 1.0, 0.0], tr));
                        }
                        let e = elem[k];
                        out[k] = [p[0] * e, p[1] * e, p[2] * e, p[3] * e];
                    }
                }
                let g = (0.0, 0.0, w * dpr, h * dpr, r);
                for s in [sh(true, 0.0, -2.0, 6.0, 0.0, 0.0, 0.0, 0.0, 0.12), sh(true, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 0.55)] {
                    inset_into(&mut out, &elem, bw, bh, g, s, dpr);
                }
            }
            _ => {}
        }
        to_image(&out, bw, bh)
    })
}

// ------------------------------------------------------------------ paths

#[derive(Debug, Clone, Copy)]
enum Seg {
    M(f32, f32),
    L(f32, f32),
    C(f32, f32, f32, f32, f32, f32),
    Q(f32, f32, f32, f32),
    A(f32, f32, f32, bool, bool, f32, f32),
    Z,
}

fn tokenize(d: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let b: Vec<char> = d.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let ch = b[i];
        if ch.is_ascii_alphabetic() && ch != 'e' && ch != 'E' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.push(ch.to_string());
        } else if ch == ',' || ch.is_whitespace() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else if ch == '-' && !cur.is_empty() && !cur.ends_with('e') && !cur.ends_with('E') {
            out.push(std::mem::take(&mut cur));
            cur.push(ch);
        } else if ch == '.' && cur.contains('.') {
            out.push(std::mem::take(&mut cur));
            cur.push(ch);
        } else {
            cur.push(ch);
        }
        i += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// SVG 路径 → 绝对坐标段（弧的标志位也可能紧挨着写，如 "a8 8 0 0 1 14-5.3"：已按空格/逗号分好）
fn parse(d: &str) -> Vec<Seg> {
    let t = tokenize(d);
    let mut segs = Vec::new();
    let (mut x, mut y, mut sx, mut sy) = (0f32, 0f32, 0f32, 0f32);
    let (mut lcx, mut lcy) = (0f32, 0f32); // 上一个三次控制点（S 用）
    let mut i = 0;
    let mut cmd = 'M';
    let num = |i: &mut usize| -> f32 {
        let v = t.get(*i).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
        *i += 1;
        v
    };
    // 弧的 flag 可能是 "01" 这种连写；这里的图标不出现，按普通数处理
    while i < t.len() {
        if t[i].chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            cmd = t[i].chars().next().unwrap();
            i += 1;
            if cmd == 'z' || cmd == 'Z' {
                segs.push(Seg::Z);
                x = sx;
                y = sy;
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
        match cmd.to_ascii_uppercase() {
            'M' => {
                x = ox + num(&mut i);
                y = oy + num(&mut i);
                sx = x;
                sy = y;
                segs.push(Seg::M(x, y));
                cmd = if rel { 'l' } else { 'L' };
            }
            'L' => {
                x = ox + num(&mut i);
                y = oy + num(&mut i);
                segs.push(Seg::L(x, y));
            }
            'H' => {
                x = (if rel { x } else { 0.0 }) + num(&mut i);
                segs.push(Seg::L(x, y));
            }
            'V' => {
                y = (if rel { y } else { 0.0 }) + num(&mut i);
                segs.push(Seg::L(x, y));
            }
            'C' => {
                let (a, b, c, d2, e, f) = (ox + num(&mut i), oy + num(&mut i), ox + num(&mut i), oy + num(&mut i), ox + num(&mut i), oy + num(&mut i));
                segs.push(Seg::C(a, b, c, d2, e, f));
                lcx = c;
                lcy = d2;
                x = e;
                y = f;
                continue;
            }
            'S' => {
                let (a, b) = (2.0 * x - lcx, 2.0 * y - lcy);
                let (c, d2, e, f) = (ox + num(&mut i), oy + num(&mut i), ox + num(&mut i), oy + num(&mut i));
                segs.push(Seg::C(a, b, c, d2, e, f));
                lcx = c;
                lcy = d2;
                x = e;
                y = f;
                continue;
            }
            'Q' => {
                let (a, b, e, f) = (ox + num(&mut i), oy + num(&mut i), ox + num(&mut i), oy + num(&mut i));
                segs.push(Seg::Q(a, b, e, f));
                x = e;
                y = f;
            }
            'A' => {
                let rx = num(&mut i);
                let ry = num(&mut i);
                let rot = num(&mut i);
                let large = num(&mut i) != 0.0;
                let sweep = num(&mut i) != 0.0;
                x = ox + num(&mut i);
                y = oy + num(&mut i);
                segs.push(Seg::A(rx, ry, rot, large, sweep, x, y));
            }
            _ => {
                i += 1;
            }
        }
        lcx = x;
        lcy = y;
    }
    segs
}

fn f(v: f32) -> String {
    let s = format!("{:.3}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s == "-0" {
        "0".into()
    } else {
        s
    }
}

pub fn rotate_path(d: &str, deg: f32) -> String {
    let deg = deg.rem_euclid(360.0);
    if deg.abs() < 0.01 {
        return d.to_string();
    }
    let key = format!("{d}|{deg:.2}");
    if let Some(s) = PATHS.with(|p| p.borrow().get(&key).cloned()) {
        return s;
    }
    let (sn, cs) = deg.to_radians().sin_cos();
    let rot = |x: f32, y: f32| -> (f32, f32) {
        let (dx, dy) = (x - 12.0, y - 12.0);
        (12.0 + dx * cs - dy * sn, 12.0 + dx * sn + dy * cs)
    };
    let mut out = String::new();
    for s in parse(d) {
        match s {
            Seg::M(x, y) => {
                let (a, b) = rot(x, y);
                out += &format!("M{} {} ", f(a), f(b));
            }
            Seg::L(x, y) => {
                let (a, b) = rot(x, y);
                out += &format!("L{} {} ", f(a), f(b));
            }
            Seg::C(a, b, c, d2, e, g) => {
                let (a, b) = rot(a, b);
                let (c, d2) = rot(c, d2);
                let (e, g) = rot(e, g);
                out += &format!("C{} {} {} {} {} {} ", f(a), f(b), f(c), f(d2), f(e), f(g));
            }
            Seg::Q(a, b, e, g) => {
                let (a, b) = rot(a, b);
                let (e, g) = rot(e, g);
                out += &format!("Q{} {} {} {} ", f(a), f(b), f(e), f(g));
            }
            Seg::A(rx, ry, r, large, sweep, x, y) => {
                let (a, b) = rot(x, y);
                out += &format!("A{} {} {} {} {} {} {} ", f(rx), f(ry), f(r + deg), large as u8, sweep as u8, f(a), f(b));
            }
            Seg::Z => out += "Z ",
        }
    }
    let out = out.trim_end().to_string();
    PATHS.with(|p| {
        let mut p = p.borrow_mut();
        if p.len() > 512 {
            p.clear();
        }
        p.insert(key, out.clone());
    });
    out
}

thread_local! {
    static PATHS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// CSS `Npx dashed` 圆角边框：描边中线（向内 width/2）上的虚线段。Chromium 的虚线：段长 = 间隔 = 3×宽，
/// 再按周长均分成整数段。
pub fn dash_rect(w: f32, h: f32, r: f32, width: f32) -> String {
    let key = format!("dash|{w:.2}|{h:.2}|{r:.2}|{width:.2}");
    if let Some(s) = PATHS.with(|p| p.borrow().get(&key).cloned()) {
        return s;
    }
    let i = width / 2.0;
    let (x0, y0, x1, y1) = (i, i, w - i, h - i);
    let r = (r - i).max(0.0).min((x1 - x0) / 2.0).min((y1 - y0) / 2.0);
    // 采样中线成折线（角上每 ~1.5px 一点）
    let mut pts: Vec<(f32, f32)> = Vec::new();
    let arc = |pts: &mut Vec<(f32, f32)>, cx: f32, cy: f32, a0: f32, a1: f32| {
        let n = ((r * (a1 - a0).abs()) / 1.5).ceil().max(1.0) as usize;
        for k in 0..=n {
            let a = a0 + (a1 - a0) * k as f32 / n as f32;
            pts.push((cx + r * a.cos(), cy + r * a.sin()));
        }
    };
    use std::f32::consts::PI;
    pts.push((x0 + r, y0));
    pts.push((x1 - r, y0));
    arc(&mut pts, x1 - r, y0 + r, -PI / 2.0, 0.0);
    pts.push((x1, y1 - r));
    arc(&mut pts, x1 - r, y1 - r, 0.0, PI / 2.0);
    pts.push((x0 + r, y1));
    arc(&mut pts, x0 + r, y1 - r, PI / 2.0, PI);
    pts.push((x0, y0 + r));
    arc(&mut pts, x0 + r, y0 + r, PI, PI * 1.5);
    let mut lens = vec![0f32];
    for k in 1..pts.len() {
        let (a, b) = (pts[k - 1], pts[k]);
        lens.push(lens[k - 1] + ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt());
    }
    let total = *lens.last().unwrap_or(&0.0);
    let unit = 3.0 * width * 2.0;
    let n = (total / unit).round().max(1.0);
    let unit = total / n;
    let dash = unit / 2.0;
    let at = |s: f32| -> (f32, f32) {
        let k = lens.partition_point(|&l| l < s).clamp(1, lens.len() - 1);
        let (l0, l1) = (lens[k - 1], lens[k]);
        let t = if l1 > l0 { (s - l0) / (l1 - l0) } else { 0.0 };
        let (a, b) = (pts[k - 1], pts[k]);
        (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
    };
    let mut out = String::new();
    let mut s = 0.0;
    while s < total - 0.01 {
        let e = (s + dash).min(total);
        let (ax, ay) = at(s);
        out += &format!("M{} {}", f(ax), f(ay));
        // 段内跟着折线走（拐角处的段是弯的）
        let k0 = lens.partition_point(|&l| l <= s);
        let k1 = lens.partition_point(|&l| l < e);
        for k in k0..k1 {
            out += &format!(" L{} {}", f(pts[k].0), f(pts[k].1));
        }
        let (bx, by) = at(e);
        out += &format!(" L{} {} ", f(bx), f(by));
        s += unit;
    }
    let out = out.trim_end().to_string();
    PATHS.with(|p| p.borrow_mut().insert(key, out.clone()));
    out
}

// ------------------------------------------------------------------ wiring

/// 给一个窗口（面板 / 设置）装上 Fx 的回调
#[macro_export]
macro_rules! install_fx {
    ($c:expr, $normal:expr) => {{
        use slint::ComponentHandle as _;
        // 提示气泡到点时问 Rust：指针还在登记的位置上吗（主人元素被重建掉时 Slint 收不到离开事件）
        $c.global::<$crate::Tip>().on_valid(|x, y, w, h| $crate::tip_valid($normal, x, y, w, h));
        let g = $c.global::<$crate::Fx>();
        g.on_shadow(|kind, w, h, r, col, dark, dpr| $crate::fx::shadow(kind.as_str(), w, h, r, col, dark, dpr));
        g.on_shadow_margin(|kind| $crate::fx::margin(kind.as_str()));
        g.on_rotate_path(|d, deg| slint::SharedString::from($crate::fx::rotate_path(d.as_str(), deg)));
        g.on_dash_rect(|w, h, r, width| slint::SharedString::from($crate::fx::dash_rect(w, h, r, width)));
        g.on_paint(|kind, w, h, col, dark, dpr| $crate::fx::paint(kind.as_str(), w, h, col, dark, dpr));
        g.on_ease(|name, x| $crate::fx::ease(name.as_str(), x));
        g.set_dpr($c.window().scale_factor());
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotate_retry_quarter() {
        let p = rotate_path("M9 6l6 6-6 6", 90.0);
        assert_eq!(p, "M18 9 L12 15 L6 9");
    }
    #[test]
    fn bezier_endpoints() {
        assert!((bezier(0.2, 0.8, 0.3, 1.0, 0.5) - 0.86).abs() < 0.05);
        assert_eq!(bezier(0.2, 0.8, 0.3, 1.0, 1.0), 1.0);
    }
}
