//! Linux Wayland：自写 Slint Platform（SCTK 0.21.1 + wlr-layer-shell + wl_shm 软件渲染）。
//!
//! 一个连接、一个事件循环，管理两种 layer surface（都在 Overlay 层，exclusive zone -1，不占位）：
//! - 面板：Slint 窗口（SoftwareRenderer → 预乘 BGRA → SlotPool 的 ARGB8888，小端内存布局就是 B,G,R,A）。
//!   收起 = 淡出后销毁 surface（Slint 窗口本身一直“显示”着，只是没有 surface 就不渲染）；打开 = 重建
//!   surface、等 configure、淡入。淡入淡出是拷贝到 shm 时整体乘 alpha（Wayland 没有窗口透明度）。
//! - 停靠条：自己画的圆角条（与旧版 native_dock/linux.rs 的 cairo 画法相同），指针进出/按下交给 dock。
//!
//! 键盘：wl_keyboard（SCTK + xkbcommon，含 compose），自己按 repeat_info 做按键重复；
//! 输入法：zwp_text_input_v3（fcitx5 / niri 支持），Slint 的 input_method_request 决定 enable/disable，
//! 预编辑 / 上屏走 i-slint-core 的 InternalKeyEvent（与 Windows 的 IMM32 路径相同）。
//!
//! dock（src/dock_wl.rs）不直接碰 Wayland 对象：它调用 `push(Cmd)`，事件循环每轮执行；
//! Wayland 事件反过来调用 dock 的 `wl_*` 函数（只改 dock 自己的状态、再 push 命令，不会重入）。
//!
//! 普通窗口（设置页）：第一个 Slint 窗口是面板（layer surface），之后创建的都是独立的 xdg toplevel
//! （服务端装饰；关闭按钮 → CloseRequested → Slint 隐藏 → 销毁 toplevel），不顶掉面板。
//!
//! 拖放：wl_data_device。文件拖到停靠条 → 打开面板（旧版 open_panel("drag")）；进入面板时预读一次
//! text/uri-list 得到项数（「拖到头像上发送 N 项」），松开时再读、解析 file:// URI 交给 dock_wl。
//!
//! 位置：layer-shell 没有全局坐标，一律是「贴哪条边 + 沿边偏移 + 离边距离」（逻辑 px），由 dock 按
//! 输出的逻辑尺寸算好。缩放：只支持整数缩放（wl_surface.set_buffer_scale）；分数缩放待做。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::os::fd::{AsFd, AsRawFd};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use i_slint_core::input::{InternalKeyEvent, KeyEventType};
use i_slint_core::window::{InputMethodRequest, WindowAdapterInternal};
use slint::platform::software_renderer::{RepaintBufferType, SoftwareRenderer};
use slint::platform::{Key, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalSize, SharedString};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers, RepeatInfo},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    data_device_manager::{
        data_device::{DataDevice, DataDeviceData, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        DataDeviceManagerState,
    },
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        xdg::{
            window::{DecorationMode, Window as XdgWindow, WindowConfigure, WindowDecorations, WindowHandler},
            XdgShell,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::{registry_queue_init, GlobalList},
    protocol::{wl_data_device::WlDataDevice, wl_data_device_manager::DndAction, wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, EventQueue, QueueHandle,
};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3::{self, ZwpTextInputV3},
};

// ------------------------------------------------------------------ pixels

pub use crate::compose::Bgra;

// ------------------------------------------------------------------ commands from the dock

/// 一个 layer surface 的位置（逻辑 px）：贴 `edge`，沿边偏移 `along`，离边 `inset`，尺寸 w×h，输出编号。
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub edge: String,
    pub along: i32,
    pub inset: i32,
    pub w: u32,
    pub h: u32,
    pub output: usize,
}

/// 停靠条外观（与旧版 DockVisual 相同的含义）。
#[derive(Clone, Debug, PartialEq)]
pub struct StripLook {
    pub edge: String,
    pub unread: bool,
    pub open: bool,
    pub hovering: bool,
    pub accent: (f64, f64, f64),
    /// unread 脉冲相位（秒）
    pub phase: f64,
}

pub enum Cmd {
    /// Some = 打开 / 移动面板（淡入），None = 收起（淡出后销毁）
    Panel(Option<Place>),
    Strip(Option<Place>),
    StripLook(StripLook),
    /// 面板主动要键盘（托盘 / toast 点击打开）：Exclusive 一下，拿到后回到 OnDemand
    PanelFocus,
    /// 普通窗口（按 WlWindow.id）显示 / 隐藏
    Top(usize, bool),
    /// 客户端自绘标题栏上的操作，作用在最近一次被按下的普通窗口上
    TopAct(TopAct),
    /// 消息 toast 的 layer surface（src/toast_wl.rs）：Some = 显示 / 移动，None = 销毁
    Toast(Option<ToastPlace>),
}

/// toast 的 layer surface：贴输出左上角、按 margin 定位（与旧版 linux_toast 的 set_layer_shell_margin 相同），
/// 逻辑 px；`input` 是只接收指针的卡片矩形（surface 内坐标），阴影外沿不挡点击。
#[derive(Clone, Debug, PartialEq)]
pub struct ToastPlace {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub output: usize,
    pub input: (i32, i32, i32, i32),
}

/// toast 上的指针事件（交给 src/toast_wl.rs）
#[derive(Clone, Copy, Debug)]
pub enum ToastPointer {
    Enter,
    Leave,
    LeftRelease,
}

/// 自绘标题栏（compositor 要求客户端装饰时，旧版 GTK 窗口画的那条标题栏）的操作
#[derive(Clone, Copy, Debug)]
pub enum TopAct {
    Move,
    Minimize,
    Maximize,
    Menu(i32, i32),
}

thread_local! {
    static CMDS: RefCell<Vec<Cmd>> = const { RefCell::new(Vec::new()) };
    /// 输出的逻辑尺寸（按 SCTK 的输出顺序），dock 算位置用
    static OUTPUTS: RefCell<Vec<(i32, i32, i32)>> = const { RefCell::new(Vec::new()) };
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// 第一个窗口（面板）已经创建：之后的都是普通窗口
    static PANEL_MADE: Cell<bool> = const { Cell::new(false) };
    /// 下一个创建的 Slint 窗口是 toast（mark_next_toast）
    static NEXT_TOAST: Cell<bool> = const { Cell::new(false) };
    static TOAST_WIN: RefCell<Weak<WlWindow>> = const { RefCell::new(Weak::new()) };
    /// 事件循环运行中新建的普通窗口，下一轮并入 State.tops
    static NEW_TOPS: RefCell<Vec<Weak<WlWindow>>> = const { RefCell::new(Vec::new()) };
    /// 后台线程读完拖放数据（回到事件循环线程后放这里）：Count(n) / Drop(files)
    static DND_DONE: RefCell<Vec<DndDone>> = const { RefCell::new(Vec::new()) };
}

enum DndDone {
    Count(u64, usize),
    Drop(Vec<String>),
}

/// 接下来创建的 Slint 窗口当作 toast（不是普通窗口，也不是面板）：在 `ToastWindow::new()` 之前调用。
pub fn mark_next_toast() {
    NEXT_TOAST.with(|t| t.set(true));
}

pub fn push(c: Cmd) {
    CMDS.with(|q| q.borrow_mut().push(c));
}

/// (逻辑宽, 逻辑高, 整数缩放)
pub fn outputs() -> Vec<(i32, i32, i32)> {
    OUTPUTS.with(|o| o.borrow().clone())
}

/// 自写后端在用（main.rs 据此决定挂 Linux dock）。
pub fn active() -> bool {
    ACTIVE.with(|a| a.get())
}

// ------------------------------------------------------------------ event loop proxy

type Job = Box<dyn FnOnce() + Send>;

struct Proxy {
    queue: Arc<Mutex<VecDeque<Job>>>,
    quit: Arc<std::sync::atomic::AtomicBool>,
    wake: Arc<std::os::unix::net::UnixStream>,
}

impl slint::platform::EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        self.quit.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::io::Write::write(&mut &*self.wake, &[1]);
        Ok(())
    }
    fn invoke_from_event_loop(&self, event: Job) -> Result<(), slint::EventLoopError> {
        self.queue.lock().unwrap().push_back(event);
        let _ = std::io::Write::write(&mut &*self.wake, &[1]);
        Ok(())
    }
}

// ------------------------------------------------------------------ the Slint window

pub struct WlWindow {
    window: slint::Window,
    renderer: SoftwareRenderer,
    size: Cell<PhysicalSize>,
    needs_redraw: Cell<bool>,
    /// Slint 要输入法：Some(光标矩形，逻辑 px) / None
    ime: RefCell<Option<(InputMethodRequestKind, i32, i32, i32, i32, String, usize, usize)>>,
    ime_dirty: Cell<bool>,
    /// 普通窗口（xdg toplevel）；false = 面板
    top: bool,
    id: Cell<usize>,
    title: RefCell<String>,
    pref: Cell<(u32, u32)>,
    min: Cell<(u32, u32)>,
    props_dirty: Cell<bool>,
    /// 已把装饰模式告诉过界面（每次显示后的第一次 configure 都要告诉）
    decor_told: Cell<bool>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum InputMethodRequestKind {
    Enable,
    Update,
}

impl WindowAdapter for WlWindow {
    fn window(&self) -> &slint::Window {
        &self.window
    }
    fn size(&self) -> PhysicalSize {
        self.size.get()
    }
    fn renderer(&self) -> &dyn slint::platform::Renderer {
        &self.renderer
    }
    fn request_redraw(&self) {
        self.needs_redraw.set(true);
    }
    fn set_visible(&self, visible: bool) -> Result<(), slint::PlatformError> {
        if self.top {
            push(Cmd::Top(self.id.get(), visible));
        }
        Ok(())
    }
    fn update_window_properties(&self, p: slint::platform::WindowProperties<'_>) {
        if !self.top {
            return;
        }
        *self.title.borrow_mut() = p.title().to_string();
        let c = p.layout_constraints();
        let pref = (c.preferred.width.round().max(1.0) as u32, c.preferred.height.round().max(1.0) as u32);
        let min = c.min.map(|m| (m.width.round() as u32, m.height.round() as u32)).unwrap_or((0, 0));
        if self.pref.get() != pref || self.min.get() != min {
            self.pref.set(pref);
            self.min.set(min);
        }
        self.props_dirty.set(true);
    }
    fn internal(&self, _: i_slint_core::InternalToken) -> Option<&dyn WindowAdapterInternal> {
        Some(self)
    }
}

impl WindowAdapterInternal for WlWindow {
    fn input_method_request(&self, req: InputMethodRequest) {
        let v = match req {
            InputMethodRequest::Enable(p) | InputMethodRequest::Update(p) => {
                let kind = if self.ime.borrow().is_some() { InputMethodRequestKind::Update } else { InputMethodRequestKind::Enable };
                let cursor = p.cursor_position.min(p.text.len());
                let anchor = p.anchor_position.unwrap_or(cursor).min(p.text.len());
                Some((
                    kind,
                    p.cursor_rect_origin.x as i32,
                    p.cursor_rect_origin.y as i32,
                    p.cursor_rect_size.width.max(1.0) as i32,
                    p.cursor_rect_size.height.max(1.0) as i32,
                    p.text.to_string(),
                    cursor,
                    anchor,
                ))
            }
            InputMethodRequest::Disable => None,
            #[allow(unreachable_patterns)]
            _ => return,
        };
        *self.ime.borrow_mut() = v;
        self.ime_dirty.set(true);
    }
}

impl WlWindow {
    fn ime_commit(&self, text: String) {
        let mut key_event = i_slint_core::items::KeyEvent::default();
        key_event.text = text.into();
        self.window.dispatch_event(WindowEvent::internal(InternalKeyEvent { event_type: KeyEventType::CommitComposition, key_event, ..Default::default() }));
    }
    fn ime_preedit(&self, text: String, sel: Option<std::ops::Range<i32>>) {
        self.window.dispatch_event(WindowEvent::internal(InternalKeyEvent {
            event_type: KeyEventType::UpdateComposition,
            preedit_text: text.into(),
            preedit_selection: sel,
            ..Default::default()
        }));
    }
}

// ------------------------------------------------------------------ Wayland state

struct PanelSurf {
    layer: LayerSurface,
    place: Place,
    configured: bool,
    width: u32,
    height: u32,
    /// 弹出（弹簧）/ 收起（exitTo）动画；None = 静止
    anim: Option<PanelAnim>,
    closing: bool,
    focus_req: bool,
}

struct StripSurf {
    layer: LayerSurface,
    place: Place,
    configured: bool,
    width: u32,
    height: u32,
    dirty: bool,
}

struct TopSurf {
    xdg: XdgWindow,
    configured: bool,
    /// compositor 要客户端自己画装饰（niri 没开 prefer-no-csd 时）/ 当前是否最大化
    csd: bool,
    maximized: bool,
    width: u32,
    height: u32,
    buf: Vec<Bgra>,
}

struct ToastSurf {
    layer: LayerSurface,
    place: ToastPlace,
    configured: bool,
    width: u32,
    height: u32,
    buf: Vec<Bgra>,
}

struct TopWin {
    id: usize,
    win: Weak<WlWindow>,
    surf: Option<TopSurf>,
}

struct State {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    qh: QueueHandle<State>,
    panel: Option<PanelSurf>,
    strip: Option<StripSurf>,
    strip_look: StripLook,
    pointer: Option<wl_pointer::WlPointer>,
    seat: Option<wl_seat::WlSeat>,
    /// 最近一次在普通窗口上按下：窗口 id + serial（xdg move / 窗口菜单要用）
    top_press: Option<(usize, u32)>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    kb_on_panel: bool,
    repeat: Option<(u32, u32)>,
    ti_manager: Option<ZwpTextInputManagerV3>,
    text_input: Option<ZwpTextInputV3>,
    ti_on_panel: bool,
    ti_enabled: bool,
    ti_pending: TiPending,
    win: Weak<WlWindow>,
    buf: Vec<Bgra>,
    xbuf: Vec<Bgra>,
    scale: i32,
    xdg: Option<XdgShell>,
    tops: Vec<TopWin>,
    /// 键盘 / 输入法焦点所在的普通窗口
    kb_top: Option<usize>,
    ti_top: Option<usize>,
    ddm: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    /// 拖放进行中：Some(在面板上)（false = 在停靠条上）
    dnd_on: Option<bool>,
    dnd_gen: u64,
    /// 已松开、正在后台读数据的拖放
    dnd_drop: Option<(DragOffer, bool, (f32, f32))>,
    toast: Option<ToastSurf>,
}

#[derive(Default)]
struct TiPending {
    preedit: Option<(String, i32, i32)>,
    commit: Option<String>,
    delete: Option<(u32, u32)>,
}

/// 旧版 spring.ts：弹出 = springTo(从边外 36px、scale .94、opacity 0 → 原位；stiffness 300 damping 24；opacity 140 ms 线性)；
/// 收起 = exitTo(220 ms cubic-bezier(.5,-.35,.75,.6) → 边外 28px、scale .96、opacity 0)
#[derive(Clone, Copy)]
struct PanelAnim {
    opening: bool,
    start: Instant,
    /// 收起的起点姿态（tx, ty, s, op）
    from: (f32, f32, f32, f32),
}

const EXIT_MS: f32 = 220.0;

/// springCurve：半隐式欧拉 dt = 1/240，每 1/60 s 取样，直到 |p-1| < .0008 且 |v| < .01（与旧版完全同一算法）
fn spring_curve() -> &'static [(f32, f32)] {
    static C: std::sync::OnceLock<Vec<(f32, f32)>> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        let (k, d, m) = (300.0f64, 24.0f64, 1.0f64);
        let dt = 1.0 / 240.0;
        let (mut p, mut v, mut t, mut last) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        let mut out = vec![(0.0f32, 0.0f32)];
        while t < 2.5 {
            let a = (-k * (p - 1.0) - d * v) / m;
            v += a * dt;
            p += v * dt;
            t += dt;
            if t - last >= 1.0 / 60.0 {
                out.push(((t * 1000.0) as f32, p as f32));
                last = t;
            }
            if (p - 1.0).abs() < 0.0008 && v.abs() < 0.01 {
                break;
            }
        }
        out.push(((t * 1000.0) as f32, 1.0));
        out
    })
}

fn spring_at(ms: f32) -> Option<f32> {
    let c = spring_curve();
    let end = c.last()?.0;
    if ms >= end {
        return None;
    }
    // WAAPI 关键帧之间线性插值
    let i = c.partition_point(|x| x.0 <= ms).clamp(1, c.len() - 1);
    let (a, b) = (c[i - 1], c[i]);
    let t = if b.0 > a.0 { (ms - a.0) / (b.0 - a.0) } else { 1.0 };
    Some(a.1 + (b.1 - a.1) * t)
}

fn edge_offset(edge: &str, dist: f32) -> (f32, f32) {
    match edge {
        "left" => (-dist, 0.0),
        "top" => (0.0, -dist),
        "bottom" => (0.0, dist),
        _ => (dist, 0.0),
    }
}

impl PanelAnim {
    /// 当前姿态 (tx, ty, scale, opacity)；None = 动画已结束
    fn pose(&self, edge: &str) -> Option<(f32, f32, f32, f32)> {
        let ms = self.start.elapsed().as_secs_f32() * 1000.0;
        if self.opening {
            let p = spring_at(ms)?;
            let (ex, ey) = edge_offset(edge, 36.0);
            let op = (ms / 140.0).min(1.0);
            Some((ex * (1.0 - p), ey * (1.0 - p), 0.94 + 0.06 * p, op))
        } else {
            if ms >= EXIT_MS {
                return None;
            }
            let e = crate::fx::bezier(0.5, -0.35, 0.75, 0.6, ms / EXIT_MS);
            let (ex, ey) = edge_offset(edge, 28.0);
            let f = self.from;
            Some((f.0 + (ex - f.0) * e, f.1 + (ey - f.1) * e, f.2 + (0.96 - f.2) * e, f.3 + (0.0 - f.3) * e))
        }
    }
}

pub struct WlPlatform {
    conn: Connection,
    queue: RefCell<Option<EventQueue<State>>>,
    state: RefCell<Option<State>>,
    jobs: Arc<Mutex<VecDeque<Job>>>,
    quit: Arc<std::sync::atomic::AtomicBool>,
    wake_rx: std::os::unix::net::UnixStream,
    wake_tx: Arc<std::os::unix::net::UnixStream>,
}

/// 无原生材质：外框 8px 留给圆角 14 的外壳与阴影（旧版 dock.rs MARGIN）
pub const FRAME: u32 = 8;
pub const W: u32 = 360 + 2 * FRAME;
pub const H: u32 = 520 + 2 * FRAME;

fn bind_ti(globals: &GlobalList, qh: &QueueHandle<State>) -> Option<ZwpTextInputManagerV3> {
    globals.bind::<ZwpTextInputManagerV3, _, _>(qh, 1..=1, TiData).map_err(|e| log::info!("text-input-v3 unavailable: {e}")).ok()
}

impl WlPlatform {
    pub fn new() -> Result<Self, String> {
        let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let compositor = CompositorState::bind(&globals, &qh).map_err(|e| format!("wl_compositor: {e}"))?;
        let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| format!("layer shell: {e}"))?;
        let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {e}"))?;
        let pool = SlotPool::new((W * H * 4) as usize, &shm).map_err(|e| e.to_string())?;
        let ti_manager = bind_ti(&globals, &qh);
        let xdg = XdgShell::bind(&globals, &qh).map_err(|e| log::warn!("xdg-shell unavailable ({e}): settings window disabled")).ok();
        let ddm = DataDeviceManagerState::bind(&globals, &qh).map_err(|e| log::warn!("wl_data_device_manager unavailable ({e}): no drag & drop")).ok();
        let mut state = State {
            registry_state: RegistryState::new(&globals),
            seat_state: SeatState::new(&globals, &qh),
            output_state: OutputState::new(&globals, &qh),
            compositor,
            layer_shell,
            shm,
            pool,
            qh: qh.clone(),
            panel: None,
            toast: None,
            strip: None,
            strip_look: StripLook { edge: "right".into(), unread: false, open: false, hovering: false, accent: (10.0 / 255.0, 132.0 / 255.0, 1.0), phase: 0.0 },
            pointer: None,
            seat: None,
            top_press: None,
            keyboard: None,
            kb_on_panel: false,
            repeat: Some((25, 600)),
            ti_manager,
            text_input: None,
            ti_on_panel: false,
            ti_enabled: false,
            ti_pending: TiPending::default(),
            win: Weak::new(),
            buf: Vec::new(),
            xbuf: Vec::new(),
            scale: 1,
            xdg,
            tops: Vec::new(),
            kb_top: None,
            ti_top: None,
            ddm,
            data_device: None,
            dnd_on: None,
            dnd_gen: 0,
            dnd_drop: None,
        };
        // 一次往返：拿到输出信息（dock 算位置要用）和 seat 能力
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        state.sync_outputs();
        let (wake_rx, wake_tx) = std::os::unix::net::UnixStream::pair().map_err(|e| e.to_string())?;
        wake_rx.set_nonblocking(true).ok();
        ACTIVE.with(|a| a.set(true));
        Ok(WlPlatform {
            conn,
            queue: RefCell::new(Some(queue)),
            state: RefCell::new(Some(state)),
            jobs: Default::default(),
            quit: Default::default(),
            wake_rx,
            wake_tx: Arc::new(wake_tx),
        })
    }
}

impl slint::platform::Platform for WlPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        // 第一个窗口 = 面板（layer surface）；之后的 = 普通窗口（xdg toplevel，设置页）；mark_next_toast 之后的 = toast
        let toast = NEXT_TOAST.with(|t| t.replace(false));
        let top = !toast && PANEL_MADE.with(|p| p.replace(true));
        let (w0, h0) = if toast { (300, 80) } else if top { (480, 620) } else { (W, H) };
        let w = Rc::<WlWindow>::new_cyclic(|weak: &Weak<WlWindow>| WlWindow {
            window: slint::Window::new(weak.clone() as Weak<dyn WindowAdapter>),
            renderer: SoftwareRenderer::new_with_repaint_buffer_type(RepaintBufferType::NewBuffer),
            size: Cell::new(PhysicalSize::new(w0, h0)),
            needs_redraw: Cell::new(true),
            ime: RefCell::new(None),
            ime_dirty: Cell::new(false),
            top,
            id: Cell::new(0),
            title: RefCell::new(String::new()),
            pref: Cell::new((w0, h0)),
            min: Cell::new((0, 0)),
            props_dirty: Cell::new(false),
            decor_told: Cell::new(false),
        });
        w.id.set(Rc::as_ptr(&w) as *const u8 as usize);
        w.window.dispatch_event(WindowEvent::Resized { size: slint::LogicalSize::new(w0 as f32, h0 as f32) });
        if toast {
            TOAST_WIN.with(|t| *t.borrow_mut() = Rc::downgrade(&w));
        } else if top {
            NEW_TOPS.with(|t| t.borrow_mut().push(Rc::downgrade(&w)));
        } else if let Some(s) = self.state.borrow_mut().as_mut() {
            s.win = Rc::downgrade(&w);
        }
        Ok(w)
    }

    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        let mut queue = self.queue.borrow_mut().take().ok_or("event loop already running")?;
        let mut state = self.state.borrow_mut().take().ok_or("no state")?;
        // 没有 dock（LANCAT_SLINT_NODOCK=1 或 dock 还没接上）：面板直接贴右缘显示
        if std::env::var("LANCAT_SLINT_NODOCK").as_deref() == Ok("1") {
            push(Cmd::Panel(Some(Place { edge: "right".into(), along: 200, inset: 0, w: W, h: H, output: 0 })));
        }
        loop {
            // 新建的普通窗口
            for w in NEW_TOPS.with(|t| std::mem::take(&mut *t.borrow_mut())) {
                if let Some(rc) = w.upgrade() {
                    state.tops.push(TopWin { id: rc.id.get(), win: w, surf: None });
                }
            }
            state.tops.retain(|t| t.win.strong_count() > 0);
            // dock 的命令
            let cmds = CMDS.with(|q| std::mem::take(&mut *q.borrow_mut()));
            for c in cmds {
                state.apply(c);
            }
            pump_wheel();
            slint::platform::update_timers_and_animations();
            state.sync_ime();
            let fading = state.panel.as_ref().is_some_and(|p| p.configured && p.anim.is_some()) || crate::compose::animating();
            if let Some(w) = state.win.upgrade() {
                let ready = state.panel.as_ref().is_some_and(|p| p.configured);
                if ready && (w.needs_redraw.get() || w.window.has_active_animations() || fading) {
                    state.draw_panel();
                }
            }
            state.finish_close();
            if state.strip.as_ref().is_some_and(|s| s.configured && s.dirty) {
                state.draw_strip();
            }
            state.draw_tops();
            state.draw_toast();
            for d in DND_DONE.with(|q| std::mem::take(&mut *q.borrow_mut())) {
                state.dnd_done(d);
            }
            queue.dispatch_pending(&mut state).map_err(|e| e.to_string())?;
            queue.flush().map_err(|e| e.to_string())?;
            let anim = fading
                || wheel_busy()
                || state.win.upgrade().is_some_and(|w| w.window.has_active_animations() && state.panel.is_some())
                || state.tops.iter().any(|t| t.surf.as_ref().is_some_and(|s| s.configured) && t.win.upgrade().is_some_and(|w| w.window.has_active_animations()))
                || state.toast.as_ref().is_some_and(|t| t.configured) && TOAST_WIN.with(|t| t.borrow().upgrade()).is_some_and(|w| w.window.has_active_animations());
            let pending_cmds = CMDS.with(|q| !q.borrow().is_empty());
            let timeout = slint::platform::duration_until_next_timer_update().map(|d| d.min(Duration::from_secs(1)));
            let ms: i32 = if pending_cmds {
                0
            } else if anim {
                8
            } else {
                timeout.map(|d| d.as_millis().min(1000) as i32).unwrap_or(-1)
            };
            if let Some(guard) = queue.prepare_read() {
                let fd = guard.connection_fd().as_raw_fd();
                let mut fds = [pollfd(fd), pollfd(self.wake_rx.as_fd().as_raw_fd())];
                let n = unsafe { poll(fds.as_mut_ptr(), 2, ms) };
                if n > 0 && fds[0].revents != 0 {
                    let _ = guard.read();
                } else {
                    drop(guard);
                }
                if fds[1].revents != 0 {
                    let mut b = [0u8; 64];
                    while std::io::Read::read(&mut &self.wake_rx, &mut b).map(|n| n > 0).unwrap_or(false) {}
                }
            }
            queue.dispatch_pending(&mut state).map_err(|e| e.to_string())?;
            loop {
                let job = self.jobs.lock().unwrap().pop_front();
                match job {
                    Some(j) => j(),
                    None => break,
                }
            }
            for d in DND_DONE.with(|q| std::mem::take(&mut *q.borrow_mut())) {
                state.dnd_done(d);
            }
            if self.quit.swap(false, std::sync::atomic::Ordering::SeqCst) {
                break;
            }
        }
        let _ = &self.conn;
        Ok(())
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn slint::platform::EventLoopProxy>> {
        Some(Box::new(Proxy { queue: self.jobs.clone(), quit: self.quit.clone(), wake: self.wake_tx.clone() }))
    }
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}
fn pollfd(fd: i32) -> PollFd {
    PollFd { fd, events: 1 /* POLLIN */, revents: 0 }
}
extern "C" {
    fn poll(fds: *mut PollFd, n: u64, timeout: i32) -> i32;
}

// ------------------------------------------------------------------ wheel (WebKitGTK 的滚动步长与平滑滚动)

/// 面板消息列表的可视高（逻辑 px），滚轮步长按它算
const PANEL_VIEW_H: f32 = 424.0;
/// WebKit ScrollAnimationSmooth 的时长（按旧版录屏量的近似）
const SMOOTH_MS: f32 = 150.0;

struct Smooth {
    win: Weak<WlWindow>,
    pos: LogicalPosition,
    total: (f32, f32),
    sent: (f32, f32),
    t0: Instant,
}

thread_local! {
    static SMOOTH: RefCell<Vec<Smooth>> = const { RefCell::new(Vec::new()) };
}

/// 滚轮：旧版 WebKitGTK —— GDK 平滑滚动的 delta（libinput 一格 15 → 1.5）× Scrollbar::pixelsPerLineStep(可视高) =
/// 可视高^(2/3)（GTK / WPE 分支），再由平滑滚动动画分几帧走完（实测设置页 971 高时一格 147 px）。
fn wheel(w: &Rc<WlWindow>, position: LogicalPosition, ha: f64, hd: i32, va: f64, vd: i32, view_h: f32) {
    let unit = |abs: f64, disc: i32| -> f32 { if abs != 0.0 { abs as f32 / 10.0 } else { disc as f32 * 1.5 } };
    let step = view_h.max(1.0).powf(2.0 / 3.0);
    let (dx, dy) = (-unit(ha, hd) * step, -unit(va, vd) * step);
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    SMOOTH.with(|q| {
        let mut q = q.borrow_mut();
        // 同一窗口上一段还没走完：剩下的并进来，从现在重新计时（WebKit 也是把新增量叠加到目标上）
        if let Some(s) = q.iter_mut().find(|s| s.win.ptr_eq(&Rc::downgrade(w))) {
            s.total = (s.total.0 - s.sent.0 + dx, s.total.1 - s.sent.1 + dy);
            s.sent = (0.0, 0.0);
            s.t0 = Instant::now();
            s.pos = position;
        } else {
            q.push(Smooth { win: Rc::downgrade(w), pos: position, total: (dx, dy), sent: (0.0, 0.0), t0: Instant::now() });
        }
    });
    pump_wheel();
}

fn pump_wheel() {
    let jobs: Vec<(Rc<WlWindow>, LogicalPosition, f32, f32)> = SMOOTH.with(|q| {
        let mut q = q.borrow_mut();
        let mut out = Vec::new();
        q.retain_mut(|s| {
            let Some(w) = s.win.upgrade() else { return false };
            let k = (s.t0.elapsed().as_secs_f32() * 1000.0 / SMOOTH_MS).min(1.0);
            // ease-out（三次）
            let e = 1.0 - (1.0 - k).powi(3);
            let want = (s.total.0 * e, s.total.1 * e);
            let d = (want.0 - s.sent.0, want.1 - s.sent.1);
            s.sent = want;
            if d.0 != 0.0 || d.1 != 0.0 {
                out.push((w, s.pos, d.0, d.1));
            }
            k < 1.0
        });
        out
    });
    for (w, position, dx, dy) in jobs {
        w.window.dispatch_event(WindowEvent::PointerScrolled { position, delta_x: dx, delta_y: dy });
    }
}

fn wheel_busy() -> bool {
    SMOOTH.with(|q| !q.borrow().is_empty())
}

fn anchor_of(edge: &str) -> Anchor {
    match edge {
        "left" => Anchor::LEFT | Anchor::TOP,
        "top" => Anchor::TOP | Anchor::LEFT,
        "bottom" => Anchor::BOTTOM | Anchor::LEFT,
        _ => Anchor::RIGHT | Anchor::TOP,
    }
}

/// layer-shell 的 margin 顺序：top, right, bottom, left
fn margins(p: &Place) -> (i32, i32, i32, i32) {
    match p.edge.as_str() {
        "left" => (p.along, 0, 0, p.inset),
        "top" => (p.inset, 0, 0, p.along),
        "bottom" => (0, 0, p.inset, p.along),
        _ => (p.along, p.inset, 0, 0),
    }
}

impl State {
    fn sync_outputs(&mut self) {
        let v: Vec<(i32, i32, i32)> = self
            .output_state
            .outputs()
            .filter_map(|o| self.output_state.info(&o))
            .map(|i| {
                let (w, h) = i.logical_size.unwrap_or_else(|| i.modes.iter().find(|m| m.current).map(|m| m.dimensions).unwrap_or((1920, 1080)));
                (w, h, i.scale_factor.max(1))
            })
            .collect();
        if let Some(&(_, _, s)) = v.first() {
            self.scale = s;
        }
        OUTPUTS.with(|o| *o.borrow_mut() = v);
    }

    fn output(&self, n: usize) -> Option<wl_output::WlOutput> {
        let list: Vec<_> = self.output_state.outputs().collect();
        list.get(n).or(list.first()).cloned()
    }

    fn new_layer(&self, ns: &'static str, p: &Place, kb: KeyboardInteractivity) -> LayerSurface {
        let surface = self.compositor.create_surface(&self.qh);
        let out = self.output(p.output);
        let layer = self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Overlay, Some(ns), out.as_ref());
        layer.set_anchor(anchor_of(&p.edge));
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(kb);
        let (t, r, b, l) = margins(p);
        layer.set_margin(t, r, b, l);
        layer.set_size(p.w, p.h);
        layer.wl_surface().set_buffer_scale(self.scale);
        layer.commit();
        layer
    }

    fn apply(&mut self, c: Cmd) {
        match c {
            Cmd::Panel(Some(p)) => {
                match self.panel.as_mut() {
                    Some(ps) if !ps.closing => {
                        if ps.place != p {
                            ps.layer.set_anchor(anchor_of(&p.edge));
                            let (t, r, b, l) = margins(&p);
                            ps.layer.set_margin(t, r, b, l);
                            ps.layer.set_size(p.w, p.h);
                            ps.layer.commit();
                            ps.place = p;
                        }
                    }
                    Some(ps) => {
                        // 正在收起又被打开：旧版 anim.cancel() 后重新 springTo（从固定起点），surface 不重建
                        ps.closing = false;
                        ps.anim = Some(PanelAnim { opening: true, start: Instant::now(), from: (0.0, 0.0, 1.0, 1.0) });
                        ps.layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
                        ps.layer.commit();
                    }
                    None => {
                        let layer = self.new_layer("lancat-panel", &p, KeyboardInteractivity::OnDemand);
                        self.panel = Some(PanelSurf { layer, place: p, configured: false, width: W, height: H, anim: Some(PanelAnim { opening: true, start: Instant::now(), from: (0.0, 0.0, 1.0, 1.0) }), closing: false, focus_req: false });
                    }
                }
            }
            Cmd::Panel(None) => {
                if let Some(ps) = self.panel.as_mut() {
                    if !ps.closing {
                        let edge = ps.place.edge.clone();
                        let from = if !ps.configured { (0.0, 0.0, 1.0, 0.0) } else { ps.anim.and_then(|a| a.pose(&edge)).unwrap_or((0.0, 0.0, 1.0, 1.0)) };
                        ps.closing = true;
                        ps.anim = Some(PanelAnim { opening: false, start: Instant::now(), from });
                        ps.layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                        ps.layer.commit();
                    }
                }
            }
            Cmd::PanelFocus => {
                if let Some(ps) = self.panel.as_mut() {
                    ps.focus_req = true;
                    ps.layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
                    ps.layer.commit();
                }
            }
            Cmd::Top(id, true) => self.show_top(id),
            Cmd::TopAct(a) => self.top_act(a),
            Cmd::Top(id, false) => {
                if let Some(t) = self.tops.iter_mut().find(|t| t.id == id) {
                    t.surf = None;
                    if let Some(w) = t.win.upgrade() {
                        w.decor_told.set(false);
                    }
                }
                if self.kb_top == Some(id) {
                    self.kb_top = None;
                    REPEAT.with(|r| *r.borrow_mut() = None);
                }
                if self.ti_top == Some(id) {
                    self.ti_top = None;
                }
            }
            Cmd::Strip(Some(p)) => match self.strip.as_mut() {
                Some(s) => {
                    if s.place != p {
                        s.layer.set_anchor(anchor_of(&p.edge));
                        let (t, r, b, l) = margins(&p);
                        s.layer.set_margin(t, r, b, l);
                        s.layer.set_size(p.w, p.h);
                        s.layer.commit();
                        s.place = p;
                        s.dirty = true;
                    }
                }
                None => {
                    let layer = self.new_layer("lancat-dock", &p, KeyboardInteractivity::None);
                    let (w, h) = (p.w, p.h);
                    self.strip = Some(StripSurf { layer, place: p, configured: false, width: w, height: h, dirty: true });
                }
            },
            Cmd::Strip(None) => {
                self.strip = None;
            }
            Cmd::Toast(Some(p)) => {
                let same_output = self.toast.as_ref().is_some_and(|t| t.place.output == p.output);
                if same_output {
                    let (changed, input_changed) = self.toast.as_ref().map(|t| (t.place != p, t.place.input != p.input)).unwrap_or((false, false));
                    if changed {
                        if let Some(t) = self.toast.as_ref() {
                            t.layer.set_margin(p.y, 0, 0, p.x);
                            t.layer.set_size(p.w, p.h);
                        }
                        if input_changed {
                            self.toast_input(&p);
                        }
                        if let Some(t) = self.toast.as_mut() {
                            t.layer.commit();
                            t.place = p;
                        }
                    }
                } else {
                    let surface = self.compositor.create_surface(&self.qh);
                    let out = self.output(p.output);
                    let layer = self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Overlay, Some("lancat-toast"), out.as_ref());
                    layer.set_anchor(Anchor::LEFT | Anchor::TOP);
                    layer.set_exclusive_zone(-1);
                    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                    layer.set_margin(p.y, 0, 0, p.x);
                    layer.set_size(p.w, p.h);
                    layer.wl_surface().set_buffer_scale(self.scale);
                    let (w, h) = (p.w, p.h);
                    self.toast = Some(ToastSurf { layer, place: p.clone(), configured: false, width: w, height: h, buf: Vec::new() });
                    self.toast_input(&p);
                    if let Some(t) = self.toast.as_ref() {
                        t.layer.commit();
                    }
                }
            }
            Cmd::Toast(None) => {
                self.toast = None;
            }
            Cmd::StripLook(l) => {
                if self.strip_look != l {
                    self.strip_look = l;
                    if let Some(s) = self.strip.as_mut() {
                        s.dirty = true;
                    }
                }
            }
        }
    }

    /// 淡出结束：销毁面板 surface，释放像素缓冲。
    fn finish_close(&mut self) {
        let done = self.panel.as_ref().is_some_and(|p| p.closing && (!p.configured || p.anim.is_none_or(|a| a.start.elapsed().as_secs_f32() * 1000.0 >= EXIT_MS)));
        if done {
            self.panel = None;
            self.kb_on_panel = false;
            self.buf = Vec::new();
            if let Some(w) = self.win.upgrade() {
                w.window.dispatch_event(WindowEvent::WindowActiveChanged(false));
                w.window.dispatch_event(WindowEvent::PointerExited);
            }
        }
    }

    fn draw_panel(&mut self) {
        let Some(w) = self.win.upgrade() else { return };
        let Some(ps) = self.panel.as_mut() else { return };
        let sc = self.scale.max(1) as u32;
        let (width, height) = (ps.width * sc, ps.height * sc);
        let n = (width * height) as usize;
        // 缓冲是我们自己的、常驻：ReusedBuffer 局部重画（每帧再整幅拷进 shm）；新分配（尺寸变了 / 收起后释放）时整幅画一次
        let fresh = self.buf.len() != n;
        if fresh {
            self.buf = vec![Bgra::default(); n];
            w.renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
        }
        w.needs_redraw.set(false);
        let out = crate::compose::render(&w.renderer, &mut self.buf, width as usize, height as usize, sc as f32, true);
        if fresh {
            w.renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
        }
        if out.composed() {
            w.needs_redraw.set(false);
        }
        // 面板整体的弹出/收起变换
        let edge = ps.place.edge.clone();
        let pose = ps.anim.and_then(|a| a.pose(&edge));
        if pose.is_none() && !ps.closing {
            ps.anim = None;
        }
        let Ok((buffer, canvas)) = self.pool.create_buffer(width as i32, height as i32, width as i32 * 4, wl_shm::Format::Argb8888) else { return };
        let src: &[Bgra] = match pose {
            Some((tx, ty, s, op)) => {
                if self.xbuf.len() != n {
                    self.xbuf = vec![Bgra::default(); n];
                }
                crate::compose::xform_frame(&self.buf, &mut self.xbuf, width as usize, height as usize, s, tx * sc as f32, ty * sc as f32, op);
                &self.xbuf
            }
            None if ps.closing => {
                // 收起动画结束、surface 还没销毁的那一帧：全透明
                if self.xbuf.len() != n {
                    self.xbuf = vec![Bgra::default(); n];
                }
                self.xbuf.iter_mut().for_each(|p| *p = Bgra::default());
                &self.xbuf
            }
            None => &self.buf,
        };
        // SAFETY: Bgra is repr(C) 4 bytes, canvas is width*height*4 bytes
        let bytes = unsafe { std::slice::from_raw_parts(src.as_ptr() as *const u8, n * 4) };
        canvas[..n * 4].copy_from_slice(bytes);
        let s = ps.layer.wl_surface();
        s.damage_buffer(0, 0, width as i32, height as i32);
        if buffer.attach_to(s).is_ok() {
            ps.layer.commit();
        }
    }

    /// 只有卡片接收指针（旧版 input_shape_combine_region）
    fn toast_input(&self, p: &ToastPlace) {
        let Some(t) = self.toast.as_ref() else { return };
        if let Ok(r) = smithay_client_toolkit::compositor::Region::new(&self.compositor) {
            let (x, y, w, h) = p.input;
            r.add(x, y, w.max(1), h.max(1));
            t.layer.wl_surface().set_input_region(Some(r.wl_region()));
        }
    }

    fn draw_toast(&mut self) {
        let Some(w) = TOAST_WIN.with(|t| t.borrow().upgrade()) else { return };
        let Some(ts) = self.toast.as_mut().filter(|t| t.configured) else { return };
        if !(w.needs_redraw.get() || w.window.has_active_animations()) {
            return;
        }
        let sc = self.scale.max(1) as u32;
        let (width, height) = (ts.width * sc, ts.height * sc);
        let n = (width * height) as usize;
        if ts.buf.len() != n {
            ts.buf = vec![Bgra::default(); n];
        }
        w.needs_redraw.set(false);
        ts.buf.iter_mut().for_each(|p| *p = Bgra::default());
        w.renderer.render(&mut ts.buf[..], width as usize);
        let Ok((buffer, canvas)) = self.pool.create_buffer(width as i32, height as i32, width as i32 * 4, wl_shm::Format::Argb8888) else { return };
        // SAFETY: Bgra is repr(C) 4 bytes
        let bytes = unsafe { std::slice::from_raw_parts(ts.buf.as_ptr() as *const u8, n * 4) };
        canvas[..n * 4].copy_from_slice(bytes);
        let s = ts.layer.wl_surface();
        s.damage_buffer(0, 0, width as i32, height as i32);
        if buffer.attach_to(s).is_ok() {
            ts.layer.commit();
        }
    }

    fn draw_strip(&mut self) {
        let Some(st) = self.strip.as_mut() else { return };
        st.dirty = false;
        let sc = self.scale.max(1) as u32;
        let (w, h) = (st.width * sc, st.height * sc);
        let Ok((buffer, canvas)) = self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888) else { return };
        paint_strip(canvas, w as usize, h as usize, &self.strip_look);
        let s = st.layer.wl_surface();
        s.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(s).is_ok() {
            st.layer.commit();
        }
    }

    /// Slint 的输入法请求 → text-input-v3（只在键盘焦点在面板上时 enable）。
    fn sync_ime(&mut self) {
        // 输入法跟着 text-input 的焦点窗口（面板或某个普通窗口）；其余窗口的请求只清掉脏标记
        let target = if self.ti_on_panel { self.win.upgrade() } else { self.ti_top.and_then(|id| self.top_window(id)) };
        let mut others: Vec<Rc<WlWindow>> = self.tops.iter().filter_map(|t| t.win.upgrade()).collect();
        others.extend(self.win.upgrade());
        let target_dirty = target.as_ref().is_some_and(|w| w.ime_dirty.get());
        for o in &others {
            if !target.as_ref().is_some_and(|t| Rc::ptr_eq(t, o)) && o.ime_dirty.replace(false) && target.is_none() && self.ti_enabled {
                // 焦点不在任何窗口：关掉
                if let Some(ti) = self.text_input.as_ref() {
                    ti.disable();
                    ti.commit();
                }
                self.ti_enabled = false;
            }
        }
        let Some(w) = target else { return };
        if !target_dirty {
            return;
        }
        w.ime_dirty.set(false);
        let Some(ti) = self.text_input.as_ref() else { return };
        let req = w.ime.borrow().clone();
        match req {
            Some((_, x, y, cw, chh, text, cursor, anchor)) => {
                if !self.ti_enabled {
                    ti.enable();
                    ti.set_content_type(zwp_text_input_v3::ContentHint::None, zwp_text_input_v3::ContentPurpose::Normal);
                    self.ti_enabled = true;
                }
                // surrounding text 最多 4000 字节（协议限制），截在字符边界上
                if text.len() < 4000 {
                    ti.set_surrounding_text(text, cursor as i32, anchor as i32);
                }
                ti.set_cursor_rectangle(x, y, cw, chh);
                ti.commit();
            }
            _ => {
                if self.ti_enabled {
                    ti.disable();
                    ti.commit();
                    self.ti_enabled = false;
                }
            }
        }
    }
}

// ------------------------------------------------------------------ normal windows (xdg toplevel)

impl State {
    fn top_window(&self, id: usize) -> Option<Rc<WlWindow>> {
        self.tops.iter().find(|t| t.id == id).and_then(|t| t.win.upgrade())
    }

    fn top_of_surface(&self, s: &wl_surface::WlSurface) -> Option<usize> {
        self.tops.iter().find(|t| t.surf.as_ref().is_some_and(|x| x.xdg.wl_surface() == s)).map(|t| t.id)
    }

    fn show_top(&mut self, id: usize) {
        let Some(xdg) = self.xdg.as_ref() else { return };
        let Some(t) = self.tops.iter_mut().find(|t| t.id == id) else { return };
        let Some(w) = t.win.upgrade() else { return };
        if let Some(ts) = t.surf.as_ref() {
            // 已经开着：带到前面（xdg-activation 需要令牌，这里只能重设标题提示用户；niri 会在原位）
            ts.xdg.set_title(w.title.borrow().clone());
            return;
        }
        let surface = self.compositor.create_surface(&self.qh);
        surface.set_buffer_scale(self.scale);
        let xw = xdg.create_window(surface, WindowDecorations::RequestServer, &self.qh);
        xw.set_title(w.title.borrow().clone());
        xw.set_app_id("lancat");
        let (mw, mh) = w.min.get();
        if mw > 0 && mh > 0 {
            xw.set_min_size(Some((mw, mh)));
        }
        xw.commit();
        w.props_dirty.set(false);
        let (pw, ph) = w.pref.get();
        t.surf = Some(TopSurf { xdg: xw, configured: false, csd: false, maximized: false, width: pw.max(1), height: ph.max(1), buf: Vec::new() });
    }

    fn draw_tops(&mut self) {
        let sc = self.scale.max(1) as u32;
        let ids: Vec<usize> = self.tops.iter().filter(|t| t.surf.as_ref().is_some_and(|s| s.configured)).map(|t| t.id).collect();
        for id in ids {
            let Some(w) = self.top_window(id) else { continue };
            if w.props_dirty.replace(false) {
                if let Some(ts) = self.tops.iter().find(|t| t.id == id).and_then(|t| t.surf.as_ref()) {
                    ts.xdg.set_title(w.title.borrow().clone());
                    let (mw, mh) = w.min.get();
                    ts.xdg.set_min_size(if mw > 0 && mh > 0 { Some((mw, mh)) } else { None });
                }
            }
            if !(w.needs_redraw.get() || w.window.has_active_animations()) {
                continue;
            }
            self.draw_top(id, &w, sc);
        }
    }

    fn draw_top(&mut self, id: usize, w: &Rc<WlWindow>, sc: u32) {
        let Some(ts) = self.tops.iter_mut().find(|t| t.id == id).and_then(|t| t.surf.as_mut()) else { return };
        let (width, height) = (ts.width * sc, ts.height * sc);
        let n = (width * height) as usize;
        if ts.buf.len() != n {
            ts.buf = vec![Bgra::default(); n];
        }
        w.needs_redraw.set(false);
        ts.buf.iter_mut().for_each(|p| *p = Bgra::default());
        w.renderer.render(&mut ts.buf[..], width as usize);
        let Ok((buffer, canvas)) = self.pool.create_buffer(width as i32, height as i32, width as i32 * 4, wl_shm::Format::Argb8888) else { return };
        // SAFETY: Bgra is repr(C) 4 bytes
        let bytes = unsafe { std::slice::from_raw_parts(ts.buf.as_ptr() as *const u8, n * 4) };
        canvas[..n * 4].copy_from_slice(bytes);
        let s = ts.xdg.wl_surface();
        s.damage_buffer(0, 0, width as i32, height as i32);
        if buffer.attach_to(s).is_ok() {
            ts.xdg.commit();
        }
    }
}

impl WindowHandler for State {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &XdgWindow) {
        let Some(id) = self.tops.iter().find(|t| t.surf.as_ref().is_some_and(|s| &s.xdg == window)).map(|t| t.id) else { return };
        if let Some(w) = self.top_window(id) {
            // Slint 决定（默认隐藏 → set_visible(false) → Cmd::Top(id, false) 销毁 toplevel）
            w.window.dispatch_event(WindowEvent::CloseRequested);
        }
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &XdgWindow, c: WindowConfigure, _: u32) {
        let sc = self.scale.max(1) as u32;
        let Some(t) = self.tops.iter_mut().find(|t| t.surf.as_ref().is_some_and(|s| &s.xdg == window)) else { return };
        let id = t.id;
        let Some(w) = t.win.upgrade() else { return };
        let ts = t.surf.as_mut().unwrap();
        let (pw, ph) = w.pref.get();
        let (mw, mh) = w.min.get();
        ts.width = c.new_size.0.map(|v| v.get()).unwrap_or(if ts.configured { ts.width } else { pw }).max(mw).max(1);
        ts.height = c.new_size.1.map(|v| v.get()).unwrap_or(if ts.configured { ts.height } else { ph }).max(mh).max(1);
        ts.configured = true;
        let (csd, maxi) = (matches!(c.decoration_mode, DecorationMode::Client), c.is_maximized());
        let decor_changed = (csd, maxi) != (ts.csd, ts.maximized) || !w.decor_told.replace(true);
        ts.csd = csd;
        ts.maximized = maxi;
        let (lw, lh) = (ts.width, ts.height);
        w.size.set(PhysicalSize::new(lw * sc, lh * sc));
        w.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: sc as f32 });
        w.window.dispatch_event(WindowEvent::Resized { size: slint::LogicalSize::new(lw as f32, lh as f32) });
        if decor_changed {
            crate::settings_ui::wl_decor(csd, maxi);
        }
        w.needs_redraw.set(true);
        self.draw_top(id, &w, sc);
    }
}

// ------------------------------------------------------------------ drag & drop (wl_data_device)

const URI_LIST: &str = "text/uri-list";

/// text/uri-list → 本地路径（file:// URI，%XX 解码；注释行和非 file 的跳过）
fn parse_uri_list(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let rest = l.strip_prefix("file://")?;
            // file://host/path：只认本机（空主机或 localhost）
            let path = if rest.starts_with('/') { rest } else { rest.strip_prefix("localhost")? };
            let b = path.as_bytes();
            let mut out = Vec::with_capacity(b.len());
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' && i + 2 < b.len() {
                    if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                }
                out.push(b[i]);
                i += 1;
            }
            String::from_utf8(out).ok()
        })
        .collect()
}

/// 后台线程读完管道，回到事件循环线程交给 State（DND_DONE）
fn read_pipe_async(pipe: smithay_client_toolkit::data_device_manager::ReadPipe, done: impl FnOnce(Vec<String>) -> DndDone + Send + 'static) {
    std::thread::spawn(move || {
        let mut pipe = pipe;
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut pipe, &mut s);
        let files = parse_uri_list(&s);
        let d = done(files);
        let _ = slint::invoke_from_event_loop(move || DND_DONE.with(|q| q.borrow_mut().push(d)));
    });
}

impl State {
    fn dnd_done(&mut self, d: DndDone) {
        match d {
            DndDone::Count(gen, n) => {
                if gen == self.dnd_gen && self.dnd_on.is_some() {
                    crate::dock::wl_drag_count(n);
                }
            }
            DndDone::Drop(files) => {
                let Some((offer, panel, pos)) = self.dnd_drop.take() else { return };
                offer.finish();
                offer.destroy();
                log::info!("drop: {} item(s) on {}", files.len(), if panel { "panel" } else { "strip" });
                crate::dock::wl_drop(panel, files, if panel { Some(pos) } else { None });
            }
        }
    }
}

impl DataDeviceHandler for State {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, dd: &WlDataDevice, x: f64, y: f64, surface: &wl_surface::WlSurface) {
        let panel = self.panel.as_ref().is_some_and(|p| p.layer.wl_surface() == surface && !p.closing);
        let strip = self.strip.as_ref().is_some_and(|s| s.layer.wl_surface() == surface);
        let Some(offer) = wayland_client::Proxy::data::<DataDeviceData>(dd).and_then(|d| d.drag_offer()) else { return };
        let files = offer.with_mime_types(|m| m.iter().any(|t| t == URI_LIST));
        if !(panel || strip) || !files {
            offer.accept_mime_type(offer.serial, None);
            offer.set_actions(DndAction::empty(), DndAction::empty());
            return;
        }
        offer.accept_mime_type(offer.serial, Some(URI_LIST.into()));
        offer.set_actions(DndAction::Copy, DndAction::Copy);
        let fresh = self.dnd_on.is_none();
        self.dnd_on = Some(panel);
        if fresh {
            // 新的一次拖拽：预读一次 uri-list 拿项数（同一次拖拽从停靠条移到面板不重读）
            self.dnd_gen += 1;
            let gen = self.dnd_gen;
            if let Ok(pipe) = offer.receive(URI_LIST.into()) {
                read_pipe_async(pipe, move |f| DndDone::Count(gen, f.len()));
            }
        }
        crate::dock::wl_drag_enter(panel, x as f32, y as f32);
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        if self.dnd_drop.is_some() {
            return; // 松开后的 leave：等数据读完
        }
        if let Some(panel) = self.dnd_on.take() {
            crate::dock::wl_drag_leave(panel);
        }
    }
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, x: f64, y: f64) {
        if self.dnd_on == Some(true) {
            crate::dock::wl_drag_motion(x as f32, y as f32);
        }
    }
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, dd: &WlDataDevice) {
        let Some(panel) = self.dnd_on.take() else { return };
        let Some(offer) = wayland_client::Proxy::data::<DataDeviceData>(dd).and_then(|d| d.drag_offer()) else { return };
        let pos = (offer.x as f32, offer.y as f32);
        match offer.receive(URI_LIST.into()) {
            Ok(pipe) => {
                self.dnd_drop = Some((offer, panel, pos));
                read_pipe_async(pipe, DndDone::Drop);
            }
            Err(e) => {
                log::warn!("drop: receive failed: {e}");
                offer.destroy();
                crate::dock::wl_drop(panel, Vec::new(), None);
            }
        }
    }
}

impl DataOfferHandler for State {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, offer: &mut DragOffer, _: DndAction) {
        if self.dnd_on.is_some() {
            offer.set_actions(DndAction::Copy, DndAction::Copy);
        }
    }
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

/// 停靠条：与旧版 native_dock/linux.rs 的 cairo 画法相同（圆角条，未悬停时朝屏幕内缩到 87%，
/// 灰色 rgba(128,128,140,.42×.75) / 悬停或打开 .42 / 未读 accent 脉冲 0.55↔1），自己做抗锯齿。
fn paint_strip(canvas: &mut [u8], w: usize, h: usize, v: &StripLook) {
    let (aw, ah) = (w as f64, h as f64);
    let vertical = v.edge == "left" || v.edge == "right";
    let (mut x, mut y, mut rw, mut rh) = (0.0, 0.0, aw, ah);
    if !v.hovering && !v.open {
        if vertical {
            rw = (aw * 0.87).max(1.0);
            if v.edge == "right" {
                x = aw - rw;
            }
        } else {
            rh = (ah * 0.87).max(1.0);
            if v.edge == "bottom" {
                y = ah - rh;
            }
        }
    }
    let r = (rw.min(rh) / 2.0).min(3.0);
    let (red, green, blue, mut alpha) = if v.unread { (v.accent.0, v.accent.1, v.accent.2, 1.0) } else { (128.0 / 255.0, 128.0 / 255.0, 140.0 / 255.0, 0.42) };
    if v.unread {
        alpha *= 0.775 + 0.225 * (v.phase * std::f64::consts::TAU / 1.8).cos();
    } else if !(v.hovering || v.open) {
        alpha *= 0.75;
    }
    let (cx, cy) = (x + rw / 2.0, y + rh / 2.0);
    let (hx, hy) = (rw / 2.0 - r, rh / 2.0 - r);
    for py in 0..h {
        for px in 0..w {
            // 圆角矩形的有符号距离（像素中心）
            let qx = ((px as f64 + 0.5 - cx).abs() - hx).max(0.0);
            let qy = ((py as f64 + 0.5 - cy).abs() - hy).max(0.0);
            let inside_x = (px as f64 + 0.5 - cx).abs() - hx;
            let inside_y = (py as f64 + 0.5 - cy).abs() - hy;
            let d = (qx * qx + qy * qy).sqrt() + inside_x.max(inside_y).min(0.0) - r;
            let cov = (0.5 - d).clamp(0.0, 1.0);
            // 0.5px 白色描边（35%）：落在边缘带里的像素再叠一点白
            let edge = (1.0 - (d + 0.25).abs() * 2.0).clamp(0.0, 1.0) * 0.35;
            let a = alpha * cov;
            let (mut r8, mut g8, mut b8) = (red * a, green * a, blue * a);
            let wa = edge * alpha * cov;
            r8 = r8 * (1.0 - wa) + wa;
            g8 = g8 * (1.0 - wa) + wa;
            b8 = b8 * (1.0 - wa) + wa;
            let a8 = a + wa * (1.0 - a);
            let i = (py * w + px) * 4;
            canvas[i] = (b8 * 255.0).round() as u8;
            canvas[i + 1] = (g8 * 255.0).round() as u8;
            canvas[i + 2] = (r8 * 255.0).round() as u8;
            canvas[i + 3] = (a8 * 255.0).round() as u8;
        }
    }
}

// ------------------------------------------------------------------ handlers

impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.sync_outputs();
        crate::dock::wl_outputs_changed();
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.sync_outputs();
        crate::dock::wl_outputs_changed();
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.sync_outputs();
        crate::dock::wl_outputs_changed();
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, l: &LayerSurface) {
        // 合成器关掉了 surface（输出拔掉等）：丢掉，dock 下次打开时重建
        if self.panel.as_ref().is_some_and(|p| &p.layer == l) {
            self.panel = None;
        }
        if self.strip.as_ref().is_some_and(|s| &s.layer == l) {
            self.strip = None;
            crate::dock::wl_outputs_changed();
        }
        if self.toast.as_ref().is_some_and(|t| &t.layer == l) {
            self.toast = None;
        }
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, l: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        if let Some(ts) = self.toast.as_mut().filter(|t| &t.layer == l) {
            if c.new_size.0 > 0 && c.new_size.1 > 0 {
                ts.width = c.new_size.0;
                ts.height = c.new_size.1;
            }
            ts.configured = true;
            let (tw, th) = (ts.width, ts.height);
            if let Some(w) = TOAST_WIN.with(|t| t.borrow().upgrade()) {
                let sc = self.scale.max(1) as u32;
                w.size.set(PhysicalSize::new(tw * sc, th * sc));
                w.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: sc as f32 });
                w.window.dispatch_event(WindowEvent::Resized { size: slint::LogicalSize::new(tw as f32, th as f32) });
                w.needs_redraw.set(true);
            }
            self.draw_toast();
            return;
        }
        if let Some(st) = self.strip.as_mut().filter(|s| &s.layer == l) {
            if c.new_size.0 > 0 && c.new_size.1 > 0 {
                st.width = c.new_size.0;
                st.height = c.new_size.1;
            }
            st.configured = true;
            st.dirty = true;
            self.draw_strip();
            return;
        }
        let Some(ps) = self.panel.as_mut().filter(|p| &p.layer == l) else { return };
        if c.new_size.0 > 0 && c.new_size.1 > 0 {
            ps.width = c.new_size.0;
            ps.height = c.new_size.1;
        }
        let first = !ps.configured;
        ps.configured = true;
        if first {
            // 弹出动画从第一帧真正上屏开始算
            if let Some(a) = ps.anim.as_mut() {
                a.start = Instant::now();
            }
        }
        let (pw, ph) = (ps.width, ps.height);
        if let Some(w) = self.win.upgrade() {
            let sc = self.scale.max(1) as u32;
            w.size.set(PhysicalSize::new(pw * sc, ph * sc));
            w.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: sc as f32 });
            w.window.dispatch_event(WindowEvent::Resized { size: slint::LogicalSize::new(pw as f32, ph as f32) });
            w.needs_redraw.set(true);
        }
        if first {
            log::info!("panel surface configured {pw}x{ph} ({} ms after start)", crate::t0().elapsed().as_millis());
        }
        self.draw_panel();
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.seat.get_or_insert_with(|| seat.clone());
        if self.data_device.is_none() {
            if let Some(m) = self.ddm.as_ref() {
                self.data_device = Some(m.get_data_device(qh, &seat));
            }
        }
        if self.text_input.is_none() {
            if let Some(m) = self.ti_manager.as_ref() {
                self.text_input = Some(m.get_text_input(&seat, qh, TiData));
            }
        }
    }
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        self.seat.get_or_insert_with(|| seat.clone());
        if self.data_device.is_none() {
            if let Some(m) = self.ddm.as_ref() {
                self.data_device = Some(m.get_data_device(qh, &seat));
            }
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).map_err(|e| log::warn!("keyboard: {e}")).ok();
        }
        if self.text_input.is_none() {
            if let Some(m) = self.ti_manager.as_ref() {
                self.text_input = Some(m.get_text_input(&seat, qh, TiData));
            }
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
        if cap == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for State {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for e in events {
            if self.toast.as_ref().is_some_and(|t| t.layer.wl_surface() == &e.surface) {
                let ev = match e.kind {
                    PointerEventKind::Enter { .. } => Some(ToastPointer::Enter),
                    PointerEventKind::Leave { .. } => Some(ToastPointer::Leave),
                    PointerEventKind::Release { button: 0x110, .. } => Some(ToastPointer::LeftRelease),
                    _ => None,
                };
                if let Some(ev) = ev {
                    // 在事件分发里不动 surface：下一轮再交给 toast
                    slint::Timer::single_shot(Duration::ZERO, move || crate::toast_wl::pointer(ev));
                }
                continue;
            }
            if self.strip.as_ref().is_some_and(|s| s.layer.wl_surface() == &e.surface) {
                match e.kind {
                    PointerEventKind::Enter { .. } => crate::dock::wl_strip_pointer(true),
                    PointerEventKind::Leave { .. } => crate::dock::wl_strip_pointer(false),
                    PointerEventKind::Press { .. } => crate::dock::wl_strip_press(),
                    _ => {}
                }
                continue;
            }
            if let Some(id) = self.top_of_surface(&e.surface) {
                self.top_pointer(id, e);
                continue;
            }
            let on_panel = self.panel.as_ref().is_some_and(|p| p.layer.wl_surface() == &e.surface);
            if !on_panel {
                continue;
            }
            let Some(w) = self.win.upgrade() else { continue };
            let position = LogicalPosition::new(e.position.0 as f32, e.position.1 as f32);
            let button = |b: u32| match b {
                0x110 => PointerEventButton::Left,
                0x111 => PointerEventButton::Right,
                0x112 => PointerEventButton::Middle,
                _ => PointerEventButton::Other,
            };
            match e.kind {
                PointerEventKind::Enter { .. } => {
                    crate::dock::wl_panel_pointer(true);
                    crate::note_pointer(false, position.x, position.y, true);
                    w.window.dispatch_event(WindowEvent::PointerMoved { position });
                }
                PointerEventKind::Motion { .. } => {
                    crate::note_pointer(false, position.x, position.y, true);
                    w.window.dispatch_event(WindowEvent::PointerMoved { position })
                }
                PointerEventKind::Leave { .. } => {
                    crate::dock::wl_panel_pointer(false);
                    crate::note_pointer(false, -1000.0, -1000.0, false);
                    w.window.dispatch_event(WindowEvent::PointerExited);
                }
                PointerEventKind::Press { button: b, .. } => {
                    crate::dock::wl_panel_press(true);
                    // 点在弹层外：先关弹层（旧版 window click），这次点击照常分发
                    crate::pointer_down(position.x, position.y);
                    w.window.dispatch_event(WindowEvent::PointerPressed { position, button: button(b) })
                }
                PointerEventKind::Release { button: b, .. } => {
                    crate::dock::wl_panel_press(false);
                    w.window.dispatch_event(WindowEvent::PointerReleased { position, button: button(b) })
                }
                PointerEventKind::Axis { horizontal, vertical, .. } => {
                    // 面板里的滚动区（消息列表）约 424 高
                    wheel(&w, position, horizontal.absolute, horizontal.discrete, vertical.absolute, vertical.discrete, PANEL_VIEW_H);
                }
            }
        }
    }
}

impl State {
    /// 普通窗口上的指针事件（不经过 dock，没有弹层外点击的处理）
    fn top_pointer(&mut self, id: usize, e: &PointerEvent) {
        let Some(w) = self.top_window(id) else { return };
        let position = LogicalPosition::new(e.position.0 as f32, e.position.1 as f32);
        let button = |b: u32| match b {
            0x110 => PointerEventButton::Left,
            0x111 => PointerEventButton::Right,
            0x112 => PointerEventButton::Middle,
            _ => PointerEventButton::Other,
        };
        match e.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                crate::note_pointer(true, position.x, position.y, true);
                w.window.dispatch_event(WindowEvent::PointerMoved { position });
            }
            PointerEventKind::Leave { .. } => {
                crate::note_pointer(true, -1000.0, -1000.0, false);
                w.window.dispatch_event(WindowEvent::PointerExited);
            }
            PointerEventKind::Press { button: b, serial, .. } => {
                self.top_press = Some((id, serial));
                w.window.dispatch_event(WindowEvent::PointerPressed { position, button: button(b) })
            }
            PointerEventKind::Release { button: b, .. } => w.window.dispatch_event(WindowEvent::PointerReleased { position, button: button(b) }),
            PointerEventKind::Axis { horizontal, vertical, .. } => {
                // 设置页：整页滚动，可视高 = 窗口高 − 自绘标题栏
                let sc = w.window.scale_factor().max(1.0);
                let view = (w.size.get().height as f32 / sc - 37.0).max(100.0);
                wheel(&w, position, horizontal.absolute, horizontal.discrete, vertical.absolute, vertical.discrete, view);
            }
        }
    }

    fn top_act(&mut self, a: TopAct) {
        let Some((id, serial)) = self.top_press else { return };
        let Some(ts) = self.tops.iter().find(|t| t.id == id).and_then(|t| t.surf.as_ref()) else { return };
        match a {
            TopAct::Move => {
                if let Some(seat) = self.seat.as_ref() {
                    ts.xdg.move_(seat, serial);
                }
            }
            TopAct::Menu(x, y) => {
                if let Some(seat) = self.seat.as_ref() {
                    ts.xdg.show_window_menu(seat, serial, (x, y));
                }
            }
            TopAct::Minimize => ts.xdg.set_minimized(),
            TopAct::Maximize => {
                if ts.maximized {
                    ts.xdg.unset_maximized();
                } else {
                    ts.xdg.set_maximized();
                }
            }
        }
    }

    /// 键盘事件的目标窗口：面板或有焦点的普通窗口
    fn kb_window(&self) -> Option<Rc<WlWindow>> {
        if self.kb_on_panel {
            self.win.upgrade()
        } else {
            self.kb_top.and_then(|id| self.top_window(id))
        }
    }
}

// ------------------------------------------------------------------ keyboard

thread_local! {
    static REPEAT: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
}

fn key_text(ev: &KeyEvent, ctrl: bool) -> Option<SharedString> {
    let k = match ev.keysym {
        Keysym::BackSpace => Some(Key::Backspace),
        Keysym::Return | Keysym::KP_Enter => Some(Key::Return),
        Keysym::Tab => Some(Key::Tab),
        Keysym::ISO_Left_Tab => Some(Key::Backtab),
        Keysym::Escape => Some(Key::Escape),
        Keysym::Delete | Keysym::KP_Delete => Some(Key::Delete),
        Keysym::Left | Keysym::KP_Left => Some(Key::LeftArrow),
        Keysym::Right | Keysym::KP_Right => Some(Key::RightArrow),
        Keysym::Up | Keysym::KP_Up => Some(Key::UpArrow),
        Keysym::Down | Keysym::KP_Down => Some(Key::DownArrow),
        Keysym::Home | Keysym::KP_Home => Some(Key::Home),
        Keysym::End | Keysym::KP_End => Some(Key::End),
        Keysym::Page_Up | Keysym::KP_Page_Up => Some(Key::PageUp),
        Keysym::Page_Down | Keysym::KP_Page_Down => Some(Key::PageDown),
        Keysym::Insert => Some(Key::Insert),
        Keysym::Shift_L | Keysym::Shift_R => Some(Key::Shift),
        Keysym::Control_L | Keysym::Control_R => Some(Key::Control),
        Keysym::Alt_L | Keysym::Alt_R => Some(Key::Alt),
        Keysym::Super_L | Keysym::Super_R => Some(Key::Meta),
        _ => None,
    };
    if let Some(k) = k {
        return Some(k.into());
    }
    // Ctrl+字母：utf8 是控制字符（^C），Slint 要的是字母本身 + control 修饰
    if ctrl {
        if let Some(c) = ev.keysym.key_char() {
            if !c.is_control() {
                return Some(c.to_lowercase().collect::<String>().into());
            }
        }
    }
    let t = ev.utf8.as_deref()?;
    if t.is_empty() || t.chars().all(|c| c.is_control()) {
        return None;
    }
    Some(t.into())
}

impl State {
    fn ctrl_down(&self) -> bool {
        CTRL.with(|c| c.get())
    }
}

thread_local! {
    static CTRL: Cell<bool> = const { Cell::new(false) };
}

impl KeyboardHandler for State {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, surface: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {
        log::debug!("wl keyboard enter");
        if let Some(id) = self.top_of_surface(surface) {
            self.kb_top = Some(id);
            if let Some(w) = self.top_window(id) {
                w.window.dispatch_event(WindowEvent::WindowActiveChanged(true));
            }
            return;
        }
        let Some(ps) = self.panel.as_mut().filter(|p| p.layer.wl_surface() == surface) else { return };
        self.kb_on_panel = true;
        if ps.focus_req {
            // 拿到了：回到 OnDemand（Exclusive 会一直霸占键盘）
            ps.focus_req = false;
            ps.layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
            ps.layer.commit();
        }
        if let Some(w) = self.win.upgrade() {
            w.window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        }
        crate::dock::wl_panel_keyboard(true);
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, surface: &wl_surface::WlSurface, _: u32) {
        log::debug!("wl keyboard leave");
        if let Some(id) = self.kb_top.filter(|id| self.top_of_surface(surface) == Some(*id) || !self.kb_on_panel) {
            self.kb_top = None;
            REPEAT.with(|r| *r.borrow_mut() = None);
            if let Some(w) = self.top_window(id) {
                w.window.dispatch_event(WindowEvent::WindowActiveChanged(false));
            }
            return;
        }
        if !self.panel.as_ref().is_some_and(|p| p.layer.wl_surface() == surface) && !self.kb_on_panel {
            return;
        }
        self.kb_on_panel = false;
        REPEAT.with(|r| *r.borrow_mut() = None);
        if let Some(w) = self.win.upgrade() {
            w.window.dispatch_event(WindowEvent::WindowActiveChanged(false));
        }
        crate::dock::wl_panel_keyboard(false);
    }
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, ev: KeyEvent) {
        let Some(w) = self.kb_window() else { return };
        if self.kb_on_panel && ev.keysym == Keysym::Escape {
            // 窗口级 Esc（旧版 Panel.svelte onKey + 表情面板的捕获）
            crate::escape_key();
            return;
        }
        let Some(text) = key_text(&ev, self.ctrl_down()) else { return };
        w.window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        if self.kb_on_panel {
            crate::dock::note_typed();
        }
        // 修饰键不重复；其余按 repeat_info 重复（niri 的 wl_keyboard 不发 repeated 事件）
        let modifier = matches!(ev.keysym, Keysym::Shift_L | Keysym::Shift_R | Keysym::Control_L | Keysym::Control_R | Keysym::Alt_L | Keysym::Alt_R | Keysym::Super_L | Keysym::Super_R);
        REPEAT_RUN.with(|r| *r.borrow_mut() = None);
        if let (false, Some((rate, delay))) = (modifier, self.repeat) {
            let ww = Rc::downgrade(&w);
            let interval = Duration::from_millis(1000 / rate.max(1) as u64);
            let t = slint::Timer::default();
            t.start(slint::TimerMode::SingleShot, Duration::from_millis(delay as u64), move || {
                let (ww, text) = (ww.clone(), text.clone());
                let rep = slint::Timer::default();
                rep.start(slint::TimerMode::Repeated, interval, move || {
                    if let Some(w) = ww.upgrade() {
                        w.window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
                    }
                });
                REPEAT_RUN.with(|r| *r.borrow_mut() = Some(rep));
            });
            REPEAT.with(|r| *r.borrow_mut() = Some(t));
        } else {
            REPEAT.with(|r| *r.borrow_mut() = None);
        }
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, ev: KeyEvent) {
        if let (Some(w), Some(text)) = (self.kb_window(), key_text(&ev, self.ctrl_down())) {
            w.window.dispatch_event(WindowEvent::KeyPressed { text });
        }
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, ev: KeyEvent) {
        REPEAT.with(|r| *r.borrow_mut() = None);
        REPEAT_RUN.with(|r| *r.borrow_mut() = None);
        if let (Some(w), Some(text)) = (self.kb_window(), key_text(&ev, self.ctrl_down())) {
            w.window.dispatch_event(WindowEvent::KeyReleased { text });
        }
    }
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, m: Modifiers, _: RawModifiers, _: u32) {
        CTRL.with(|c| c.set(m.ctrl));
    }
    fn update_repeat_info(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, info: RepeatInfo) {
        self.repeat = match info {
            RepeatInfo::Repeat { rate, delay } => Some((rate.get(), delay)),
            RepeatInfo::Disable => None,
        };
    }
}

thread_local! {
    static REPEAT_RUN: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
}

// ------------------------------------------------------------------ text-input-v3

/// text-input-v3 对象的 user data（本地类型，才能给它实现 SCTK 的 Dispatch2）。
pub struct TiData;

impl Dispatch2<ZwpTextInputManagerV3, State> for TiData {
    fn event(&self, _: &mut State, _: &ZwpTextInputManagerV3, _: <ZwpTextInputManagerV3 as wayland_client::Proxy>::Event, _: &Connection, _: &QueueHandle<State>) {}
}

impl Dispatch2<ZwpTextInputV3, State> for TiData {
    fn event(&self, s: &mut State, _: &ZwpTextInputV3, ev: zwp_text_input_v3::Event, _: &Connection, _: &QueueHandle<State>) {
        use zwp_text_input_v3::Event;
        match ev {
            Event::Enter { surface } => {
                s.ti_on_panel = s.panel.as_ref().is_some_and(|p| p.layer.wl_surface() == &surface);
                s.ti_top = s.top_of_surface(&surface);
                if let Some(w) = if s.ti_on_panel { s.win.upgrade() } else { s.ti_top.and_then(|id| s.top_window(id)) } {
                    w.ime_dirty.set(true);
                }
            }
            Event::Leave { .. } => {
                let prev = if s.ti_on_panel { s.win.upgrade() } else { s.ti_top.and_then(|id| s.top_window(id)) };
                s.ti_on_panel = false;
                s.ti_top = None;
                if s.ti_enabled {
                    if let Some(ti) = s.text_input.as_ref() {
                        ti.disable();
                        ti.commit();
                    }
                    s.ti_enabled = false;
                }
                if let Some(w) = prev {
                    w.ime_preedit(String::new(), None);
                }
            }
            Event::PreeditString { text, cursor_begin, cursor_end } => {
                s.ti_pending.preedit = Some((text.unwrap_or_default(), cursor_begin, cursor_end));
            }
            Event::CommitString { text } => {
                s.ti_pending.commit = Some(text.unwrap_or_default());
            }
            Event::DeleteSurroundingText { before_length, after_length } => {
                s.ti_pending.delete = Some((before_length, after_length));
            }
            Event::Done { .. } => {
                // 协议规定的应用顺序：删除 surrounding → 上屏 commit → 设置新的预编辑
                let p = std::mem::take(&mut s.ti_pending);
                let Some(w) = (if s.ti_on_panel { s.win.upgrade() } else { s.ti_top.and_then(|id| s.top_window(id)) }) else { return };
                if let Some((before, after)) = p.delete {
                    if before > 0 || after > 0 {
                        log::debug!("text-input: delete_surrounding {before}/{after} (not applied)");
                    }
                }
                if let Some(t) = p.commit {
                    if !t.is_empty() {
                        w.ime_commit(t);
                        if s.ti_on_panel {
                            crate::dock::note_typed();
                        }
                    }
                }
                match p.preedit {
                    Some((t, b, e)) => {
                        let sel = (b >= 0 && e >= 0).then(|| b.min(e)..b.max(e));
                        w.ime_preedit(t, sel);
                    }
                    None => w.ime_preedit(String::new(), None),
                }
                w.needs_redraw.set(true);
            }
            _ => {}
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(State);

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(State);
