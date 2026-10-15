//! 面板从停靠条「长出来」/ 缩回去的液态玻璃变形（两个平台共用的几何、弹簧和逐像素合成）。
//!
//! 形状：圆角矩形从停靠条的矩形长成面板的矩形。垂直于屏幕边的方向（往外长）和沿着屏幕边的方向
//! （拉长）用两根不同的弹簧，沿边方向稍慢一点，所以它是先「挤」出来再展开，像液体从条里流出来。
//! 弹簧参数按 Apple 的 duration / bounce（见 spring.rs）：
//!   打开：往外 (0.40 s, 回弹 0.22)，沿边 (0.46 s, 回弹 0.16) —— 介于 SwiftUI .snappy 与 .bouncy 之间；
//!   收起：往回 (0.30 s, 0)，沿边 (0.26 s, 0) —— .smooth，没有回弹，沿边先收。
//! 中途反向（打开一半又收起）从当前形状接着走，不跳。
//!
//! 材质：背后屏幕的模糊图（Windows 打开前截一张；Linux 没有就只用色调），圆角边缘一圈像透镜一样
//! 向内折射（越靠边弯得越多，和 toast 的 liquid glass 同一个算法，但跟着形状每帧重算），
//! 外加一圈高光描边、顶部高光和柔和阴影。面板内容（它自己最后一帧的截图）在形状长到一半后淡入，
//! 以形状中心等比缩放、裁在形状里。

use crate::compose::Bgra;
use crate::spring::Spring;
use std::time::Instant;

/// (x0, y0, x1, y1)，帧内物理像素
pub type R = [f64; 4];

const OPEN_OUT: Spring = Spring::new(0.40, 0.22);
const OPEN_ALONG: Spring = Spring::new(0.46, 0.16);
const CLOSE_OUT: Spring = Spring::new(0.30, 0.0);
const CLOSE_ALONG: Spring = Spring::new(0.26, 0.0);

pub fn env_ms_scale() -> f64 {
    // LANCAT_MORPH_SLOW=4：慢放 4 倍（调试 / 录屏对比用）
    std::env::var("LANCAT_MORPH_SLOW").ok().and_then(|v| v.parse::<f64>().ok()).filter(|v| *v > 0.0).unwrap_or(1.0)
}

#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub rect: R,
    pub radius: f64,
    /// 面板内容的不透明度 0..1
    pub content: f64,
    /// 打开进度（往外那根弹簧，0..≈1.05），用于颜色从停靠条的 accent 过渡到玻璃
    pub grow: f64,
    pub done: bool,
}

pub struct Anim {
    pub opening: bool,
    from: R,
    to: R,
    from_r: f64,
    to_r: f64,
    from_c: f64,
    from_g: f64,
    vertical_edge: bool,
    t0: Instant,
    slow: f64,
}

fn smooth(a: f64, b: f64, x: f64) -> f64 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Anim {
    /// `from`/`to`：起止矩形；`edge`：停靠的屏幕边；起点内容不透明度与打开进度（中途反向时接着走）
    pub fn new(opening: bool, from: R, to: R, from_r: f64, to_r: f64, edge: &str, from_c: f64, from_g: f64) -> Anim {
        Anim { opening, from, to, from_r, to_r, from_c, from_g, vertical_edge: edge == "left" || edge == "right", t0: Instant::now(), slow: env_ms_scale() }
    }

    /// 从现在重新计时（Wayland：第一帧真正上屏时才开始）
    #[allow(dead_code)]
    pub fn restart(&mut self) {
        self.t0 = Instant::now();
    }

    pub fn frame(&self) -> Frame {
        let t = self.t0.elapsed().as_secs_f64() / self.slow;
        let (out, along) = if self.opening { (OPEN_OUT, OPEN_ALONG) } else { (CLOSE_OUT, CLOSE_ALONG) };
        let (po, done_o) = out.sample(t);
        let (pa, done_a) = along.sample(t);
        let lerp = |a: f64, b: f64, p: f64| a + (b - a) * p;
        let (f, g) = (self.from, self.to);
        // 竖边（左/右）：x 是往外的方向，y 沿边
        let (px, py) = if self.vertical_edge { (po, pa) } else { (pa, po) };
        let rect = [lerp(f[0], g[0], px), lerp(f[1], g[1], py), lerp(f[2], g[2], px), lerp(f[3], g[3], py)];
        let radius = lerp(self.from_r, self.to_r, po.clamp(0.0, 1.0));
        let (content, grow) = if self.opening {
            // 形状长到 30% 开始显出内容，85% 时完全显出
            let c = smooth(0.30, 0.85, po);
            (self.from_c + (1.0 - self.from_c) * c, self.from_g + (1.0 - self.from_g) * po)
        } else {
            // 收起：内容在前 35% 的时间里先淡掉，形状再缩回条里
            let c = smooth(0.0, 0.35 * out.duration, t);
            (self.from_c * (1.0 - c), self.from_g * (1.0 - po))
        };
        Frame { rect, radius, content, grow, done: done_o && done_a }
    }
}

// ------------------------------------------------------------------ 合成

pub struct Look {
    pub dark: bool,
    /// 缩放（物理 px / 逻辑 px）
    pub scale: f64,
    /// 停靠条的颜色（形状刚出来时的颜色）
    pub accent: (f64, f64, f64),
}

/// 圆角矩形的有向距离（内部为负）和外法线
fn sdf(px: f64, py: f64, r: &R, rad: f64) -> (f64, f64, f64) {
    let (hx, hy) = ((r[2] - r[0]) / 2.0, (r[3] - r[1]) / 2.0);
    let rad = rad.min(hx).min(hy).max(0.0);
    let (fx, fy) = (px - (r[0] + hx), py - (r[1] + hy));
    let qx = fx.abs() - (hx - rad);
    let qy = fy.abs() - (hy - rad);
    let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - rad;
    let (nx, ny) = if qx > 0.0 && qy > 0.0 {
        let l = qx.hypot(qy).max(1e-6);
        (qx / l, qy / l)
    } else if qx > qy {
        (1.0, 0.0)
    } else {
        (0.0, 1.0)
    };
    (d, nx * fx.signum(), ny * fy.signum())
}

/// 面板内容：最后一帧截图（预乘 BGRA）和它在帧里的最终矩形
pub struct Content<'a> {
    pub px: &'a [Bgra],
    pub w: usize,
    pub h: usize,
    /// 最终矩形（帧内物理像素，左上角）
    pub at: (f64, f64),
}

#[inline]
fn sample(c: &Content, x: f64, y: f64) -> [f32; 4] {
    // 双线性，越界为透明
    let (x, y) = (x - 0.5, y - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = ((x - x0) as f32, (y - y0) as f32);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let get = |xx: i64, yy: i64| -> [f32; 4] {
        if xx < 0 || yy < 0 || xx >= c.w as i64 || yy >= c.h as i64 {
            return [0.0; 4];
        }
        let p = c.px[yy as usize * c.w + xx as usize];
        [p.b as f32, p.g as f32, p.r as f32, p.a as f32]
    };
    let (a, b, cc, d) = (get(x0, y0), get(x0 + 1, y0), get(x0, y0 + 1), get(x0 + 1, y0 + 1));
    let mut o = [0f32; 4];
    for i in 0..4 {
        let top = a[i] + (b[i] - a[i]) * fx;
        let bot = cc[i] + (d[i] - cc[i]) * fx;
        o[i] = top + (bot - top) * fy;
    }
    o
}

/// 把一帧画进 `out`（w × h，预乘 BGRA，整幅覆盖）。`glass`：同尺寸的模糊背景（不透明），None = 没有背景，只用色调。
pub fn render(out: &mut [Bgra], w: usize, h: usize, f: &Frame, glass: Option<&[Bgra]>, content: &Content, look: &Look) {
    render_impl(out, w, h, f, glass, content, look, None, false);
}

/// Windows（有原生材质）：静止面板 = Slint 帧（Theme.glass 的淡外壳 + 内容 + winglass 高光）叠在
/// 液态玻璃底（`render` 以面板矩形画的玻璃 + 色调，见 win.rs set_liquid）上。变形收敛到它：
/// - 形状就是那层玻璃底（同一张模糊截图、同一色调 / 透明度 / 高光），内容 = 1、形状 = 面板时与之逐像素一致；
/// - 面板内容叠在形状上（这正是静止面板的合成方式），按形状矩形拉伸（不是等比缩放），
///   内容自己的外壳和高光描边与形状边缘重合，不会出现一大一小两圈轮廓；
/// - `content.at` 是卡片在内容图里的左上角（Windows 面板窗口就是卡片，传 (0, 0)）。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn render_stretch(out: &mut [Bgra], w: usize, h: usize, f: &Frame, glass: Option<&[Bgra]>, content: &Content, look: &Look) {
    render_impl(out, w, h, f, glass, content, look, None, true);
}

/// Linux（没有原生材质）：静止面板就是 Slint 画的整幅帧（半透明的 Theme.glass 外壳 + 外框里的阴影），
/// 合成器直接叠在桌面上。变形要收敛到它：
/// - 形状的色调用静止面板同一个颜色和透明度（`tint` / `alpha`，即 Theme.glass），不再额外叠一层不透明色调；
/// - 面板内容不是「盖」在形状上，而是和形状交叉淡入淡出（内容自己带着外壳背景），内容 = 1 时输出就是那一帧本身；
/// - 内容按形状的矩形拉伸（不是等比缩放），外壳边框和形状边缘重合，不会出现一大一小两圈轮廓；
/// - `content` 是整幅帧（含外框），`content.at` 是卡片在帧里的左上角（卡片四周外框等宽）。
pub struct Rest {
    pub tint: (f64, f64, f64),
    pub alpha: f64,
}

#[cfg_attr(windows, allow(dead_code))]
pub fn render_rest(out: &mut [Bgra], w: usize, h: usize, f: &Frame, glass: Option<&[Bgra]>, content: &Content, look: &Look, rest: &Rest) {
    render_impl(out, w, h, f, glass, content, look, Some(rest), false);
}

#[allow(clippy::too_many_arguments)]
fn render_impl(out: &mut [Bgra], w: usize, h: usize, f: &Frame, glass: Option<&[Bgra]>, content: &Content, look: &Look, rest: Option<&Rest>, stretch: bool) {
    let sc = look.scale;
    let r = f.rect;
    let rad = f.radius;
    let shadow = 14.0 * sc;
    let sh_dy = 2.0 * sc;
    let bevel = (14.0 * sc).min((r[2] - r[0]).min(r[3] - r[1]) / 2.0).max(1.0);
    let strength = 18.0 * sc;
    let rim_w = 1.2 * sc;
    let g = f.grow.clamp(0.0, 1.0);
    // 形状的颜色：一开始是停靠条的 accent，长出来后变成玻璃色调
    let k_acc = (1.0 - g * 2.2).clamp(0.0, 1.0);
    let (tint, ta) = match (rest, look.dark, glass.is_some()) {
        (Some(rs), _, _) => (rs.tint, rs.alpha),
        (None, true, true) => ((0.10, 0.10, 0.12), 0.40),
        (None, false, true) => ((0.98, 0.98, 0.99), 0.46),
        (None, true, false) => ((0.13, 0.13, 0.15), 0.90),
        (None, false, false) => ((0.97, 0.97, 0.98), 0.92),
    };
    let mix = |a: f64, b: f64| a * (1.0 - k_acc) + b * k_acc;
    let body = (mix(tint.0, look.accent.0), mix(tint.1, look.accent.1), mix(tint.2, look.accent.2));
    let body_a = mix(ta, 0.85);
    let rim_a = if look.dark { 0.30 } else { 0.75 } * g.max(0.25);
    let sheen_a = if look.dark { 0.10 } else { 0.30 } * (1.0 - f.content * 0.7) * g;
    let shadow_a = 0.20 * g.max(0.25);
    // 内容：以形状中心等比缩放
    let (pw, ph) = (content.w as f64, content.h as f64);
    let k = ((r[2] - r[0]) / pw).min((r[3] - r[1]) / ph).max(1e-3);
    let (ccx, ccy) = ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
    let c_op = f.content as f32;
    // rest / stretch 模式：形状矩形 → 内容帧里卡片矩形的拉伸映射（rest 在形状外也照样外推，取到外框里的阴影）
    let xf = rest.is_some();
    let (card_w, card_h) = ((pw - 2.0 * content.at.0).max(1.0), (ph - 2.0 * content.at.1).max(1.0));
    let (kx, ky) = (card_w / (r[2] - r[0]).max(1e-3), card_h / (r[3] - r[1]).max(1e-3));
    let (cax, cay) = content.at;
    // 只算形状 + 阴影的包围盒，其余清空
    let x_lo = ((r[0] - shadow).floor().max(0.0)) as usize;
    let x_hi = ((r[2] + shadow).ceil().min(w as f64)) as usize;
    let y_lo = ((r[1] - shadow).floor().max(0.0)) as usize;
    let y_hi = ((r[3] + shadow + sh_dy).ceil().min(h as f64)) as usize;
    let rows_per = (h / 6).max(16);
    std::thread::scope(|s| {
        for (ci, chunk) in out.chunks_mut(rows_per * w).enumerate() {
            let y_base = ci * rows_per;
            s.spawn(move || {
                for (ry, row) in chunk.chunks_mut(w).enumerate() {
                    let y = y_base + ry;
                    if y < y_lo || y >= y_hi || x_lo >= x_hi {
                        row.fill(Bgra::default());
                        continue;
                    }
                    row[..x_lo].fill(Bgra::default());
                    row[x_hi..].fill(Bgra::default());
                    let fy = y as f64 + 0.5;
                    for x in x_lo..x_hi {
                        let fx = x as f64 + 0.5;
                        let (d, nx, ny) = sdf(fx, fy, &r, rad);
                        // 阴影（向下偏一点）
                        let (ds, _, _) = sdf(fx, fy - sh_dy, &r, rad);
                        let sh = if ds > -shadow { (shadow_a * (-(ds.max(0.0) / (shadow * 0.42)).powi(2)).exp()) as f32 } else { shadow_a as f32 };
                        let cov = (0.5 - d).clamp(0.0, 1.0) as f32;
                        if cov <= 0.0 {
                            if xf && c_op > 0.0 {
                                let c = sample(content, cax + (fx - r[0]) * kx, cay + (fy - r[1]) * ky);
                                let k0 = 1.0 - c_op;
                                let a0 = sh * 255.0;
                                row[x] = Bgra { b: (c[0] * c_op).min(255.0) as u8, g: (c[1] * c_op).min(255.0) as u8, r: (c[2] * c_op).min(255.0) as u8, a: (a0 * k0 + c[3] * c_op).min(255.0) as u8 };
                            } else {
                                row[x] = Bgra { b: 0, g: 0, r: 0, a: (sh * 255.0) as u8 };
                            }
                            continue;
                        }
                        let depth = -d;
                        // 玻璃底
                        let (mut b, mut gg, mut rr, mut a): (f32, f32, f32, f32);
                        if let Some(gl) = glass {
                            // 凸透镜轮廓（circle map）：中间不折射，越靠边弯得越多；边缘带里 R/B 偏移略有不同（轻微色散）
                            let off = if depth > 0.0 && depth < bevel {
                                let t = 1.0 - depth / bevel;
                                (1.0 - (1.0 - t * t).max(0.0).sqrt()) * strength
                            } else {
                                0.0
                            };
                            let at = |k: f64| -> Bgra {
                                let sx = ((fx - nx * off * k) as i64).clamp(0, w as i64 - 1) as usize;
                                let sy = ((fy - ny * off * k) as i64).clamp(0, h as i64 - 1) as usize;
                                gl[sy * w + sx]
                            };
                            let p = at(1.0);
                            let (pr, pb) = if off > 0.5 { (at(1.08).r, at(0.92).b) } else { (p.r, p.b) };
                            // 饱和度 ×1.25（玻璃把颜色「聚」起来，不发灰）
                            let (r0, g0, b0) = (pr as f32, p.g as f32, pb as f32);
                            let lum = 0.2126 * r0 + 0.7152 * g0 + 0.0722 * b0;
                            let sat = |c: f32| (lum + (c - lum) * 1.25).clamp(0.0, 255.0);
                            let (r0, g0, b0) = (sat(r0), sat(g0), sat(b0));
                            // 透镜边缘也多亮一点
                            let lift = if depth < bevel { ((1.0 - depth / bevel).powi(2) * 22.0) as f32 } else { 0.0 };
                            let ba = body_a as f32;
                            b = (b0 + lift).min(255.0) * (1.0 - ba) + body.2 as f32 * 255.0 * ba;
                            gg = (g0 + lift).min(255.0) * (1.0 - ba) + body.1 as f32 * 255.0 * ba;
                            rr = (r0 + lift).min(255.0) * (1.0 - ba) + body.0 as f32 * 255.0 * ba;
                            a = 255.0;
                        } else {
                            let ba = body_a as f32;
                            b = body.2 as f32 * 255.0 * ba;
                            gg = body.1 as f32 * 255.0 * ba;
                            rr = body.0 as f32 * 255.0 * ba;
                            a = 255.0 * ba;
                        }
                        // 顶部高光
                        let tyk = ((fy - r[1]) / ((r[3] - r[1]) * 0.45).max(1.0)).clamp(0.0, 1.0);
                        let sheen = (sheen_a * (1.0 - tyk)) as f32;
                        if sheen > 0.0 {
                            b += (255.0 - b) * sheen;
                            gg += (255.0 - gg) * sheen;
                            rr += (255.0 - rr) * sheen;
                            a += (255.0 - a) * sheen;
                        }
                        // 面板内容
                        if c_op > 0.0 && !xf && !stretch {
                            let c = sample(content, (fx - ccx) / k + pw / 2.0, (fy - ccy) / k + ph / 2.0);
                            let ca = c[3] / 255.0 * c_op;
                            if ca > 0.0 {
                                let inv = 1.0 - ca;
                                b = c[0] * c_op + b * inv;
                                gg = c[1] * c_op + gg * inv;
                                rr = c[2] * c_op + rr * inv;
                                a = c[3] * c_op + a * inv;
                            }
                        }
                        // 高光描边：上沿亮、下沿暗一点
                        if depth < rim_w {
                            let kk = (1.0 - depth / rim_w).clamp(0.0, 1.0) as f32;
                            let light = (0.55 - 0.45 * ny) as f32;
                            let ra = rim_a as f32 * kk * light.clamp(0.1, 1.0);
                            b += (255.0 - b) * ra;
                            gg += (255.0 - gg) * ra;
                            rr += (255.0 - rr) * ra;
                            a += (255.0 - a) * ra;
                        }
                        // stretch：内容叠在整个玻璃底（含描边）之上，和静止面板（帧叠 lq_base）的合成顺序一致
                        if c_op > 0.0 && stretch {
                            let c = sample(content, cax + (fx - r[0]) * kx, cay + (fy - r[1]) * ky);
                            let inv = 1.0 - c[3] / 255.0 * c_op;
                            b = c[0] * c_op + b * inv;
                            gg = c[1] * c_op + gg * inv;
                            rr = c[2] * c_op + rr * inv;
                            a = c[3] * c_op + a * inv;
                        }
                        // 覆盖率 + 下面的阴影
                        let (b, gg, rr, a) = (b * cov, gg * cov, rr * cov, a * cov);
                        let sa = sh * 255.0 * (1.0 - a / 255.0);
                        if xf && c_op > 0.0 {
                            // 和内容帧交叉淡化（预乘空间线性混合 = 合成后的观感线性混合）
                            let c = sample(content, cax + (fx - r[0]) * kx, cay + (fy - r[1]) * ky);
                            let k0 = 1.0 - c_op;
                            let a = a + sa;
                            row[x] = Bgra {
                                b: (b * k0 + c[0] * c_op).min(255.0) as u8,
                                g: (gg * k0 + c[1] * c_op).min(255.0) as u8,
                                r: (rr * k0 + c[2] * c_op).min(255.0) as u8,
                                a: (a * k0 + c[3] * c_op).min(255.0) as u8,
                            };
                            continue;
                        }
                        row[x] = Bgra { b: b.min(255.0) as u8, g: gg.min(255.0) as u8, r: rr.min(255.0) as u8, a: (a + sa).min(255.0) as u8 };
                    }
                }
            });
        }
    });
}

/// 半分辨率模糊：先 2×2 缩小、在小图上做三次盒式模糊（半径减半），再双线性放大。≈ 4 倍快，
/// 模糊过的图看不出分辨率差别。`r`：全分辨率下的半径。
pub fn blur_half(px: &[Bgra], w: usize, h: usize, r: f64) -> Vec<Bgra> {
    let (sw, sh) = (w.div_ceil(2).max(1), h.div_ceil(2).max(1));
    let mut small = vec![Bgra::default(); sw * sh];
    for y in 0..sh {
        for x in 0..sw {
            let (mut b, mut g, mut rr) = (0u32, 0u32, 0u32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = px[(2 * y + dy).min(h - 1) * w + (2 * x + dx).min(w - 1)];
                b += p.b as u32;
                g += p.g as u32;
                rr += p.r as u32;
            }
            small[y * sw + x] = Bgra { b: (b / 4) as u8, g: (g / 4) as u8, r: (rr / 4) as u8, a: 255 };
        }
    }
    let rs = (r / 2.0).round().max(1.0) as usize;
    for _ in 0..3 {
        box_blur(&mut small, sw, sh, rs);
    }
    let mut out = vec![Bgra::default(); w * h];
    for y in 0..h {
        let fy = ((y as f64 + 0.5) / 2.0 - 0.5).clamp(0.0, (sh - 1) as f64);
        let (y0, ty) = (fy.floor() as usize, (fy - fy.floor()) as f32);
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..w {
            let fx = ((x as f64 + 0.5) / 2.0 - 0.5).clamp(0.0, (sw - 1) as f64);
            let (x0, tx) = (fx.floor() as usize, (fx - fx.floor()) as f32);
            let x1 = (x0 + 1).min(sw - 1);
            let (a, b, c, d) = (small[y0 * sw + x0], small[y0 * sw + x1], small[y1 * sw + x0], small[y1 * sw + x1]);
            let mixc = |a: u8, b: u8, c: u8, d: u8| -> u8 {
                let t = a as f32 + (b as f32 - a as f32) * tx;
                let u = c as f32 + (d as f32 - c as f32) * tx;
                (t + (u - t) * ty).round() as u8
            };
            out[y * w + x] = Bgra { b: mixc(a.b, b.b, c.b, d.b), g: mixc(a.g, b.g, c.g, d.g), r: mixc(a.r, b.r, c.r, d.r), a: 255 };
        }
    }
    out
}

/// 一次横向 + 一次纵向的滑动窗口盒式模糊（不透明 BGRA，忽略 alpha）。三次 ≈ 高斯。
pub fn box_blur(px: &mut [Bgra], w: usize, h: usize, r: usize) {
    if w == 0 || h == 0 || r == 0 {
        return;
    }
    let win = (2 * r + 1) as u32;
    let mut tmp = vec![Bgra::default(); px.len()];
    let ch = |p: &Bgra, c: usize| -> u32 {
        match c {
            0 => p.b as u32,
            1 => p.g as u32,
            _ => p.r as u32,
        }
    };
    let set = |p: &mut Bgra, c: usize, v: u32| match c {
        0 => p.b = v as u8,
        1 => p.g = v as u8,
        _ => p.r = v as u8,
    };
    for y in 0..h {
        let row = y * w;
        for c in 0..3 {
            let mut acc: u32 = 0;
            for k in 0..=2 * r {
                acc += ch(&px[row + k.saturating_sub(r).min(w - 1)], c);
            }
            for x in 0..w {
                set(&mut tmp[row + x], c, acc / win);
                acc = acc + ch(&px[row + (x + r + 1).min(w - 1)], c) - ch(&px[row + x.saturating_sub(r)], c);
            }
        }
        for x in 0..w {
            tmp[row + x].a = 255;
        }
    }
    for x in 0..w {
        for c in 0..3 {
            let mut acc: u32 = 0;
            for k in 0..=2 * r {
                acc += ch(&tmp[k.saturating_sub(r).min(h - 1) * w + x], c);
            }
            for y in 0..h {
                set(&mut px[y * w + x], c, acc / win);
                acc = acc + ch(&tmp[(y + r + 1).min(h - 1) * w + x], c) - ch(&tmp[y.saturating_sub(r) * w + x], c);
            }
        }
        for y in 0..h {
            px[y * w + x].a = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grows_from_strip_to_panel() {
        let strip = [994.0, 300.0, 1000.0, 460.0];
        let panel = [640.0, 120.0, 1000.0, 640.0];
        let a = Anim::new(true, strip, panel, 3.0, 8.0, "right", 0.0, 0.0);
        let f = a.frame();
        assert!((f.rect[0] - strip[0]).abs() < 1.0 && f.content == 0.0);
        let mut out = vec![Bgra::default(); 400 * 600];
        let c = vec![Bgra { b: 200, g: 200, r: 200, a: 255 }; 360 * 520];
        let content = Content { px: &c, w: 360, h: 520, at: (20.0, 40.0) };
        let f = Frame { rect: [20.0, 40.0, 380.0, 560.0], radius: 8.0, content: 1.0, grow: 1.0, done: true };
        render(&mut out, 400, 600, &f, None, &content, &Look { dark: false, scale: 1.0, accent: (0.04, 0.52, 1.0) });
        // 中心是内容
        assert_eq!(out[300 * 400 + 200].a, 255);
        // 远处透明
        assert_eq!(out[5 * 400 + 5].a, 0);
    }

    #[test]
    fn stretch_mode_converges_to_rest_composite() {
        // Windows：形状 = 面板、内容 = 1 时，输出 = 内容叠在 render(内容空) 的玻璃底上（= set_liquid + WM_PAINT）
        let (w, h) = (40usize, 50usize);
        let glass: Vec<Bgra> = (0..w * h).map(|i| Bgra { b: (i % 13 * 9) as u8, g: (i % 7 * 20) as u8, r: (i % 5 * 40) as u8, a: 255 }).collect();
        let c: Vec<Bgra> = (0..w * h).map(|i| { let a = (i % 9 * 25) as u8; Bgra { b: a / 2, g: a / 3, r: a, a } }).collect();
        let look = Look { dark: true, scale: 1.0, accent: (0.04, 0.52, 1.0) };
        let f = Frame { rect: [0.0, 0.0, w as f64, h as f64], radius: 8.0, content: 1.0, grow: 1.0, done: true };
        let none = [Bgra::default()];
        let mut base = vec![Bgra::default(); w * h];
        render(&mut base, w, h, &f, Some(&glass), &Content { px: &none, w: 1, h: 1, at: (0.0, 0.0) }, &look);
        let mut out = vec![Bgra::default(); w * h];
        render_stretch(&mut out, w, h, &f, Some(&glass), &Content { px: &c, w, h, at: (0.0, 0.0) }, &look);
        for i in 0..w * h {
            let (o, b, cc) = (out[i], base[i], c[i]);
            if b.a < 255 {
                continue; // 圆角外缘：真面板由 DWM 裁角
            }
            let inv = 255 - cc.a as i32;
            let e = cc.b as i32 + (b.b as i32 * inv + 127) / 255;
            assert!((o.b as i32 - e).abs() <= 2, "px {i}: {} vs {}", o.b, e);
        }
    }

    #[test]
    fn rest_mode_converges_to_panel_frame() {
        // 内容 = 整幅帧（外框 8 px）；形状 = 卡片、内容 = 1 时，输出必须就是那一帧
        let (w, h, fr) = (60usize, 80usize, 8.0);
        let mut c = vec![Bgra::default(); w * h];
        for (i, p) in c.iter_mut().enumerate() {
            let a = (i % 7 * 30) as u8;
            *p = Bgra { b: a / 2, g: a / 3, r: a / 4, a };
        }
        let content = Content { px: &c, w, h, at: (fr, fr) };
        let f = Frame { rect: [fr, fr, w as f64 - fr, h as f64 - fr], radius: 6.0, content: 1.0, grow: 1.0, done: false };
        let mut out = vec![Bgra::default(); w * h];
        let rest = Rest { tint: (0.96, 0.96, 0.97), alpha: 0.86 };
        render_rest(&mut out, w, h, &f, None, &content, &Look { dark: false, scale: 1.0, accent: (0.04, 0.52, 1.0) }, &rest);
        for i in 0..w * h {
            let (o, e) = (out[i], c[i]);
            assert!((o.a as i32 - e.a as i32).abs() <= 1 && (o.b as i32 - e.b as i32).abs() <= 1, "px {i}: {} {} vs {} {}", o.a, o.b, e.a, e.b);
        }
    }
}
