//! 消息 toast（Linux Wayland）：与旧版 src-tauri/src/native_dock/linux_toast.rs 相同的设计和时序。
//! 面板关着时有人发来消息，停靠条鼓成两行胶囊（头像、名字、消息）停留几秒；悬停时不收，点击打开该会话的面板，
//! 否则缩回停靠条。layer surface（Overlay，贴输出左上角、按 margin 定位，含 10px 阴影外沿），只有卡片接收指针。
//! 绘制在 ui/toast.slint（ToastWindow），这里只算几何、驱动 t。只在事件循环线程上用。

use std::cell::RefCell;
use std::time::{Duration, Instant};

use slint::ComponentHandle as _;

use crate::wl::{self, Cmd, ToastPlace, ToastPointer};
use crate::ToastWindow;

const ANIM_MS: f64 = 280.0;
const SHADOW: f64 = 10.0;
/// 胶囊离屏幕边的距离
const INSET: f64 = 4.0;

pub struct ToastSpec {
    pub peer_id: String,
    pub name: String,
    pub text: String,
    pub edge: String,
    /// 停靠条在输出里的矩形（逻辑 px）与输出编号
    pub strip: (f64, f64, f64, f64),
    pub output: usize,
    pub dark: bool,
    pub hold_ms: u32,
    /// 动态表情 / 贴纸的 Lottie（有就在头像位播大号表情）
    pub emoji: Option<String>,
}

struct State {
    ui: ToastWindow,
    spec: Option<ToastSpec>,
    size: (f64, f64),
    t: f64,
    from: f64,
    to: f64,
    t0: Instant,
    anim: slint::Timer,
    hide: slint::Timer,
    hovering: bool,
    pop: Option<crate::toast_emoji::Pop>,
}

thread_local! {
    static S: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    S.with(|s| s.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

pub fn shown() -> bool {
    with(|s| s.spec.is_some() && s.to > 0.5).unwrap_or(false)
}

fn ensure() -> bool {
    if with(|_| ()).is_some() {
        return true;
    }
    wl::mark_next_toast();
    let ui = match ToastWindow::new() {
        Ok(u) => u,
        Err(e) => {
            log::warn!("toast window: {e}");
            return false;
        }
    };
    // 旧版用 GTK 的界面字体（gtk-font-name 的族名），pango 绝对字号 12.5 / 12
    ui.set_family(crate::metrics::gtk_font().0.into());
    let _ = ui.show();
    S.with(|s| {
        *s.borrow_mut() = Some(State {
            ui,
            spec: None,
            size: (0.0, 0.0),
            t: 0.0,
            from: 0.0,
            to: 0.0,
            t0: Instant::now(),
            anim: slint::Timer::default(),
            hide: slint::Timer::default(),
            hovering: false,
            pop: None,
        })
    });
    true
}

pub fn show(spec: ToastSpec) {
    if !ensure() {
        return;
    }
    let hold = spec.hold_ms;
    with(|s| {
        // 表情：逻辑 56 px × 缩放（输出缩放未知时按 2 渲染，回弹放大也不糊）
        s.pop = spec.emoji.as_deref().and_then(|j| crate::toast_emoji::Pop::new(j, (crate::toast_emoji::SIZE * 2.0 * 1.15) as u32));
        let has_emoji = s.pop.is_some();
        push_emoji_frame(s);
        let ui = &s.ui;
        ui.set_name(spec.name.clone().into());
        ui.set_text(spec.text.clone().into());
        ui.set_initials(crate::initials(&spec.name).into());
        ui.set_avatar(crate::avatar_colors(&spec.peer_id, false).1);
        ui.set_dark(spec.dark);
        ui.set_has_emoji(has_emoji);
        ui.set_pop(0.0);
        ui.set_edge(spec.edge.clone().into());
        let (sx, sy, sw, sh) = spec.strip;
        ui.set_strip_w(sw as f32);
        ui.set_strip_h(sh as f32);
        let (pw, ph) = (ui.get_pill_w() as f64, ui.get_pill_h() as f64);
        let m = SHADOW;
        let (cx, cy) = (sx + sw / 2.0, sy + sh / 2.0);
        let bw = pw.max(sw) + INSET + m;
        let bh = ph.max(sh) + 2.0 * m;
        let (bw2, bh2) = (pw.max(sw) + 2.0 * m, ph.max(sh) + INSET + m);
        let (x, y, w, h) = match spec.edge.as_str() {
            "left" => (sx, cy - bh / 2.0, bw, bh),
            "top" => (cx - bw2 / 2.0, sy, bw2, bh2),
            "bottom" => (cx - bw2 / 2.0, sy + sh - bh2, bw2, bh2),
            _ => (sx + sw - bw, cy - bh / 2.0, bw, bh),
        };
        let (x, y) = (x.max(0.0), y.max(0.0));
        s.size = (w.round(), h.round());
        let (fw, fh) = s.size;
        // 卡片最终矩形（t = 1）：输入区域
        let (ix, iy) = match spec.edge.as_str() {
            "left" => (INSET, fh / 2.0 - ph / 2.0),
            "top" => (fw / 2.0 - pw / 2.0, INSET),
            "bottom" => (fw / 2.0 - pw / 2.0, fh - ph - INSET),
            _ => (fw - pw - INSET, fh / 2.0 - ph / 2.0),
        };
        let input = (ix.floor() as i32, iy.floor() as i32, pw.ceil() as i32, ph.ceil() as i32);
        let replacing = s.spec.is_some() && s.to > 0.5;
        let output = spec.output;
        s.spec = Some(spec);
        wl::push(Cmd::Toast(Some(ToastPlace { x: x.round() as i32, y: y.round() as i32, w: fw as u32, h: fh as u32, output, input })));
        if !replacing {
            s.t = 0.0;
            s.ui.set_t(0.0);
        }
        animate(s, 1.0);
        if !s.hovering {
            arm_hide(s, hold);
        }
    });
}

/// 表情的当前帧和弹出缩放 → ToastWindow
fn push_emoji_frame(s: &mut State) {
    let Some(p) = s.pop.as_mut() else { return };
    let px = p.px();
    let rgba = p.frame_rgba();
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(px, px);
    buf.make_mut_bytes().copy_from_slice(&rgba);
    s.ui.set_emoji(slint::Image::from_rgba8_premultiplied(buf));
    s.ui.set_pop(p.scale() as f32);
}

fn arm_hide(s: &mut State, ms: u32) {
    s.hide.start(slint::TimerMode::SingleShot, Duration::from_millis(ms as u64), || {
        with(|s| animate(s, 0.0));
    });
}

/// 收起 toast（面板打开了、停靠条移动了……）
pub fn dismiss() {
    with(|s| {
        if s.spec.is_some() && s.to > 0.0 {
            animate(s, 0.0);
        }
    });
}

fn animate(s: &mut State, to: f64) {
    s.from = s.t;
    s.to = to;
    s.t0 = Instant::now();
    if to <= 0.0 {
        s.hide.stop();
    }
    if !s.anim.running() {
        s.anim.start(slint::TimerMode::Repeated, Duration::from_millis(16), || {
            with(|s| {
                let done = step(s);
                s.ui.set_t(s.t as f32);
                push_emoji_frame(s);
                // 有表情时动画定时器一直跑（表情在循环播放），直到收回
                if done && (s.pop.is_none() || s.to <= 0.0) {
                    s.anim.stop();
                    if s.to <= 0.0 {
                        s.spec = None;
                        s.hovering = false;
                        s.pop = None;
                        wl::push(Cmd::Toast(None));
                    }
                }
            });
        });
    }
}

/// easeOutBack 式：一点点回弹，读起来像从停靠条里「鼓」出来
fn ease(t: f64) -> f64 {
    let c1 = 1.2;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

fn step(s: &mut State) -> bool {
    let k = (s.t0.elapsed().as_secs_f64() * 1000.0 / ANIM_MS).min(1.0);
    let e = if s.to > s.from { ease(k) } else { k * k };
    s.t = s.from + (s.to - s.from) * e;
    k >= 1.0
}

/// wl.rs：toast surface 上的指针
pub fn pointer(e: ToastPointer) {
    match e {
        ToastPointer::Enter => {
            with(|s| {
                s.hovering = true;
                s.hide.stop();
            });
        }
        ToastPointer::Leave => {
            with(|s| {
                s.hovering = false;
                if s.to > 0.5 {
                    arm_hide(s, 1500);
                }
            });
        }
        ToastPointer::LeftRelease => {
            let peer = with(|s| s.spec.as_ref().map(|p| p.peer_id.clone())).flatten();
            if let Some(peer) = peer {
                with(|s| animate(s, 0.0));
                crate::dock::open_panel(&format!("toast:{peer}"));
            }
        }
    }
}
