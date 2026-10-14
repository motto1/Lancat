//! Lottie 渲染：ThorVG（thorvg-sys 源码编译，CPU 软件光栅）→ 预乘 RGBA → slint::Image。
//! 旧版用 dotlottie-web（同样是 ThorVG，WASM）。这里直接调 ThorVG C API，只用到十几个函数，
//! 自己声明 extern，避免依赖 bindgen 生成的枚举命名。

use std::ffi::{c_char, c_void};
use std::sync::Once;

#[allow(non_camel_case_types)]
type Tvg_Result = i32;
#[allow(non_camel_case_types)]
type Tvg_Canvas = *mut c_void;
#[allow(non_camel_case_types)]
type Tvg_Paint = *mut c_void;
#[allow(non_camel_case_types)]
type Tvg_Animation = *mut c_void;

const TVG_COLORSPACE_ABGR8888: i32 = 0; // 预乘，内存字节序 R,G,B,A（小端）
const TVG_ENGINE_OPTION_DEFAULT: i32 = 1;

extern "C" {
    fn tvg_engine_init(threads: u32) -> Tvg_Result;
    fn tvg_swcanvas_create(op: i32) -> Tvg_Canvas;
    fn tvg_swcanvas_set_target(canvas: Tvg_Canvas, buffer: *mut u32, stride: u32, w: u32, h: u32, cs: i32) -> Tvg_Result;
    fn tvg_canvas_destroy(canvas: Tvg_Canvas) -> Tvg_Result;
    fn tvg_canvas_add(canvas: Tvg_Canvas, paint: Tvg_Paint) -> Tvg_Result;
    fn tvg_canvas_update(canvas: Tvg_Canvas) -> Tvg_Result;
    fn tvg_canvas_draw(canvas: Tvg_Canvas, clear: bool) -> Tvg_Result;
    fn tvg_canvas_sync(canvas: Tvg_Canvas) -> Tvg_Result;
    fn tvg_picture_load_data(picture: Tvg_Paint, data: *const c_char, size: u32, mimetype: *const c_char, rpath: *const c_char, copy: bool) -> Tvg_Result;
    fn tvg_picture_set_size(picture: Tvg_Paint, w: f32, h: f32) -> Tvg_Result;
    fn tvg_picture_get_size(picture: Tvg_Paint, w: *mut f32, h: *mut f32) -> Tvg_Result;
    fn tvg_paint_translate(paint: Tvg_Paint, x: f32, y: f32) -> Tvg_Result;
    fn tvg_animation_new() -> Tvg_Animation;
    fn tvg_animation_get_picture(a: Tvg_Animation) -> Tvg_Paint;
    fn tvg_animation_set_frame(a: Tvg_Animation, no: f32) -> Tvg_Result;
    fn tvg_animation_get_total_frame(a: Tvg_Animation, cnt: *mut f32) -> Tvg_Result;
    fn tvg_animation_get_duration(a: Tvg_Animation, d: *mut f32) -> Tvg_Result;
    fn tvg_animation_del(a: Tvg_Animation) -> Tvg_Result;
}

// 链接 thorvg-sys 编出的静态库
use thorvg_sys as _;

/// ThorVG 的软件渲染器有进程级共享状态（内存池按调度线程号取槽，engine_init(0) 时所有调用方都落在同一槽），
/// 不能从多个线程同时用：海报线程、悬停线程、UI 线程（聊天贴纸）都要先拿这把锁。
static TVG: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn tvg_lock() -> std::sync::MutexGuard<'static, ()> {
    TVG.lock().unwrap_or_else(|e| e.into_inner())
}

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        tvg_engine_init(0);
    });
}

/// 一个已加载的 Lottie 动画 + 它自己的画布（px×px 正方形，内容按比例居中）。
pub struct Player {
    anim: Tvg_Animation,
    canvas: Tvg_Canvas,
    buf: Vec<u32>,
    px: u32,
    pub total: f32,
    pub duration: f32,
}

impl Player {
    pub fn new(json: &str, px: u32) -> Option<Player> {
        init();
        let px = px.max(8);
        let _g = tvg_lock();
        unsafe {
            let anim = tvg_animation_new();
            if anim.is_null() {
                return None;
            }
            let pic = tvg_animation_get_picture(anim);
            let mime = c"lottie+json".as_ptr();
            if tvg_picture_load_data(pic, json.as_ptr() as *const c_char, json.len() as u32, mime, std::ptr::null(), true) != 0 {
                tvg_animation_del(anim);
                return None;
            }
            let (mut w, mut h) = (0f32, 0f32);
            tvg_picture_get_size(pic, &mut w, &mut h);
            if w <= 0.0 || h <= 0.0 {
                tvg_animation_del(anim);
                return None;
            }
            let s = px as f32 / w.max(h);
            let (dw, dh) = (w * s, h * s);
            tvg_picture_set_size(pic, dw, dh);
            tvg_paint_translate(pic, (px as f32 - dw) / 2.0, (px as f32 - dh) / 2.0);
            let canvas = tvg_swcanvas_create(TVG_ENGINE_OPTION_DEFAULT);
            if canvas.is_null() {
                tvg_animation_del(anim);
                return None;
            }
            let mut buf = vec![0u32; (px * px) as usize];
            tvg_swcanvas_set_target(canvas, buf.as_mut_ptr(), px, px, px, TVG_COLORSPACE_ABGR8888);
            // 画布接管 picture（销毁画布时一起释放），动画对象本身仍要 del
            tvg_canvas_add(canvas, pic);
            let (mut total, mut duration) = (0f32, 0f32);
            tvg_animation_get_total_frame(anim, &mut total);
            tvg_animation_get_duration(anim, &mut duration);
            Some(Player { anim, canvas, buf, px, total, duration })
        }
    }

    /// 渲染第 `frame` 帧（0..total）→ 预乘 RGBA 字节（px×px×4）
    pub fn render_raw(&mut self, frame: f32) -> Vec<u8> {
        self.draw(frame);
        let mut out = Vec::with_capacity(self.buf.len() * 4);
        for p in &self.buf {
            out.extend_from_slice(&p.to_le_bytes());
        }
        out
    }

    fn draw(&mut self, frame: f32) {
        let _g = tvg_lock();
        unsafe {
            tvg_animation_set_frame(self.anim, frame.clamp(0.0, (self.total - 1.0).max(0.0)));
            self.buf.fill(0);
            tvg_canvas_update(self.canvas);
            tvg_canvas_draw(self.canvas, true);
            tvg_canvas_sync(self.canvas);
        }
    }

    pub fn px(&self) -> u32 {
        self.px
    }

    /// 帧率（每秒帧数）
    pub fn fps(&self) -> f32 {
        if self.duration > 0.0 {
            self.total / self.duration
        } else {
            30.0
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _g = tvg_lock();
        unsafe {
            tvg_canvas_destroy(self.canvas);
            tvg_animation_del(self.anim);
        }
    }
}
