//! 阶段 3 spike：Linux Wayland 自写 Slint Platform（SCTK 0.21 + wlr-layer-shell + wl_shm 软件渲染）。
//! 目前：一个贴右边缘的 layer surface（Overlay 层，不占独占区），SoftwareRenderer 渲染到预乘 BGRA，
//! 拷进 SlotPool 的 ARGB8888 缓冲（小端内存布局就是 B,G,R,A，与 Windows 的 DIB 相同），指针事件 → Slint。
//! 尚未做：键盘（xkbcommon）、text-input-v3 输入法、分数缩放、停靠条 / toast（下一步）。
//! `LANCAT_SLINT_WL=1` 才启用；默认仍走 winit 后端。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::os::fd::{AsFd, AsRawFd};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slint::platform::software_renderer::{PremultipliedRgbaColor, RepaintBufferType, SoftwareRenderer, TargetPixel};
use slint::platform::{PointerEventButton, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalSize};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, EventQueue, QueueHandle,
};

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Bgra {
    b: u8,
    g: u8,
    r: u8,
    a: u8,
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

pub struct WlWindow {
    window: slint::Window,
    renderer: SoftwareRenderer,
    size: Cell<PhysicalSize>,
    needs_redraw: Cell<bool>,
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
}

/// Wayland 侧状态（SCTK 的各个 Handler 实现在它上面）。
struct State {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    pointer: Option<wl_pointer::WlPointer>,
    configured: bool,
    width: u32,
    height: u32,
    win: Weak<WlWindow>,
    buf: Vec<Bgra>,
    closed: bool,
}

pub struct WlPlatform {
    conn: Connection,
    queue: RefCell<Option<EventQueue<State>>>,
    state: RefCell<Option<State>>,
    jobs: Arc<Mutex<VecDeque<Job>>>,
    quit: Arc<std::sync::atomic::AtomicBool>,
    wake_rx: std::os::unix::net::UnixStream,
    wake_tx: Arc<std::os::unix::net::UnixStream>,
    win: RefCell<Weak<WlWindow>>,
}

pub const W: u32 = 360;
pub const H: u32 = 520;

impl WlPlatform {
    pub fn new() -> Result<Self, String> {
        let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
        let (globals, queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let compositor = CompositorState::bind(&globals, &qh).map_err(|e| format!("wl_compositor: {e}"))?;
        let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| format!("layer shell: {e}"))?;
        let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {e}"))?;
        let surface = compositor.create_surface(&qh);
        let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("lancat-panel"), None);
        layer.set_anchor(Anchor::RIGHT);
        layer.set_margin(0, 8, 0, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        layer.set_size(W, H);
        layer.commit();
        let pool = SlotPool::new((W * H * 4) as usize, &shm).map_err(|e| e.to_string())?;
        let state = State {
            registry_state: RegistryState::new(&globals),
            seat_state: SeatState::new(&globals, &qh),
            output_state: OutputState::new(&globals, &qh),
            shm,
            pool,
            layer,
            pointer: None,
            configured: false,
            width: W,
            height: H,
            win: Weak::new(),
            buf: Vec::new(),
            closed: false,
        };
        let (wake_rx, wake_tx) = std::os::unix::net::UnixStream::pair().map_err(|e| e.to_string())?;
        wake_rx.set_nonblocking(true).ok();
        Ok(WlPlatform {
            conn,
            queue: RefCell::new(Some(queue)),
            state: RefCell::new(Some(state)),
            jobs: Default::default(),
            quit: Default::default(),
            wake_rx,
            wake_tx: Arc::new(wake_tx),
            win: RefCell::new(Weak::new()),
        })
    }
}

impl slint::platform::Platform for WlPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let w = Rc::<WlWindow>::new_cyclic(|weak: &Weak<WlWindow>| WlWindow {
            window: slint::Window::new(weak.clone() as Weak<dyn WindowAdapter>),
            renderer: SoftwareRenderer::new_with_repaint_buffer_type(RepaintBufferType::ReusedBuffer),
            size: Cell::new(PhysicalSize::new(W, H)),
            needs_redraw: Cell::new(true),
        });
        w.window.dispatch_event(WindowEvent::Resized { size: slint::LogicalSize::new(W as f32, H as f32) });
        *self.win.borrow_mut() = Rc::downgrade(&w);
        if let Some(s) = self.state.borrow_mut().as_mut() {
            s.win = Rc::downgrade(&w);
        }
        Ok(w)
    }

    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        let mut queue = self.queue.borrow_mut().take().ok_or("event loop already running")?;
        let mut state = self.state.borrow_mut().take().ok_or("no state")?;
        loop {
            slint::platform::update_timers_and_animations();
            if let Some(w) = state.win.upgrade() {
                if state.configured && (w.needs_redraw.get() || w.window.has_active_animations()) {
                    state.draw();
                }
            }
            queue.dispatch_pending(&mut state).map_err(|e| e.to_string())?;
            queue.flush().map_err(|e| e.to_string())?;
            let timeout = slint::platform::duration_until_next_timer_update().map(|d| d.min(Duration::from_secs(1)));
            let anim = state.win.upgrade().is_some_and(|w| w.window.has_active_animations());
            let ms: i32 = if anim { 8 } else { timeout.map(|d| d.as_millis() as i32).unwrap_or(-1) };
            if let Some(guard) = queue.prepare_read() {
                let fd = guard.connection_fd().as_raw_fd();
                let mut fds = [libc_pollfd(fd), libc_pollfd(self.wake_rx.as_fd().as_raw_fd())];
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
            if state.closed || self.quit.swap(false, std::sync::atomic::Ordering::SeqCst) {
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
fn libc_pollfd(fd: i32) -> PollFd {
    PollFd { fd, events: 1 /* POLLIN */, revents: 0 }
}
extern "C" {
    fn poll(fds: *mut PollFd, n: u64, timeout: i32) -> i32;
}

impl State {
    fn draw(&mut self) {
        let Some(w) = self.win.upgrade() else { return };
        let (width, height) = (self.width, self.height);
        let n = (width * height) as usize;
        if self.buf.len() != n {
            self.buf = vec![Bgra::default(); n];
            w.renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
            w.renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
        }
        w.needs_redraw.set(false);
        w.renderer.render(&mut self.buf, width as usize);
        let Ok((buffer, canvas)) = self.pool.create_buffer(width as i32, height as i32, width as i32 * 4, wl_shm::Format::Argb8888) else { return };
        // SAFETY: Bgra is repr(C) 4 bytes, canvas is width*height*4 bytes
        let src = unsafe { std::slice::from_raw_parts(self.buf.as_ptr() as *const u8, n * 4) };
        canvas[..n * 4].copy_from_slice(src);
        let s = self.layer.wl_surface();
        s.damage_buffer(0, 0, width as i32, height as i32);
        if buffer.attach_to(s).is_ok() {
            self.layer.commit();
        }
    }
}

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
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.closed = true;
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        if c.new_size.0 > 0 && c.new_size.1 > 0 {
            self.width = c.new_size.0;
            self.height = c.new_size.1;
        }
        if let Some(w) = self.win.upgrade() {
            w.size.set(PhysicalSize::new(self.width, self.height));
            w.window.dispatch_event(WindowEvent::Resized { size: slint::LogicalSize::new(self.width as f32, self.height as f32) });
            w.needs_redraw.set(true);
        }
        if !self.configured {
            log::info!("layer surface configured {}x{} ({} ms after start)", self.width, self.height, crate::t0().elapsed().as_millis());
        }
        self.configured = true;
        self.draw();
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for State {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        let Some(w) = self.win.upgrade() else { return };
        for e in events {
            if &e.surface != self.layer.wl_surface() {
                continue;
            }
            let position = LogicalPosition::new(e.position.0 as f32, e.position.1 as f32);
            let button = |b: u32| match b {
                0x110 => PointerEventButton::Left,
                0x111 => PointerEventButton::Right,
                0x112 => PointerEventButton::Middle,
                _ => PointerEventButton::Other,
            };
            match e.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => w.window.dispatch_event(WindowEvent::PointerMoved { position }),
                PointerEventKind::Leave { .. } => w.window.dispatch_event(WindowEvent::PointerExited),
                PointerEventKind::Press { button: b, .. } => w.window.dispatch_event(WindowEvent::PointerPressed { position, button: button(b) }),
                PointerEventKind::Release { button: b, .. } => w.window.dispatch_event(WindowEvent::PointerReleased { position, button: button(b) }),
                PointerEventKind::Axis { horizontal, vertical, .. } => {
                    w.window.dispatch_event(WindowEvent::PointerScrolled { position, delta_x: -horizontal.absolute as f32, delta_y: -vertical.absolute as f32 })
                }
            }
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
