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
    ANIMS.with(|m| !m.borrow().is_empty()) || LAYER_ANIM.with(|a| a.get())
}

/// 合成帧之后要不要接着画（层里有动画在跑）
pub fn layer_anim() -> bool {
    LAYER_ANIM.with(|a| a.get())
}

thread_local! {
    /// 上一个合成帧里有 Slint 属性动画在跑。分层合成时渲染器只记得最后一层读过的属性，别的层里的动画
    /// 推进时不会自己请求重画（has_active_animations 也在下一次 update 时清掉），所以由这里接着要帧。
    static LAYER_ANIM: Cell<bool> = const { Cell::new(false) };
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
    if !stats_on() {
        return render_inner(renderer, buf, w, h, scale, reused);
    }
    thread_local! { static LAST: Cell<Option<Instant>> = const { Cell::new(None) }; }
    let t = Instant::now();
    let out = render_inner(renderer, buf, w, h, scale, reused);
    let gap = LAST.with(|l| l.replace(Some(t))).map(|p| t.duration_since(p).as_secs_f32() * 1000.0).unwrap_or(0.0);
    let ph = PH.with(|p| std::mem::take(&mut *p.borrow_mut()));
    log::info!("frame {:.1} ms (gap {:.1} ms){}{ph}", t.elapsed().as_secs_f32() * 1000.0, gap, if matches!(out, Out::Full) { " composed" } else { "" });
    out
}

fn render_inner(renderer: &SoftwareRenderer, buf: &mut [Bgra], w: usize, h: usize, scale: f32, reused: bool) -> Out {
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
        // 常驻缩放（拖拽时内容 scale(.97)）
        let p = if l.shrink > 0.0005 { Some(Pose { s: (1.0 - l.shrink) * p.map_or(1.0, |p| p.s), ..p.unwrap_or(Pose::ID) }) } else { p };
        let needs = p.is_some() || l.blur > 0.0 || l.lq > 0.0;
        if needs || any {
            any = true;
            active.push((l, p.unwrap_or(Pose::ID)));
        }
    }
    let set_flags = |ids: &[i32]| {
        let has = |i: i32| ids.contains(&i);
        let pairs: [(i32, fn(&Fx, bool)); 9] = [
            (2, |f, v| f.set_a2(v)),
            (3, |f, v| f.set_a3(v)),
            (4, |f, v| f.set_a4(v)),
            (5, |f, v| f.set_a5(v)),
            (6, |f, v| f.set_a6(v)),
            (7, |f, v| f.set_a7(v)),
            (8, |f, v| f.set_a8(v)),
            (9, |f, v| f.set_a9(v)),
            (10, |f, v| f.set_a10(v)),
        ];
        for (i, set) in pairs {
            set(&fx, has(i));
        }
    };
    let shown: Vec<i32> = active.iter().filter(|(l, _)| l.blur > 0.0 || l.lq > 0.0).map(|(l, _)| l.id).collect();
    drop_bd_cache(&shown);
    if active.is_empty() {
        LAYER_ANIM.with(|a| a.set(false));
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
    let t0 = Instant::now();
    renderer.render(buf, w);
    phase("base", t0);
    TMP.with(|t| {
        let mut tmp = t.borrow_mut();
        if tmp.len() != buf.len() {
            *tmp = vec![Bgra::default(); buf.len()];
        }
        for (l, p) in &active {
            fx.set_pass(l.id);
            clear(&mut tmp);
            let t0 = Instant::now();
            renderer.render(&mut tmp[..], w);
            if stats_on() {
                phase(&format!("r{}", l.id), t0);
            }
            let t0 = Instant::now();
            // 层矩形（物理 px）与变换
            let (lx, ly, lw, lh) = (l.x * scale, l.y * scale, l.w * scale, l.h * scale);
            let (ox, oy) = (lx + p.ox * lw, ly + p.oy * lh);
            let (tx, ty) = (p.tx * scale, p.ty * scale);
            let map = |x: f32, y: f32| (ox + (x - ox) * p.s + tx, oy + (y - oy) * p.s + ty);
            if l.blur > 0.0 && l.lq <= 0.0 {
                // 纯模糊（表情面板、拖放选择器）：底下的模糊结果与层的姿态无关，按层的静止矩形外扩一圈算一次缓存，
                // 弹出动画的每帧只按当前矩形裁圆角、乘透明度贴上（模糊半径取静止时的值）
                let (x0, y0) = map(lx, ly);
                let (x1, y1) = map(lx + lw, ly + lh);
                let mg = lw.max(lh) * 0.12 + 16.0 * scale;
                let region = (
                    (lx - mg).floor().max(0.0) as usize,
                    (ly - mg).floor().max(0.0) as usize,
                    ((lx + lw + mg).ceil().max(0.0) as usize).min(w),
                    ((ly + lh + mg).ceil().max(0.0) as usize).min(h),
                );
                backdrop_static(l.id, buf, w, h, region, (x0, y0, x1, y1), l.r * scale * p.s, l.blur * scale, l.sat, p.op);
            } else if l.blur > 0.0 || l.lq > 0.0 {
                let (x0, y0) = map(lx, ly);
                let (x1, y1) = map(lx + lw, ly + lh);
                // liquid 的 bevel = min(14, h/2)（CSS px）
                let bevel = 14.0f32.min(l.h / 2.0) * scale * p.s;
                backdrop_cached(l.id, buf, w, h, (x0, y0, x1, y1), l.r * scale * p.s, l.blur * scale * p.s, l.sat, l.lq * scale * p.s, bevel, p.op);
            }
            if p.identity() {
                let m = 96.0 * scale;
                over_rect(buf, &tmp, w, h, (lx - m, ly - m, lx + lw + m, ly + lh + m));
            } else {
                // 层内容（含阴影）可能超出层矩形：按矩形外扩 64px 的变换包围盒采样
                let m = 64.0 * scale;
                let (bx0, by0) = map(lx - m, ly - m);
                let (bx1, by1) = map(lx + lw + m, ly + lh + m);
                xform_into(buf, &tmp, w, h, (bx0, by0, bx1, by1), ox, oy, p.s, tx, ty, p.op);
            }
            if stats_on() {
                phase(&format!("c{}", l.id), t0);
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
    let more = ui.window().has_active_animations();
    LAYER_ANIM.with(|a| a.set(more));
    if more {
        ui.window().request_redraw();
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

/// 只叠一个矩形（层矩形外扩阴影边距）：小层（提示、toast）不必扫整幅
fn over_rect(dst: &mut [Bgra], src: &[Bgra], w: usize, h: usize, r: (f32, f32, f32, f32)) {
    let x0 = r.0.floor().max(0.0) as usize;
    let y0 = r.1.floor().max(0.0) as usize;
    let x1 = (r.2.ceil().max(0.0) as usize).min(w);
    let y1 = (r.3.ceil().max(0.0) as usize).min(h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    if x0 == 0 && y0 == 0 && x1 == w && y1 == h {
        return over_all(dst, src);
    }
    for y in y0..y1 {
        let (d, s) = (&mut dst[y * w + x0..y * w + x1], &src[y * w + x0..y * w + x1]);
        for (d, s) in d.iter_mut().zip(s) {
            over_px(d, *s);
        }
    }
}

/// src 按「以 (ox,oy) 缩放 s 再平移 (tx,ty)」变换后叠到 dst 上（只算包围盒内），乘 opacity。
/// 缩放是各向同性的：每列 / 每行的源坐标和权重先算好，像素里只做定点双线性。
#[allow(clippy::too_many_arguments)]
fn xform_into(dst: &mut [Bgra], src: &[Bgra], w: usize, h: usize, bbox: (f32, f32, f32, f32), ox: f32, oy: f32, s: f32, tx: f32, ty: f32, op: f32) {
    if s <= 0.001 || op <= 0.001 {
        return;
    }
    let x0 = bbox.0.min(bbox.2).floor().max(0.0) as usize;
    let y0 = bbox.1.min(bbox.3).floor().max(0.0) as usize;
    let x1 = (bbox.0.max(bbox.2).ceil().max(0.0) as usize).min(w);
    let y1 = (bbox.1.max(bbox.3).ceil().max(0.0) as usize).min(h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    // 源坐标（像素中心在 +0.5）→ (左/上邻居下标, 右/下邻居的 8 位权重)
    let map = |d: usize, o: f32, t: f32| {
        let v = o + (d as f32 + 0.5 - t - o) / s - 0.5;
        let f = v.floor();
        (f as i32, ((v - f) * 256.0).round().min(256.0) as u32)
    };
    let cols: Vec<(i32, u32)> = (x0..x1).map(|x| map(x, ox, tx)).collect();
    let opk = (op.clamp(0.0, 1.0) * 256.0).round() as u32;
    let (wi, hi) = (w as i32, h as i32);
    let px = |x: i32, y: i32| -> [u32; 4] {
        if x < 0 || y < 0 || x >= wi || y >= hi {
            [0; 4]
        } else {
            let p = src[y as usize * w + x as usize];
            [p.b as u32, p.g as u32, p.r as u32, p.a as u32]
        }
    };
    for y in y0..y1 {
        let (sy, wy) = map(y, oy, ty);
        if sy < -1 || sy >= hi {
            continue;
        }
        let row = &mut dst[y * w..(y + 1) * w];
        for (i, x) in (x0..x1).enumerate() {
            let (sx, wx) = cols[i];
            if sx < -1 || sx >= wi {
                continue;
            }
            let (p00, p01, p10, p11) = (px(sx, sy), px(sx + 1, sy), px(sx, sy + 1), px(sx + 1, sy + 1));
            let mut c = [0u32; 4];
            for k in 0..4 {
                let top = p00[k] * (256 - wx) + p01[k] * wx;
                let bot = p10[k] * (256 - wx) + p11[k] * wx;
                c[k] = ((top * (256 - wy) + bot * wy + (1 << 15)) >> 16) * opk >> 8;
            }
            if c[3] == 0 {
                continue;
            }
            over_px(&mut row[x], Bgra { b: c[0].min(255) as u8, g: c[1].min(255) as u8, r: c[2].min(255) as u8, a: c[3].min(255) as u8 });
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
/// 背景滤镜的结果按层缓存：下面的已合成像素和参数都没变（拖放选择器悬停时选择器底下的面板是静止的）
/// 就直接贴上一帧的结果，省掉整幅高斯模糊。
struct BdCache {
    key: [f32; 10],
    w: usize,
    input: Vec<Bgra>,
    output: Vec<Bgra>,
}

thread_local! {
    static PH: RefCell<String> = const { RefCell::new(String::new()) };
}

fn stats_on() -> bool {
    static STATS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *STATS.get_or_init(|| std::env::var_os("LANCAT_FRAME_STATS").is_some())
}

fn phase(name: &str, t: Instant) {
    if stats_on() {
        PH.with(|p| p.borrow_mut().push_str(&format!(" {name}={:.1}", t.elapsed().as_secs_f32() * 1000.0)));
    }
}

thread_local! {
    static BD: RefCell<HashMap<i32, BdCache>> = RefCell::new(HashMap::new());
}

#[allow(clippy::too_many_arguments)]
fn backdrop_cached(id: i32, dst: &mut [Bgra], w: usize, h: usize, rect: (f32, f32, f32, f32), radius: f32, sigma: f32, sat: f32, depth: f32, bevel: f32, op: f32) {
    let key = [rect.0, rect.1, rect.2, rect.3, radius, sigma, sat, depth, bevel, op];
    let t = Instant::now();
    // None = 要整幅重算；Some(None) = 全中；Some(Some(win)) = 只有这块输入变了（已外扩成受影响的输出范围）
    let plan: Option<Option<(usize, usize, usize, usize)>> = BD.with(|m| {
        let m = m.borrow();
        let c = m.get(&id)?;
        if c.key != key || c.w != w || c.input.len() != dst.len() {
            return None;
        }
        // 逐行比，记下变化的外接框
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        for y in 0..h {
            let (a, b) = (&c.input[y * w..(y + 1) * w], &dst[y * w..(y + 1) * w]);
            if a == b {
                continue;
            }
            let l = a.iter().zip(b).position(|(p, q)| p != q).unwrap_or(0);
            let r = w - a.iter().rev().zip(b.iter().rev()).position(|(p, q)| p != q).unwrap_or(0);
            x0 = x0.min(l);
            x1 = x1.max(r);
            y0 = y0.min(y);
            y1 = y1.max(y + 1);
        }
        if x0 == usize::MAX {
            return Some(None);
        }
        let r = backdrop_reach(sigma);
        let win = (x0.saturating_sub(r), y0.saturating_sub(r), (x1 + r).min(w), (y1 + r).min(h));
        if stats_on() {
            PH.with(|p| p.borrow_mut().push_str(&format!(" d{id}=[{x0},{y0}..{x1},{y1}]")));
        }
        // 液态位移会从层内别处取样：变化碰到层（含读取外扩）就整块重算；碰不到时局部路径什么也不用算
        if depth > 0.0 {
            let (rx0, ry0, rx1, ry1) = (rect.0.min(rect.2), rect.1.min(rect.3), rect.0.max(rect.2), rect.1.max(rect.3));
            let touches = (win.0 as f32) < rx1 + r as f32 && (win.2 as f32) > rx0 - r as f32 && (win.1 as f32) < ry1 + r as f32 && (win.3 as f32) > ry0 - r as f32;
            return if touches { None } else { Some(Some(win)) };
        }
        if (win.2 - win.0) * (win.3 - win.1) * 2 > w * h {
            return None;
        }
        Some(Some(win))
    });
    match plan {
        Some(None) => {
            BD.with(|m| dst.copy_from_slice(&m.borrow()[&id].output));
            phase("bd-hit", t);
        }
        Some(Some(win)) => {
            let input = dst.to_vec();
            backdrop(dst, w, h, rect, radius, sigma, sat, depth, bevel, op, Some(win));
            BD.with(|m| {
                let mut m = m.borrow_mut();
                let c = m.get_mut(&id).expect("cache entry");
                // 窗口外贴上一帧的结果，窗口内是刚算的
                for y in 0..h {
                    let row = y * w;
                    if y < win.1 || y >= win.3 {
                        dst[row..row + w].copy_from_slice(&c.output[row..row + w]);
                    } else {
                        dst[row..row + win.0].copy_from_slice(&c.output[row..row + win.0]);
                        dst[row + win.2..row + w].copy_from_slice(&c.output[row + win.2..row + w]);
                    }
                }
                c.input = input;
                c.output.copy_from_slice(dst);
            });
            phase("bd-part", t);
        }
        None => {
            let input = dst.to_vec();
            backdrop(dst, w, h, rect, radius, sigma, sat, depth, bevel, op, None);
            BD.with(|m| {
                let mut m = m.borrow_mut();
                let c = m.entry(id).or_insert_with(|| BdCache { key, w, input: Vec::new(), output: Vec::new() });
                c.key = key;
                c.w = w;
                c.input = input;
                c.output.clear();
                c.output.extend_from_slice(dst);
            });
            phase("bd-full", t);
        }
    }
}

/// 层关闭后丢掉它的滤镜缓存（不常驻几 MB）
fn drop_bd_cache(shown: &[i32]) {
    BD.with(|m| m.borrow_mut().retain(|id, _| shown.contains(id)));
    BS.with(|m| m.borrow_mut().retain(|id, _| shown.contains(id)));
}

/// 纯模糊层的缓存：region 内「模糊 + 饱和度」之后的像素（与层的当前姿态、圆角、透明度无关）
struct BsCache {
    key: [f32; 6],
    w: usize,
    input: Vec<Bgra>,
    filt: Vec<Bgra>,
}

thread_local! {
    static BS: RefCell<HashMap<i32, BsCache>> = RefCell::new(HashMap::new());
}

#[allow(clippy::too_many_arguments)]
fn backdrop_static(id: i32, dst: &mut [Bgra], w: usize, h: usize, region: (usize, usize, usize, usize), rect: (f32, f32, f32, f32), radius: f32, sigma: f32, sat: f32, op: f32) {
    let (gx0, gy0, gx1, gy1) = region;
    if gx1 <= gx0 || gy1 <= gy0 || op <= 0.001 {
        return;
    }
    let key = [gx0 as f32, gy0 as f32, gx1 as f32, gy1 as f32, sigma, sat];
    let reach = backdrop_reach(sigma);
    let t = Instant::now();
    // None = 整块重算；Some(None) = 输入没变；Some(Some(win)) = 只重算 win
    let plan: Option<Option<(usize, usize, usize, usize)>> = BS.with(|m| {
        let m = m.borrow();
        let c = m.get(&id)?;
        if c.key != key || c.w != w || c.input.len() != dst.len() {
            return None;
        }
        let (ry0, ry1) = (gy0.saturating_sub(reach), (gy1 + reach).min(h));
        let (rx0, rx1) = (gx0.saturating_sub(reach), (gx1 + reach).min(w));
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        for y in ry0..ry1 {
            let (a, b) = (&c.input[y * w + rx0..y * w + rx1], &dst[y * w + rx0..y * w + rx1]);
            if a == b {
                continue;
            }
            let l = a.iter().zip(b).position(|(p, q)| p != q).unwrap_or(0);
            let r = a.len() - a.iter().rev().zip(b.iter().rev()).position(|(p, q)| p != q).unwrap_or(0);
            x0 = x0.min(rx0 + l);
            x1 = x1.max(rx0 + r);
            y0 = y0.min(y);
            y1 = y1.max(y + 1);
        }
        if x0 == usize::MAX {
            return Some(None);
        }
        let win = (x0.saturating_sub(reach).max(gx0), y0.saturating_sub(reach).max(gy0), (x1 + reach).min(gx1), (y1 + reach).min(gy1));
        if win.2 <= win.0 || win.3 <= win.1 {
            return Some(None);
        }
        if (win.2 - win.0) * (win.3 - win.1) * 2 > (gx1 - gx0) * (gy1 - gy0) {
            return None;
        }
        Some(Some(win))
    });
    let recompute = |win: (usize, usize, usize, usize), full: bool| {
        let mut tmp = dst.to_vec();
        backdrop(&mut tmp, w, h, (gx0 as f32, gy0 as f32, gx1 as f32, gy1 as f32), 0.0, sigma, sat, 0.0, 0.0, 1.0, Some(win));
        BS.with(|m| {
            let mut m = m.borrow_mut();
            let c = m.entry(id).or_insert_with(|| BsCache { key, w, input: Vec::new(), filt: Vec::new() });
            if full || c.filt.len() != dst.len() {
                c.filt = vec![Bgra::default(); dst.len()];
            }
            for y in win.1..win.3 {
                c.filt[y * w + win.0..y * w + win.2].copy_from_slice(&tmp[y * w + win.0..y * w + win.2]);
            }
            c.key = key;
            c.w = w;
            c.input.clear();
            c.input.extend_from_slice(dst);
        });
    };
    match plan {
        Some(None) => phase("bs-hit", t),
        Some(Some(win)) => {
            recompute(win, false);
            phase("bs-part", t);
        }
        None => {
            recompute(region, true);
            phase("bs-full", t);
        }
    }
    // 按当前矩形（圆角）× 透明度贴上
    let t = Instant::now();
    let (rx0, ry0, rx1, ry1) = (rect.0.min(rect.2), rect.1.min(rect.3), rect.0.max(rect.2), rect.1.max(rect.3));
    let x0 = (rx0.floor().max(0.0) as usize).max(gx0);
    let y0 = (ry0.floor().max(0.0) as usize).max(gy0);
    let x1 = (rx1.ceil().max(0.0) as usize).min(gx1);
    let y1 = (ry1.ceil().max(0.0) as usize).min(gy1);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let op = op.clamp(0.0, 1.0);
    let r = radius.min((rx1 - rx0) / 2.0).min((ry1 - ry0) / 2.0).max(0.0);
    BS.with(|m| {
        let m = m.borrow();
        let Some(c) = m.get(&id) else { return };
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            // 不在圆角带里的行：整行内部覆盖率只看左右边
            let in_band = py < ry0 + r || py > ry1 - r;
            let row = y * w;
            for x in x0..x1 {
                let px = x as f32 + 0.5;
                let cov = if !in_band && px > rx0 + r.max(1.0) && px < rx1 - r.max(1.0) {
                    ((py - ry0).min(ry1 - py) + 0.5).clamp(0.0, 1.0)
                } else {
                    rrect_cov(px, py, rx0, ry0, rx1, ry1, radius)
                };
                let k = cov * op;
                if k <= 0.0 {
                    continue;
                }
                let n = c.filt[row + x];
                let d = &mut dst[row + x];
                if k >= 0.999 {
                    *d = n;
                    continue;
                }
                let ki = (k * 256.0) as i32;
                let mixv = |o: u8, n: u8| (o as i32 + (((n as i32 - o as i32) * ki) >> 8)).clamp(0, 255) as u8;
                d.b = mixv(d.b, n.b);
                d.g = mixv(d.g, n.g);
                d.r = mixv(d.r, n.r);
                d.a = mixv(d.a, n.a);
            }
        }
    });
    phase("bs-mix", t);
}

/// 模糊的降采样倍数（与 blur_clamped 相同）
fn blur_factor(sigma: f32) -> usize {
    if sigma < 0.3 {
        1
    } else if sigma > 12.0 {
        4
    } else if sigma > 6.0 {
        2
    } else {
        1
    }
}

/// 背景滤镜的影响半径（物理 px）：输出像素只取决于这个距离内的输入
fn backdrop_reach(sigma: f32) -> usize {
    (sigma * 3.0).ceil() as usize + 2 + 2 * blur_factor(sigma)
}

/// `win`：只重算这个矩形里的输出（物理 px，右下不含）；别处的 dst 不动。读取范围自动外扩，降采样网格与整幅时对齐。
#[allow(clippy::too_many_arguments)]
fn backdrop(dst: &mut [Bgra], w: usize, h: usize, rect: (f32, f32, f32, f32), radius: f32, sigma: f32, sat: f32, depth: f32, bevel: f32, op: f32, win: Option<(usize, usize, usize, usize)>) {
    let (rx0, ry0, rx1, ry1) = (rect.0.min(rect.2), rect.1.min(rect.3), rect.0.max(rect.2), rect.1.max(rect.3));
    let pad = (sigma * 3.0).ceil() as i32 + 2;
    let mut ix0 = (rx0.floor() as i32 - pad).max(0) as usize;
    let mut iy0 = (ry0.floor() as i32 - pad).max(0) as usize;
    let mut ix1 = ((rx1.ceil() as i32 + pad).max(0) as usize).min(w);
    let mut iy1 = ((ry1.ceil() as i32 + pad).max(0) as usize).min(h);
    if ix1 <= ix0 || iy1 <= iy0 {
        return;
    }
    let (wx0, wy0, wx1, wy1) = win.unwrap_or((0, 0, w, h));
    if win.is_some() {
        let f = blur_factor(sigma);
        let r = backdrop_reach(sigma);
        let a0 = wx0.saturating_sub(r).max(ix0);
        let b0 = wy0.saturating_sub(r).max(iy0);
        ix0 += (a0 - ix0) / f * f;
        iy0 += (b0 - iy0) / f * f;
        ix1 = (wx1 + r).min(ix1);
        iy1 = (wy1 + r).min(iy1);
        if ix1 <= ix0 || iy1 <= iy0 {
            return;
        }
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
        // 四个通道互不相干：各开一个线程（整幅 σ14 的模糊单线程要 ~40 ms，拖放选择器每帧都可能要）
        if bw * bh >= 16 * 1024 {
            std::thread::scope(|sc| {
                for c in ch.iter_mut() {
                    sc.spawn(move || blur_clamped(c, bw, bh, sigma));
                }
            });
        } else {
            for c in ch.iter_mut() {
                blur_clamped(c, bw, bh, sigma);
            }
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
        if iy0 + y < wy0 || iy0 + y >= wy1 {
            continue;
        }
        let py = (iy0 + y) as f32 + 0.5;
        for x in 0..bw {
            if ix0 + x < wx0 || ix0 + x >= wx1 {
                continue;
            }
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
    let f = blur_factor(sigma);
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
