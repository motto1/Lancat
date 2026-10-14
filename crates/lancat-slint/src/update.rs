//! 通过 GitHub Releases 检查和安装新版本。
//!
//! - 检查：`GET https://api.github.com/repos/motto1/Lancat/releases/latest`，按标签（`v0.2.0`）比版本号。
//! - 安装：下载本平台的单文件（`lancat-slint-windows-x64.exe` / `lancat-slint-linux-x64`）到程序所在目录，
//!   按发布里的 `SHA256SUMS` 校验，然后替换正在运行的程序（Windows：先把自己改名为 `.old`，运行中的 exe
//!   允许改名；Linux：直接 rename 覆盖），再用 `--wait-pid <旧进程>` 启动新版并退出。新版等旧进程退出
//!   （释放单实例锁和端口）后才继续启动，并删掉 `.old`。
//! - 网络请求交给系统自带的 curl（Windows 10 1803+ 自带 curl.exe，走系统证书和代理设置），不往程序里塞 TLS 库。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 当前版本：云端编译时由标签注入（`LANCAT_VERSION=0.2.0`），本地编译用 Cargo.toml 的版本。
pub const VERSION: &str = match option_env!("LANCAT_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

pub const REPO: &str = "motto1/Lancat";

#[cfg(windows)]
const ASSET: &str = "lancat-slint-windows-x64.exe";
#[cfg(not(windows))]
const ASSET: &str = "lancat-slint-linux-x64";

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Release {
    /// 不带 v 的版本号
    pub version: String,
    pub notes: String,
    pub page: String,
    url: String,
    sums_url: Option<String>,
}

/// "v1.2.3" / "1.2.3-beta" → (1, 2, 3)；预发布后缀忽略。
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

pub fn is_newer(remote: &str, local: &str) -> bool {
    match (parse(remote), parse(local)) {
        (Some(r), Some(l)) => r > l,
        _ => false,
    }
}

fn curl() -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("curl.exe");
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    }
    #[cfg(not(windows))]
    Command::new("curl")
}

fn run(mut c: Command) -> Result<Vec<u8>, String> {
    let out = c.output().map_err(|e| format!("无法运行 curl：{e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        return Err(if err.is_empty() { format!("网络请求失败（curl 退出码 {:?}）", out.status.code()) } else { format!("网络请求失败：{err}") });
    }
    Ok(out.stdout)
}

fn get(url: &str) -> Result<Vec<u8>, String> {
    let mut c = curl();
    c.args(["-fsSL", "--connect-timeout", "15", "--max-time", "60", "-H", "Accept: application/vnd.github+json"]);
    c.args(["-H", &format!("User-Agent: Lancat/{VERSION}")]);
    c.arg(url);
    run(c)
}

/// 查最新发布。`Ok(None)` = 已是最新。阻塞，放到后台线程调用。
pub fn check() -> Result<Option<Release>, String> {
    let body = get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))?;
    let j: serde_json::Value = serde_json::from_slice(&body).map_err(|e| format!("发布信息解析失败：{e}"))?;
    let tag = j["tag_name"].as_str().ok_or("发布信息里没有版本号")?;
    if !is_newer(tag, VERSION) {
        return Ok(None);
    }
    let assets = j["assets"].as_array().cloned().unwrap_or_default();
    let find = |name: &str| assets.iter().find(|a| a["name"].as_str() == Some(name)).and_then(|a| a["browser_download_url"].as_str()).map(String::from);
    let url = find(ASSET).ok_or_else(|| format!("新版本 {tag} 没有本平台的安装文件"))?;
    Ok(Some(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        notes: j["body"].as_str().unwrap_or("").to_string(),
        page: j["html_url"].as_str().unwrap_or("").to_string(),
        url,
        sums_url: find("SHA256SUMS"),
    }))
}

fn current_exe() -> Result<PathBuf, String> {
    let p = std::env::current_exe().map_err(|e| format!("找不到程序路径：{e}"))?;
    Ok(std::fs::canonicalize(&p).unwrap_or(p))
}

fn old_path(exe: &Path) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(".old");
    PathBuf::from(s)
}

fn sha256_hex(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// 下载、校验并替换程序文件。阻塞。成功后返回新程序路径（与原路径相同），调用 [`restart`] 生效。
pub fn install(rel: &Release) -> Result<PathBuf, String> {
    let exe = current_exe()?;
    let dir = exe.parent().ok_or("程序目录无效")?;
    let tmp = dir.join(format!(".lancat-update-{}.tmp", rel.version));
    let _ = std::fs::remove_file(&tmp);

    let mut c = curl();
    c.args(["-fsSL", "--connect-timeout", "15", "--max-time", "900", "-o"]).arg(&tmp).arg(&rel.url);
    if let Err(e) = run(c) {
        let _ = std::fs::remove_file(&tmp);
        // 写不进程序目录（比如装在 Program Files）时 curl 报的是写文件失败
        return Err(if std::fs::metadata(dir).map(|m| m.permissions().readonly()).unwrap_or(false) { format!("程序所在目录不可写：{}", dir.display()) } else { e });
    }

    if let Some(sums) = &rel.sums_url {
        let text = String::from_utf8(get(sums)?).unwrap_or_default();
        let want = text.lines().find_map(|l| {
            let mut it = l.split_whitespace();
            let (h, n) = (it.next()?, it.next()?);
            (n.trim_start_matches('*') == ASSET).then(|| h.to_ascii_lowercase())
        });
        let got = sha256_hex(&tmp)?;
        if want.as_deref() != Some(got.as_str()) {
            let _ = std::fs::remove_file(&tmp);
            return Err("下载的文件校验不通过，已丢弃".into());
        }
    }
    let len = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    if len < 1_000_000 {
        let _ = std::fs::remove_file(&tmp);
        return Err("下载的文件不完整".into());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    {
        let old = old_path(&exe);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&exe, &old).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("无法替换程序文件：{e}")
        })?;
        if let Err(e) = std::fs::rename(&tmp, &exe) {
            let _ = std::fs::rename(&old, &exe);
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("无法替换程序文件：{e}"));
        }
    }
    #[cfg(not(windows))]
    std::fs::rename(&tmp, &exe).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("无法替换程序文件：{e}")
    })?;
    log::info!("update: installed {} over {}", rel.version, exe.display());
    Ok(exe)
}

/// 启动新版（它会等本进程退出），然后退出事件循环。
pub fn restart(exe: &Path) -> Result<(), String> {
    let mut c = Command::new(exe);
    c.arg("--wait-pid").arg(std::process::id().to_string());
    c.spawn().map_err(|e| format!("新版启动失败：{e}"))?;
    #[cfg(windows)]
    crate::dock::tray::remove();
    let _ = slint::quit_event_loop();
    Ok(())
}

/// 启动最早期调用：带 `--wait-pid <pid>`（刚更新完）时等旧进程退出（最多 20 s），并清理 `.old`。
pub fn early_start() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--wait-pid") {
        if let Some(pid) = args.get(i + 1).and_then(|p| p.parse::<u32>().ok()) {
            wait_pid(pid, std::time::Duration::from_secs(20));
            log::info!("update: started after the old process {pid} exited");
        }
    }
    if let Ok(exe) = current_exe() {
        let old = old_path(&exe);
        if old.exists() {
            for _ in 0..10 {
                if std::fs::remove_file(&old).is_ok() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        }
    }
}

#[cfg(windows)]
fn wait_pid(pid: u32, max: std::time::Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            let _ = WaitForSingleObject(h, max.as_millis() as u32);
            let _ = CloseHandle(h);
        }
    }
}

#[cfg(not(windows))]
fn wait_pid(pid: u32, max: std::time::Duration) {
    let t = std::time::Instant::now();
    let p = PathBuf::from(format!("/proc/{pid}"));
    while p.exists() && t.elapsed() < max {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("garbage", "0.1.0"));
        assert_eq!(parse("v1.2.3-beta.1"), Some((1, 2, 3)));
    }
}

// ───────────────────────── 界面线程这一侧：状态、自动检查、一键更新 ─────────────────────────

use std::cell::RefCell;

/// "idle" | "checking" | "latest" | "available" | "downloading" | "error"
#[derive(Clone, Default)]
pub struct Status {
    pub state: String,
    pub text: String,
}

thread_local! {
    static STATUS: RefCell<Status> = RefCell::new(Status { state: "idle".into(), text: format!("当前版本 {VERSION}") });
    static LATEST: RefCell<Option<Release>> = const { RefCell::new(None) };
    static NOTIFIED: RefCell<String> = const { RefCell::new(String::new()) };
    static AUTO: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
}

pub fn status() -> Status {
    STATUS.with(|s| s.borrow().clone())
}

fn set_status(state: &str, text: String) {
    STATUS.with(|s| *s.borrow_mut() = Status { state: state.into(), text });
    crate::settings_ui::update_status_changed();
}

fn short(e: &str) -> String {
    let e: String = e.chars().take(40).collect();
    e
}

/// 启动后 30 s 查一次，之后每 6 小时一次（设置里关掉「自动检查更新」就不查）。
pub fn start_auto(node: std::sync::Arc<lancat_core::Node>) {
    let n = node.clone();
    let first = std::env::var("LANCAT_SLINT_UPDATE_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    slint::Timer::single_shot(std::time::Duration::from_secs(first), move || {
        if n.settings().auto_update {
            check_bg(false);
        }
    });
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(6 * 3600), move || {
        if node.settings().auto_update {
            check_bg(false);
        }
    });
    AUTO.with(|a| *a.borrow_mut() = Some(t));
}

/// 后台检查；`manual` = 用户点了「检查更新」（不弹提醒，结果显示在设置里）。
pub fn check_bg(manual: bool) {
    let st = status().state;
    if st == "checking" || st == "downloading" {
        return;
    }
    if manual {
        set_status("checking", "正在检查更新…".into());
    }
    std::thread::spawn(move || {
        let r = check();
        let _ = slint::invoke_from_event_loop(move || match r {
            Ok(Some(rel)) => {
                log::info!("update: {} available (running {VERSION})", rel.version);
                set_status("available", format!("发现新版本 {}（当前 {VERSION}）", rel.version));
                let v = rel.version.clone();
                LATEST.with(|l| *l.borrow_mut() = Some(rel));
                let first = NOTIFIED.with(|n| {
                    let mut n = n.borrow_mut();
                    let first = *n != v;
                    *n = v.clone();
                    first
                });
                // 测试：LANCAT_SLINT_AUTOUPDATE=1 发现新版就直接装（验证下载 / 替换 / 重启整条链路）
                if std::env::var_os("LANCAT_SLINT_AUTOUPDATE").is_some() {
                    slint::Timer::single_shot(std::time::Duration::ZERO, install_bg);
                    return;
                }
                if !manual && first {
                    crate::dock::on_update_alert(&format!("Lancat {v} 已发布"));
                }
            }
            Ok(None) => {
                LATEST.with(|l| *l.borrow_mut() = None);
                set_status("latest", format!("当前版本 {VERSION}，已是最新"));
            }
            Err(e) => {
                log::warn!("update check: {e}");
                if manual {
                    set_status("error", format!("检查失败：{}", short(&e)));
                }
            }
        });
    });
}

/// 下载并安装最新版，成功后自动重启到新版。
pub fn install_bg() {
    let Some(rel) = LATEST.with(|l| l.borrow().clone()) else {
        check_bg(true);
        return;
    };
    if status().state == "downloading" {
        return;
    }
    set_status("downloading", format!("正在下载 {}…", rel.version));
    std::thread::spawn(move || {
        let r = install(&rel);
        let _ = slint::invoke_from_event_loop(move || match r.and_then(|exe| {
            set_status("downloading", "更新完成，正在重启…".into());
            restart(&exe)
        }) {
            Ok(()) => {}
            Err(e) => {
                log::warn!("update install: {e}");
                set_status("error", format!("更新失败：{}", short(&e)));
            }
        });
    });
}

/// 在浏览器里打开新版本的发布页（更新说明）。
pub fn open_release_page() {
    let page = LATEST.with(|l| l.borrow().as_ref().map(|r| r.page.clone())).unwrap_or_else(|| format!("https://github.com/{REPO}/releases"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("cmd").args(["/c", "start", ""]).arg(&page).creation_flags(0x0800_0000).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("xdg-open").arg(&page).spawn();
    }
}
