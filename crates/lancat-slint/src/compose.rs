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

thread_local! {
    /// 这一帧是在 GPU 上合成的（结果在 GPU 纹理里，CPU 缓冲不是这一帧）
    static GPU_FRAME: Cell<bool> = const { Cell::new(false) };
}

/// 上一次 render() 的结果在 GPU 上（win.rs：不用上传 CPU 缓冲，直接呈现）
pub fn gpu_frame() -> bool {
    GPU_FRAME.with(|g| g.get())
}

/// 有入场/弹出姿态动画在跑（高刷新率下由事件循环逐帧要帧，不靠 15 ms 的 tick）
pub fn pose_animating() -> bool {
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
    /// 增量合成：只有这块变了（x, y, w, h）；切换 Fx.pass 引起的重绘请求同样不算数
    Inc((i32, i32, u32, u32)),
}

impl Out {
    /// 合成过（调用方丢掉切换 pass 引起的重绘请求）
    pub fn composed(&self) -> bool {
        matches!(self, Out::Full | Out::Inc(_))
    }
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
    #[cfg(windows)]
    let ph = if crate::gpu::active() { format!("{ph} gpu present={:.1}", crate::gpu::last_present_ms()) } else { ph };
    log::info!("frame {:.1} ms (gap {:.1} ms){}{ph}", t.elapsed().as_secs_f32() * 1000.0, gap, if matches!(out, Out::Full) { " composed" } else { "" });
    out
}

fn render_inner(renderer: &SoftwareRenderer, buf: &mut [Bgra], w: usize, h: usize, scale: f32, reused: bool) -> Out {
    GPU_FRAME.with(|g| g.set(false));
    #[cfg(windows)]
    if crate::gpu::compose_enabled() {
        return render_gpu(renderer, buf, w, h, scale, reused);
    }
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
    let was = WAS.with(|x| x.replace(true));
    let ids: Vec<i32> = active.iter().map(|(l, _)| l.id).collect();
    set_flags(&ids);
    let sig = layer_sig(&active, w, h, scale);
    // 增量：单层、姿态不变、上一帧也是合成且缓存齐全 → 只重画真正变了的那块（悬停动画 30px 格子等）
    let mut cmp: Option<(Vec<Bgra>, (usize, usize, usize, usize))> = None;
    let inc_ok = reused
        && was
        && active.len() == 1
        && INC.with(|c| {
            let c = c.borrow();
            c.valid && c.sig == sig && c.base.len() == buf.len()
        })
        && TMP.with(|t| t.borrow().len() == buf.len());
    if inc_ok {
        // 帧与帧之间一直是 pass 0、且上一帧末尾已在 pass 0 登记过缓存：此时的脏区就是真实变化
        renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
        let t0 = Instant::now();
        let reg = renderer.render_by_line(Rearm);
        phase("probe", t0);
        let Some((dx, dy, dw, dh)) = region_box(&reg) else {
            return finish(&ui, None);
        };
        let d = (dx.max(0) as usize, dy.max(0) as usize, ((dx + dw as i32).max(0) as usize).min(w), ((dy + dh as i32).max(0) as usize).min(h));
        if stats_on() {
            PH.with(|p| p.borrow_mut().push_str(&format!(" d={},{} {}x{}", d.0, d.1, d.2 - d.0, d.3 - d.1)));
        }
        if d.2 > d.0 && d.3 > d.1 {
            if let Some(o) = incremental(renderer, &fx, buf, w, h, scale, &active[0], d) {
                fx.set_pass(0);
                if !inc_check() {
                    return finish_rearm(renderer, &ui, reused, Some(o));
                }
                // LANCAT_INC_CHECK=1：再整帧合成一次，逐像素比对增量结果
                cmp = Some((buf.to_vec(), o));
            }
            // 液态层且脏区落进它的取样范围：整帧合成
            fx.set_pass(0);
        } else {
            return finish(&ui, None);
        }
    }
    renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
    // 拖放选择器（层 5）的模糊底图冻结后，它下面的基底和各层都不再影响画面：直接用冻结时存下的合成结果，
    // 省掉每帧的基底 + 缩小内容层（约 15 ms）
    let i5 = active.iter().position(|(l, _)| l.id == 5);
    let frozen5 = i5.is_some() && bs_frozen(5, buf.len());
    if i5.is_none() {
        UNDER5.with(|u| u.borrow_mut().clear());
        FROZEN_TMP.with(|f| f.borrow_mut().clear());
    }
    let skip_below = frozen5
        && UNDER5.with(|u| {
            let u = u.borrow();
            u.len() == buf.len() && {
                buf.copy_from_slice(&u);
                true
            }
        });
    fx.set_pass(1);
    let t0 = Instant::now();
    if !skip_below {
        clear(buf);
        renderer.render(buf, w);
    }
    phase("base", t0);
    BASE_SKIP.with(|b| b.set(None));
    INC.with(|c| {
        let mut c = c.borrow_mut();
        c.base.clear();
        c.base.extend_from_slice(buf);
        c.sig = sig.clone();
        c.valid = active.len() == 1;
    });
    TMP.with(|t| {
        let mut tmp = t.borrow_mut();
        if tmp.len() != buf.len() {
            *tmp = vec![Bgra::default(); buf.len()];
        }
        for (k, (l, p)) in active.iter().enumerate() {
            if skip_below && Some(k) < i5 {
                continue;
            }
            let t0 = Instant::now();
            // 冻结期间层 5 以上的静态层（窗口边框层 8）内容不变：渲染结果缓存起来，不必每帧重画
            let cached = frozen5 && Some(k) > i5 && l.id != 6 && FROZEN_TMP.with(|f| {
                f.borrow().get(&l.id).is_some_and(|v| {
                    let ok = v.len() == tmp.len();
                    if ok {
                        tmp.copy_from_slice(v);
                    }
                    ok
                })
            });
            if !cached {
                fx.set_pass(l.id);
                clear(&mut tmp);
                renderer.render(&mut tmp[..], w);
                if frozen5 && Some(k) > i5 && l.id != 6 {
                    FROZEN_TMP.with(|f| f.borrow_mut().insert(l.id, tmp.to_vec()));
                }
            }
            if stats_on() {
                phase(&format!("r{}", l.id), t0);
            }
            let t0 = Instant::now();
            if Some(k) == i5 && frozen5 {
                // 层 5 冻结：「下面所有内容 + 模糊底图」整体存一份，之后每帧只叠层 5 自己的内容
                if !skip_below {
                    compose_layer(buf, &tmp, w, h, scale, l, p, (0, 0, w, h), None, Part::Backdrop);
                    UNDER5.with(|u| {
                        let mut u = u.borrow_mut();
                        u.clear();
                        u.extend_from_slice(buf);
                    });
                }
                compose_layer(buf, &tmp, w, h, scale, l, p, (0, 0, w, h), None, Part::Content);
            } else {
                compose_layer(buf, &tmp, w, h, scale, l, p, (0, 0, w, h), None, Part::All);
            }
            if stats_on() {
                phase(&format!("c{}", l.id), t0);
            }
        }
    });
    // 帧与帧之间回到普通遍：否则基底一直是 visible:false，期间来的指针 / 按键事件会让 Slint 把
    // 「变得不可见」的焦点项（输入框、表情搜索框）夺走焦点（toast 的背景模糊会让合成持续 2.6 s）。
    fx.set_pass(0);
    if let Some((inc, o)) = cmp {
        let (mut n, mut mx) = (0usize, 0i32);
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for (i, (a, b)) in inc.iter().zip(buf.iter()).enumerate() {
            let dd = (a.r as i32 - b.r as i32).abs().max((a.g as i32 - b.g as i32).abs()).max((a.b as i32 - b.b as i32).abs()).max((a.a as i32 - b.a as i32).abs());
            if dd > 1 {
                n += 1;
                mx = mx.max(dd);
                let (x, y) = (i % w, i / w);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        log::info!("inc-check: o={:?} {n} px differ (>1), max {mx}, box {x0},{y0}..{x1},{y1}", o);
    }
    finish_rearm(renderer, &ui, reused, None)
}

fn inc_check() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("LANCAT_INC_CHECK").is_ok_and(|v| v == "1"))
}

/// 层的几何签名：变了（姿态、矩形、尺寸、缩放）就不能增量
fn layer_sig(active: &[(crate::FxLayer, Pose)], w: usize, h: usize, scale: f32) -> Vec<f32> {
    let mut v = vec![w as f32, h as f32, scale];
    for (l, p) in active {
        v.extend_from_slice(&[l.id as f32, l.x, l.y, l.w, l.h, l.r, l.blur, l.sat, l.lq, l.shrink, p.tx, p.ty, p.s, p.op, p.ox, p.oy]);
    }
    v
}

/// 层矩形里的基底变化（被层盖住、只经模糊透出）最多拖这么久补画
const BASE_LAG: Duration = Duration::from_millis(250);

thread_local! {
    /// 从哪一帧起跳过了基底重画（None = 基底是新的）
    static BASE_SKIP: Cell<Option<Instant>> = const { Cell::new(None) };
}

struct Inc {
    sig: Vec<f32>,
    base: Vec<Bgra>,
    valid: bool,
}

thread_local! {
    static INC: RefCell<Inc> = const { RefCell::new(Inc { sig: Vec::new(), base: Vec::new(), valid: false }) };
}

/// 层的合成几何（物理 px）
struct Geo {
    lx: f32,
    ly: f32,
    lw: f32,
    lh: f32,
    ox: f32,
    oy: f32,
    tx: f32,
    ty: f32,
}

impl Geo {
    fn new(l: &crate::FxLayer, p: &Pose, scale: f32) -> Geo {
        let (lx, ly, lw, lh) = (l.x * scale, l.y * scale, l.w * scale, l.h * scale);
        Geo { lx, ly, lw, lh, ox: lx + p.ox * lw, oy: ly + p.oy * lh, tx: p.tx * scale, ty: p.ty * scale }
    }
    fn map(&self, p: &Pose, x: f32, y: f32) -> (f32, f32) {
        (self.ox + (x - self.ox) * p.s + self.tx, self.oy + (y - self.oy) * p.s + self.ty)
    }
    /// 纯模糊层的取样区域：静止矩形外扩一圈
    fn bs_region(&self, scale: f32, w: usize, h: usize) -> (usize, usize, usize, usize) {
        let mg = self.lw.max(self.lh) * 0.12 + 16.0 * scale;
        (
            (self.lx - mg).floor().max(0.0) as usize,
            (self.ly - mg).floor().max(0.0) as usize,
            ((self.lx + self.lw + mg).ceil().max(0.0) as usize).min(w),
            ((self.ly + self.lh + mg).ceil().max(0.0) as usize).min(h),
        )
    }
    /// 层内容（含阴影）落到输出上的包围盒
    fn out_box(&self, p: &Pose, scale: f32) -> (f32, f32, f32, f32) {
        if p.identity() {
            let m = 96.0 * scale;
            (self.lx - m, self.ly - m, self.lx + self.lw + m, self.ly + self.lh + m)
        } else {
            let m = 64.0 * scale;
            let (bx0, by0) = self.map(p, self.lx - m, self.ly - m);
            let (bx1, by1) = self.map(p, self.lx + self.lw + m, self.ly + self.lh + m);
            (bx0.min(bx1), by0.min(by1), bx0.max(bx1), by0.max(by1))
        }
    }
}

fn isect(a: (usize, usize, usize, usize), b: (usize, usize, usize, usize)) -> Option<(usize, usize, usize, usize)> {
    let r = (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    (r.2 > r.0 && r.3 > r.1).then_some(r)
}

fn union(a: (usize, usize, usize, usize), b: (usize, usize, usize, usize)) -> (usize, usize, usize, usize) {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

fn fbox(b: (f32, f32, f32, f32), w: usize, h: usize) -> (usize, usize, usize, usize) {
    (b.0.floor().max(0.0) as usize, b.1.floor().max(0.0) as usize, (b.2.ceil().max(0.0) as usize).min(w), (b.3.ceil().max(0.0) as usize).min(h))
}

/// 一层：背景滤镜 + 内容（只动 clip 内的输出像素）。`bs_win`：增量时纯模糊缓存已更新过，不再按输入比对
#[allow(clippy::too_many_arguments)]
fn compose_layer(buf: &mut [Bgra], tmp: &[Bgra], w: usize, h: usize, scale: f32, l: &crate::FxLayer, p: &Pose, clip: (usize, usize, usize, usize), bs_ready: Option<()>, part: Part) {
    let g = Geo::new(l, p, scale);
    let (x0, y0) = g.map(p, g.lx, g.ly);
    let (x1, y1) = g.map(p, g.lx + g.lw, g.ly + g.lh);
    if part == Part::Content {
    } else if l.blur > 0.0 && l.lq <= 0.0 {
        // 纯模糊（表情面板、拖放选择器）：底下的模糊结果与层的姿态无关，按层的静止矩形外扩一圈算一次缓存，
        // 弹出动画的每帧只按当前矩形裁圆角、乘透明度贴上（模糊半径取静止时的值）
        let region = g.bs_region(scale, w, h);
        if bs_ready.is_some() {
            bs_mix(l.id, buf, w, region, (x0, y0, x1, y1), l.r * scale * p.s, p.op, clip);
        } else {
            backdrop_static(l.id, buf, w, h, region, (x0, y0, x1, y1), l.r * scale * p.s, l.blur * scale, l.sat, p.op);
        }
    } else if l.blur > 0.0 || l.lq > 0.0 {
        // liquid 的 bevel = min(14, h/2)（CSS px）
        let bevel = 14.0f32.min(l.h / 2.0) * scale * p.s;
        backdrop_cached(l.id, buf, w, h, (x0, y0, x1, y1), l.r * scale * p.s, l.blur * scale * p.s, l.sat, l.lq * scale * p.s, bevel, p.op);
    }
    if part == Part::Backdrop {
        return;
    }
    let ob = g.out_box(p, scale);
    let ob = fbox(ob, w, h);
    let Some(c) = isect(ob, clip) else { return };
    let cf = (c.0 as f32, c.1 as f32, c.2 as f32, c.3 as f32);
    if p.identity() {
        over_rect(buf, tmp, w, h, cf);
    } else {
        // 层内容（含阴影）可能超出层矩形：按矩形外扩 64px 的变换包围盒采样
        xform_into(buf, tmp, w, h, cf, g.ox, g.oy, p.s, g.tx, g.ty, p.op);
    }
}

/// 增量合成：只重画脏区 d 影响到的输出。返回输出的变化矩形；None = 做不了（调用方整帧合成）
#[allow(clippy::too_many_arguments)]
fn incremental(renderer: &SoftwareRenderer, fx: &Fx<'_>, buf: &mut [Bgra], w: usize, h: usize, scale: f32, lp: &(crate::FxLayer, Pose), d: (usize, usize, usize, usize)) -> Option<(usize, usize, usize, usize)> {
    let (l, p) = lp;
    let g = Geo::new(l, p, scale);
    if l.lq > 0.0 {
        // 液态 / 折射层的缓存按输入整块比对：脏区碰到它的取样范围就整帧
        let m = (l.blur * 3.0 + l.lq + 32.0) * scale;
        let (x0, y0) = g.map(p, g.lx, g.ly);
        let (x1, y1) = g.map(p, g.lx + g.lw, g.ly + g.lh);
        let near = fbox((x0.min(x1) - m, y0.min(y1) - m, x0.max(x1) + m, y0.max(y1) + m), w, h);
        if isect(near, d).is_some() {
            return None;
        }
    }
    // 逐行渲染（render_by_line）画不出 Path（图标、勾选），只能整遍 render()：
    // 层整遍重画进 TMP；基底只在脏区伸出层矩形时重画（层矩形里的基底被层盖住，只经模糊透出来：
    // 这种变化最多攒 BASE_LAG 再补一次基底，悬停动画就不用每帧重画整窗基底）
    let inner = fbox((g.lx, g.ly, g.lx + g.lw, g.ly + g.lh), w, h);
    let inside = p.identity() && d.0 >= inner.0 && d.1 >= inner.1 && d.2 <= inner.2 && d.3 <= inner.3;
    let stale = BASE_SKIP.with(|b| b.get().is_some_and(|t| t.elapsed() >= BASE_LAG));
    let mut d = d;
    renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
    if !inside || stale {
        let t0 = Instant::now();
        fx.set_pass(1);
        INC.with(|c| {
            let mut c = c.borrow_mut();
            clear(&mut c.base);
            renderer.render(&mut c.base, w);
        });
        BASE_SKIP.with(|b| b.set(None));
        phase("base-inc", t0);
        if inside {
            // 补的基底：模糊取样范围里都可能变了
            d = union(d, g.bs_region(scale, w, h));
        }
    } else {
        BASE_SKIP.with(|b| {
            if b.get().is_none() {
                b.set(Some(Instant::now()));
            }
        });
    }
    let t0 = Instant::now();
    fx.set_pass(l.id);
    TMP.with(|t| {
        let mut t = t.borrow_mut();
        clear(&mut t);
        renderer.render(&mut t[..], w);
    });
    phase("layer-inc", t0);
    let t0 = Instant::now();
    // 输出受影响的范围
    let mut o = d;
    let pure_blur = l.blur > 0.0 && l.lq <= 0.0;
    let mut bs_ok = false;
    if pure_blur {
        let region = g.bs_region(scale, w, h);
        if let Some(dd) = isect(region, d) {
            let base = INC.with(|c| std::mem::take(&mut c.borrow_mut().base));
            let r = bs_update(l.id, &base, w, h, region, l.blur * scale, l.sat, dd);
            INC.with(|c| c.borrow_mut().base = base);
            match r {
                Some(Some(win)) => o = union(o, win),
                Some(None) => {}
                None => return None,
            }
        }
        bs_ok = true;
    }
    if !p.identity() {
        let ob = fbox(g.out_box(p, scale), w, h);
        let lb = fbox((g.lx - 64.0 * scale, g.ly - 64.0 * scale, g.lx + g.lw + 64.0 * scale, g.ly + g.lh + 64.0 * scale), w, h);
        // 变换层：层内容变了 → 整个变换包围盒重贴
        if isect(lb, d).is_some() {
            o = union(o, ob);
        }
    }
    // 输出 = 基底 → 滤镜 → 层，只在 o 里
    INC.with(|c| {
        let c = c.borrow();
        for y in o.1..o.3 {
            buf[y * w + o.0..y * w + o.2].copy_from_slice(&c.base[y * w + o.0..y * w + o.2]);
        }
    });
    TMP.with(|t| {
        let tmp = t.borrow();
        compose_layer(buf, &tmp, w, h, scale, l, p, o, bs_ok.then_some(()), Part::All);
    });
    phase("c-inc", t0);
    Some(o)
}

/// 合成帧收尾：pass 0 下重新登记渲染器缓存与属性跟踪
fn finish_rearm(renderer: &SoftwareRenderer, ui: &PanelWindow, reused: bool, o: Option<(usize, usize, usize, usize)>) -> Out {
    // set_pass(0) 让 redraw_tracker 变脏并请求了一次重画（调用方会丢掉它），跟踪器此后不再通知任何变化 ——
    // 在 pass 0 下跑一遍只建场景、不光栅化的逐行渲染（不调用 render_fn），依赖重新登记为「全部可见」的完整集合；
    // ReusedBuffer：同时把逐项缓存填成 pass 0 的状态，下一帧开头的探测就只剩真实变化
    let t0 = Instant::now();
    renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
    renderer.render_by_line(Rearm);
    phase("rearm", t0);
    if !reused {
        renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
    }
    match (finish(ui, o), o) {
        (Out::Partial(Some(r)), Some(_)) => Out::Inc(r),
        (_, None) => Out::Full,
        (x, _) => x,
    }
}

fn finish(ui: &PanelWindow, o: Option<(usize, usize, usize, usize)>) -> Out {
    let more = ui.window().has_active_animations();
    LAYER_ANIM.with(|a| a.set(more));
    if more {
        ui.window().request_redraw();
    }
    Out::Partial(o.map(|(x0, y0, x1, y1)| (x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32)))
}

/// 只为让渲染器求值场景（重新登记属性依赖）：每行都不调用 render_fn，零光栅化
struct Rearm;

impl slint::platform::software_renderer::LineBufferProvider for Rearm {
    type TargetPixel = Bgra;
    fn process_line(&mut self, _line: usize, _range: core::ops::Range<usize>, _render_fn: impl FnOnce(&mut [Bgra])) {}
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
    use rayon::prelude::*;
    dst.par_chunks_mut(16384).zip(src.par_chunks(16384)).for_each(|(d, s)| {
        for (d, s) in d.iter_mut().zip(s) {
            over_px(d, *s);
        }
    });
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
    use rayon::prelude::*;
    dst.par_chunks_mut(w).enumerate().skip(y0).take(y1 - y0).for_each(|(y, row)| {
        let (sy, wy) = map(y, oy, ty);
        if sy < -1 || sy >= hi {
            return;
        }
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
    });
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
    /// 缓存建立的时刻（层出现的时刻）
    born: Instant,
    /// 层 5：出现 0.6 s 后已按静止的底图重算过一次，之后冻结
    settled: bool,
}

thread_local! {
    /// 层 5 冻结后，它下面所有内容合成好的样子
    static UNDER5: RefCell<Vec<Bgra>> = const { RefCell::new(Vec::new()) };
    /// 层 5 冻结期间，它上面静态层的渲染结果
    static FROZEN_TMP: RefCell<HashMap<i32, Vec<Bgra>>> = RefCell::new(HashMap::new());
}

#[derive(Clone, Copy, PartialEq)]
enum Part {
    All,
    Backdrop,
    Content,
}

/// 层 `id` 的纯模糊底图是否已冻结（见 backdrop_static）
fn bs_frozen(id: i32, len: usize) -> bool {
    BS.with(|m| m.borrow().get(&id).is_some_and(|c| c.input.len() == len && c.settled))
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
        // 拖放选择器（层 5）是盖住整个面板的模态遮罩：拖动时底下的内容（缩小动画收尾、悬停效果）每帧都有细小变化，
        // 逐帧重算 14px 模糊要 20~50 ms，拖动直接掉到 10 FPS。出现 0.6 s（缩小动画结束）后冻结底图；层隐藏时缓存会被丢掉。
        // 前 0.6 s（面板从隐藏条展开、缩小动画）底图每帧都在变：沿用第一次算的模糊（反正选择器正在淡入），
        // 动画结束后按静止的底图整块重算一次再冻结。
        if id == 5 {
            if c.settled || c.born.elapsed().as_secs_f32() <= 0.6 {
                return Some(None);
            }
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
            let c = m.entry(id).or_insert_with(|| BsCache { key, w, input: Vec::new(), filt: Vec::new(), born: Instant::now(), settled: false });
            if full || c.filt.len() != dst.len() {
                c.filt = vec![Bgra::default(); dst.len()];
            }
            for y in win.1..win.3 {
                c.filt[y * w + win.0..y * w + win.2].copy_from_slice(&tmp[y * w + win.0..y * w + win.2]);
            }
            c.key = key;
            c.w = w;
            if id == 5 && c.born.elapsed().as_secs_f32() > 0.6 {
                c.settled = true;
            }
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
    bs_mix(id, dst, w, region, rect, radius, op, (0, 0, w, h));
}


/// 把纯模糊层缓存的滤后像素按矩形（圆角）× 透明度贴到 dst（只动 clip 内）
#[allow(clippy::too_many_arguments)]
fn bs_mix(id: i32, dst: &mut [Bgra], w: usize, region: (usize, usize, usize, usize), rect: (f32, f32, f32, f32), radius: f32, op: f32, clip: (usize, usize, usize, usize)) {
    let t = Instant::now();
    let (rx0, ry0, rx1, ry1) = (rect.0.min(rect.2), rect.1.min(rect.3), rect.0.max(rect.2), rect.1.max(rect.3));
    let (gx0, gy0, gx1, gy1) = region;
    let x0 = (rx0.floor().max(0.0) as usize).max(gx0).max(clip.0);
    let y0 = (ry0.floor().max(0.0) as usize).max(gy0).max(clip.1);
    let x1 = (rx1.ceil().max(0.0) as usize).min(gx1).min(clip.2);
    let y1 = (ry1.ceil().max(0.0) as usize).min(gy1).min(clip.3);
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

/// 增量：底图只在 d 里变了 → 更新缓存的输入并只重算受影响的输出窗口；返回重算的窗口
fn bs_update(id: i32, base: &[Bgra], w: usize, h: usize, region: (usize, usize, usize, usize), sigma: f32, sat: f32, d: (usize, usize, usize, usize)) -> Option<Option<(usize, usize, usize, usize)>> {
    let (gx0, gy0, gx1, gy1) = region;
    let reach = backdrop_reach(sigma);
    let t = Instant::now();
    let r = BS.with(|m| {
        let mut m = m.borrow_mut();
        let c = m.get_mut(&id)?;
        if c.input.len() != base.len() || c.filt.len() != base.len() {
            return None;
        }
        // 只在真正变了的行列上重算（悬停动画只改层，底图在 d 里没变 → 模糊一点不用重算）
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        for y in d.1..d.3 {
            let (a, b) = (&mut c.input[y * w + d.0..y * w + d.2], &base[y * w + d.0..y * w + d.2]);
            if a == b {
                continue;
            }
            let l = a.iter().zip(b).position(|(p, q)| p != q).unwrap_or(0);
            let r = a.len() - a.iter().rev().zip(b.iter().rev()).position(|(p, q)| p != q).unwrap_or(0);
            x0 = x0.min(d.0 + l);
            x1 = x1.max(d.0 + r);
            y0 = y0.min(y);
            y1 = y1.max(y + 1);
            a.copy_from_slice(b);
        }
        if x0 == usize::MAX {
            return Some(None);
        }
        let d = (x0, y0, x1, y1);
        let win = (d.0.saturating_sub(reach).max(gx0), d.1.saturating_sub(reach).max(gy0), (d.2 + reach).min(gx1), (d.3 + reach).min(gy1));
        if win.2 <= win.0 || win.3 <= win.1 {
            return Some(None);
        }
        let mut tmp = c.input.clone();
        backdrop(&mut tmp, w, h, (gx0 as f32, gy0 as f32, gx1 as f32, gy1 as f32), 0.0, sigma, sat, 0.0, 0.0, 1.0, Some(win));
        for y in win.1..win.3 {
            c.filt[y * w + win.0..y * w + win.2].copy_from_slice(&tmp[y * w + win.0..y * w + win.2]);
        }
        Some(Some(win))
    });
    phase("bs-inc", t);
    r
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
    use rayon::prelude::*;
    let ch = &ch;
    dst.par_chunks_mut(w).enumerate().skip(iy0).take(bh).for_each(|(yy, drow)| {
        let y = yy - iy0;
        if yy < wy0 || yy >= wy1 {
            return;
        }
        let py = yy as f32 + 0.5;
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
            let d = &mut drow[ix0 + x];
            let mixv = |o: u8, n: f32| (o as f32 + (n - o as f32) * c).round().clamp(0.0, 255.0) as u8;
            d.b = mixv(d.b, ch[0][i]);
            d.g = mixv(d.g, ch[1][i]);
            d.r = mixv(d.r, ch[2][i]);
            d.a = mixv(d.a, ch[3][i]);
        }
    });
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
    use rayon::prelude::*;
    buf.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let sy = (y as f32 + 0.5) / f as f32;
        for (x, v) in row.iter_mut().enumerate() {
            let sx = (x as f32 + 0.5) / f as f32;
            *v = bilinear(&small, sw, sh, sx, sy);
        }
    });
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
        // 竖：按行顺序扫，整行一起累加（逐列扫的步长是 w，缓存命中极差）
        let rowat = |i: i64| &tmp[i.clamp(0, h as i64 - 1) as usize * w..][..w];
        let mut acc = vec![0f32; w];
        for i in -(r as i64)..=r as i64 {
            for (a, v) in acc.iter_mut().zip(rowat(i)) {
                *a += v;
            }
        }
        for y in 0..h {
            let (add, sub) = (rowat(y as i64 + r as i64 + 1), rowat(y as i64 - r as i64));
            let out = &mut buf[y * w..(y + 1) * w];
            for x in 0..w {
                out[x] = acc[x] * k;
                acc[x] += add[x] - sub[x];
            }
        }
    }
}

// ------------------------------------------------------------------ GPU 合成（Windows，见 gpu.rs）

#[cfg(windows)]
thread_local! {
    /// GPU 路径的层 5（拖放选择器）冻结状态：出现时刻、是否已按静止底图重算并冻结
    static G5: Cell<Option<(Instant, bool)>> = const { Cell::new(None) };
    /// 冻结期间层 5 以上静态层的纹理已是最新（不再渲染/上传）
    static GFROZEN: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
    /// GPU 路径上一帧的几何签名（单层、签名不变时可以只重画层、复用基底纹理）
    static GSIG: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
    /// 本帧上传纹理用掉的时间（已含在 base / rN 里，单独列出来）
    static UP_MS: Cell<f32> = const { Cell::new(0.0) };
    /// 上一帧收尾做过 rearm（渲染器的逐项缓存是 pass 0 的状态，增量探测可信）
    static GREARMED: Cell<bool> = const { Cell::new(false) };
    /// 层 5 出现后、冻结前：基底和静态层（层 5、6 以外）本次拖拽里渲染过的内容键（层矩形 + 窗口尺寸）
    static GPRE: RefCell<HashMap<i32, [f32; 8]>> = RefCell::new(HashMap::new());
    /// 上一帧所有活动层的内容键（层 id + 矩形 + 窗口尺寸，不含姿态）
    static GCKEY: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
    /// 上一次探测没有任何变化（只有姿态在动）；连续跳过 rearm 的帧数
    static GPROBE_CLEAN: Cell<bool> = const { Cell::new(false) };
    static GSKIP: Cell<u32> = const { Cell::new(0) };
    /// 探测到变化后先停几帧不探测（拖放选择器的弹簧动画每帧都在变，探测白花 ~3 ms）
    static GPCOOL: Cell<u32> = const { Cell::new(0) };
}

#[cfg(windows)]
fn content_key(l: Option<&crate::FxLayer>, w: usize, h: usize, scale: f32) -> [f32; 8] {
    match l {
        Some(l) => [l.x, l.y, l.w, l.h, l.r, w as f32, h as f32, scale],
        None => [0.0, 0.0, 0.0, 0.0, 0.0, w as f32, h as f32, scale],
    }
}


/// 层内容在源坐标里可能占的行（层矩形外扩 96 px 阴影边距，与 CPU 版 out_box 的边距相同）
#[cfg(windows)]
fn layer_rows(g: &Geo, scale: f32, h: usize) -> (usize, usize) {
    let m = 96.0 * scale;
    (((g.ly - m).floor().max(0.0) as usize).min(h), ((g.ly + g.lh + m).ceil().max(0.0) as usize).min(h))
}

/// GPU 版的 render_inner：Slint 照旧在 CPU 上逐层光栅化（基底 + 每个活动层），滤镜/变换/叠加在 GPU 上。
/// 渲染器缓存、pass 切换、层 5 冻结、增量探测的规则与 CPU 版相同。
#[cfg(windows)]
fn render_gpu(renderer: &SoftwareRenderer, buf: &mut [Bgra], w: usize, h: usize, scale: f32, reused: bool) -> Out {
    use crate::gpu;
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
    let ids: Vec<i32> = active.iter().map(|(l, _)| l.id).collect();
    gpu::retain(&shown, &ids);
    if active.is_empty() {
        LAYER_ANIM.with(|a| a.set(false));
        G5.with(|g| g.set(None));
        GFROZEN.with(|f| f.borrow_mut().clear());
        gpu::drop_under5();
        if fx.get_pass() != 0 {
            fx.set_pass(0);
            set_flags(&[]);
        }
        if WAS.with(|w| w.replace(false)) {
            set_flags(&[]);
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
    if !gpu::frame_begin(w, h) {
        log::warn!("gpu: frame textures unavailable");
        return Out::Partial(None);
    }
    let was = WAS.with(|x| x.replace(true));
    set_flags(&ids);
    let sig = layer_sig(&active, w, h, scale);
    // 探测：上一帧以 pass 0 的 rearm 收尾、层的内容键没变、纹理都在 → 在 pass 0 下跑一遍不光栅化的逐行渲染拿脏区。
    // 没有脏区 = 只有姿态在动（入场动画、内容缩放回弹）：所有纹理照用，只在 GPU 上按新姿态重合成。
    // 单层且脏区落在层矩形里：复用基底纹理（最多拖 BASE_LAG），只重画层。
    let ckey: Vec<f32> = {
        let mut v = vec![w as f32, h as f32, scale];
        for (l, _) in &active {
            v.extend_from_slice(&[l.id as f32, l.x, l.y, l.w, l.h, l.r]);
        }
        v
    };
    let mut reuse_base = false;
    let mut pose_only = false;
    let cool = GPCOOL.with(|c| c.replace(c.get().saturating_sub(1)));
    let probe_ok = reused
        && was
        && GREARMED.with(|r| r.get())
        && gpu::has_base()
        && GCKEY.with(|k| *k.borrow() == ckey)
        && active.iter().all(|(l, _)| gpu::has_layer(l.id))
        && (GPROBE_CLEAN.with(|p| p.get()) || cool == 0);
    if probe_ok {
        renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
        let t0 = Instant::now();
        let reg = renderer.render_by_line(Rearm);
        phase("probe", t0);
        let d = region_box(&reg)
            .map(|(dx, dy, dw, dh)| (dx.max(0) as usize, dy.max(0) as usize, ((dx + dw as i32).max(0) as usize).min(w), ((dy + dh as i32).max(0) as usize).min(h)))
            .filter(|d| d.2 > d.0 && d.3 > d.1);
        GPROBE_CLEAN.with(|p| p.set(d.is_none()));
        if d.is_some() {
            GPCOOL.with(|c| c.set(6));
        }
        if stats_on() {
            if let Some(d) = d {
                PH.with(|p| p.borrow_mut().push_str(&format!(" d={},{} {}x{}", d.0, d.1, d.2 - d.0, d.3 - d.1)));
            }
        }
        match d {
            None if GSIG.with(|g| *g.borrow() == sig) => {
                // 什么都没变：显示的仍是上一帧
                GPU_FRAME.with(|g| g.set(true));
                return finish(&ui, None);
            }
            None => pose_only = true,
            Some(d) if active.len() == 1 => {
                let (l, p) = &active[0];
                let g = Geo::new(l, p, scale);
                let inner = fbox((g.lx, g.ly, g.lx + g.lw, g.ly + g.lh), w, h);
                let inside = p.identity() && d.0 >= inner.0 && d.1 >= inner.1 && d.2 <= inner.2 && d.3 <= inner.3;
                let stale = BASE_SKIP.with(|b| b.get().is_some_and(|t| t.elapsed() >= BASE_LAG));
                reuse_base = inside && !stale;
            }
            Some(_) => {}
        }
        if stats_on() && pose_only {
            PH.with(|ph| ph.borrow_mut().push_str(" pose-only"));
        }
    }
    GCKEY.with(|k| *k.borrow_mut() = ckey);
    if !pose_only {
        // 只重合成的帧不碰渲染器：换 NewBuffer 会清掉逐项缓存，下一帧的探测就成了整窗
        renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
    }
    let i5 = active.iter().position(|(l, _)| l.id == 5);
    if i5.is_none() {
        G5.with(|g| g.set(None));
        GFROZEN.with(|f| f.borrow_mut().clear());
        gpu::drop_under5();
    }
    let frozen5 = i5.is_some() && G5.with(|g| g.get().is_some_and(|(_, s)| s)) && gpu::has_bs(5);
    let skip_below = frozen5 && gpu::has_under5();
    // 层 5 出现后、冻结前（~0.6 s，选择器淡入 + 内容缩小动画）：基底和层 5/6 以外的层内容不变，只有姿态在动 ——
    // 第一帧渲染后沿用 GPU 上的纹理，到冻结那一帧（按静止底图重算模糊）再全部重画一次。
    // 期间这些层里若有内容变化，最多晚 0.6 s 出现（选择器淡入后它们本来就被模糊底图盖住）。
    let settle5 = G5.with(|g| g.get().is_some_and(|(born, s)| !s && born.elapsed().as_secs_f32() > 0.6));
    let pre5 = i5.is_some() && !frozen5 && !settle5 && G5.with(|g| g.get().is_some());
    if i5.is_none() || settle5 || frozen5 {
        GPRE.with(|p| p.borrow_mut().clear());
    }
    let pre_hit = |id: i32, key: [f32; 8]| pre5 && GPRE.with(|p| p.borrow().get(&id) == Some(&key));
    let base_key = content_key(None, w, h, scale);
    let t0 = Instant::now();
    if skip_below {
        gpu::start(true);
    } else if pose_only || (gpu::has_base() && pre_hit(1, base_key)) {
        gpu::start(false);
    } else if reuse_base {
        BASE_SKIP.with(|b| {
            if b.get().is_none() {
                b.set(Some(Instant::now()));
            }
        });
        gpu::start(false);
    } else {
        fx.set_pass(1);
        clear(buf);
        renderer.render(buf, w);
        let tu = Instant::now();
        gpu::upload_base(buf, w, h);
        UP_MS.with(|u| u.set(u.get() + tu.elapsed().as_secs_f32() * 1000.0));
        BASE_SKIP.with(|b| b.set(None));
        if i5.is_some() && !frozen5 {
            GPRE.with(|p| p.borrow_mut().insert(1, base_key));
        }
        gpu::start(false);
    }
    phase("base", t0);
    GSIG.with(|g| *g.borrow_mut() = sig);
    TMP.with(|t| {
        let mut tmp = t.borrow_mut();
        if tmp.len() != buf.len() {
            *tmp = vec![Bgra::default(); buf.len()];
        }
        for (k, (l, p)) in active.iter().enumerate() {
            if skip_below && Some(k) < i5 {
                continue;
            }
            let t0 = Instant::now();
            let g = Geo::new(l, p, scale);
            let static_above = frozen5 && Some(k) > i5 && l.id != 6;
            let key = content_key(Some(l), w, h, scale);
            let pre_static = i5.is_some() && !frozen5 && Some(k) != i5 && l.id != 6;
            let cached = pose_only || (static_above && gpu::has_layer(l.id) && GFROZEN.with(|f| f.borrow().contains(&l.id))) || (pre_static && gpu::has_layer(l.id) && pre_hit(l.id, key));
            if !cached {
                fx.set_pass(l.id);
                clear(&mut tmp);
                renderer.render(&mut tmp[..], w);
                let tu = Instant::now();
                gpu::upload_layer(l.id, &tmp, w, layer_rows(&g, scale, h));
                UP_MS.with(|u| u.set(u.get() + tu.elapsed().as_secs_f32() * 1000.0));
                if static_above {
                    GFROZEN.with(|f| f.borrow_mut().push(l.id));
                }
                if pre_static {
                    GPRE.with(|p| p.borrow_mut().insert(l.id, key));
                }
            }
            if stats_on() {
                phase(&format!("r{}", l.id), t0);
            }
            let t0 = Instant::now();
            let (x0, y0) = g.map(p, g.lx, g.ly);
            let (x1, y1) = g.map(p, g.lx + g.lw, g.ly + g.lh);
            let rect = [x0, y0, x1, y1];
            let backdrop_done = skip_below && Some(k) == i5;
            if backdrop_done {
            } else if l.blur > 0.0 && l.lq <= 0.0 {
                // 纯模糊：与 backdrop_static 相同的重算规则（层 5：出现时算一次，0.6 s 后按静止底图再算一次并冻结；
                // 其他层：每帧按当前底图算 —— GPU 上很便宜，等价于 CPU 版的「输入变了就重算」）
                let recompute = if l.id == 5 {
                    match G5.with(|g| g.get()) {
                        None => {
                            G5.with(|g| g.set(Some((Instant::now(), false))));
                            true
                        }
                        Some((born, false)) if born.elapsed().as_secs_f32() > 0.6 => {
                            G5.with(|g| g.set(Some((born, true))));
                            true
                        }
                        _ => !gpu::has_bs(5),
                    }
                } else {
                    true
                };
                gpu::backdrop_blur(l.id, recompute, rect, l.r * scale * p.s, l.blur * scale, l.sat, p.op);
                if stats_on() && recompute {
                    PH.with(|ph| ph.borrow_mut().push_str(" bs-full"));
                }
            } else if l.blur > 0.0 || l.lq > 0.0 {
                let bevel = 14.0f32.min(l.h / 2.0) * scale * p.s;
                gpu::backdrop_liquid(rect, l.r * scale * p.s, l.blur * scale * p.s, l.sat, l.lq * scale * p.s, bevel, p.op);
            }
            if Some(k) == i5 && frozen5 && !skip_below {
                // 层 5 冻结：「下面所有内容 + 模糊底图」存一份，之后每帧从它开始
                gpu::save_under5();
            }
            let ob = fbox(g.out_box(p, scale), w, h);
            gpu::over_layer(l.id, g.ox, g.oy, p.s, g.tx, g.ty, p.op, ob);
            if stats_on() {
                phase(&format!("c{}", l.id), t0);
            }
        }
    });
    fx.set_pass(0);
    gpu::end_compose();
    if stats_on() {
        let up = UP_MS.with(|u| u.replace(0.0));
        PH.with(|p| p.borrow_mut().push_str(&format!(" up={up:.1}")));
    }
    GPU_FRAME.with(|g| g.set(true));
    if pose_only {
        // 整帧都在 pass 0 下：探测已经把渲染器缓存和依赖登记成 pass 0 的状态，不用 rearm
        GSKIP.with(|s| s.set(0));
        let _ = finish(&ui, None);
        return Out::Full;
    }
    // 还有动画在跑：下一帧无论如何都会画（LAYER_ANIM 接着要帧），省掉这次整场景的 rearm（~1.5–3 ms）；
    // 动画停下的那一帧照常 rearm，之后的属性变化才会通知重画。上次探测是干净的（只有姿态在动）或已连续
    // 跳过 6 帧时照样 rearm，让下一帧能探测、走只重合成的路
    let skips = GSKIP.with(|s| s.get());
    if ui.window().has_active_animations() && !GPROBE_CLEAN.with(|p| p.get()) && skips < 6 {
        GSKIP.with(|s| s.set(skips + 1));
        GREARMED.with(|r| r.set(false));
        renderer.set_repaint_buffer_type(if reused { RepaintBufferType::ReusedBuffer } else { RepaintBufferType::NewBuffer });
        let _ = finish(&ui, None);
        return Out::Full;
    }
    GSKIP.with(|s| s.set(0));
    GREARMED.with(|r| r.set(true));
    finish_rearm(renderer, &ui, reused, None)
}
