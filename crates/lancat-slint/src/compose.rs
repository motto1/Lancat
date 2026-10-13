//! 合成层：软件渲染器没有 transform 和 backdrop-filter，旧版的弹出动画（表情面板 / 附件菜单 / 右键菜单 /
//! 拖放选择器 / toast 的 scale+translate+opacity）和背景模糊（选择器 blur(14px) saturate(160%)、toast 与选择器
//! 提示的 liquid 位移 + blur + saturate）在这里做，等价于浏览器合成器的做法（变换的是图层位图）：
//!
//! 1. 本帧有「活动层」（在动画中，或有背景模糊）时，先只画基底（`Fx.pass = 1`，活动层隐藏，非活动层照常画在基底里）；
//! 2. 每个活动层单独画一遍（`Fx.pass = id`）到临时缓冲，背景滤镜作用在它下面的已合成像素上，再按当前姿态
//!    （以层矩形内的 transform-origin 缩放 + 平移 + 透明度，双线性采样）叠上去；
//! 3. 顺序 = `fx-layers` 的顺序；一旦某层活动，排在它后面的可见层也一起按层画（保持 z 序）。
//!
//! 没有活动层时走普通单遍渲染（Windows 下只重画脏区）。两端（win.rs / wl.rs）共用。

use crate::{Fx, PanelWindow};
use slint::platform::software_renderer::{PremultipliedRgbaColor, RepaintBufferType, SoftwareRenderer, TargetPixel};
use slint::{ComponentHandle, Model};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 预乘 BGRA（32 位 DIB / wl_shm Argb8888 同布局）
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
pub struct Bgra {
    pub b: u8,
    pub g: u8,
    pub r: u8,
    pub a: u8,
}

impl TargetPixel for Bgra {
    fn blend(&mut self, c: PremultipliedRgbaColor) {
        let ia = (255 - c.alpha) as u16;
        self.r = (self.r as u16 * ia / 255) as u8 + c.red;
        self.g = (self.g as u16 * ia / 255) as u8 + c.green;
        self.b = (self.b as u16 * ia / 255) as u8 + c.blue;
        self.a = (self.a as u16 + c.alpha as u16 - (self.a as u16 * c.alpha as u16) / 255) as u8;
    }
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Bgra { b, g, r, a: 255 }
    }
    fn background() -> Self {
        Bgra::default()
    }
}

// ------------------------------------------------------------------ layer animations

#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub tx: f32,
    pub ty: f32,
    pub s: f32,
    pub op: f32,
    /// transform-origin（层矩形内的比例）
    pub ox: f32,
    pub oy: f32,
}

impl Pose {
    pub const ID: Pose = Pose { tx: 0.0, ty: 0.0, s: 1.0, op: 1.0, ox: 0.5, oy: 0.5 };
    fn identity(&self) -> bool {
        self.tx.abs() < 0.01 && self.ty.abs() < 0.01 && (self.s - 1.0).abs() < 0.0005 && self.op >= 0.999
    }
}

struct Anim {
    start: Instant,
    dur: f32,
    curve: (f32, f32, f32, f32),
    from: Pose,
}

thread_local! {
    static PANEL: RefCell<Option<slint::Weak<PanelWindow>>> = const { RefCell::new(None) };
    static ANIMS: RefCell<HashMap<i32, Anim>> = RefCell::new(HashMap::new());
    static TMP: RefCell<Vec<Bgra>> = const { RefCell::new(Vec::new()) };
    static WAS: Cell<bool> = const { Cell::new(false) };
    static TICK: slint::Timer = slint::Timer::default();
}

pub fn set_panel(w: slint::Weak<PanelWindow>) {
    PANEL.with(|p| *p.borrow_mut() = Some(w));
}

fn panel() -> Option<PanelWindow> {
    PANEL.with(|p| p.borrow().as_ref().and_then(|w| w.upgrade()))
}

/// 层的入场动画（CSS keyframes from → 原样，timing function 作用在整段上）
pub fn start(id: i32) {
    let a = match id {
        // 表情面板 pop：300 ms cubic-bezier(.2,.95,.3,1.18)，from translateY(10) scale(.92) opacity 0，origin 12% 100%
        2 => Anim { start: Instant::now(), dur: 300.0, curve: (0.2, 0.95, 0.3, 1.18), from: Pose { tx: 0.0, ty: 10.0, s: 0.92, op: 0.0, ox: 0.12, oy: 1.0 } },
        // 附件菜单 pop-in：160 ms ease-spring，from scale(.9) opacity 0，origin bottom left
        3 => Anim { start: Instant::now(), dur: 160.0, curve: (0.2, 0.9, 0.3, 1.4), from: Pose { tx: 0.0, ty: 0.0, s: 0.9, op: 0.0, ox: 0.0, oy: 1.0 } },
        // 右键菜单 menu-in：160 ms ease，from scale(.95) opacity 0，origin top left
        4 => Anim { start: Instant::now(), dur: 160.0, curve: (0.2, 0.8, 0.3, 1.0), from: Pose { tx: 0.0, ty: 0.0, s: 0.95, op: 0.0, ox: 0.0, oy: 0.0 } },
        // 拖放选择器 picker-in：240 ms cubic-bezier(.2,.9,.3,1)，opacity 0 → 1
        5 => Anim { start: Instant::now(), dur: 240.0, curve: (0.2, 0.9, 0.3, 1.0), from: Pose { op: 0.0, ..Pose::ID } },
        // toast-in：260 ms cubic-bezier(.2,.9,.3,1.2)，from translate(-50%, 8px) scale(.96) opacity 0
        7 => Anim { start: Instant::now(), dur: 260.0, curve: (0.2, 0.9, 0.3, 1.2), from: Pose { tx: 0.0, ty: 8.0, s: 0.96, op: 0.0, ox: 0.5, oy: 0.5 } },
        _ => return,
    };
    ANIMS.with(|m| m.borrow_mut().insert(id, a));
    ensure_tick();
    if let Some(p) = panel() {
        p.window().request_redraw();
    }
}

/// 层关闭：丢掉它的动画（旧版没有退出动画）
pub fn stop(id: i32) {
    ANIMS.with(|m| m.borrow_mut().remove(&id));
}

fn ensure_tick() {
    TICK.with(|t| {
        if t.running() {
            return;
        }
        t.start(slint::TimerMode::Repeated, Duration::from_millis(15), || {
            let alive = ANIMS.with(|m| {
                let mut m = m.borrow_mut();
                // 结束后多留一帧，最后一帧是精确的终态
                m.retain(|_, a| a.start.elapsed().as_secs_f32() * 1000.0 < a.dur + 34.0);
                !m.is_empty()
            });
            if let Some(p) = panel() {
                p.window().request_redraw();
            }
            if !alive {
                TICK.with(|t| t.stop());
            }
        });
    });
}

pub fn animating() -> bool {
    ANIMS.with(|m| !m.borrow().is_empty())
}

fn pose(id: i32) -> Option<Pose> {
    ANIMS.with(|m| {
        let m = m.borrow();
        let a = m.get(&id)?;
        let t = a.start.elapsed().as_secs_f32() * 1000.0 / a.dur;
        if t >= 1.0 {
            return None;
        }
        let e = crate::fx::bezier(a.curve.0, a.curve.1, a.curve.2, a.curve.3, t);
        let f = a.from;
        Some(Pose { tx: f.tx * (1.0 - e), ty: f.ty * (1.0 - e), s: f.s + (1.0 - f.s) * e, op: (f.op + (1.0 - f.op) * e).clamp(0.0, 1.0), ox: f.ox, oy: f.oy })
    })
}

// ------------------------------------------------------------------ frame

pub enum Out {
    /// 普通单遍：渲染器给的脏区（物理 px，x, y, w, h）
    Partial(Option<(i32, i32, u32, u32)>),
    /// 合成了：整帧都变了
    Full,
}

fn clear(buf: &mut [Bgra]) {
    // SAFETY: Bgra 是 4 字节 repr(C)，全零 = 透明
    unsafe { std::ptr::write_bytes(buf.as_mut_ptr(), 0, buf.len()) };
}

/// 面板的一帧。`reused`：平台平时用 ReusedBuffer（只重画脏区）。
pub fn render(renderer: &SoftwareRenderer, buf: &mut [Bgra], w: usize, h: usize, scale: f32, reused: bool) -> Out {
    let Some(ui) = panel() else {
        let r = renderer.render(buf, w);
        return Out::Partial(region_box(&r));
    };
    let fx = ui.global::<Fx>();
    let layers = ui.get_fx_layers();
    let mut active: Vec<(crate::FxLayer, Pose)> = Vec::new();
    let mut any = false;
    for l in layers.iter() {
        if !l.shown {
            continue;
        }
        let p = pose(l.id).or_else(|| if l.parent != 0 { pose(l.parent).map(|p| Pose { tx: 0.0, ty: 0.0, s: 1.0, ..p }) } else { None });
        let needs = p.is_some() || l.blur > 0.0 || l.lq > 0.0;
        if needs || any {
            any = true;
            active.push((l, p.unwrap_or(Pose::ID)));
        }
    }
    let set_flags = |ids: &[i32]| {
        let has = |i: i32| ids.contains(&i);
        let pairs: [(i32, fn(&Fx, bool)); 8] = [
            (2, |f, v| f.set_a2(v)),
            (3, |f, v| f.set_a3(v)),
            (4, |f, v| f.set_a4(v)),
            (5, |f, v| f.set_a5(v)),
            (6, |f, v| f.set_a6(v)),
            (7, |f, v| f.set_a7(v)),
            (8, |f, v| f.set_a8(v)),
            (9, |f, v| f.set_a9(v)),
        ];
        for (i, set) in pairs {
            set(&fx, has(i));
        }
    };
    if active.is_empty() {
        if fx.get_pass() != 0 {
            fx.set_pass(0);
            set_flags(&[]);
        }
        if WAS.with(|w| w.replace(false)) {
            set_flags(&[]);
            // 上一帧是合成的：渲染器记的「上一帧」不是缓冲里的内容 → 整帧重画一次
            clear(buf);
            renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
            renderer.render(buf, w);
            if reused {
                renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
            }
            return Out::Full;
        }
        let r = renderer.render(buf, w);
        return Out::Partial(region_box(&r));
    }
    WAS.with(|x| x.set(true));
    let ids: Vec<i32> = active.iter().map(|(l, _)| l.id).collect();
    set_flags(&ids);
    renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
    fx.set_pass(1);
    clear(buf);
    renderer.render(buf, w);
    TMP.with(|t| {
        let mut tmp = t.borrow_mut();
        if tmp.len() != buf.len() {
            *tmp = vec![Bgra::default(); buf.len()];
        }
        for (l, p) in &active {
            fx.set_pass(l.id);
            clear(&mut tmp);
            renderer.render(&mut tmp[..], w);
            // 层矩形（物理 px）与变换
            let (lx, ly, lw, lh) = (l.x * scale, l.y * scale, l.w * scale, l.h * scale);
            let (ox, oy) = (lx + p.ox * lw, ly + p.oy * lh);
            let (tx, ty) = (p.tx * scale, p.ty * scale);
            let map = |x: f32, y: f32| (ox + (x - ox) * p.s + tx, oy + (y - oy) * p.s + ty);
            if l.blur > 0.0 || l.lq > 0.0 {
                let (x0, y0) = map(lx, ly);
                let (x1, y1) = map(lx + lw, ly + lh);
                // liquid 的 bevel = min(14, h/2)（CSS px）
                let bevel = 14.0f32.min(l.h / 2.0) * scale * p.s;
                backdrop(buf, w, h, (x0, y0, x1, y1), l.r * scale * p.s, l.blur * scale * p.s, l.sat, l.lq * scale * p.s, bevel, p.op);
            }
            if p.identity() {
                over_all(buf, &tmp);
            } else {
                // 层内容（含阴影）可能超出层矩形：按矩形外扩 64px 的变换包围盒采样
                let m = 64.0 * scale;
                let (bx0, by0) = map(lx - m, ly - m);
                let (bx1, by1) = map(lx + lw + m, ly + lh + m);
                xform_into(buf, &tmp, w, h, (bx0, by0, bx1, by1), ox, oy, p.s, tx, ty, p.op);
            }
        }
    });
    // 帧与帧之间回到普通遍：否则基底一直是 visible:false，期间来的指针 / 按键事件会让 Slint 把
    // 「变得不可见」的焦点项（输入框、表情搜索框）夺走焦点（toast 的背景模糊会让合成持续 2.6 s）。
    // 由此引起的重绘请求由调用方丢掉（Out::Full 之后清 needs_redraw）。
    fx.set_pass(0);
    if reused {
        renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
    }
    Out::Full
}

fn region_box(r: &slint::platform::software_renderer::PhysicalRegion) -> Option<(i32, i32, u32, u32)> {
    let o = r.bounding_box_origin();
    let s = r.bounding_box_size();
    if s.width == 0 || s.height == 0 {
        None
    } else {
        Some((o.x, o.y, s.width, s.height))
    }
}

#[inline]
fn over_px(d: &mut Bgra, s: Bgra) {
    if s.a == 0 {
        return;
    }
    if s.a == 255 {
        *d = s;
        return;
    }
    let ia = 255 - s.a as u32;
    d.r = (s.r as u32 + (d.r as u32 * ia + 127) / 255).min(255) as u8;
    d.g = (s.g as u32 + (d.g as u32 * ia + 127) / 255).min(255) as u8;
    d.b = (s.b as u32 + (d.b as u32 * ia + 127) / 255).min(255) as u8;
    d.a = (s.a as u32 + (d.a as u32 * ia + 127) / 255).min(255) as u8;
}

fn over_all(dst: &mut [Bgra], src: &[Bgra]) {
    for (d, s) in dst.iter_mut().zip(src) {
        over_px(d, *s);
    }
}

#[inline]
fn sample(src: &[Bgra], w: usize, h: usize, x: f32, y: f32) -> [f32; 4] {
    // 双线性（像素中心在 +0.5）
    let fx = x - 0.5;
    let fy = y - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let ax = fx - x0;
    let ay = fy - y0;
    let mut acc = [0f32; 4];
    for (dy, wy) in [(0i32, 1.0 - ay), (1, ay)] {
        let yy = y0 as i32 + dy;
        if yy < 0 || yy >= h as i32 || wy == 0.0 {
            continue;
        }
        for (dx, wx) in [(0i32, 1.0 - ax), (1, ax)] {
            let xx = x0 as i32 + dx;
            if xx < 0 || xx >= w as i32 || wx == 0.0 {
                continue;
            }
            let p = src[yy as usize * w + xx as usize];
            let k = wx * wy;
            acc[0] += p.b as f32 * k;
            acc[1] += p.g as f32 * k;
            acc[2] += p.r as f32 * k;
            acc[3] += p.a as f32 * k;
        }
    }
    acc
}

/// src 按「以 (ox,oy) 缩放 s 再平移 (tx,ty)」变换后叠到 dst 上（只算包围盒内），乘 opacity
#[allow(clippy::too_many_arguments)]
fn xform_into(dst: &mut [Bgra], src: &[Bgra], w: usize, h: usize, bbox: (f32, f32, f32, f32), ox: f32, oy: f32, s: f32, tx: f32, ty: f32, op: f32) {
    if s <= 0.001 || op <= 0.001 {
        return;
    }
    let x0 = bbox.0.min(bbox.2).floor().max(0.0) as usize;
    let y0 = bbox.1.min(bbox.3).floor().max(0.0) as usize;
    let x1 = (bbox.0.max(bbox.2).ceil() as usize).min(w);
    let y1 = (bbox.1.max(bbox.3).ceil() as usize).min(h);
    for y in y0..y1 {
        let sy = oy + (y as f32 + 0.5 - ty - oy) / s;
        for x in x0..x1 {
            let sx = ox + (x as f32 + 0.5 - tx - ox) / s;
            let c = sample(src, w, h, sx, sy);
            if c[3] < 0.5 {
                continue;
            }
            let p = Bgra { b: (c[0] * op + 0.5) as u8, g: (c[1] * op + 0.5) as u8, r: (c[2] * op + 0.5) as u8, a: (c[3] * op + 0.5) as u8 };
            over_px(&mut dst[y * w + x], p);
        }
    }
}

/// 整帧变换（面板弹出/收起）：dst = src 以中心缩放 s、平移 (tx,ty)、乘 opacity；dst 先清空
pub fn xform_frame(src: &[Bgra], dst: &mut [Bgra], w: usize, h: usize, s: f32, tx: f32, ty: f32, op: f32) {
    clear(dst);
    if (s - 1.0).abs() < 0.0005 && tx.abs() < 0.01 && ty.abs() < 0.01 {
        if op >= 0.999 {
            dst.copy_from_slice(src);
        } else {
            let k = (op.clamp(0.0, 1.0) * 256.0) as u32;
            for (d, p) in dst.iter_mut().zip(src) {
                *d = Bgra { b: ((p.b as u32 * k) >> 8) as u8, g: ((p.g as u32 * k) >> 8) as u8, r: ((p.r as u32 * k) >> 8) as u8, a: ((p.a as u32 * k) >> 8) as u8 };
            }
        }
        return;
    }
    let (ox, oy) = (w as f32 / 2.0, h as f32 / 2.0);
    xform_into(dst, src, w, h, (0.0, 0.0, w as f32, h as f32), ox, oy, s, tx, ty, op.clamp(0.0, 1.0));
}

// ------------------------------------------------------------------ backdrop-filter

fn rrect_cov(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let (w, h) = (x1 - x0, y1 - y0);
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let qx = (px - (x0 + x1) / 2.0).abs() - (w / 2.0 - r);
    let qy = (py - (y0 + y1) / 2.0).abs() - (h / 2.0 - r);
    let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
    (0.5 - d).clamp(0.0, 1.0)
}

/// liquid.ts 的位移图：圆角矩形边缘 bevel 内向中心弯折，强度 (1 - d/bevel)²，方向 = 外法线
fn liquid_n(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, bevel: f32) -> (f32, f32, f32) {
    let (hx, hy) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let r = r.min(hx).min(hy);
    let (fx, fy) = (px - (x0 + hx), py - (y0 + hy));
    let qx = fx.abs() - (hx - r);
    let qy = fy.abs() - (hy - r);
    let out = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
    let d = -out;
    if d < 0.0 || d >= bevel {
        return (0.0, 0.0, 0.0);
    }
    let (mut nx, mut ny) = if qx > 0.0 && qy > 0.0 {
        let l = (qx * qx + qy * qy).sqrt().max(1e-6);
        (qx / l, qy / l)
    } else if qx > qy {
        (1.0, 0.0)
    } else {
        (0.0, 1.0)
    };
    nx *= fx.signum();
    ny *= fy.signum();
    (nx, ny, (1.0 - d / bevel).powi(2))
}

/// 在 dst 的 rect 区域做 backdrop-filter：[liquid 位移] → blur(σ) → saturate(sat)，裁成圆角，按 opacity 混合
#[allow(clippy::too_many_arguments)]
fn backdrop(dst: &mut [Bgra], w: usize, h: usize, rect: (f32, f32, f32, f32), radius: f32, sigma: f32, sat: f32, depth: f32, bevel: f32, op: f32) {
    let (rx0, ry0, rx1, ry1) = (rect.0.min(rect.2), rect.1.min(rect.3), rect.0.max(rect.2), rect.1.max(rect.3));
    let pad = (sigma * 3.0).ceil() as i32 + 2;
    let ix0 = (rx0.floor() as i32 - pad).max(0) as usize;
    let iy0 = (ry0.floor() as i32 - pad).max(0) as usize;
    let ix1 = ((rx1.ceil() as i32 + pad).max(0) as usize).min(w);
    let iy1 = ((ry1.ceil() as i32 + pad).max(0) as usize).min(h);
    if ix1 <= ix0 || iy1 <= iy0 {
        return;
    }
    let (bw, bh) = (ix1 - ix0, iy1 - iy0);
    // 平面 f32（预乘）
    let mut ch: [Vec<f32>; 4] = std::array::from_fn(|_| vec![0f32; bw * bh]);
    for y in 0..bh {
        for x in 0..bw {
            let p = dst[(iy0 + y) * w + ix0 + x];
            let k = y * bw + x;
            ch[0][k] = p.b as f32;
            ch[1][k] = p.g as f32;
            ch[2][k] = p.r as f32;
            ch[3][k] = p.a as f32;
        }
    }
    if depth > 0.0 {
        let src = ch.clone();
        for y in 0..bh {
            let py = (iy0 + y) as f32 + 0.5;
            if py < ry0 || py > ry1 {
                continue;
            }
            for x in 0..bw {
                let px = (ix0 + x) as f32 + 0.5;
                if px < rx0 || px > rx1 {
                    continue;
                }
                let (nx, ny, k) = liquid_n(px, py, rx0, ry0, rx1, ry1, radius, bevel);
                if k == 0.0 {
                    continue;
                }
                // 从更靠里处取样（feDisplacementMap scale = depth×2，R = 128 − nx·k·127）
                let sx = (px - depth * nx * k).clamp(rx0, rx1) - ix0 as f32;
                let sy = (py - depth * ny * k).clamp(ry0, ry1) - iy0 as f32;
                let i = y * bw + x;
                for c in 0..4 {
                    ch[c][i] = bilinear(&src[c], bw, bh, sx, sy);
                }
            }
        }
    }
    if sigma >= 0.3 {
        for c in ch.iter_mut() {
            blur_clamped(c, bw, bh, sigma);
        }
    }
    if (sat - 1.0).abs() > 0.001 {
        let s = sat;
        let m = [
            [0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s],
            [0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s],
            [0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s],
        ];
        for i in 0..bw * bh {
            let (r, g, b) = (ch[2][i], ch[1][i], ch[0][i]);
            let a = ch[3][i];
            ch[2][i] = (m[0][0] * r + m[0][1] * g + m[0][2] * b).clamp(0.0, a);
            ch[1][i] = (m[1][0] * r + m[1][1] * g + m[1][2] * b).clamp(0.0, a);
            ch[0][i] = (m[2][0] * r + m[2][1] * g + m[2][2] * b).clamp(0.0, a);
        }
    }
    let op = op.clamp(0.0, 1.0);
    for y in 0..bh {
        let py = (iy0 + y) as f32 + 0.5;
        for x in 0..bw {
            let px = (ix0 + x) as f32 + 0.5;
            let c = rrect_cov(px, py, rx0, ry0, rx1, ry1, radius) * op;
            if c <= 0.0 {
                continue;
            }
            let i = y * bw + x;
            let d = &mut dst[(iy0 + y) * w + ix0 + x];
            let mixv = |o: u8, n: f32| (o as f32 + (n - o as f32) * c).round().clamp(0.0, 255.0) as u8;
            d.b = mixv(d.b, ch[0][i]);
            d.g = mixv(d.g, ch[1][i]);
            d.r = mixv(d.r, ch[2][i]);
            d.a = mixv(d.a, ch[3][i]);
        }
    }
}

fn bilinear(src: &[f32], w: usize, h: usize, x: f32, y: f32) -> f32 {
    let fx = (x - 0.5).clamp(0.0, (w - 1) as f32);
    let fy = (y - 0.5).clamp(0.0, (h - 1) as f32);
    let x0 = fx.floor() as usize;
    let y0 = fy.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let ax = fx - x0 as f32;
    let ay = fy - y0 as f32;
    let a = src[y0 * w + x0] * (1.0 - ax) + src[y0 * w + x1] * ax;
    let b = src[y1 * w + x0] * (1.0 - ax) + src[y1 * w + x1] * ax;
    a * (1.0 - ay) + b * ay
}

/// 高斯近似（三次盒式），边缘取边上的值；σ 大时先 2×/4× 降采样再升回来（大半径下看不出差别）
fn blur_clamped(buf: &mut [f32], w: usize, h: usize, sigma: f32) {
    let f = if sigma > 12.0 { 4 } else if sigma > 6.0 { 2 } else { 1 };
    if f == 1 {
        blur_full(buf, w, h, sigma);
        return;
    }
    let (sw, sh) = (w.div_ceil(f), h.div_ceil(f));
    let mut small = vec![0f32; sw * sh];
    for y in 0..sh {
        for x in 0..sw {
            let mut acc = 0f32;
            let mut n = 0f32;
            for yy in y * f..((y + 1) * f).min(h) {
                for xx in x * f..((x + 1) * f).min(w) {
                    acc += buf[yy * w + xx];
                    n += 1.0;
                }
            }
            small[y * sw + x] = acc / n.max(1.0);
        }
    }
    blur_full(&mut small, sw, sh, sigma / f as f32);
    for y in 0..h {
        for x in 0..w {
            let sx = (x as f32 + 0.5) / f as f32;
            let sy = (y as f32 + 0.5) / f as f32;
            buf[y * w + x] = bilinear(&small, sw, sh, sx, sy);
        }
    }
}

fn blur_full(buf: &mut [f32], w: usize, h: usize, sigma: f32) {
    if w == 0 || h == 0 {
        return;
    }
    let mut tmp = vec![0f32; buf.len()];
    for b in crate::fx::boxes_for_gauss(sigma, 3) {
        let r = (b - 1) / 2;
        if r == 0 {
            continue;
        }
        let k = 1.0 / (2 * r + 1) as f32;
        // 横
        for y in 0..h {
            let row = &buf[y * w..(y + 1) * w];
            let at = |i: i64| row[i.clamp(0, w as i64 - 1) as usize];
            let mut acc: f32 = (-(r as i64)..=r as i64).map(at).sum();
            for x in 0..w {
                tmp[y * w + x] = acc * k;
                acc += at(x as i64 + r as i64 + 1) - at(x as i64 - r as i64);
            }
        }
        // 竖
        for x in 0..w {
            let at = |i: i64| tmp[i.clamp(0, h as i64 - 1) as usize * w + x];
            let mut acc: f32 = (-(r as i64)..=r as i64).map(at).sum();
            for y in 0..h {
                buf[y * w + x] = acc * k;
                acc += at(y as i64 + r as i64 + 1) - at(y as i64 - r as i64);
            }
        }
    }
}
