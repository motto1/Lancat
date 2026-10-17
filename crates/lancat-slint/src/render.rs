//! 渲染方式（GPU / CPU）—— 两个平台共用：设置（Settings.render_mode）、环境变量覆盖、自动回退的状态、
//! 设置页上的说明文字，以及运行中切换。
//! - GPU：Windows = D3D11 + DirectComposition（gpu.rs），Linux/Wayland = EGL + GLES 3（gl.rs）。
//! - CPU：原来的软件路径（Slint SoftwareRenderer + CPU 合成，Windows 走 GDI / UpdateLayeredWindow，Wayland 走 wl_shm）。
//! - `LANCAT_CPU_COMPOSE=1` 两个平台都强制 CPU（优先于设置）。GPU 初始化失败自动用 CPU，设置页显示出来。

use std::sync::atomic::{AtomicBool, Ordering};

static PREF_CPU: AtomicBool = AtomicBool::new(false);
static FAILED: AtomicBool = AtomicBool::new(false);

/// 环境变量强制 CPU
pub fn env_forced_cpu() -> bool {
    std::env::var("LANCAT_CPU_COMPOSE").as_deref() == Ok("1")
}

/// 设置里选了 CPU
pub fn pref_cpu() -> bool {
    PREF_CPU.load(Ordering::Relaxed)
}

/// 想用 GPU（没被环境变量 / 设置关掉）
pub fn wanted() -> bool {
    !env_forced_cpu() && !pref_cpu()
}

/// GPU 初始化失败（设备 / 上下文 / 着色器 / 交换链）：自动退回 CPU
pub fn mark_failed() {
    FAILED.store(true, Ordering::Relaxed);
}

pub fn failed() -> bool {
    FAILED.load(Ordering::Relaxed)
}

pub fn clear_failed() {
    FAILED.store(false, Ordering::Relaxed);
}

fn is_cpu(mode: &str) -> bool {
    mode == "cpu"
}

/// 启动时（建窗口之前）按设置定下来
pub fn init(settings: &lancat_core::Settings) {
    let cpu = is_cpu(&settings.render_mode);
    PREF_CPU.store(cpu, Ordering::Relaxed);
    #[cfg(windows)]
    crate::gpu::set_pref_cpu(cpu);
    log::info!("render mode: setting {:?}{}", settings.render_mode, if env_forced_cpu() { " (LANCAT_CPU_COMPOSE=1: CPU)" } else { "" });
}

/// 这个平台能不能当场切换（不能就是「重启后生效」）
pub fn live() -> bool {
    #[cfg(windows)]
    return true;
    #[cfg(target_os = "linux")]
    return crate::wl::active();
    #[allow(unreachable_code)]
    false
}

/// 设置页换了渲染方式
pub fn apply(mode: &str) {
    let cpu = is_cpu(mode);
    let was = PREF_CPU.swap(cpu, Ordering::Relaxed);
    if was == cpu {
        return;
    }
    if !cpu {
        clear_failed();
    }
    #[cfg(windows)]
    crate::win::apply_render_mode(cpu);
    #[cfg(target_os = "linux")]
    if crate::wl::active() {
        crate::wl::apply_render_mode(cpu);
    }
}

/// 实际状态："gpu" | "warp"（只用软件 D3D 呈现）| "pending"（还在初始化）| "cpu"（设置）| "cpu-env" | "failed" | "none"（这个后端没有 GPU 路径）
pub fn status() -> &'static str {
    if env_forced_cpu() {
        return "cpu-env";
    }
    #[cfg(windows)]
    let s = crate::gpu::status();
    #[cfg(target_os = "linux")]
    let s = if crate::wl::active() { crate::gl::status() } else { "none" };
    #[cfg(not(any(windows, target_os = "linux")))]
    let s = "none";
    s
}

/// 设置页「渲染方式」下面的一行说明（空 = 不显示）
pub fn note() -> String {
    let st = status();
    let want_gpu = !pref_cpu();
    let s = match st {
        "cpu-env" => "环境变量 LANCAT_CPU_COMPOSE=1 强制使用 CPU 渲染",
        "failed" if want_gpu => "GPU 不可用，已自动改用 CPU 渲染",
        "warp" if want_gpu => "没有可用的显卡（软件 D3D），合成在 CPU 上做",
        "pending" if want_gpu => "正在初始化 GPU…",
        "none" => "当前图形后端不支持 GPU 渲染，使用 CPU",
        "gpu" if !want_gpu || !live() => "重启后生效",
        "cpu" if want_gpu && !live() => "重启后生效",
        "gpu" => "当前：GPU",
        "cpu" => "当前：CPU",
        _ => "",
    };
    s.to_string()
}
