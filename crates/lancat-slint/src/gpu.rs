//! Windows：面板的 GPU 呈现 + 合成（D3D11 + DXGI composition swapchain + DirectComposition，预乘 alpha）。
//!
//! - 呈现：面板窗口带 WS_EX_NOREDIRECTIONBITMAP，内容是一个 DComp visual（swapchain 当 content）。
//!   整窗淡入淡出的透明度在呈现着色器里乘（不再靠 LWA_ALPHA）；停放时 win.rs 仍加 WS_EX_LAYERED |
//!   WS_EX_TRANSPARENT 让点击穿透。
//! - 合成：compose.rs 的分层合成（基底 + 各活动层）在 GPU 上做：每层 Slint 的 CPU 缓冲上传成纹理，
//!   背景滤镜（liquid 位移 → 降采样 → 三次盒式模糊 → 饱和度 → 圆角覆盖 × 透明度混合）和层的
//!   缩放/平移/透明度 + 预乘 over 都在像素着色器里，算法与 compose.rs 的 CPU 版逐步对应。
//! - LANCAT_CPU_COMPOSE=1 或 D3D 初始化失败：走原来的 CPU 路径（SetDIBitsToDevice）。
#![allow(clippy::too_many_arguments)]

use crate::compose::Bgra;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;
use windows::core::{Interface, PCSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::Fxc::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::WaitForSingleObjectEx;

const HLSL: &str = r#"
cbuffer C : register(b0) {
  float4 rect;  // x0 y0 x1 y1（物理 px）
  float4 p0;    // radius, op, sat, depth
  float4 p1;    // bevel, s, f, lqflag
  float4 p2;    // ox, oy, tx, ty
  float4 p3;    // srcW, srcH, smallW, smallH
  float4 p4;    // boxR, dirX, dirY, 0
};
Texture2D T0 : register(t0);
Texture2D T1 : register(t1);
SamplerState SL : register(s0); // linear, clamp
SamplerState SB : register(s1); // linear, border 0

struct V { float4 pos : SV_Position; };
V vs(uint id : SV_VertexID) {
  V o; float2 uv = float2((id << 1) & 2, id & 2);
  o.pos = float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
  return o;
}

float rrect_cov(float2 p, float4 r, float rad) {
  float2 sz = r.zw - r.xy;
  rad = max(min(rad, min(sz.x, sz.y) * 0.5), 0);
  float2 q = abs(p - (r.xy + r.zw) * 0.5) - (sz * 0.5 - rad);
  float d = length(max(q, 0)) + min(max(q.x, q.y), 0) - rad;
  return saturate(0.5 - d);
}

float3 liquid_n(float2 p, float4 r, float rad, float bevel) {
  float2 hh = (r.zw - r.xy) * 0.5;
  rad = min(rad, min(hh.x, hh.y));
  float2 f = p - (r.xy + hh);
  float2 q = abs(f) - (hh - rad);
  float d = -(length(max(q, 0)) + min(max(q.x, q.y), 0) - rad);
  if (d < 0 || d >= bevel) return float3(0, 0, 0);
  float2 n;
  if (q.x > 0 && q.y > 0) n = q / max(length(q), 1e-6);
  else if (q.x > q.y) n = float2(1, 0);
  else n = float2(0, 1);
  n *= float2(f.x >= 0 ? 1 : -1, f.y >= 0 ? 1 : -1);
  float k = 1 - d / bevel;
  return float3(n, k * k);
}

// 呈现：帧（可选以中心缩放 s）叠在液态玻璃底上，整体乘窗口透明度
float4 ps_present(V i) : SV_Target {
  float2 d = i.pos.xy;
  float2 src = p2.xy + (d - p2.xy) / p1.y;
  float4 c = T0.Sample(SB, src / p3.xy);
  if (p1.w > 0.5) { float4 b = T1.Load(int3(d, 0)); c = c + b * (1 - c.a); }
  return c * p0.y;
}

// 层内容：以 (ox,oy) 缩放 s 再平移 (tx,ty)、乘 opacity（预乘 over 由混合状态做）
float4 ps_over(V i) : SV_Target {
  float2 src = p2.xy + (i.pos.xy - p2.zw - p2.xy) / p1.y;
  return T0.Sample(SB, src / p3.xy) * p0.y;
}

// liquid 位移：圆角矩形边缘 bevel 内从更靠里处取样
float4 ps_disp(V i) : SV_Target {
  float2 p = i.pos.xy;
  if (p.x >= rect.x && p.x <= rect.z && p.y >= rect.y && p.y <= rect.w) {
    float3 n = liquid_n(p, rect, p0.x, p1.x);
    if (n.z > 0) {
      float2 s = clamp(p - p0.w * n.xy * n.z, rect.xy, rect.zw);
      return T0.Sample(SL, s / p3.xy);
    }
  }
  return T0.Load(int3(p, 0));
}

// f×f 盒式降采样（只平均落在图内的像素）
float4 ps_down(V i) : SV_Target {
  int f = (int)p1.z;
  int2 o = int2(i.pos.xy) * f;
  int2 mx = int2(p3.xy);
  float4 acc = 0; float n = 0;
  [unroll] for (int y = 0; y < 4; y++) {
    [unroll] for (int x = 0; x < 4; x++) {
      int2 q = o + int2(x, y);
      if (x < f && y < f && q.x < mx.x && q.y < mx.y) { acc += T0.Load(int3(q, 0)); n += 1; }
    }
  }
  return acc / max(n, 1);
}

// 一次盒式模糊（半径 r，方向 dir），边缘取边上的值
float4 ps_box(V i) : SV_Target {
  int r = (int)p4.x;
  int2 dir = int2(p4.yz);
  int2 c = int2(i.pos.xy);
  int2 mx = int2(p3.zw) - 1;
  float4 acc = 0;
  [loop] for (int k = -r; k <= r; k++) acc += T0.Load(int3(clamp(c + dir * k, int2(0, 0), mx), 0));
  return acc / (2 * r + 1);
}

// 背景滤镜收尾：滤后像素（双线性升回原尺寸）→ 饱和度 → 圆角覆盖 × 透明度 与原像素混合
float4 ps_mix(V i) : SV_Target {
  float2 p = i.pos.xy;
  float4 d = T0.Load(int3(p, 0));
  float c = rrect_cov(p, rect, p0.x) * p0.y;
  if (c <= 0) return d;
  float4 n = T1.Sample(SL, (p / p1.z) / p3.zw);
  float s = p0.z;
  if (abs(s - 1) > 0.001) {
    float3 m0 = float3(0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s);
    float3 m1 = float3(0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s);
    float3 m2 = float3(0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s);
    float3 rgb = n.rgb;
    n.r = clamp(dot(m0, rgb), 0, n.a);
    n.g = clamp(dot(m1, rgb), 0, n.a);
    n.b = clamp(dot(m2, rgb), 0, n.a);
  }
  return d + (n - d) * c;
}
"#;

/// 变形窗口（以及面板的液态玻璃底）：圆角矩形玻璃 + 透镜折射 + 色调 + 高光 + 阴影 + 拉伸叠内容，
/// 与 morph.rs render_impl（stretch 模式）逐步对应。坐标：目标像素（SV_Position，像素中心 +0.5）。
const MORPH_HLSL: &str = r#"
cbuffer M : register(b0) {
  float4 rect;  // 形状 x0 y0 x1 y1（目标 px）
  float4 m0;    // radius, shadow, sh_dy, bevel
  float4 m1;    // strength, rim_w, content op, 整体透明度
  float4 body;  // 色调 rgb, 色调透明度
  float4 m2;    // rim_a, sheen_a, shadow_a, 有玻璃
  float4 gmap;  // 目标 → 玻璃全分辨率坐标的偏移 xy, 1/(f*smallW), 1/(f*smallH)
  float4 cmap;  // 内容：卡片左上 cax cay, kx ky
  float4 csz;   // 1/cw, 1/ch, 0, 0
};
Texture2D G : register(t0);
Texture2D C : register(t1);
SamplerState SL : register(s0);
SamplerState SB : register(s1);
struct V { float4 pos : SV_Position; };

float sdf_n(float2 p, float4 r, float rad, out float2 n) {
  float2 hh = (r.zw - r.xy) * 0.5;
  rad = max(min(rad, min(hh.x, hh.y)), 0);
  float2 f = p - (r.xy + hh);
  float2 q = abs(f) - (hh - rad);
  float d = length(max(q, 0)) + min(max(q.x, q.y), 0) - rad;
  if (q.x > 0 && q.y > 0) n = q / max(length(q), 1e-6);
  else if (q.x > q.y) n = float2(1, 0);
  else n = float2(0, 1);
  n *= float2(f.x >= 0 ? 1 : -1, f.y >= 0 ? 1 : -1);
  return d;
}

float4 ps_morph(V i) : SV_Target {
  float2 p = i.pos.xy;
  float2 nrm; float2 nn;
  float d = sdf_n(p, rect, m0.x, nrm);
  float ds = sdf_n(p - float2(0, m0.z), rect, m0.x, nn);
  float shadow = m0.y;
  float sh = ds > -shadow ? m2.z * exp(-pow(max(ds, 0) / (shadow * 0.42), 2)) : m2.z;
  float cov = saturate(0.5 - d);
  if (cov <= 0) return float4(0, 0, 0, sh) * m1.w;
  float depth = -d;
  float bevel = m0.w;
  float ba = body.a;
  float3 rgb; float a;
  if (m2.w > 0.5) {
    float off = 0;
    if (depth > 0 && depth < bevel) { float t = 1 - depth / bevel; off = (1 - sqrt(max(1 - t * t, 0))) * m1.x; }
    float2 q = p + gmap.xy;
    float3 g1 = G.Sample(SL, (q - nrm * off) * gmap.zw).rgb;
    float3 c0 = g1;
    if (off > 0.5) {
      c0.r = G.Sample(SL, (q - nrm * off * 1.08) * gmap.zw).r;
      c0.b = G.Sample(SL, (q - nrm * off * 0.92) * gmap.zw).b;
    }
    float lum = dot(c0, float3(0.2126, 0.7152, 0.0722));
    c0 = saturate(lum + (c0 - lum) * 1.25);
    float lift = depth < bevel ? pow(1 - depth / bevel, 2) * (22.0 / 255.0) : 0;
    rgb = min(c0 + lift, 1) * (1 - ba) + body.rgb * ba;
    a = 1;
  } else {
    rgb = body.rgb * ba;
    a = ba;
  }
  float tyk = saturate((p.y - rect.y) / max((rect.w - rect.y) * 0.45, 1));
  float sheen = m2.y * (1 - tyk);
  if (sheen > 0) { rgb += (1 - rgb) * sheen; a += (1 - a) * sheen; }
  float rimw = m1.y;
  if (depth < rimw) {
    float kk = saturate(1 - depth / rimw);
    float light = clamp(0.55 - 0.45 * nrm.y, 0.1, 1);
    float ra = m2.x * kk * light;
    rgb += (1 - rgb) * ra; a += (1 - a) * ra;
  }
  if (m1.z > 0) {
    float2 cp = cmap.xy + (p - rect.xy) * cmap.zw;
    float4 c = C.Sample(SB, cp * csz.xy);
    float inv = 1 - c.a * m1.z;
    rgb = c.rgb * m1.z + rgb * inv;
    a = c.a * m1.z + a * inv;
  }
  rgb *= cov; a *= cov;
  return float4(rgb, a + sh * (1 - a)) * m1.w;
}
"#;

struct Tex {
    t: ID3D11Texture2D,
    srv: Option<ID3D11ShaderResourceView>,
    rtv: Option<ID3D11RenderTargetView>,
    w: u32,
    h: u32,
}

struct Shaders {
    vs: ID3D11VertexShader,
    present: ID3D11PixelShader,
    over: ID3D11PixelShader,
    disp: ID3D11PixelShader,
    down: ID3D11PixelShader,
    boxp: ID3D11PixelShader,
    mix: ID3D11PixelShader,
    morph: ID3D11PixelShader,
}

/// 纯模糊层的模糊结果（与层姿态无关，compose 决定何时重算）
struct BsTex {
    tex: Tex,
    f: u32,
}

pub struct Gpu {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    wait: HANDLE,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    bb: Option<ID3D11RenderTargetView>,
    sw: u32,
    sh: u32,
    sh_: Shaders,
    s_lin: ID3D11SamplerState,
    s_bord: ID3D11SamplerState,
    b_over: ID3D11BlendState,
    b_none: ID3D11BlendState,
    rs: ID3D11RasterizerState,
    cb: ID3D11Buffer,
    /// 变形着色器的常量（32 个 float）
    cb2: ID3D11Buffer,
    // 帧尺寸的纹理
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
    lq: Option<Tex>,
    small: HashMap<u32, [Tex; 2]>,
    layers: HashMap<i32, (Tex, (u32, u32))>,
    bs: HashMap<i32, BsTex>,
    staging: Option<ID3D11Texture2D>,
    frame_gpu: bool,
    opacity: f32,
    settle: f32,
    pending_wait: bool,
    frame_ms: u32,
    warp: bool,
}

thread_local! {
    static GPU: RefCell<Option<Gpu>> = const { RefCell::new(None) };
}

fn log_err<T>(what: &str, r: windows::core::Result<T>) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            log::warn!("gpu: {what} failed: {e}");
            None
        }
    }
}

/// UI 线程 id（设备建好后给它发唤醒消息；切回 GPU 时重建设备用）
static UI_THREAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 环境变量 LANCAT_CPU_COMPOSE=1：强制 CPU（优先于设置）
pub fn env_forced_cpu() -> bool {
    crate::render::env_forced_cpu()
}

/// 想不想用 GPU（LANCAT_CPU_COMPOSE=1 或设置里选了 CPU 就不用；见 render.rs）
pub fn wanted() -> bool {
    crate::render::wanted()
}

/// 设置里的渲染方式（启动时在建面板窗口之前设；运行中切换见 win.rs apply_render_mode）
pub fn set_pref_cpu(cpu: bool) {
    if cpu {
        // 后台已经建好的设备用不上了
        if PREP_STATE.load(std::sync::atomic::Ordering::Acquire) == 1 && !active() {
            PREP.lock().unwrap().take();
        }
    }
}

/// 实际的渲染状态（设置页显示）："gpu" | "warp"（软件 D3D，只用来呈现）| "pending" | "cpu"（设置）|
/// "cpu-env"（LANCAT_CPU_COMPOSE=1）| "failed"（GPU 初始化失败，自动用 CPU）
pub fn status() -> &'static str {
    if env_forced_cpu() {
        return "cpu-env";
    }
    if active() {
        return if hardware() { "gpu" } else { "warp" };
    }
    if crate::render::pref_cpu() {
        return "cpu";
    }
    if crate::render::failed() || PREP_STATE.load(std::sync::atomic::Ordering::Acquire) == 2 {
        return "failed";
    }
    "pending"
}

/// 切到 CPU：丢掉设备、交换链和所有纹理（调用方先把用 GPU 的窗口换回 CPU 版）
pub fn shutdown() {
    drop_panel_glass();
    let g = GPU.with(|g| g.borrow_mut().take());
    if let Some(g) = g {
        unsafe {
            g.ctx.ClearState();
            g.ctx.Flush();
        }
        drop(g);
        log::info!("gpu: shut down (CPU rendering)");
    }
    PREP.lock().unwrap().take();
    PREP_STATE.store(0, std::sync::atomic::Ordering::Release);
    PREP_STARTED.store(false, std::sync::atomic::Ordering::Release);
}

/// 切回 GPU：后台重新建设备（窗口按 GPU 重建后 poll_attach 接上）
pub fn restart() {
    crate::render::clear_failed();
    let t = UI_THREAD.load(std::sync::atomic::Ordering::Relaxed);
    if t != 0 {
        prewarm(t);
    }
}

fn compile(src: &str, entry: &str, target: &str) -> Option<Vec<u8>> {
    let e = std::ffi::CString::new(entry).ok()?;
    let t = std::ffi::CString::new(target).ok()?;
    let mut code: Option<ID3DBlob> = None;
    let mut err: Option<ID3DBlob> = None;
    let r = unsafe {
        D3DCompile(src.as_ptr() as *const _, src.len(), PCSTR::null(), None, None, PCSTR(e.as_ptr() as *const u8), PCSTR(t.as_ptr() as *const u8), D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &mut code, Some(&mut err))
    };
    if let Err(x) = r {
        let msg = err
            .map(|b| unsafe { String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())).into_owned() })
            .unwrap_or_default();
        log::warn!("gpu: compile {entry} failed: {x} {msg}");
        return None;
    }
    let b = code?;
    Some(unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec() })
}

/// 后台线程建好的设备（COM 指针跨线程交接一次：D3D11 设备本身是自由线程的，立即上下文交接后只在 UI 线程用）
struct Prep {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
    sh: Shaders,
    warp: bool,
}
unsafe impl Send for Prep {}

static PREP: std::sync::Mutex<Option<Prep>> = std::sync::Mutex::new(None);
/// 0 = 还在建，1 = 好了（在 PREP 里），2 = 失败
static PREP_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static PREP_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn create_device(kind: D3D_DRIVER_TYPE) -> Option<(ID3D11Device, ID3D11DeviceContext)> {
    let mut dev: Option<ID3D11Device> = None;
    let mut ctx: Option<ID3D11DeviceContext> = None;
    let levels = [D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0];
    let r = unsafe { D3D11CreateDevice(None, kind, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, Some(&levels), D3D11_SDK_VERSION, Some(&mut dev), None, Some(&mut ctx)) };
    log_err("D3D11CreateDevice", r)?;
    Some((dev?, ctx?))
}

fn build_prep() -> Option<Prep> {
    let t = Instant::now();
    // 测试降级路径：LANCAT_GPU_TEST=fail（设备失败 → 窗口换回 CPU 版）/ warp（只用 WARP 呈现，合成走 CPU）
    let test = std::env::var("LANCAT_GPU_TEST").unwrap_or_default();
    if test == "fail" {
        log::warn!("gpu: LANCAT_GPU_TEST=fail");
        return None;
    }
    let hw = if test == "warp" { None } else { create_device(D3D_DRIVER_TYPE_HARDWARE) };
    let (dev, ctx, warp) = match hw {
        Some((d, c)) => (d, c, false),
        None => {
            // 没有可用的硬件设备：WARP 只用来呈现（合成仍走 CPU）
            let (d, c) = create_device(D3D_DRIVER_TYPE_WARP)?;
            (d, c, true)
        }
    };
    let t1 = t.elapsed().as_secs_f32() * 1000.0;
    let ps = |e: &str| -> Option<ID3D11PixelShader> {
        let code = compile(HLSL, e, "ps_4_0")?;
        let mut s = None;
        log_err(e, unsafe { dev.CreatePixelShader(&code, None, Some(&mut s)) })?;
        s
    };
    let vs = (|| {
        let code = compile(HLSL, "vs", "vs_4_0")?;
        let mut s = None;
        log_err("vs", unsafe { dev.CreateVertexShader(&code, None, Some(&mut s)) })?;
        s
    })();
    let morph = (|| {
        let code = compile(MORPH_HLSL, "ps_morph", "ps_4_0")?;
        let mut s = None;
        log_err("ps_morph", unsafe { dev.CreatePixelShader(&code, None, Some(&mut s)) })?;
        s
    })();
    let sh = Shaders { vs: vs?, present: ps("ps_present")?, over: ps("ps_over")?, disp: ps("ps_disp")?, down: ps("ps_down")?, boxp: ps("ps_box")?, mix: ps("ps_mix")?, morph: morph? };
    let name = unsafe { dev.cast::<IDXGIDevice>().and_then(|d| d.GetAdapter()).and_then(|a| a.GetDesc()) }
        .map(|d| String::from_utf16_lossy(&d.Description[..d.Description.iter().position(|c| *c == 0).unwrap_or(d.Description.len())]))
        .unwrap_or_default();
    log::info!("gpu: device {name:?} {:?}{} in {:.1} ms, shaders {:.1} ms (background)", unsafe { dev.GetFeatureLevel() }, if warp { " WARP" } else { "" }, t1, t.elapsed().as_secs_f32() * 1000.0 - t1);
    Some(Prep { dev, ctx, sh, warp })
}

/// 进程一开始就在后台线程建设备、编译着色器（硬件设备 + 编译要 ~0.5 s，不能挡住第一帧）；
/// 好了之后给 UI 线程发一条唤醒消息（`ui_thread`），事件循环里 poll_attach 接上。
pub fn prewarm(ui_thread: u32) {
    UI_THREAD.store(ui_thread, std::sync::atomic::Ordering::Relaxed);
    if env_forced_cpu() || PREP_STARTED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        let p = build_prep();
        if crate::render::pref_cpu() {
            // 设置里是 CPU：设备用不上，放掉（显卡驱动的线程和内存）
            drop(p);
            PREP_STARTED.store(false, std::sync::atomic::Ordering::Release);
            return;
        }
        let ok = p.is_some();
        *PREP.lock().unwrap() = p;
        PREP_STATE.store(if ok { 1 } else { 2 }, std::sync::atomic::Ordering::Release);
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(ui_thread, windows::Win32::UI::WindowsAndMessaging::WM_APP + 1, WPARAM(0), LPARAM(0));
        }
    });
}

pub enum Attach {
    Pending,
    Ok,
    Failed,
}

/// 窗口（带 NOREDIRECTIONBITMAP）建好后在事件循环里反复调用，直到设备好了并接上交换链
pub fn poll_attach(hwnd: HWND, w: u32, h: u32) -> Attach {
    match PREP_STATE.load(std::sync::atomic::Ordering::Acquire) {
        0 => return Attach::Pending,
        2 => return Attach::Failed,
        _ => {}
    }
    let Some(p) = PREP.lock().unwrap().take() else { return Attach::Failed };
    if attach(hwnd, w, h, p) {
        Attach::Ok
    } else {
        Attach::Failed
    }
}

/// GPU 合成可用（WARP 设备或 LANCAT_GPU_COMPOSE=0 时只用来呈现，合成走 CPU）
pub fn compose_enabled() -> bool {
    with(|g| !g.warp).unwrap_or(false) && std::env::var("LANCAT_GPU_COMPOSE").as_deref() != Ok("0")
}

/// 第二步（窗口建好之后）：swapchain + DComp target。失败 = 调用方去掉 NOREDIRECTIONBITMAP 重建窗口。
fn attach(hwnd: HWND, w: u32, h: u32, p: Prep) -> bool {
    let Prep { dev, ctx, sh, warp } = p;
    let g = (|| -> windows::core::Result<Gpu> {
        unsafe {
            let (w, h) = (w.max(1), h.max(1));
            let (swap, wait, dcomp, target, visual) = make_swap(&dev, hwnd, w, h)?;
            let samp = |addr: D3D11_TEXTURE_ADDRESS_MODE| -> windows::core::Result<ID3D11SamplerState> {
                let d = D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                    AddressU: addr,
                    AddressV: addr,
                    AddressW: addr,
                    MipLODBias: 0.0,
                    MaxAnisotropy: 1,
                    ComparisonFunc: D3D11_COMPARISON_NEVER,
                    BorderColor: [0.0; 4],
                    MinLOD: 0.0,
                    MaxLOD: 0.0,
                };
                let mut s = None;
                dev.CreateSamplerState(&d, Some(&mut s))?;
                Ok(s.unwrap())
            };
            let blend = |on: bool| -> windows::core::Result<ID3D11BlendState> {
                let mut d = D3D11_BLEND_DESC::default();
                d.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                    BlendEnable: on.into(),
                    SrcBlend: D3D11_BLEND_ONE,
                    DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                    BlendOp: D3D11_BLEND_OP_ADD,
                    SrcBlendAlpha: D3D11_BLEND_ONE,
                    DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                    BlendOpAlpha: D3D11_BLEND_OP_ADD,
                    RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
                };
                let mut s = None;
                dev.CreateBlendState(&d, Some(&mut s))?;
                Ok(s.unwrap())
            };
            let rd = D3D11_RASTERIZER_DESC { FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, ScissorEnable: true.into(), DepthClipEnable: true.into(), ..Default::default() };
            let mut rs = None;
            dev.CreateRasterizerState(&rd, Some(&mut rs))?;
            let bd = D3D11_BUFFER_DESC { ByteWidth: 128, Usage: D3D11_USAGE_DYNAMIC, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32, MiscFlags: 0, StructureByteStride: 0 };
            let mut cb = None;
            dev.CreateBuffer(&bd, None, Some(&mut cb))?;
            let bd2 = D3D11_BUFFER_DESC { ByteWidth: 128, ..bd };
            let mut cb2 = None;
            dev.CreateBuffer(&bd2, None, Some(&mut cb2))?;
            let mut g = Gpu {
                s_lin: samp(D3D11_TEXTURE_ADDRESS_CLAMP)?,
                s_bord: samp(D3D11_TEXTURE_ADDRESS_BORDER)?,
                b_over: blend(true)?,
                b_none: blend(false)?,
                rs: rs.unwrap(),
                cb: cb.unwrap(),
                cb2: cb2.unwrap(),
                dev: dev.clone(),
                ctx: ctx.clone(),
                swap,
                wait,
                _dcomp: dcomp,
                _target: target,
                _visual: visual,
                bb: None,
                sw: w,
                sh: h,
                sh_: sh,
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
                lq: None,
                small: HashMap::new(),
                layers: HashMap::new(),
                bs: HashMap::new(),
                staging: None,
                frame_gpu: false,
                opacity: 0.0,
                settle: 1.0,
                pending_wait: false,
                frame_ms: refresh_ms(),
                warp,
            };
            g.make_bb()?;
            Ok(g)
        }
    })();
    match g {
        Ok(g) => {
            log::info!("gpu: DirectComposition swapchain attached {}x{}, frame {} ms", g.sw, g.sh, g.frame_ms);
            unsafe {
                let _ = windows::Win32::Media::timeBeginPeriod(1);
            }
            GPU.with(|x| *x.borrow_mut() = Some(g));
            true
        }
        Err(e) => {
            log::warn!("gpu: attach failed: {e}; falling back to CPU present");
            crate::render::mark_failed();
            false
        }
    }
}

/// 给一个 HWND（带 WS_EX_NOREDIRECTIONBITMAP）建 composition swapchain + DComp target/visual
unsafe fn make_swap(dev: &ID3D11Device, hwnd: HWND, w: u32, h: u32) -> windows::core::Result<(IDXGISwapChain1, HANDLE, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual)> {
    let dxgi: IDXGIDevice = dev.cast()?;
    let adapter = dxgi.GetAdapter()?;
    let factory: IDXGIFactory2 = adapter.GetParent()?;
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: w.max(1),
        Height: h.max(1),
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
        Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
    };
    let swap = factory.CreateSwapChainForComposition(dev, &desc, None)?;
    let sc2: IDXGISwapChain2 = swap.cast()?;
    sc2.SetMaximumFrameLatency(1)?;
    let wait = sc2.GetFrameLatencyWaitableObject();
    let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
    let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
    let visual = dcomp.CreateVisual()?;
    visual.SetContent(&swap)?;
    target.SetRoot(&visual)?;
    dcomp.Commit()?;
    Ok((swap, wait, dcomp, target, visual))
}

fn refresh_ms() -> u32 {
    use windows::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};
    let mut ti = DWM_TIMING_INFO { cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
    let hz = unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut ti) }
        .ok()
        .and_then(|_| (ti.rateRefresh.uiDenominator > 0).then(|| ti.rateRefresh.uiNumerator as f32 / ti.rateRefresh.uiDenominator as f32))
        .filter(|hz| *hz > 20.0)
        .unwrap_or(60.0);
    ((1000.0 / hz).floor() as u32).max(2)
}

pub fn active() -> bool {
    GPU.with(|g| g.borrow().is_some())
}

fn with<R>(f: impl FnOnce(&mut Gpu) -> R) -> Option<R> {
    GPU.with(|g| g.borrow_mut().as_mut().map(f))
}

impl Gpu {
    unsafe fn make_bb(&mut self) -> windows::core::Result<()> {
        let b: ID3D11Texture2D = self.swap.GetBuffer(0)?;
        let mut rtv = None;
        self.dev.CreateRenderTargetView(&b, None, Some(&mut rtv))?;
        self.bb = rtv;
        Ok(())
    }

    fn tex(&self, w: u32, h: u32, fmt: DXGI_FORMAT) -> Option<Tex> {
        let d = D3D11_TEXTURE2D_DESC {
            Width: w.max(1),
            Height: h.max(1),
            MipLevels: 1,
            ArraySize: 1,
            Format: fmt,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        unsafe {
            let mut t = None;
            log_err("CreateTexture2D", self.dev.CreateTexture2D(&d, None, Some(&mut t)))?;
            let t = t?;
            let mut srv = None;
            log_err("CreateSRV", self.dev.CreateShaderResourceView(&t, None, Some(&mut srv)))?;
            let mut rtv = None;
            log_err("CreateRTV", self.dev.CreateRenderTargetView(&t, None, Some(&mut rtv)))?;
            if let Some(r) = &rtv {
                self.ctx.ClearRenderTargetView(r, &[0.0; 4]);
            }
            Some(Tex { t, srv, rtv, w: w.max(1), h: h.max(1) })
        }
    }

    /// 帧尺寸变了：重建帧尺寸的纹理。返回是否重建过（调用方整帧上传）
    fn ensure(&mut self, w: u32, h: u32) -> bool {
        if self.fw == w && self.fh == h && self.cpu.is_some() {
            return false;
        }
        self.fw = w;
        self.fh = h;
        let f = DXGI_FORMAT_B8G8R8A8_UNORM;
        self.cpu = self.tex(w, h, f);
        self.base = self.tex(w, h, f);
        self.acc = [self.tex(w, h, f), self.tex(w, h, f)];
        self.disp = None;
        self.under5 = None;
        self.has_under5 = false;
        self.has_base = false;
        self.small.clear();
        self.layers.clear();
        self.bs.clear();
        self.staging = None;
        self.frame_gpu = false;
        true
    }

    fn upload(&self, t: &Tex, src: &[Bgra], w: usize, y0: usize, y1: usize) {
        let y1 = y1.min(t.h as usize).min(src.len() / w.max(1));
        if y1 <= y0 || w as u32 != t.w {
            return;
        }
        let b = D3D11_BOX { left: 0, top: y0 as u32, front: 0, right: w as u32, bottom: y1 as u32, back: 1 };
        unsafe { self.ctx.UpdateSubresource(&t.t, 0, Some(&b), src[y0 * w..].as_ptr() as *const _, (w * 4) as u32, 0) };
    }

    fn draw(&self, ps: &ID3D11PixelShader, rt: &Tex, srvs: [Option<&Tex>; 2], cb: &[f32; 24], over: bool, scissor: Option<RECT>) {
        self.draw_rtv(ps, rt.rtv.as_ref(), rt.w, rt.h, [srvs[0].and_then(|t| t.srv.clone()), srvs[1].and_then(|t| t.srv.clone())], cb, over, scissor);
    }

    fn draw_rtv(&self, ps: &ID3D11PixelShader, rtv: Option<&ID3D11RenderTargetView>, w: u32, h: u32, srvs: [Option<ID3D11ShaderResourceView>; 2], cb: &[f32; 24], over: bool, scissor: Option<RECT>) {
        self.draw_with(ps, rtv, w, h, srvs, &self.cb, cb, over, scissor);
    }

    fn draw_with(&self, ps: &ID3D11PixelShader, rtv: Option<&ID3D11RenderTargetView>, w: u32, h: u32, srvs: [Option<ID3D11ShaderResourceView>; 2], buf: &ID3D11Buffer, cb: &[f32], over: bool, scissor: Option<RECT>) {
        let Some(rtv) = rtv else { return };
        unsafe {
            let c = &self.ctx;
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            if c.Map(buf, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).is_ok() {
                std::ptr::copy_nonoverlapping(cb.as_ptr(), m.pData as *mut f32, cb.len().min(32));
                c.Unmap(buf, 0);
            }
            c.PSSetShaderResources(0, Some(&[None, None]));
            c.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            c.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: w as f32, Height: h as f32, MinDepth: 0.0, MaxDepth: 1.0 }]));
            c.RSSetState(&self.rs);
            let full = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
            c.RSSetScissorRects(Some(&[scissor.unwrap_or(full)]));
            c.OMSetBlendState(if over { &self.b_over } else { &self.b_none }, None, 0xffff_ffff);
            c.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            c.VSSetShader(&self.sh_.vs, None);
            c.PSSetShader(ps, None);
            c.PSSetConstantBuffers(0, Some(&[Some(buf.clone())]));
            c.PSSetSamplers(0, Some(&[Some(self.s_lin.clone()), Some(self.s_bord.clone())]));
            c.PSSetShaderResources(0, Some(&srvs));
            c.Draw(3, 0);
            c.PSSetShaderResources(0, Some(&[None, None]));
        }
    }

    fn small_pair(&mut self, f: u32) -> bool {
        let (sw, sh) = (self.fw.div_ceil(f), self.fh.div_ceil(f));
        if self.small.get(&f).is_some_and(|p| p[0].w == sw && p[0].h == sh) {
            return true;
        }
        let fmt = DXGI_FORMAT_R16G16B16A16_FLOAT;
        match (self.tex(sw, sh, fmt), self.tex(sw, sh, fmt)) {
            (Some(a), Some(b)) => {
                self.small.insert(f, [a, b]);
                true
            }
            _ => false,
        }
    }

    /// src（帧尺寸）→ 降采样 f → 三次盒式模糊（与 compose::blur_clamped 同一套盒子）。返回 (f, 结果在 small[f][i])
    fn blur(&mut self, src: &Tex, sigma: f32) -> Option<(u32, usize)> {
        let f = blur_factor(sigma);
        if !self.small_pair(f) {
            return None;
        }
        let pair = self.small.get(&f)?;
        let (sw, sh) = (pair[0].w as f32, pair[0].h as f32);
        // 布局：rect 0..4 | p0 4..8 | p1 8..12 | p2 12..16 | p3 16..20 | p4 20..24
        let mut cb = [0f32; 24];
        cb[10] = f as f32;
        cb[16] = self.fw as f32;
        cb[17] = self.fh as f32;
        cb[18] = sw;
        cb[19] = sh;
        self.draw(&self.sh_.down, &pair[0], [Some(src), None], &cb, false, None);
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
                self.draw(&self.sh_.boxp, &pair[1 - i], [Some(&pair[i]), None], &cb, false, None);
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
        self.draw(&self.sh_.mix, b, [Some(a), Some(filt)], &cb, false, None);
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

// ------------------------------------------------------------------ compose API（compose.rs 调用）

/// 帧开始：确保帧尺寸的纹理
pub fn frame_begin(w: usize, h: usize) -> bool {
    with(|g| {
        g.ensure(w as u32, h as u32);
        g.acc[0].is_some() && g.acc[1].is_some() && g.base.is_some()
    })
    .unwrap_or(false)
}

/// 基底：整幅上传，并作为累积缓冲的起点
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

/// 累积缓冲从基底（或层 5 冻结时存的「层 5 底下的全部」）开始
pub fn start(from_under5: bool) {
    with(|g| {
        g.cur = 0;
        let src = if from_under5 { g.under5.as_ref() } else { g.base.as_ref() };
        if let (Some(s), Some(a)) = (src, g.acc[0].as_ref()) {
            unsafe { g.ctx.CopyResource(&a.t, &s.t) };
        }
    });
}

pub fn has_under5() -> bool {
    with(|g| g.has_under5).unwrap_or(false)
}

pub fn save_under5() {
    with(|g| {
        if g.under5.is_none() {
            g.under5 = g.tex(g.fw, g.fh, DXGI_FORMAT_B8G8R8A8_UNORM);
        }
        if let (Some(u), Some(a)) = (g.under5.as_ref(), g.acc[g.cur].as_ref()) {
            unsafe { g.ctx.CopyResource(&u.t, &a.t) };
            g.has_under5 = true;
        }
    });
}

pub fn drop_under5() {
    with(|g| {
        g.has_under5 = false;
    });
}

/// 层 `id` 的内容（整窗大小的 CPU 缓冲）上传：只传 rows（源坐标的行范围）与上次 rows 的并集
pub fn upload_layer(id: i32, tmp: &[Bgra], w: usize, rows: (usize, usize)) {
    with(|g| {
        let (fw, fh) = (g.fw, g.fh);
        let fresh = !g.layers.get(&id).is_some_and(|(t, _)| t.w == fw && t.h == fh);
        if fresh {
            let Some(t) = g.tex(fw, fh, DXGI_FORMAT_B8G8R8A8_UNORM) else { return };
            g.layers.insert(id, (t, (0, fh)));
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

/// liquid / 折射层的背景滤镜：作用在当前累积结果上
pub fn backdrop_liquid(rect: [f32; 4], radius: f32, sigma: f32, sat: f32, depth: f32, bevel: f32, op: f32) {
    with(|g| {
        if op <= 0.001 {
            return;
        }
        let r = [rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3])];
        let fmt = DXGI_FORMAT_B8G8R8A8_UNORM;
        let src_is_disp = depth > 0.0;
        if src_is_disp {
            if g.disp.is_none() {
                g.disp = g.tex(g.fw, g.fh, fmt);
            }
            let mut cb = [0f32; 24];
            cb[..4].copy_from_slice(&r);
            cb[4] = radius;
            cb[7] = depth;
            cb[8] = bevel;
            cb[16] = g.fw as f32;
            cb[17] = g.fh as f32;
            if let (Some(d), Some(a)) = (g.disp.as_ref(), g.acc[g.cur].as_ref()) {
                g.draw(&g.sh_.disp, d, [Some(a), None], &cb, false, None);
            }
        }
        // 取样源：位移结果或当前累积（借出来，避免和 &mut g 冲突）
        let src = if src_is_disp { g.disp.take() } else { g.acc[g.cur].take() };
        let Some(src) = src else { return };
        if sigma >= 0.3 {
            if let Some((f, i)) = g.blur(&src, sigma) {
                if src_is_disp { g.disp = Some(src) } else { g.acc[g.cur] = Some(src) }
                let pair = g.small.remove(&f).unwrap();
                g.mix(&pair[i], f, r, radius, sat, op);
                g.small.insert(f, pair);
                return;
            }
            if src_is_disp { g.disp = Some(src) } else { g.acc[g.cur] = Some(src) }
            return;
        }
        if src_is_disp {
            g.mix(&src, 1, r, radius, sat, op);
            g.disp = Some(src);
        } else {
            // 无位移无模糊：只有饱和度
            let copy = g.tex(g.fw, g.fh, fmt);
            g.acc[g.cur] = Some(src);
            if let (Some(c), Some(a)) = (copy.as_ref(), g.acc[g.cur].as_ref()) {
                unsafe { g.ctx.CopyResource(&c.t, &a.t) };
                g.mix(c, 1, r, radius, sat, op);
            }
        }
    });
}

/// 纯模糊层：`recompute` 时按当前累积结果重算模糊并缓存（与姿态无关），然后按当前矩形贴上
pub fn backdrop_blur(id: i32, recompute: bool, rect: [f32; 4], radius: f32, sigma: f32, sat: f32, op: f32) {
    with(|g| {
        let need = recompute || !g.bs.contains_key(&id);
        if need {
            let Some(src) = g.acc[g.cur].take() else { return };
            let r = g.blur(&src, sigma);
            g.acc[g.cur] = Some(src);
            let Some((f, i)) = r else { return };
            let (w, h) = { let p = &g.small[&f]; (p[0].w, p[0].h) };
            let ok = g.bs.get(&id).is_some_and(|b| b.f == f && b.tex.w == w && b.tex.h == h);
            if !ok {
                let Some(t) = g.tex(w, h, DXGI_FORMAT_R16G16B16A16_FLOAT) else { return };
                g.bs.insert(id, BsTex { tex: t, f });
            }
            let p = &g.small[&f];
            unsafe { g.ctx.CopyResource(&g.bs[&id].tex.t, &p[i].t) };
        }
        if op <= 0.001 {
            return;
        }
        let r = [rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3])];
        let b = g.bs.remove(&id).unwrap();
        g.mix(&b.tex, b.f, r, radius, sat, op);
        g.bs.insert(id, b);
    });
}

pub fn has_bs(id: i32) -> bool {
    with(|g| g.bs.contains_key(&id)).unwrap_or(false)
}

/// 层关闭：丢掉它的纹理
pub fn retain(shown: &[i32], active: &[i32]) {
    with(|g| {
        g.bs.retain(|id, _| shown.contains(id));
        g.layers.retain(|id, _| active.contains(id));
    });
}

/// 层内容按姿态叠到累积结果上（clip：输出像素的矩形，与 CPU 版的包围盒裁剪一致）
pub fn over_layer(id: i32, ox: f32, oy: f32, s: f32, tx: f32, ty: f32, op: f32, clip: (usize, usize, usize, usize)) {
    with(|g| {
        if s <= 0.001 || op <= 0.001 || clip.2 <= clip.0 || clip.3 <= clip.1 {
            return;
        }
        let mut k = [0f32; 24];
        k[5] = op.clamp(0.0, 1.0); // p0.y
        k[9] = s; // p1.y
        k[12..16].copy_from_slice(&[ox, oy, tx, ty]); // p2
        k[16] = g.fw as f32; // p3.xy
        k[17] = g.fh as f32;
        let Some((t, _)) = g.layers.get(&id) else { return };
        let Some(a) = g.acc[g.cur].as_ref() else { return };
        let sc = RECT { left: clip.0 as i32, top: clip.1 as i32, right: clip.2 as i32, bottom: clip.3 as i32 };
        g.draw(&g.sh_.over, a, [Some(t), None], &k, true, Some(sc));
    });
}

/// 合成结束：累积结果就是这一帧
pub fn end_compose() {
    with(|g| g.frame_gpu = true);
}

pub fn frame_gpu() -> bool {
    with(|g| g.frame_gpu).unwrap_or(false)
}

// ------------------------------------------------------------------ present API（win.rs 调用）

/// CPU 渲染的帧：只上传 y0..y1 这几行（纹理新建过就整幅）
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

pub fn set_opacity(a: f32) {
    with(|g| g.opacity = a.clamp(0.0, 1.0));
}

pub fn set_settle(s: f32) {
    with(|g| g.settle = s);
}

/// 液态玻璃底（窗口尺寸，预乘）；None = 去掉
pub fn set_lq(base: Option<(&[Bgra], usize, usize)>) {
    drop_panel_glass();
    with(|g| match base {
        Some((px, w, h)) if px.len() == w * h && w > 0 => {
            if !g.lq.as_ref().is_some_and(|t| t.w == w as u32 && t.h == h as u32) {
                g.lq = g.tex(w as u32, h as u32, DXGI_FORMAT_B8G8R8A8_UNORM);
            }
            if let Some(t) = g.lq.as_ref() {
                g.upload(t, px, w, 0, h);
            }
        }
        _ => g.lq = None,
    });
}

/// 交换链跟着窗口尺寸
pub fn resize(w: u32, h: u32) {
    with(|g| unsafe {
        let (w, h) = (w.max(1), h.max(1));
        if g.sw == w && g.sh == h {
            return;
        }
        g.bb = None;
        g.ctx.OMSetRenderTargets(None, None);
        g.ctx.ClearState();
        g.ctx.Flush();
        if let Err(e) = g.swap.ResizeBuffers(2, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT) {
            log::warn!("gpu: ResizeBuffers {w}x{h} failed: {e}");
            return;
        }
        g.sw = w;
        g.sh = h;
        if let Err(e) = g.make_bb() {
            log::warn!("gpu: backbuffer after resize failed: {e}");
        }
    });
}

thread_local! {
    static PRESENT_MS: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

/// 呈现当前帧（GPU 合成结果或 CPU 帧）× 透明度，叠液态玻璃底
pub fn present() {
    let t = Instant::now();
    with(|g| unsafe {
        if g.pending_wait {
            WaitForSingleObjectEx(g.wait, 50, false);
            g.pending_wait = false;
        }
        let src = if g.frame_gpu { g.acc[g.cur].as_ref() } else { g.cpu.as_ref() };
        let mut cb = [0f32; 24];
        cb[5] = g.opacity;
        cb[9] = g.settle.max(0.01);
        cb[11] = if g.lq.is_some() { 1.0 } else { 0.0 };
        cb[12] = g.fw as f32 / 2.0;
        cb[13] = g.fh as f32 / 2.0;
        cb[16] = g.fw.max(1) as f32;
        cb[17] = g.fh.max(1) as f32;
        let Some(bb) = g.bb.clone() else { return };
        match src {
            Some(s) if g.opacity > 0.0 => {
                let srvs = [s.srv.clone(), g.lq.as_ref().and_then(|t| t.srv.clone())];
                g.draw_rtv(&g.sh_.present, Some(&bb), g.sw, g.sh, srvs, &cb, false, None);
            }
            _ => g.ctx.ClearRenderTargetView(&bb, &[0.0; 4]),
        }
        let hr = g.swap.Present(1, DXGI_PRESENT(0));
        if hr.is_err() {
            log::warn!("gpu: Present failed: {hr:?}");
        }
        g.pending_wait = true;
    });
    PRESENT_MS.with(|p| p.set(t.elapsed().as_secs_f32() * 1000.0));
}

/// 上一次 present 的 CPU 耗时（帧统计用）
pub fn last_present_ms() -> f32 {
    PRESENT_MS.with(|p| p.get())
}

/// 动画中：上一帧呈现过就等交换链「可以画下一帧」（跟 vsync 对齐）
pub fn wait_handle() -> Option<HANDLE> {
    with(|g| g.pending_wait.then_some(g.wait)).flatten()
}

pub fn waited() {
    with(|g| g.pending_wait = false);
}

pub fn frame_ms() -> u32 {
    with(|g| g.frame_ms).unwrap_or(16)
}

/// 当前显示的帧读回 CPU（变形动画要面板的快照）
pub fn readback() -> Option<(Vec<Bgra>, usize, usize)> {
    with(|g| unsafe {
        let src = if g.frame_gpu { g.acc[g.cur].as_ref() } else { g.cpu.as_ref() }?;
        let (w, h) = (src.w as usize, src.h as usize);
        if g.staging.is_none() {
            let d = D3D11_TEXTURE2D_DESC {
                Width: src.w,
                Height: src.h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut t = None;
            log_err("staging", g.dev.CreateTexture2D(&d, None, Some(&mut t)))?;
            g.staging = t;
        }
        let st = g.staging.as_ref()?;
        g.ctx.CopyResource(st, &src.t);
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        log_err("Map staging", g.ctx.Map(st, 0, D3D11_MAP_READ, 0, Some(&mut m)))?;
        let mut out = vec![Bgra::default(); w * h];
        for y in 0..h {
            let p = (m.pData as *const u8).add(y * m.RowPitch as usize) as *const Bgra;
            std::ptr::copy_nonoverlapping(p, out[y * w..].as_mut_ptr(), w);
        }
        g.ctx.Unmap(st, 0);
        Some((out, w, h))
    })
    .flatten()
}

// ------------------------------------------------------------------ 其它窗口（变形窗口等，和面板共用设备）

/// 和面板共用 D3D 设备的另一个 DComp 交换链（窗口要带 WS_EX_NOREDIRECTIONBITMAP）
pub struct Surface {
    swap: IDXGISwapChain1,
    wait: HANDLE,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    bb: Option<ID3D11RenderTargetView>,
    pub w: u32,
    pub h: u32,
}

impl Surface {
    pub fn new(hwnd: HWND, w: u32, h: u32) -> Option<Surface> {
        with(|g| unsafe {
            let r = (|| -> windows::core::Result<Surface> {
                let (swap, wait, d, t, v) = make_swap(&g.dev, hwnd, w, h)?;
                let mut s = Surface { swap, wait, _dcomp: d, _target: t, _visual: v, bb: None, w: w.max(1), h: h.max(1) };
                s.make_bb(&g.dev)?;
                Ok(s)
            })();
            log_err("surface", r)
        })
        .flatten()
    }

    unsafe fn make_bb(&mut self, dev: &ID3D11Device) -> windows::core::Result<()> {
        let b: ID3D11Texture2D = self.swap.GetBuffer(0)?;
        let mut rtv = None;
        dev.CreateRenderTargetView(&b, None, Some(&mut rtv))?;
        self.bb = rtv;
        Ok(())
    }

    pub fn resize(&mut self, w: u32, h: u32) -> bool {
        let (w, h) = (w.max(1), h.max(1));
        if self.w == w && self.h == h && self.bb.is_some() {
            return true;
        }
        with(|g| unsafe {
            self.bb = None;
            g.ctx.OMSetRenderTargets(None, None);
            g.ctx.ClearState();
            g.ctx.Flush();
            if let Err(e) = self.swap.ResizeBuffers(2, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT) {
                log::warn!("gpu: surface ResizeBuffers {w}x{h} failed: {e}");
                return false;
            }
            self.w = w;
            self.h = h;
            log_err("surface backbuffer", self.make_bb(&g.dev)).is_some()
        })
        .unwrap_or(false)
    }

    /// 交换链的 frame-latency 句柄：有信号 = 可以画下一帧（跟 vsync / DWM 合成节拍）
    pub fn handle(&self) -> HANDLE {
        self.wait
    }

    pub fn wait(&self, ms: u32) -> bool {
        unsafe { WaitForSingleObjectEx(self.wait, ms, false) == WAIT_OBJECT_0 }
    }

    pub fn clear(&self) {
        with(|g| unsafe {
            if let Some(bb) = &self.bb {
                g.ctx.ClearRenderTargetView(bb, &[0.0; 4]);
            }
        });
    }

    pub fn present(&self) {
        unsafe {
            let hr = self.swap.Present(1, DXGI_PRESENT(0));
            if hr.is_err() {
                log::warn!("gpu: surface Present failed: {hr:?}");
            }
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.wait);
        }
    }
}

/// 背后屏幕的模糊图（GPU 上）：半分辨率、三次盒式模糊（= morph::blur_half），`org` 是全分辨率左上角的屏幕坐标
pub struct Glass {
    small: Tex,
    pub org: (i32, i32),
    pub w: u32,
    pub h: u32,
}

/// 截到的屏幕像素（不透明 BGRA）上传并在 GPU 上模糊。`radius`：全分辨率半径（同 blur_half）
pub fn glass_from_pixels(px: &[Bgra], w: usize, h: usize, org: (i32, i32), radius: f64) -> Option<std::rc::Rc<Glass>> {
    if w == 0 || h == 0 || px.len() != w * h {
        return None;
    }
    with(|g| {
        let g: &Gpu = g;
        let full = g.tex(w as u32, h as u32, DXGI_FORMAT_B8G8R8A8_UNORM)?;
        g.upload(&full, px, w, 0, h);
        let (sw, sh) = (w.div_ceil(2) as u32, h.div_ceil(2) as u32);
        let fmt = DXGI_FORMAT_R16G16B16A16_FLOAT;
        let pair = [g.tex(sw, sh, fmt)?, g.tex(sw, sh, fmt)?];
        let mut cb = [0f32; 24];
        cb[10] = 2.0;
        cb[16] = w as f32;
        cb[17] = h as f32;
        cb[18] = sw as f32;
        cb[19] = sh as f32;
        g.draw(&g.sh_.down, &pair[0], [Some(&full), None], &cb, false, None);
        let rs = (radius / 2.0).round().max(1.0) as f32;
        let mut i = 0usize;
        for _ in 0..3 {
            for dir in [(1.0, 0.0), (0.0, 1.0)] {
                cb[20] = rs;
                cb[21] = dir.0;
                cb[22] = dir.1;
                g.draw(&g.sh_.boxp, &pair[1 - i], [Some(&pair[i]), None], &cb, false, None);
                i = 1 - i;
            }
        }
        let [a, b] = pair;
        let small = if i == 0 { a } else { b };
        Some(std::rc::Rc::new(Glass { small, org, w: w as u32, h: h as u32 }))
    })
    .flatten()
}

/// GPU 上的一张图（面板快照）
pub struct GTex(Tex);

impl GTex {
    pub fn size(&self) -> (usize, usize) {
        (self.0.w as usize, self.0.h as usize)
    }
}

/// 面板当前显示的帧（GPU 合成结果或上传的 CPU 帧）复制一份：变形动画用，不读回 CPU
pub fn snapshot() -> Option<GTex> {
    with(|g| {
        let g: &Gpu = g;
        let src = if g.frame_gpu { g.acc[g.cur].as_ref() } else { g.cpu.as_ref() }?;
        let t = g.tex(src.w, src.h, DXGI_FORMAT_B8G8R8A8_UNORM)?;
        unsafe { g.ctx.CopyResource(&t.t, &src.t) };
        Some(GTex(t))
    })
    .flatten()
}

/// 变形一帧的输入
pub struct MorphDraw<'a> {
    pub frame: &'a crate::morph::Frame,
    pub look: &'a crate::morph::Look,
    /// 目标左上角的屏幕坐标（玻璃按屏幕坐标取样）
    pub org: (f64, f64),
    pub glass: Option<&'a Glass>,
    /// 内容和卡片在内容图里的左上角（拉伸模式）；None = 不叠内容
    pub content: Option<(&'a GTex, (f64, f64))>,
    /// 整体透明度（淡出）
    pub alpha: f32,
}

/// 与 morph::render_impl 同一套参数（Windows：stretch 模式，没有 Rest）
fn morph_cb(d: &MorphDraw) -> [f32; 32] {
    let f = d.frame;
    let look = d.look;
    let sc = look.scale;
    let r = f.rect;
    let bevel = (14.0 * sc).min((r[2] - r[0]).min(r[3] - r[1]) / 2.0).max(1.0);
    let g = f.grow.clamp(0.0, 1.0);
    let k_acc = (1.0 - g * 2.2).clamp(0.0, 1.0);
    let (tint, ta) = match (look.dark, d.glass.is_some()) {
        (true, true) => ((0.10, 0.10, 0.12), 0.40),
        (false, true) => ((0.98, 0.98, 0.99), 0.46),
        (true, false) => ((0.13, 0.13, 0.15), 0.90),
        (false, false) => ((0.97, 0.97, 0.98), 0.92),
    };
    let mix = |a: f64, b: f64| a * (1.0 - k_acc) + b * k_acc;
    let body = [mix(tint.0, look.accent.0), mix(tint.1, look.accent.1), mix(tint.2, look.accent.2), mix(ta, 0.85)];
    let rim_a = if look.dark { 0.30 } else { 0.75 } * g.max(0.25);
    let sheen_a = if look.dark { 0.10 } else { 0.30 } * (1.0 - f.content * 0.7) * g;
    let shadow_a = 0.20 * g.max(0.25);
    let mut k = [0f32; 32];
    k[0..4].copy_from_slice(&[r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32]);
    k[4..8].copy_from_slice(&[f.radius as f32, (14.0 * sc) as f32, (2.0 * sc) as f32, bevel as f32]);
    let c_op = if d.content.is_some() { f.content.clamp(0.0, 1.0) } else { 0.0 };
    k[8..12].copy_from_slice(&[(18.0 * sc) as f32, (1.2 * sc) as f32, c_op as f32, d.alpha.clamp(0.0, 1.0)]);
    k[12..16].copy_from_slice(&[body[0] as f32, body[1] as f32, body[2] as f32, body[3] as f32]);
    k[16..20].copy_from_slice(&[rim_a as f32, sheen_a as f32, shadow_a as f32, if d.glass.is_some() { 1.0 } else { 0.0 }]);
    if let Some(gl) = d.glass {
        let (sw, sh) = (gl.small.w as f64, gl.small.h as f64);
        k[20..24].copy_from_slice(&[(d.org.0 - gl.org.0 as f64) as f32, (d.org.1 - gl.org.1 as f64) as f32, (1.0 / (2.0 * sw)) as f32, (1.0 / (2.0 * sh)) as f32]);
    }
    if let Some((c, at)) = d.content {
        let (pw, ph) = (c.0.w as f64, c.0.h as f64);
        let (cw, ch) = ((pw - 2.0 * at.0).max(1.0), (ph - 2.0 * at.1).max(1.0));
        let (kx, ky) = (cw / (r[2] - r[0]).max(1e-3), ch / (r[3] - r[1]).max(1e-3));
        k[24..28].copy_from_slice(&[at.0 as f32, at.1 as f32, kx as f32, ky as f32]);
        k[28..32].copy_from_slice(&[(1.0 / pw) as f32, (1.0 / ph) as f32, 0.0, 0.0]);
    }
    k
}

/// 画一帧变形到 surface（整幅覆盖）
pub fn draw_morph(surf: &Surface, d: &MorphDraw) {
    with(|g| {
        let g: &Gpu = g;
        let cb = morph_cb(d);
        let srvs = [d.glass.and_then(|x| x.small.srv.clone()), d.content.and_then(|(c, _)| c.0.srv.clone())];
        g.draw_with(&g.sh_.morph, surf.bb.as_ref(), surf.w, surf.h, srvs, &g.cb2, &cb, false, None);
    });
}

thread_local! {
    /// 面板液态玻璃底用的那张模糊图（收起动画直接接着用）
    static PANEL_GLASS: RefCell<Option<std::rc::Rc<Glass>>> = const { RefCell::new(None) };
}

/// 面板的液态玻璃底在 GPU 上画（= win.rs set_liquid 的 CPU 版：morph::render，形状 = 整个面板，无内容）。
/// `org`：面板左上角屏幕坐标。glass 留着给收起动画。
pub fn set_lq_glass(glass: std::rc::Rc<Glass>, org: (i32, i32), w: usize, h: usize, look: &crate::morph::Look, frame: &crate::morph::Frame) -> bool {
    let ok = with(|g| {
        if !g.lq.as_ref().is_some_and(|t| t.w == w as u32 && t.h == h as u32) {
            g.lq = g.tex(w as u32, h as u32, DXGI_FORMAT_B8G8R8A8_UNORM);
        }
        let g: &Gpu = g;
        let Some(t) = g.lq.as_ref() else { return false };
        let d = MorphDraw { frame, look, org: (org.0 as f64, org.1 as f64), glass: Some(&glass), content: None, alpha: 1.0 };
        let cb = morph_cb(&d);
        g.draw_with(&g.sh_.morph, t.rtv.as_ref(), t.w, t.h, [glass.small.srv.clone(), None], &g.cb2, &cb, false, None);
        true
    })
    .unwrap_or(false);
    PANEL_GLASS.with(|p| *p.borrow_mut() = ok.then_some(glass));
    ok
}

pub fn panel_glass() -> Option<std::rc::Rc<Glass>> {
    PANEL_GLASS.with(|p| p.borrow().clone())
}

pub fn drop_panel_glass() {
    PANEL_GLASS.with(|p| *p.borrow_mut() = None);
}

/// 硬件设备（不是 WARP）
pub fn hardware() -> bool {
    with(|g| !g.warp).unwrap_or(false)
}
