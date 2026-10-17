//! Linux/Wayland：GPU 呈现 + 合成（EGL + GLES 3.0，wl_egl_window）。
//!
//! 与 Windows 的 gpu.rs（D3D11 + DirectComposition）一一对应：同一套合成 API（compose.rs 的 render_gpu
//! 两个平台共用，`crate::gpu` 在 Linux 上就是这个模块）、同一套着色器（HLSL 入口 ps_over / ps_disp /
//! ps_down / ps_box / ps_mix / ps_morph 逐行移植成 GLSL ES 3.00，常量布局相同：`U[i]` = cbuffer 的第 i 个 float4）。
//!
//! 约定：
//! - 纹理第 0 行 = 图像最上面一行；离屏渲染时 gl_FragCoord 的 (x, y) 就是「自上而下」的像素中心，
//!   与 HLSL 的 SV_Position 一致。只有画到窗口（EGL 默认帧缓冲，原点在左下）时翻转：U[8].x = 窗口高。
//! - CPU 缓冲是预乘 BGRA：上传成 RGBA8，再用纹理 swizzle 把 R/B 换回来；着色器画出来的纹理是正常的 RGBA。
//!   所以纹理之间的复制一律走一次绘制（ps_copy），不做原始字节复制。
//! - GLES 3.0 没有 border 采样：sample_b() 按双线性权重算出 border = 0 的结果（与 D3D 的 BORDER 一致）。
//! - EGL / libwayland-egl 都是运行时加载（dlopen），没有就自动退回 wl_shm 的 CPU 路径。
#![allow(clippy::too_many_arguments)]
#![allow(dead_code)]

use crate::compose::Bgra;
use glow::HasContext;
use khronos_egl as egl;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::rc::Rc;
use std::time::Instant;

const VS: &str = r#"#version 300 es
void main() {
  vec2 uv = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  gl_Position = vec4(uv * 2.0 - 1.0, 0.0, 1.0);
}
"#;

/// 合成着色器的公共头：U[0..6] = HLSL cbuffer C（rect p0 p1 p2 p3 p4），U[8].x = 翻转高度（画到窗口时）
const HEAD: &str = r#"#version 300 es
precision highp float;
precision highp int;
uniform vec4 U[9];
uniform highp sampler2D T0;
uniform highp sampler2D T1;
out vec4 o;
#define rect U[0]
#define p0 U[1]
#define p1 U[2]
#define p2 U[3]
#define p3 U[4]
#define p4 U[5]
vec2 PX() { return U[8].x > 0.0 ? vec2(gl_FragCoord.x, U[8].x - gl_FragCoord.y) : gl_FragCoord.xy; }
// 双线性 + border 0（D3D11_TEXTURE_ADDRESS_BORDER）：夹边采样 × 落在图内的权重
vec4 sample_b(highp sampler2D t, vec2 uv) {
  vec2 sz = vec2(textureSize(t, 0));
  vec2 u = uv * sz;
  vec2 f = clamp(min(u + 0.5, sz + 0.5 - u), 0.0, 1.0);
  return texture(t, uv) * f.x * f.y;
}
float rrect_cov(vec2 p, vec4 r, float rad) {
  vec2 sz = r.zw - r.xy;
  rad = max(min(rad, min(sz.x, sz.y) * 0.5), 0.0);
  vec2 q = abs(p - (r.xy + r.zw) * 0.5) - (sz * 0.5 - rad);
  float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - rad;
  return clamp(0.5 - d, 0.0, 1.0);
}
vec3 liquid_n(vec2 p, vec4 r, float rad, float bevel) {
  vec2 hh = (r.zw - r.xy) * 0.5;
  rad = min(rad, min(hh.x, hh.y));
  vec2 f = p - (r.xy + hh);
  vec2 q = abs(f) - (hh - rad);
  float d = -(length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - rad);
  if (d < 0.0 || d >= bevel) return vec3(0.0);
  vec2 n;
  if (q.x > 0.0 && q.y > 0.0) n = q / max(length(q), 1e-6);
  else if (q.x > q.y) n = vec2(1.0, 0.0);
  else n = vec2(0.0, 1.0);
  n *= vec2(f.x >= 0.0 ? 1.0 : -1.0, f.y >= 0.0 ? 1.0 : -1.0);
  float k = 1.0 - d / bevel;
  return vec3(n, k * k);
}
"#;

/// ps_over：以 (ox,oy) 缩放 s 再平移 (tx,ty)、乘 opacity（预乘 over 由混合状态做）。也用来画到窗口（姿态变换）
const PS_OVER: &str = r#"
void main() {
  vec2 src = p2.xy + (PX() - p2.zw - p2.xy) / p1.y;
  o = sample_b(T0, src / p3.xy) * p0.y;
}
"#;

/// 纹理之间的复制（swizzle 过的上传纹理 → 普通 RGBA）
const PS_COPY: &str = r#"
void main() { o = texelFetch(T0, ivec2(PX()), 0); }
"#;

const PS_DISP: &str = r#"
void main() {
  vec2 p = PX();
  if (p.x >= rect.x && p.x <= rect.z && p.y >= rect.y && p.y <= rect.w) {
    vec3 n = liquid_n(p, rect, p0.x, p1.x);
    if (n.z > 0.0) {
      vec2 s = clamp(p - p0.w * n.xy * n.z, rect.xy, rect.zw);
      o = texture(T0, s / p3.xy);
      return;
    }
  }
  o = texelFetch(T0, ivec2(p), 0);
}
"#;

const PS_DOWN: &str = r#"
void main() {
  int f = int(p1.z);
  ivec2 b = ivec2(PX()) * f;
  ivec2 mx = ivec2(p3.xy);
  vec4 acc = vec4(0.0); float n = 0.0;
  for (int y = 0; y < 4; y++) {
    for (int x = 0; x < 4; x++) {
      ivec2 q = b + ivec2(x, y);
      if (x < f && y < f && q.x < mx.x && q.y < mx.y) { acc += texelFetch(T0, q, 0); n += 1.0; }
    }
  }
  o = acc / max(n, 1.0);
}
"#;

const PS_BOX: &str = r#"
void main() {
  int r = int(p4.x);
  ivec2 dir = ivec2(p4.yz);
  ivec2 c = ivec2(PX());
  ivec2 mx = ivec2(p3.zw) - 1;
  vec4 acc = vec4(0.0);
  for (int k = -r; k <= r; k++) acc += texelFetch(T0, clamp(c + dir * k, ivec2(0), mx), 0);
  o = acc / float(2 * r + 1);
}
"#;

const PS_MIX: &str = r#"
void main() {
  vec2 p = PX();
  vec4 d = texelFetch(T0, ivec2(p), 0);
  float c = rrect_cov(p, rect, p0.x) * p0.y;
  if (c <= 0.0) { o = d; return; }
  vec4 n = texture(T1, (p / p1.z) / p3.zw);
  float s = p0.z;
  if (abs(s - 1.0) > 0.001) {
    vec3 m0 = vec3(0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s);
    vec3 m1 = vec3(0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s);
    vec3 m2 = vec3(0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s);
    vec3 rgb = n.rgb;
    n.r = clamp(dot(m0, rgb), 0.0, n.a);
    n.g = clamp(dot(m1, rgb), 0.0, n.a);
    n.b = clamp(dot(m2, rgb), 0.0, n.a);
  }
  o = d + (n - d) * c;
}
"#;

/// ps_morph（= gpu.rs MORPH_HLSL + morph.rs render_impl 的 rest 模式）。U[0..8] = HLSL cbuffer M，
/// U[8] = (翻转高度, rest 模式, 0, 0)。rest 模式（Linux）：色调用静止面板外壳色，内容和形状交叉淡化。
const PS_MORPH: &str = r#"#version 300 es
precision highp float;
precision highp int;
uniform vec4 U[9];
uniform highp sampler2D T0;
uniform highp sampler2D T1;
out vec4 o;
#define rect U[0]
#define m0 U[1]
#define m1 U[2]
#define body U[3]
#define m2 U[4]
#define gmap U[5]
#define cmap U[6]
#define csz U[7]
vec4 sample_b(highp sampler2D t, vec2 uv) {
  vec2 sz = vec2(textureSize(t, 0));
  vec2 u = uv * sz;
  vec2 f = clamp(min(u + 0.5, sz + 0.5 - u), 0.0, 1.0);
  return texture(t, uv) * f.x * f.y;
}
float sdf_n(vec2 p, vec4 r, float rad, out vec2 n) {
  vec2 hh = (r.zw - r.xy) * 0.5;
  rad = max(min(rad, min(hh.x, hh.y)), 0.0);
  vec2 f = p - (r.xy + hh);
  vec2 q = abs(f) - (hh - rad);
  float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - rad;
  if (q.x > 0.0 && q.y > 0.0) n = q / max(length(q), 1e-6);
  else if (q.x > q.y) n = vec2(1.0, 0.0);
  else n = vec2(0.0, 1.0);
  n *= vec2(f.x >= 0.0 ? 1.0 : -1.0, f.y >= 0.0 ? 1.0 : -1.0);
  return d;
}
void main() {
  vec2 p = U[8].x > 0.0 ? vec2(gl_FragCoord.x, U[8].x - gl_FragCoord.y) : gl_FragCoord.xy;
  bool rest = U[8].y > 0.5;
  vec2 nrm; vec2 nn;
  float d = sdf_n(p, rect, m0.x, nrm);
  float ds = sdf_n(p - vec2(0.0, m0.z), rect, m0.x, nn);
  float shadow = m0.y;
  float sh = ds > -shadow ? m2.z * exp(-pow(max(ds, 0.0) / (shadow * 0.42), 2.0)) : m2.z;
  float cov = clamp(0.5 - d, 0.0, 1.0);
  float cop = m1.z;
  vec4 c = vec4(0.0);
  if (cop > 0.0) c = sample_b(T1, (cmap.xy + (p - rect.xy) * cmap.zw) * csz.xy);
  if (cov <= 0.0) {
    if (rest && cop > 0.0) { o = vec4(c.rgb * cop, sh * (1.0 - cop) + c.a * cop) * m1.w; return; }
    o = vec4(0.0, 0.0, 0.0, sh) * m1.w;
    return;
  }
  float depth = -d;
  float bevel = m0.w;
  float ba = body.a;
  vec3 rgb; float a;
  if (m2.w > 0.5) {
    float off = 0.0;
    if (depth > 0.0 && depth < bevel) { float t = 1.0 - depth / bevel; off = (1.0 - sqrt(max(1.0 - t * t, 0.0))) * m1.x; }
    vec2 q = p + gmap.xy;
    vec3 c0 = texture(T0, (q - nrm * off) * gmap.zw).rgb;
    if (off > 0.5) {
      c0.r = texture(T0, (q - nrm * off * 1.08) * gmap.zw).r;
      c0.b = texture(T0, (q - nrm * off * 0.92) * gmap.zw).b;
    }
    float lum = dot(c0, vec3(0.2126, 0.7152, 0.0722));
    c0 = clamp(lum + (c0 - lum) * 1.25, 0.0, 1.0);
    float lift = depth < bevel ? pow(1.0 - depth / bevel, 2.0) * (22.0 / 255.0) : 0.0;
    rgb = min(c0 + lift, 1.0) * (1.0 - ba) + body.rgb * ba;
    a = 1.0;
  } else {
    rgb = body.rgb * ba;
    a = ba;
  }
  float tyk = clamp((p.y - rect.y) / max((rect.w - rect.y) * 0.45, 1.0), 0.0, 1.0);
  float sheen = m2.y * (1.0 - tyk);
  if (sheen > 0.0) { rgb += (1.0 - rgb) * sheen; a += (1.0 - a) * sheen; }
  float rimw = m1.y;
  if (depth < rimw) {
    float kk = clamp(1.0 - depth / rimw, 0.0, 1.0);
    float light = clamp(0.55 - 0.45 * nrm.y, 0.1, 1.0);
    float ra = m2.x * kk * light;
    rgb += (1.0 - rgb) * ra; a += (1.0 - a) * ra;
  }
  if (cop > 0.0 && !rest) {
    float inv = 1.0 - c.a * cop;
    rgb = c.rgb * cop + rgb * inv;
    a = c.a * cop + a * inv;
  }
  rgb *= cov; a *= cov;
  float sa = sh * (1.0 - a);
  if (rest && cop > 0.0) {
    float k0 = 1.0 - cop;
    o = vec4(rgb * k0 + c.rgb * cop, (a + sa) * k0 + c.a * cop) * m1.w;
    return;
  }
  o = vec4(rgb, a + sa) * m1.w;
}
"#;

// ------------------------------------------------------------------ libwayland-egl（dlopen）

struct WlEglLib {
    _lib: libloading::Library,
    create: unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void,
    resize: unsafe extern "C" fn(*mut c_void, i32, i32, i32, i32),
    destroy: unsafe extern "C" fn(*mut c_void),
}

fn wl_egl() -> Option<&'static WlEglLib> {
    static LIB: std::sync::OnceLock<Option<WlEglLib>> = std::sync::OnceLock::new();
    LIB.get_or_init(|| unsafe {
        let lib = libloading::Library::new("libwayland-egl.so.1").or_else(|_| libloading::Library::new("libwayland-egl.so")).map_err(|e| log::warn!("gl: libwayland-egl: {e}")).ok()?;
        let create = *lib.get::<unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void>(b"wl_egl_window_create\0").ok()?;
        let resize = *lib.get::<unsafe extern "C" fn(*mut c_void, i32, i32, i32, i32)>(b"wl_egl_window_resize\0").ok()?;
        let destroy = *lib.get::<unsafe extern "C" fn(*mut c_void)>(b"wl_egl_window_destroy\0").ok()?;
        Some(WlEglLib { _lib: lib, create, resize, destroy })
    })
    .as_ref()
}

// ------------------------------------------------------------------ 设备

struct Tex {
    t: glow::Texture,
    fbo: glow::Framebuffer,
    w: u32,
    h: u32,
}

struct Prog {
    p: glow::Program,
    u: Option<glow::UniformLocation>,
    t0: Option<glow::UniformLocation>,
    t1: Option<glow::UniformLocation>,
}

struct Progs {
    over: Prog,
    copy: Prog,
    disp: Prog,
    down: Prog,
    boxp: Prog,
    mix: Prog,
    morph: Prog,
}

type Egl = egl::DynamicInstance<egl::EGL1_4>;

pub struct Gl {
    egl: Egl,
    dpy: egl::Display,
    cfg: egl::Config,
    ctx: egl::Context,
    gl: glow::Context,
    vao: glow::VertexArray,
    pr: Progs,
    /// 半精度纹理能当渲染目标（EXT_color_buffer_half_float / EXT_color_buffer_float）
    half_rt: bool,
    // 帧尺寸的纹理（与 gpu.rs 相同）
    fw: u32,
    fh: u32,
    cpu: Option<Tex>,
    base: Option<Tex>,
    acc: [Option<Tex>; 2],
    cur: usize,
    disp: Option<Tex>,
    under5: Option<Tex>,
    has_under5: bool,
    has_base: bool,
    small: HashMap<u32, [Tex; 2]>,
    layers: HashMap<i32, (Tex, (u32, u32))>,
    bs: HashMap<i32, (Tex, u32)>,
    frame_gpu: bool,
}

thread_local! {
    static GL: RefCell<Option<Gl>> = const { RefCell::new(None) };
    static PRESENT_MS: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

fn with<R>(f: impl FnOnce(&mut Gl) -> R) -> Option<R> {
    GL.with(|g| g.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

pub fn active() -> bool {
    GL.with(|g| g.borrow().is_some())
}

/// 合成也在 GPU 上（LANCAT_GPU_COMPOSE=0：只用来呈现）
pub fn compose_enabled() -> bool {
    active() && std::env::var("LANCAT_GPU_COMPOSE").as_deref() != Ok("0")
}

pub fn last_present_ms() -> f32 {
    PRESENT_MS.with(|p| p.get())
}

/// 见 render::status
pub fn status() -> &'static str {
    if crate::render::pref_cpu() {
        return "cpu";
    }
    if active() {
        return "gpu";
    }
    if crate::render::failed() {
        return "failed";
    }
    "pending"
}

fn compile(gl: &glow::Context, src_fs: &str, name: &str) -> Option<Prog> {
    unsafe {
        let p = gl.create_program().ok()?;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, VS.to_string()), (glow::FRAGMENT_SHADER, src_fs.to_string())] {
            let s = gl.create_shader(kind).ok()?;
            gl.shader_source(s, &src);
            gl.compile_shader(s);
            if !gl.get_shader_compile_status(s) {
                log::warn!("gl: compile {name} failed: {}", gl.get_shader_info_log(s));
                return None;
            }
            gl.attach_shader(p, s);
            shaders.push(s);
        }
        gl.link_program(p);
        if !gl.get_program_link_status(p) {
            log::warn!("gl: link {name} failed: {}", gl.get_program_info_log(p));
            return None;
        }
        for s in shaders {
            gl.detach_shader(p, s);
            gl.delete_shader(s);
        }
        let u = gl.get_uniform_location(p, "U[0]").or_else(|| gl.get_uniform_location(p, "U"));
        let t0 = gl.get_uniform_location(p, "T0");
        let t1 = gl.get_uniform_location(p, "T1");
        Some(Prog { p, u, t0, t1 })
    }
}

/// 建 EGL 显示 / 上下文、编译着色器。`display`：libwayland-client 的 wl_display*（系统后端）。
/// 失败 = 一直用 CPU（render::mark_failed，设置页显示「GPU 不可用」）。
use std::sync::atomic::{AtomicBool, Ordering};

/// 上下文丢了（EGL_CONTEXT_LOST：GPU 复位、驱动重启、部分驱动待机唤醒后）；事件循环里 take_lost() 接手
static LOST: AtomicBool = AtomicBool::new(false);
/// 测试钩子 LANCAT_GPU_TEST=lose（lose:毫秒，默认 6000）：上下文建好这么久后假装丢一次
static LOSE_AT: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
static LOSE_DONE: AtomicBool = AtomicBool::new(false);

fn lose_test_ms() -> Option<u64> {
    let t = std::env::var("LANCAT_GPU_TEST").ok()?;
    let rest = t.strip_prefix("lose")?;
    Some(rest.strip_prefix(':').and_then(|v| v.parse().ok()).unwrap_or(6000))
}

fn arm_lose_test() {
    if lose_test_ms().is_some() && !LOSE_DONE.load(Ordering::Acquire) {
        *LOSE_AT.lock().unwrap() = Some(Instant::now());
    }
}

/// 上下文丢了 → true（只返回一次）
pub fn take_lost() -> bool {
    LOST.swap(false, Ordering::AcqRel)
}

/// 后台线程建好的上下文（建完在那个线程上 release，交给 UI 线程 make_current 一次）
struct SendGl(Gl);
// SAFETY: EGL 上下文在建它的线程上已经 release（不在任何线程上 current），之后只在 UI 线程用
unsafe impl Send for SendGl {}

static ASYNC: std::sync::Mutex<Option<Option<SendGl>>> = std::sync::Mutex::new(None);
static ASYNC_BUSY: AtomicBool = AtomicBool::new(false);

/// 启动时在后台线程建 EGL 上下文、编译着色器（Mesa 加载驱动 + 编译 ~0.3–0.5 s，不再挡住第一帧和输入）；
/// 好了唤醒事件循环，`poll_async()` 接上。期间 surface 走 wl_shm，接上后整幅重画到 EGL surface。
pub fn init_async(display: *mut c_void) {
    if active() || ASYNC_BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    let d = display as usize;
    std::thread::spawn(move || {
        let t = Instant::now();
        let test = std::env::var("LANCAT_GPU_TEST").unwrap_or_default();
        let r = if test == "fail" {
            log::warn!("gl: LANCAT_GPU_TEST=fail");
            None
        } else {
            init_inner(d as *mut c_void)
        };
        let r = r.map(|g| {
            let _ = g.egl.make_current(g.dpy, None, None, None);
            SendGl(g)
        });
        log::info!("gl: background init {} in {:.1} ms", if r.is_some() { "ok" } else { "failed" }, t.elapsed().as_secs_f32() * 1000.0);
        *ASYNC.lock().unwrap() = Some(r);
        let _ = slint::invoke_from_event_loop(|| {});
    });
}

/// 后台初始化结束了吗：刚接上返回 true（调用方让所有 surface 整幅重画）
pub fn poll_async() -> bool {
    let Some(r) = ASYNC.lock().unwrap().take() else { return false };
    ASYNC_BUSY.store(false, Ordering::Release);
    match r {
        Some(SendGl(g)) => {
            if !crate::render::wanted() || active() {
                let _ = g.egl.destroy_context(g.dpy, g.ctx);
                return false;
            }
            if g.egl.make_current(g.dpy, None, None, Some(g.ctx)).is_err() {
                log::warn!("gl: make_current on the UI thread failed, using wl_shm (CPU) rendering");
                let _ = g.egl.destroy_context(g.dpy, g.ctx);
                crate::render::mark_failed();
                return false;
            }
            let ver = unsafe { g.gl.get_parameter_string(glow::VERSION) };
            let ren = unsafe { g.gl.get_parameter_string(glow::RENDERER) };
            log::info!("gl: {ren:?} {ver:?} attached (half-float targets {})", g.half_rt);
            GL.with(|x| *x.borrow_mut() = Some(g));
            arm_lose_test();
            true
        }
        None => {
            crate::render::mark_failed();
            log::warn!("gl: unavailable, using wl_shm (CPU) rendering");
            false
        }
    }
}

/// 后台初始化还没结束
pub fn initializing() -> bool {
    ASYNC_BUSY.load(Ordering::Acquire)
}

pub fn init(display: *mut c_void) -> bool {
    if active() {
        return true;
    }
    let t = Instant::now();
    let test = std::env::var("LANCAT_GPU_TEST").unwrap_or_default();
    let r = if test == "fail" {
        log::warn!("gl: LANCAT_GPU_TEST=fail");
        None
    } else {
        init_inner(display)
    };
    match r {
        Some(g) => {
            let ver = unsafe { g.gl.get_parameter_string(glow::VERSION) };
            let ren = unsafe { g.gl.get_parameter_string(glow::RENDERER) };
            log::info!("gl: {ren:?} {ver:?} in {:.1} ms (half-float targets {})", t.elapsed().as_secs_f32() * 1000.0, g.half_rt);
            GL.with(|x| *x.borrow_mut() = Some(g));
            arm_lose_test();
            true
        }
        None => {
            crate::render::mark_failed();
            log::warn!("gl: unavailable, using wl_shm (CPU) rendering");
            false
        }
    }
}

fn init_inner(display: *mut c_void) -> Option<Gl> {
    wl_egl()?;
    let egl = unsafe { Egl::load_required() }.map_err(|e| log::warn!("gl: libEGL: {e}")).ok()?;
    let dpy = unsafe { egl.get_display(display as egl::NativeDisplayType) }?;
    egl.initialize(dpy).map_err(|e| log::warn!("gl: eglInitialize: {e}")).ok()?;
    egl.bind_api(egl::OPENGL_ES_API).ok()?;
    let attrs = [
        egl::RED_SIZE, 8, egl::GREEN_SIZE, 8, egl::BLUE_SIZE, 8, egl::ALPHA_SIZE, 8,
        egl::SURFACE_TYPE, egl::WINDOW_BIT,
        egl::RENDERABLE_TYPE, egl::OPENGL_ES3_BIT,
        egl::NONE,
    ];
    let cfg = egl.choose_first_config(dpy, &attrs).ok().flatten().or_else(|| {
        log::warn!("gl: no RGBA8 GLES3 EGL config");
        None
    })?;
    let ctx = egl.create_context(dpy, cfg, None, &[egl::CONTEXT_MAJOR_VERSION, 3, egl::CONTEXT_MINOR_VERSION, 0, egl::NONE]).map_err(|e| log::warn!("gl: eglCreateContext: {e}")).ok()?;
    // 没有窗口 surface 时（建纹理 / 编译）用 surfaceless（EGL_KHR_surfaceless_context，Mesa 都有）
    if egl.make_current(dpy, None, None, Some(ctx)).is_err() {
        log::warn!("gl: surfaceless make_current failed");
        let _ = egl.destroy_context(dpy, ctx);
        return None;
    }
    let gl = unsafe { glow::Context::from_loader_function(|s| egl.get_proc_address(s).map_or(std::ptr::null(), |f| f as *const c_void)) };
    let pr = Progs {
        over: compile(&gl, &format!("{HEAD}{PS_OVER}"), "over")?,
        copy: compile(&gl, &format!("{HEAD}{PS_COPY}"), "copy")?,
        disp: compile(&gl, &format!("{HEAD}{PS_DISP}"), "disp")?,
        down: compile(&gl, &format!("{HEAD}{PS_DOWN}"), "down")?,
        boxp: compile(&gl, &format!("{HEAD}{PS_BOX}"), "box")?,
        mix: compile(&gl, &format!("{HEAD}{PS_MIX}"), "mix")?,
        morph: compile(&gl, PS_MORPH, "morph")?,
    };
    let vao = unsafe { gl.create_vertex_array() }.ok()?;
    let ext = gl.supported_extensions();
    let half_rt = ext.contains("GL_EXT_color_buffer_half_float") || ext.contains("GL_EXT_color_buffer_float");
    Some(Gl {
        egl,
        dpy,
        cfg,
        ctx,
        gl,
        vao,
        pr,
        half_rt,
        fw: 0,
        fh: 0,
        cpu: None,
        base: None,
        acc: [None, None],
        cur: 0,
        disp: None,
        under5: None,
        has_under5: false,
        has_base: false,
        small: HashMap::new(),
        layers: HashMap::new(),
        bs: HashMap::new(),
        frame_gpu: false,
    })
}

/// 切到 CPU：丢掉上下文和所有纹理（调用方先把 surface 换回 wl_shm）
pub fn shutdown() {
    let g = GL.with(|g| g.borrow_mut().take());
    if let Some(g) = g {
        let _ = g.egl.make_current(g.dpy, None, None, None);
        let _ = g.egl.destroy_context(g.dpy, g.ctx);
        log::info!("gl: shut down (CPU rendering)");
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Fmt {
    /// CPU 上传的预乘 BGRA（swizzle R/B）
    Bgra,
    Rgba,
    Half,
}

impl Gl {
    fn tex(&self, w: u32, h: u32, fmt: Fmt) -> Option<Tex> {
        let gl = &self.gl;
        let (w, h) = (w.max(1), h.max(1));
        unsafe {
            let t = gl.create_texture().ok()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(t));
            let fmt = if fmt == Fmt::Half && !self.half_rt { Fmt::Rgba } else { fmt };
            match fmt {
                Fmt::Half => gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::RGBA16F, w as i32, h as i32),
                _ => gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::RGBA8, w as i32, h as i32),
            }
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            if fmt == Fmt::Bgra {
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_SWIZZLE_R, glow::BLUE as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_SWIZZLE_B, glow::RED as i32);
            }
            let fbo = gl.create_framebuffer().ok()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(t), 0);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.disable(glow::SCISSOR_TEST);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            Some(Tex { t, fbo, w, h })
        }
    }

    fn free(&self, t: Tex) {
        unsafe {
            self.gl.delete_framebuffer(t.fbo);
            self.gl.delete_texture(t.t);
        }
    }

    fn upload(&self, t: &Tex, src: &[Bgra], w: usize, y0: usize, y1: usize) {
        let y1 = y1.min(t.h as usize).min(src.len() / w.max(1));
        if y1 <= y0 || w as u32 != t.w {
            return;
        }
        // SAFETY: Bgra 是 4 字节 repr(C)
        let bytes = unsafe { std::slice::from_raw_parts(src[y0 * w..y1 * w].as_ptr() as *const u8, (y1 - y0) * w * 4) };
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(t.t));
            self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            self.gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, y0 as i32, w as i32, (y1 - y0) as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(bytes)));
        }
    }

    /// 画一遍全屏三角形。`target`：None = 当前 EGL 窗口 surface（`flip` = 它的高）
    fn draw(&self, pr: &Prog, target: Option<&Tex>, vw: u32, vh: u32, srcs: [Option<&Tex>; 2], u: &[f32], flip: f32, over: bool, scissor: Option<(i32, i32, i32, i32)>) {
        let gl = &self.gl;
        let mut k = [0f32; 36];
        let n = u.len().min(32);
        k[..n].copy_from_slice(&u[..n]);
        k[32] = flip;
        if u.len() > 33 {
            k[33] = u[33];
        }
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, target.map(|t| t.fbo));
            gl.viewport(0, 0, vw as i32, vh as i32);
            match scissor {
                Some((x0, y0, x1, y1)) => {
                    gl.enable(glow::SCISSOR_TEST);
                    // 离屏纹理：行号就是自上而下的 y；窗口：翻转
                    let y = if target.is_some() { y0 } else { vh as i32 - y1 };
                    gl.scissor(x0, y, (x1 - x0).max(0), (y1 - y0).max(0));
                }
                None => gl.disable(glow::SCISSOR_TEST),
            }
            if over {
                gl.enable(glow::BLEND);
                gl.blend_func_separate(glow::ONE, glow::ONE_MINUS_SRC_ALPHA, glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            } else {
                gl.disable(glow::BLEND);
            }
            gl.use_program(Some(pr.p));
            gl.uniform_4_f32_slice(pr.u.as_ref(), &k);
            for (i, (s, loc)) in srcs.iter().zip([&pr.t0, &pr.t1]).enumerate() {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                gl.bind_texture(glow::TEXTURE_2D, s.map(|t| t.t));
                gl.uniform_1_i32(loc.as_ref(), i as i32);
            }
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    fn copy(&self, dst: &Tex, src: &Tex) {
        self.draw(&self.pr.copy, Some(dst), dst.w, dst.h, [Some(src), None], &[0.0; 24], 0.0, false, None);
    }

    fn ensure(&mut self, w: u32, h: u32) -> bool {
        if self.fw == w && self.fh == h && self.cpu.is_some() {
            return false;
        }
        self.fw = w;
        self.fh = h;
        for t in [self.cpu.take(), self.base.take(), self.acc[0].take(), self.acc[1].take(), self.disp.take(), self.under5.take()].into_iter().flatten() {
            self.free(t);
        }
        for (_, [a, b]) in std::mem::take(&mut self.small) {
            self.free(a);
            self.free(b);
        }
        for (_, (t, _)) in std::mem::take(&mut self.layers) {
            self.free(t);
        }
        for (_, (t, _)) in std::mem::take(&mut self.bs) {
            self.free(t);
        }
        self.cpu = self.tex(w, h, Fmt::Bgra);
        self.base = self.tex(w, h, Fmt::Bgra);
        self.acc = [self.tex(w, h, Fmt::Rgba), self.tex(w, h, Fmt::Rgba)];
        self.has_under5 = false;
        self.has_base = false;
        self.frame_gpu = false;
        true
    }

    fn small_pair(&mut self, f: u32) -> bool {
        let (sw, sh) = (self.fw.div_ceil(f), self.fh.div_ceil(f));
        if self.small.get(&f).is_some_and(|p| p[0].w == sw && p[0].h == sh) {
            return true;
        }
        if let Some([a, b]) = self.small.remove(&f) {
            self.free(a);
            self.free(b);
        }
        match (self.tex(sw, sh, Fmt::Half), self.tex(sw, sh, Fmt::Half)) {
            (Some(a), Some(b)) => {
                self.small.insert(f, [a, b]);
                true
            }
            _ => false,
        }
    }

    /// = gpu.rs Gpu::blur
    fn blur(&mut self, src: &Tex, sigma: f32) -> Option<(u32, usize)> {
        let f = blur_factor(sigma);
        if !self.small_pair(f) {
            return None;
        }
        let pair = self.small.get(&f)?;
        let (sw, sh) = (pair[0].w as f32, pair[0].h as f32);
        let mut cb = [0f32; 24];
        cb[10] = f as f32;
        cb[16] = self.fw as f32;
        cb[17] = self.fh as f32;
        cb[18] = sw;
        cb[19] = sh;
        self.draw(&self.pr.down, Some(&pair[0]), pair[0].w, pair[0].h, [Some(src), None], &cb, 0.0, false, None);
        let mut i = 0usize;
        for b in crate::fx::boxes_for_gauss(sigma / f as f32, 3) {
            let r = (b - 1) / 2;
            if r == 0 {
                continue;
            }
            for dir in [(1.0, 0.0), (0.0, 1.0)] {
                cb[20] = r as f32;
                cb[21] = dir.0;
                cb[22] = dir.1;
                self.draw(&self.pr.boxp, Some(&pair[1 - i]), pair[0].w, pair[0].h, [Some(&pair[i]), None], &cb, 0.0, false, None);
                i = 1 - i;
            }
        }
        Some((f, i))
    }

    fn mix(&mut self, filt: &Tex, f: u32, rect: [f32; 4], radius: f32, sat: f32, op: f32) {
        let mut cb = [0f32; 24];
        cb[..4].copy_from_slice(&rect);
        cb[4] = radius;
        cb[5] = op.clamp(0.0, 1.0);
        cb[6] = sat;
        cb[10] = f as f32;
        cb[16] = self.fw as f32;
        cb[17] = self.fh as f32;
        cb[18] = filt.w as f32;
        cb[19] = filt.h as f32;
        let (Some(a), Some(b)) = (self.acc[self.cur].as_ref(), self.acc[1 - self.cur].as_ref()) else { return };
        self.draw(&self.pr.mix, Some(b), b.w, b.h, [Some(a), Some(filt)], &cb, 0.0, false, None);
        self.cur = 1 - self.cur;
    }
}

fn blur_factor(sigma: f32) -> u32 {
    if sigma > 12.0 {
        4
    } else if sigma > 6.0 {
        2
    } else {
        1
    }
}

// ------------------------------------------------------------------ compose API（与 gpu.rs 相同）

pub fn frame_begin(w: usize, h: usize) -> bool {
    with(|g| {
        g.ensure(w as u32, h as u32);
        g.acc[0].is_some() && g.acc[1].is_some() && g.base.is_some()
    })
    .unwrap_or(false)
}

pub fn upload_base(buf: &[Bgra], w: usize, h: usize) {
    with(|g| {
        if let Some(b) = g.base.as_ref() {
            g.upload(b, buf, w, 0, h);
            g.has_base = true;
        }
    });
}

pub fn has_base() -> bool {
    with(|g| g.has_base).unwrap_or(false)
}

pub fn start(from_under5: bool) {
    with(|g| {
        g.cur = 0;
        let src = if from_under5 { g.under5.as_ref() } else { g.base.as_ref() };
        if let (Some(s), Some(a)) = (src, g.acc[0].as_ref()) {
            g.copy(a, s);
        }
    });
}

pub fn has_under5() -> bool {
    with(|g| g.has_under5).unwrap_or(false)
}

pub fn save_under5() {
    with(|g| {
        if g.under5.is_none() {
            g.under5 = g.tex(g.fw, g.fh, Fmt::Rgba);
        }
        if let (Some(u), Some(a)) = (g.under5.as_ref(), g.acc[g.cur].as_ref()) {
            g.copy(u, a);
            g.has_under5 = true;
        }
    });
}

pub fn drop_under5() {
    with(|g| g.has_under5 = false);
}

pub fn upload_layer(id: i32, tmp: &[Bgra], w: usize, rows: (usize, usize)) {
    with(|g| {
        let (fw, fh) = (g.fw, g.fh);
        let fresh = !g.layers.get(&id).is_some_and(|(t, _)| t.w == fw && t.h == fh);
        if fresh {
            let Some(t) = g.tex(fw, fh, Fmt::Bgra) else { return };
            if let Some((old, _)) = g.layers.insert(id, (t, (0, fh))) {
                g.free(old);
            }
        }
        let (t, prev) = g.layers.remove(&id).unwrap();
        let y0 = rows.0.min(prev.0 as usize);
        let y1 = rows.1.max(prev.1 as usize);
        g.upload(&t, tmp, w, y0, y1);
        g.layers.insert(id, (t, (rows.0 as u32, rows.1 as u32)));
    });
}

pub fn has_layer(id: i32) -> bool {
    with(|g| g.layers.contains_key(&id)).unwrap_or(false)
}

pub fn backdrop_liquid(rect: [f32; 4], radius: f32, sigma: f32, sat: f32, depth: f32, bevel: f32, op: f32) {
    with(|g| {
        if op <= 0.001 {
            return;
        }
        let r = [rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3])];
        let src_is_disp = depth > 0.0;
        if src_is_disp {
            if g.disp.is_none() {
                g.disp = g.tex(g.fw, g.fh, Fmt::Rgba);
            }
            let mut cb = [0f32; 24];
            cb[..4].copy_from_slice(&r);
            cb[4] = radius;
            cb[7] = depth;
            cb[8] = bevel;
            cb[16] = g.fw as f32;
            cb[17] = g.fh as f32;
            if let (Some(d), Some(a)) = (g.disp.as_ref(), g.acc[g.cur].as_ref()) {
                g.draw(&g.pr.disp, Some(d), d.w, d.h, [Some(a), None], &cb, 0.0, false, None);
            }
        }
        let src = if src_is_disp { g.disp.take() } else { g.acc[g.cur].take() };
        let Some(src) = src else { return };
        if sigma >= 0.3 {
            let r2 = g.blur(&src, sigma);
            if src_is_disp {
                g.disp = Some(src)
            } else {
                g.acc[g.cur] = Some(src)
            }
            if let Some((f, i)) = r2 {
                let pair = g.small.remove(&f).unwrap();
                g.mix(&pair[i], f, r, radius, sat, op);
                g.small.insert(f, pair);
            }
            return;
        }
        if src_is_disp {
            g.mix(&src, 1, r, radius, sat, op);
            g.disp = Some(src);
        } else {
            g.acc[g.cur] = Some(src);
            let copy = g.tex(g.fw, g.fh, Fmt::Rgba);
            if let (Some(c), Some(a)) = (copy.as_ref(), g.acc[g.cur].as_ref()) {
                g.copy(c, a);
            }
            if let Some(c) = copy {
                g.mix(&c, 1, r, radius, sat, op);
                g.free(c);
            }
        }
    });
}

pub fn backdrop_blur(id: i32, recompute: bool, rect: [f32; 4], radius: f32, sigma: f32, sat: f32, op: f32) {
    with(|g| {
        let need = recompute || !g.bs.contains_key(&id);
        if need {
            let Some(src) = g.acc[g.cur].take() else { return };
            let r = g.blur(&src, sigma);
            g.acc[g.cur] = Some(src);
            let Some((f, i)) = r else { return };
            let (w, h) = {
                let p = &g.small[&f];
                (p[0].w, p[0].h)
            };
            let ok = g.bs.get(&id).is_some_and(|(t, bf)| *bf == f && t.w == w && t.h == h);
            if !ok {
                let Some(t) = g.tex(w, h, Fmt::Half) else { return };
                if let Some((old, _)) = g.bs.insert(id, (t, f)) {
                    g.free(old);
                }
            }
            let p = &g.small[&f];
            g.copy(&g.bs[&id].0, &p[i]);
        }
        if op <= 0.001 {
            return;
        }
        let r = [rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3])];
        let (t, f) = g.bs.remove(&id).unwrap();
        g.mix(&t, f, r, radius, sat, op);
        g.bs.insert(id, (t, f));
    });
}

pub fn has_bs(id: i32) -> bool {
    with(|g| g.bs.contains_key(&id)).unwrap_or(false)
}

pub fn retain(shown: &[i32], active: &[i32]) {
    with(|g| {
        let drop_bs: Vec<i32> = g.bs.keys().copied().filter(|id| !shown.contains(id)).collect();
        for id in drop_bs {
            if let Some((t, _)) = g.bs.remove(&id) {
                g.free(t);
            }
        }
        let drop_l: Vec<i32> = g.layers.keys().copied().filter(|id| !active.contains(id)).collect();
        for id in drop_l {
            if let Some((t, _)) = g.layers.remove(&id) {
                g.free(t);
            }
        }
    });
}

pub fn over_layer(id: i32, ox: f32, oy: f32, s: f32, tx: f32, ty: f32, op: f32, clip: (usize, usize, usize, usize)) {
    with(|g| {
        if s <= 0.001 || op <= 0.001 || clip.2 <= clip.0 || clip.3 <= clip.1 {
            return;
        }
        let mut k = [0f32; 24];
        k[5] = op.clamp(0.0, 1.0);
        k[9] = s;
        k[12..16].copy_from_slice(&[ox, oy, tx, ty]);
        k[16] = g.fw as f32;
        k[17] = g.fh as f32;
        let Some((t, _)) = g.layers.get(&id) else { return };
        let Some(a) = g.acc[g.cur].as_ref() else { return };
        g.draw(&g.pr.over, Some(a), a.w, a.h, [Some(t), None], &k, 0.0, true, Some((clip.0 as i32, clip.1 as i32, clip.2 as i32, clip.3 as i32)));
    });
}

pub fn end_compose() {
    with(|g| g.frame_gpu = true);
}

pub fn frame_gpu() -> bool {
    with(|g| g.frame_gpu).unwrap_or(false)
}

// ------------------------------------------------------------------ 呈现（wl.rs）

/// CPU 渲染的帧：只上传 y0..y1（纹理新建过就整幅）
pub fn upload_cpu(buf: &[Bgra], w: usize, h: usize, y0: usize, y1: usize) {
    with(|g| {
        let fresh = g.ensure(w as u32, h as u32);
        if let Some(t) = g.cpu.as_ref() {
            if fresh {
                g.upload(t, buf, w, 0, h);
            } else {
                g.upload(t, buf, w, y0, y1);
            }
        }
        g.frame_gpu = false;
    });
}

/// 一个 wl_surface 上的 EGL 窗口 surface（wl_egl_window）
pub struct Surf {
    win: *mut c_void,
    es: egl::Surface,
    pub w: u32,
    pub h: u32,
}

impl Surf {
    /// `surface`：wl_surface 的 wl_proxy*（系统后端），`w`/`h`：缓冲像素尺寸
    pub fn new(surface: *mut c_void, w: u32, h: u32) -> Option<Surf> {
        let lib = wl_egl()?;
        with(|g| unsafe {
            let win = (lib.create)(surface, w.max(1) as i32, h.max(1) as i32);
            if win.is_null() {
                log::warn!("gl: wl_egl_window_create failed");
                return None;
            }
            match g.egl.create_window_surface(g.dpy, g.cfg, win as egl::NativeWindowType, None) {
                Ok(es) => Some(Surf { win, es, w: w.max(1), h: h.max(1) }),
                Err(e) => {
                    log::warn!("gl: eglCreateWindowSurface: {e}");
                    (lib.destroy)(win);
                    None
                }
            }
        })
        .flatten()
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        let (w, h) = (w.max(1), h.max(1));
        if self.w == w && self.h == h {
            return;
        }
        if let Some(lib) = wl_egl() {
            unsafe { (lib.resize)(self.win, w as i32, h as i32, 0, 0) };
        }
        self.w = w;
        self.h = h;
    }

    /// 设为当前、不等 vsync（帧节拍由事件循环 / frame 回调决定，eglSwapBuffers 不阻塞 UI 线程）
    fn bind(&self, g: &Gl) -> bool {
        if g.egl.make_current(g.dpy, Some(self.es), Some(self.es), Some(g.ctx)).is_err() {
            return false;
        }
        let _ = g.egl.swap_interval(g.dpy, 0);
        true
    }

    /// 交换（= attach + damage + wl_surface.commit）
    fn swap(&self, g: &Gl) {
        match g.egl.swap_buffers(g.dpy, self.es) {
            Err(egl::Error::ContextLost) => {
                if !LOST.swap(true, Ordering::AcqRel) {
                    log::warn!("gl: eglSwapBuffers: EGL_CONTEXT_LOST (GPU reset / driver restart / resume)");
                }
            }
            Err(e) => log::warn!("gl: eglSwapBuffers: {e}"),
            Ok(()) => {
                if let (Some(ms), Some(t0)) = (lose_test_ms(), *LOSE_AT.lock().unwrap()) {
                    if t0.elapsed().as_millis() as u64 >= ms && !LOSE_DONE.swap(true, Ordering::AcqRel) {
                        log::warn!("gl: LANCAT_GPU_TEST=lose: simulating EGL_CONTEXT_LOST");
                        LOST.store(true, Ordering::Release);
                    }
                }
            }
        }
    }
}

impl Drop for Surf {
    fn drop(&mut self) {
        with(|g| {
            let _ = g.egl.make_current(g.dpy, None, None, Some(g.ctx));
            let _ = g.egl.destroy_surface(g.dpy, self.es);
        });
        if let Some(lib) = wl_egl() {
            unsafe { (lib.destroy)(self.win) };
        }
    }
}

/// 面板这一帧（GPU 合成结果或上传的 CPU 帧）按姿态（以中心缩放 s、平移 tx/ty、透明度 op，= compose::xform_frame）
/// 画到 surface 并提交。`clear` = 整幅透明（收起动画结束那一帧）
pub fn present_panel(surf: &Surf, pose: (f32, f32, f32, f32), clear: bool) {
    let t = Instant::now();
    with(|g| {
        if !surf.bind(g) {
            return;
        }
        unsafe {
            g.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            g.gl.viewport(0, 0, surf.w as i32, surf.h as i32);
            g.gl.disable(glow::SCISSOR_TEST);
            g.gl.clear_color(0.0, 0.0, 0.0, 0.0);
            g.gl.clear(glow::COLOR_BUFFER_BIT);
        }
        let src = if g.frame_gpu { g.acc[g.cur].as_ref() } else { g.cpu.as_ref() };
        if let (Some(s), false) = (src, clear) {
            let (tx, ty, sc, op) = pose;
            let mut k = [0f32; 24];
            k[5] = op.clamp(0.0, 1.0);
            k[9] = sc.max(0.001);
            k[12..16].copy_from_slice(&[s.w as f32 / 2.0, s.h as f32 / 2.0, tx, ty]);
            k[16] = s.w as f32;
            k[17] = s.h as f32;
            g.draw(&g.pr.over, None, surf.w, surf.h, [Some(s), None], &k, surf.h as f32, false, None);
        }
        surf.swap(g);
    });
    PRESENT_MS.with(|p| p.set(t.elapsed().as_secs_f32() * 1000.0));
}

/// GPU 上的一张图（收起时冻结的面板帧、变形的玻璃底）
pub struct GTex(Tex);

impl Drop for GTex {
    fn drop(&mut self) {
        let t = Tex { t: self.0.t, fbo: self.0.fbo, w: self.0.w, h: self.0.h };
        with(|g| g.free(t));
    }
}

/// 面板当前这一帧复制一份（收起变形冻结的那一帧）
pub fn snapshot() -> Option<GTex> {
    with(|g| {
        let src = if g.frame_gpu { g.acc[g.cur].as_ref() } else { g.cpu.as_ref() }?;
        let t = g.tex(src.w, src.h, Fmt::Rgba)?;
        g.copy(&t, src);
        Some(GTex(t))
    })
    .flatten()
}

/// 截到的背景（已模糊的 CPU 图，面板 surface 尺寸）上传
pub fn upload_glass(px: &[Bgra], w: usize, h: usize) -> Option<GTex> {
    if w == 0 || h == 0 || px.len() != w * h {
        return None;
    }
    with(|g| {
        let t = g.tex(w as u32, h as u32, Fmt::Bgra)?;
        g.upload(&t, px, w, 0, h);
        Some(GTex(t))
    })
    .flatten()
}

/// Linux 的变形（render_rest：形状在面板 surface 里画，内容 = 面板整幅帧，和形状交叉淡化）画到 surface 并提交
pub fn present_morph(surf: &Surf, f: &crate::morph::Frame, look: &crate::morph::Look, rest: &crate::morph::Rest, glass: Option<&GTex>, snap: Option<&GTex>, at: (f64, f64)) {
    let t = Instant::now();
    with(|g| {
        if !surf.bind(g) {
            return;
        }
        let content = match snap {
            Some(s) => Some(&s.0),
            None => {
                if g.frame_gpu {
                    g.acc[g.cur].as_ref()
                } else {
                    g.cpu.as_ref()
                }
            }
        };
        let k = morph_u(f, look, rest, glass.map(|x| &x.0), content, at);
        let mut u = [0f32; 36];
        u[..32].copy_from_slice(&k);
        u[33] = 1.0; // rest 模式
        if f.done {
            unsafe {
                g.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                g.gl.viewport(0, 0, surf.w as i32, surf.h as i32);
                g.gl.disable(glow::SCISSOR_TEST);
                g.gl.clear_color(0.0, 0.0, 0.0, 0.0);
                g.gl.clear(glow::COLOR_BUFFER_BIT);
            }
        } else {
            g.draw(&g.pr.morph, None, surf.w, surf.h, [glass.map(|x| &x.0), content], &u, surf.h as f32, false, None);
        }
        surf.swap(g);
    });
    PRESENT_MS.with(|p| p.set(t.elapsed().as_secs_f32() * 1000.0));
}

/// morph.rs render_impl（rest 模式）的参数，布局 = gpu.rs morph_cb
fn morph_u(f: &crate::morph::Frame, look: &crate::morph::Look, rest: &crate::morph::Rest, glass: Option<&Tex>, content: Option<&Tex>, at: (f64, f64)) -> [f32; 32] {
    let sc = look.scale;
    let r = f.rect;
    let bevel = (14.0 * sc).min((r[2] - r[0]).min(r[3] - r[1]) / 2.0).max(1.0);
    let gr = f.grow.clamp(0.0, 1.0);
    let k_acc = (1.0 - gr * 2.2).clamp(0.0, 1.0);
    let (tint, ta) = (rest.tint, rest.alpha);
    let mix = |a: f64, b: f64| a * (1.0 - k_acc) + b * k_acc;
    let body = [mix(tint.0, look.accent.0), mix(tint.1, look.accent.1), mix(tint.2, look.accent.2), mix(ta, 0.85)];
    let rim_a = if look.dark { 0.30 } else { 0.75 } * gr.max(0.25);
    let sheen_a = if look.dark { 0.10 } else { 0.30 } * (1.0 - f.content * 0.7) * gr;
    let shadow_a = 0.20 * gr.max(0.25);
    let mut k = [0f32; 32];
    k[0..4].copy_from_slice(&[r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32]);
    k[4..8].copy_from_slice(&[f.radius as f32, (14.0 * sc) as f32, (2.0 * sc) as f32, bevel as f32]);
    let c_op = if content.is_some() { f.content.clamp(0.0, 1.0) } else { 0.0 };
    k[8..12].copy_from_slice(&[(18.0 * sc) as f32, (1.2 * sc) as f32, c_op as f32, 1.0]);
    k[12..16].copy_from_slice(&[body[0] as f32, body[1] as f32, body[2] as f32, body[3] as f32]);
    k[16..20].copy_from_slice(&[rim_a as f32, sheen_a as f32, shadow_a as f32, if glass.is_some() { 1.0 } else { 0.0 }]);
    if let Some(gl) = glass {
        // 玻璃 = 面板 surface 尺寸的全分辨率图（CPU 已模糊），与目标像素一一对应
        k[20..24].copy_from_slice(&[0.0, 0.0, 1.0 / gl.w as f32, 1.0 / gl.h as f32]);
    }
    if let Some(c) = content {
        let (pw, ph) = (c.w as f64, c.h as f64);
        let (cw, ch) = ((pw - 2.0 * at.0).max(1.0), (ph - 2.0 * at.1).max(1.0));
        let (kx, ky) = (cw / (r[2] - r[0]).max(1e-3), ch / (r[3] - r[1]).max(1e-3));
        k[24..28].copy_from_slice(&[at.0 as f32, at.1 as f32, kx as f32, ky as f32]);
        k[28..32].copy_from_slice(&[(1.0 / pw) as f32, (1.0 / ph) as f32, 0.0, 0.0]);
    }
    k
}

/// 不经过合成的普通窗口 / toast / 停靠条：CPU 画好的整幅预乘 BGRA 上传成纹理再画到 surface（GPU 呈现）
pub struct Blit {
    tex: Option<Tex>,
}

impl Blit {
    pub fn new() -> Blit {
        Blit { tex: None }
    }

    /// `rows`：变了的行（None = 整幅）
    pub fn present(&mut self, surf: &Surf, px: &[Bgra], w: usize, h: usize, rows: Option<(usize, usize)>) {
        if w == 0 || h == 0 || px.len() < w * h {
            return;
        }
        let tex = self.tex.take();
        let tex = with(|g| {
            let mut fresh = false;
            let t = match tex {
                Some(t) if t.w == w as u32 && t.h == h as u32 => t,
                other => {
                    if let Some(o) = other {
                        g.free(o);
                    }
                    fresh = true;
                    g.tex(w as u32, h as u32, Fmt::Bgra)?
                }
            };
            let (y0, y1) = if fresh { (0, h) } else { rows.unwrap_or((0, h)) };
            g.upload(&t, px, w, y0, y1);
            if surf.bind(g) {
                let mut k = [0f32; 24];
                k[5] = 1.0;
                k[9] = 1.0;
                k[16] = w as f32;
                k[17] = h as f32;
                g.draw(&g.pr.over, None, surf.w, surf.h, [Some(&t), None], &k, surf.h as f32, false, None);
                surf.swap(g);
            }
            Some(t)
        })
        .flatten();
        self.tex = tex;
    }
}

impl Drop for Blit {
    fn drop(&mut self) {
        if let Some(t) = self.tex.take() {
            with(|g| g.free(t));
        }
    }
}

impl Default for Blit {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
fn _unused(_: Rc<()>) {}
