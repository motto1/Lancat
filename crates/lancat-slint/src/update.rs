//! 通过 GitHub Releases 检查和安装新版本（默认自动更新）。
//!
//! - 检查：`GET https://api.github.com/repos/motto1/Lancat/releases/latest`，按标签（`v0.2.0`）比版本号。
//!   启动后几秒查一次，之后每 6 小时一次（设置「自动更新」关掉就不查）。
//! - 下载：本平台的单文件（`lancat-slint-windows-x64.exe` / `lancat-slint-linux-x64`）下到程序所在目录的临时文件，
//!   按发布里的 `SHA256SUMS` 校验（自动更新时必须有校验文件），并检查大小。
//! - 安装：自动更新等到「空闲」（面板收起、没有传输、输入框没草稿、没开设置 / 文件对话框）才换；
//!   手动点「立即更新」马上换。换法：正在运行的程序改名为 `.old`（Windows 运行中的 exe 允许改名），
//!   新文件改名到原路径，用 `--wait-pid <旧进程>` 启动新版，再启动一个「守护」（旧版程序的副本，
//!   `--update-guard`），然后旧版退出。
//! - 回退：新版事件循环跑起来几秒后写「启动成功」（[`mark_ok`]）。守护在 2 分钟内没等到这句话（新版崩溃、
//!   启动失败、卡死）就结束新版、把 `.old` 换回来、启动旧版，并记下这个版本不再自动安装（手动更新仍可）。
//! - 网络请求交给系统自带的 curl（Windows 10 1803+ 自带 curl.exe，走系统证书和代理设置），不往程序里塞 TLS 库。
//! - 状态文件 `.lancat-update.json` 放在程序目录（与程序文件同生同灭，换数据目录的测试实例互不影响）。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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

/// 新版这么久没报「启动成功」就回退
const GUARD_TIMEOUT: Duration = Duration::from_secs(120);
/// 新版事件循环跑起来这么久算启动成功
const OK_AFTER: Duration = Duration::from_secs(4);

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
    c.args(["-fsSL", "--connect-timeout", "15", "--max-time", "60", "--retry", "2", "-H", "Accept: application/vnd.github+json"]);
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

fn with_suffix(exe: &Path, suffix: &str) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn old_path(exe: &Path) -> PathBuf {
    with_suffix(exe, ".old")
}

fn bad_path(exe: &Path) -> PathBuf {
    with_suffix(exe, ".bad")
}

/// 守护用的旧版副本（不能直接跑 `.old`：Windows 上运行中的文件改不回去）
fn guard_path(exe: &Path) -> PathBuf {
    let dir = exe.parent().unwrap_or(Path::new("."));
    #[cfg(windows)]
    return dir.join(".lancat-update-guard.exe");
    #[cfg(not(windows))]
    return dir.join(".lancat-update-guard");
}

fn state_path(exe: &Path) -> PathBuf {
    exe.parent().unwrap_or(Path::new(".")).join(".lancat-update.json")
}

fn read_state(exe: &Path) -> serde_json::Value {
    std::fs::read(state_path(exe)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| serde_json::json!({}))
}

fn write_state(exe: &Path, v: &serde_json::Value) {
    let p = state_path(exe);
    let tmp = with_suffix(&p, ".tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

fn edit_state(exe: &Path, f: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>)) {
    let mut v = read_state(exe);
    if !v.is_object() {
        v = serde_json::json!({});
    }
    f(v.as_object_mut().unwrap());
    write_state(exe, &v);
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

/// 程序目录能不能写（装在 Program Files / /usr/bin 之类的地方就只能提醒，不能自己换）
fn dir_writable(dir: &Path) -> bool {
    let p = dir.join(format!(".lancat-write-test-{}", std::process::id()));
    let ok = std::fs::write(&p, b"x").is_ok();
    let _ = std::fs::remove_file(&p);
    ok
}

/// 开发中的编译产物（cargo 的 target 目录里）：不自动换掉
fn is_dev_build(exe: &Path) -> bool {
    exe.components().any(|c| c.as_os_str() == "target") && exe.components().any(|c| matches!(c.as_os_str().to_str(), Some("debug" | "release" | "fastrel")))
}

/// 下载并校验到程序目录的临时文件。阻塞。`need_sums`：没有 SHA256SUMS 就不装（自动更新）。
fn download(rel: &Release, need_sums: bool) -> Result<PathBuf, String> {
    let exe = current_exe()?;
    let dir = exe.parent().ok_or("程序目录无效")?;
    if !dir_writable(dir) {
        return Err(format!("程序所在目录不可写：{}", dir.display()));
    }
    let tmp = dir.join(format!(".lancat-update-{}.tmp", rel.version));
    let _ = std::fs::remove_file(&tmp);
    let fail = |e: String| {
        let _ = std::fs::remove_file(&tmp);
        e
    };

    let mut c = curl();
    c.args(["-fsSL", "--connect-timeout", "15", "--max-time", "900", "--retry", "2", "-o"]).arg(&tmp).arg(&rel.url);
    run(c).map_err(fail)?;

    match &rel.sums_url {
        Some(sums) => {
            let text = String::from_utf8(get(sums).map_err(fail)?).unwrap_or_default();
            let want = text.lines().find_map(|l| {
                let mut it = l.split_whitespace();
                let (h, n) = (it.next()?, it.next()?);
                (n.trim_start_matches('*') == ASSET).then(|| h.to_ascii_lowercase())
            });
            let got = sha256_hex(&tmp).map_err(fail)?;
            if want.as_deref() != Some(got.as_str()) {
                return Err(fail("下载的文件校验不通过，已丢弃".into()));
            }
        }
        None if need_sums => return Err(fail("新版本没有校验文件（SHA256SUMS），不自动安装".into())),
        None => {}
    }
    let len = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    if len < 1_000_000 {
        return Err(fail("下载的文件不完整".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| fail(e.to_string()))?;
    }
    Ok(tmp)
}

/// rename，被杀毒软件 / 索引短暂占用时重试一会儿
fn rename_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last = None;
    for _ in 0..10 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(last.unwrap())
}

/// 换文件、启动新版和守护，然后退出事件循环。失败时把文件换回去，旧版接着跑。
fn apply(rel: &Release, tmp: &Path) -> Result<(), String> {
    let exe = current_exe()?;
    if !tmp.exists() {
        return Err("下载的新版文件不见了".into());
    }
    let old = old_path(&exe);
    let guard = guard_path(&exe);
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::remove_file(&guard);
    rename_retry(&exe, &old).map_err(|e| format!("无法替换程序文件：{e}"))?;
    let undo = |e: String| {
        let _ = std::fs::remove_file(&exe);
        let _ = rename_retry(&old, &exe);
        let _ = std::fs::remove_file(tmp);
        e
    };
    if let Err(e) = rename_retry(tmp, &exe) {
        return Err(undo(format!("无法替换程序文件：{e}")));
    }
    // 守护 = 旧版的副本；复制不了就不回退保护（照样更新，只是少一层保险）
    let guard_ok = std::fs::copy(&old, &guard).is_ok();

    edit_state(&exe, |s| {
        s.insert("phase".into(), "applied".into());
        s.insert("from".into(), VERSION.into());
        s.insert("to".into(), rel.version.clone().into());
    });
    let mut c = Command::new(&exe);
    c.arg("--wait-pid").arg(std::process::id().to_string());
    let child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            edit_state(&exe, |s| {
                s.insert("phase".into(), "idle".into());
            });
            let _ = std::fs::remove_file(&guard);
            return Err(undo(format!("新版启动失败：{e}")));
        }
    };
    if guard_ok {
        let mut g = Command::new(&guard);
        g.arg("--update-guard").arg(child.id().to_string()).arg("--exe").arg(&exe);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            g.creation_flags(0x0800_0000 | 0x0000_0008); // CREATE_NO_WINDOW | DETACHED_PROCESS
        }
        if let Err(e) = g.spawn() {
            log::warn!("update: guard not started: {e}");
        }
    }
    log::info!("update: installed {} over {}, restarting", rel.version, exe.display());
    #[cfg(windows)]
    crate::dock::tray::remove();
    let _ = slint::quit_event_loop();
    Ok(())
}

// ───────────────────────── 启动早期：等旧进程、守护、回退、清理 ─────────────────────────

/// 本次启动之前发生的回退（显示在设置里）
static ROLLED_BACK: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 启动最早期调用（单实例检查之前）。
/// - `--update-guard <pid> --exe <path>`：守护模式，处理完就退出进程，不会返回。
/// - `--wait-pid <pid>`：刚换成新版，等旧进程退出（最多 20 s）。
pub fn early_start() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    if let (Some(pid), Some(exe)) = (arg("--update-guard").and_then(|p| p.parse::<u32>().ok()), arg("--exe")) {
        guard(pid, Path::new(&exe));
        std::process::exit(0);
    }
    if let Some(pid) = arg("--wait-pid").and_then(|p| p.parse::<u32>().ok()) {
        wait_pid(pid, Duration::from_secs(20));
        log::info!("update: started after the old process {pid} exited");
    }
    let Ok(exe) = current_exe() else { return };
    let dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    let st = read_state(&exe);
    let phase = st["phase"].as_str().unwrap_or("");
    if phase == "rolledback" {
        let to = st["to"].as_str().unwrap_or("").to_string();
        log::warn!("update: {to} failed to start, rolled back to {VERSION}");
        *ROLLED_BACK.lock().unwrap() = Some(to);
        edit_state(&exe, |s| {
            s.insert("phase".into(), "idle".into());
        });
    }
    // 残留的下载临时文件
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with(".lancat-update-") && n.ends_with(".tmp") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    // 不是刚更新完（等着报「启动成功」）：旧程序文件、坏版本、守护副本都可以删了
    if phase != "applied" {
        cleanup(&exe);
    }
}

fn cleanup(exe: &Path) {
    let files = [old_path(exe), bad_path(exe), guard_path(exe)];
    std::thread::spawn(move || {
        // 守护进程刚退出时文件可能还占着：多试几次
        for _ in 0..20 {
            let mut left = false;
            for f in &files {
                if f.exists() && std::fs::remove_file(f).is_err() {
                    left = true;
                }
            }
            if !left {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

/// 守护：等新版报「启动成功」；新版退出或超时就回退到旧版。
fn guard(pid: u32, exe: &Path) {
    let t = std::time::Instant::now();
    loop {
        if read_state(exe)["phase"].as_str() == Some("ok") {
            return;
        }
        if !pid_alive(pid) {
            // 刚好在退出前写了 ok？
            std::thread::sleep(Duration::from_millis(300));
            if read_state(exe)["phase"].as_str() == Some("ok") {
                return;
            }
            break;
        }
        if t.elapsed() > GUARD_TIMEOUT {
            kill_pid(pid);
            wait_pid(pid, Duration::from_secs(10));
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    rollback(exe);
}

fn rollback(exe: &Path) {
    let old = old_path(exe);
    if !old.exists() {
        return;
    }
    let bad = bad_path(exe);
    let _ = std::fs::remove_file(&bad);
    if rename_retry(exe, &bad).is_err() {
        let _ = std::fs::remove_file(exe);
    }
    if rename_retry(&old, exe).is_err() && std::fs::copy(&old, exe).is_err() {
        return;
    }
    edit_state(exe, |s| {
        let to = s.get("to").cloned().unwrap_or_default();
        s.insert("phase".into(), "rolledback".into());
        s.insert("skip".into(), to);
    });
    let _ = Command::new(exe).spawn();
}

/// 新版跑起来了：告诉守护不用回退，并清理旧程序文件。
fn mark_ok() {
    let Ok(exe) = current_exe() else { return };
    let st = read_state(&exe);
    if st["phase"].as_str() != Some("applied") {
        return;
    }
    let to = st["to"].as_str().unwrap_or("").to_string();
    let from = st["from"].as_str().unwrap_or("").to_string();
    edit_state(&exe, |s| {
        s.insert("phase".into(), "ok".into());
        s.remove("skip");
    });
    log::info!("update: {VERSION} started fine (from {from})");
    if to == VERSION {
        set_status("latest", format!("已从 {from} 更新到 {VERSION}"));
    }
    // 守护每 0.5 s 看一次状态，等它退出再删副本
    let exe2 = exe.clone();
    slint::Timer::single_shot(Duration::from_secs(2), move || cleanup(&exe2));
}

/// 自动更新不再装的版本（上次装上后没能启动）
fn skipped() -> Option<String> {
    current_exe().ok().and_then(|e| read_state(&e)["skip"].as_str().map(String::from)).filter(|s| !s.is_empty())
}

#[cfg(windows)]
fn wait_pid(pid: u32, max: Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            let _ = WaitForSingleObject(h, max.as_millis() as u32);
            let _ = CloseHandle(h);
        }
    }
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(h) => {
                let alive = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
                let _ = CloseHandle(h);
                alive
            }
            Err(_) => false,
        }
    }
}

#[cfg(windows)]
fn kill_pid(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
            let _ = TerminateProcess(h, 1);
            let _ = CloseHandle(h);
        }
    }
}

#[cfg(not(windows))]
fn wait_pid(pid: u32, max: Duration) {
    let t = std::time::Instant::now();
    while pid_alive(pid) && t.elapsed() < max {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(not(windows))]
fn pid_alive(pid: u32) -> bool {
    // 僵尸进程（父进程还没收尸）也算退出了
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => !s.rsplit(')').next().is_some_and(|r| r.trim_start().starts_with('Z')),
        Err(_) => false,
    }
}

#[cfg(not(windows))]
fn kill_pid(pid: u32) {
    let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
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

// ───────────────────────── 界面线程这一侧：状态、自动检查、自动 / 一键更新 ─────────────────────────

use std::cell::RefCell;

/// "idle" | "checking" | "latest" | "available" | "downloading" | "ready" | "error"
#[derive(Clone, Default)]
pub struct Status {
    pub state: String,
    pub text: String,
}

thread_local! {
    static STATUS: RefCell<Status> = RefCell::new(Status { state: "idle".into(), text: format!("当前版本 {VERSION}") });
    static LATEST: RefCell<Option<Release>> = const { RefCell::new(None) };
    /// 已下载并校验好的新版（等空闲时安装）
    static STAGED: RefCell<Option<(Release, PathBuf)>> = const { RefCell::new(None) };
    static NOTIFIED: RefCell<String> = const { RefCell::new(String::new()) };
    static AUTO: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    static IDLE: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    static AUTO_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
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

/// 启动后几秒查一次，之后每 6 小时一次（设置里关掉「自动更新」就不查）。
pub fn start_auto(node: std::sync::Arc<lancat_core::Node>) {
    // 事件循环转起来了 = 这个版本能用：告诉守护（刚更新完时）不必回退
    slint::Timer::single_shot(OK_AFTER, mark_ok);
    if let Some(v) = ROLLED_BACK.lock().unwrap().take() {
        set_status("error", format!("更新到 {v} 后无法启动，已恢复到 {VERSION}"));
    }
    let n = node.clone();
    let first = std::env::var("LANCAT_SLINT_UPDATE_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    slint::Timer::single_shot(Duration::from_secs(first), move || {
        AUTO_ON.with(|a| a.set(n.settings().auto_update));
        if n.settings().auto_update {
            check_bg(false);
        }
    });
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_secs(6 * 3600), move || {
        AUTO_ON.with(|a| a.set(node.settings().auto_update));
        if node.settings().auto_update {
            check_bg(false);
        }
    });
    AUTO.with(|a| *a.borrow_mut() = Some(t));
}

/// 设置里切换了「自动更新」
pub fn auto_changed(on: bool) {
    AUTO_ON.with(|a| a.set(on));
    if on {
        if STAGED.with(|s| s.borrow().is_some()) {
            if let Some(v) = STAGED.with(|s| s.borrow().as_ref().map(|(r, _)| r.version.clone())) {
                set_status("ready", format!("新版本 {v} 已下载，空闲时自动安装"));
            }
            wait_idle();
        } else {
            check_bg(false);
        }
    } else {
        IDLE.with(|t| if let Some(t) = t.borrow().as_ref() { t.stop() });
        if status().state == "ready" {
            set_status("available", STAGED.with(|s| s.borrow().as_ref().map(|(r, _)| format!("新版本 {} 已下载（当前 {VERSION}）", r.version))).unwrap_or_default());
        }
    }
}

/// 后台检查；`manual` = 用户点了「检查更新」（不弹提醒，结果显示在设置里）。
pub fn check_bg(manual: bool) {
    let st = status().state;
    if st == "checking" || st == "downloading" || st == "ready" {
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
                LATEST.with(|l| *l.borrow_mut() = Some(rel.clone()));
                // 测试：LANCAT_SLINT_AUTOUPDATE=1 发现新版就直接装（验证下载 / 替换 / 重启整条链路）
                if std::env::var_os("LANCAT_SLINT_AUTOUPDATE").is_some() {
                    slint::Timer::single_shot(Duration::ZERO, install_bg);
                    return;
                }
                if manual {
                    return;
                }
                if AUTO_ON.with(|a| a.get()) && auto_install_ok(&v) {
                    stage_bg(rel);
                    return;
                }
                notify(&v);
            }
            Ok(None) => {
                LATEST.with(|l| *l.borrow_mut() = None);
                if status().state != "error" || manual {
                    set_status("latest", format!("当前版本 {VERSION}，已是最新"));
                }
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

/// 这一版能不能自动装：不是上次装失败的版本、程序目录可写、不是开发编译
fn auto_install_ok(v: &str) -> bool {
    if skipped().as_deref() == Some(v) {
        log::info!("update: {v} failed to start last time; not auto-installing it again");
        return false;
    }
    let Ok(exe) = current_exe() else { return false };
    if is_dev_build(&exe) {
        log::info!("update: development build ({}), not auto-installing", exe.display());
        return false;
    }
    if !exe.parent().is_some_and(dir_writable) {
        log::info!("update: {} is not writable, notify only", exe.display());
        return false;
    }
    true
}

/// 有新版、但不能自动装：同一个版本只提醒一次
fn notify(v: &str) {
    let first = NOTIFIED.with(|n| {
        let mut n = n.borrow_mut();
        let first = *n != v;
        *n = v.to_string();
        first
    });
    if first {
        crate::dock::on_update_alert(&format!("Lancat {v} 已发布"));
    }
}

/// 自动更新：后台下载校验，然后等空闲安装。下载失败就提醒（下次检查再试）。
fn stage_bg(rel: Release) {
    set_status("downloading", format!("正在下载 {}…", rel.version));
    std::thread::spawn(move || {
        let r = download(&rel, true);
        let _ = slint::invoke_from_event_loop(move || match r {
            Ok(tmp) => {
                log::info!("update: {} downloaded, installing when idle", rel.version);
                set_status("ready", format!("新版本 {} 已下载，空闲时自动安装", rel.version));
                STAGED.with(|s| *s.borrow_mut() = Some((rel, tmp)));
                wait_idle();
            }
            Err(e) => {
                log::warn!("update download: {e}");
                set_status("error", format!("自动更新失败：{}", short(&e)));
                notify(&rel.version);
            }
        });
    });
}

/// 每 20 s 看一次是否空闲；空闲就安装（面板收起、没有传输、没有草稿、没开设置 / 对话框）
fn wait_idle() {
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_secs(20), || {
        if !AUTO_ON.with(|a| a.get()) || crate::update_busy() {
            return;
        }
        IDLE.with(|t| if let Some(t) = t.borrow().as_ref() { t.stop() });
        apply_staged();
    });
    IDLE.with(|i| *i.borrow_mut() = Some(t));
}

fn apply_staged() {
    let Some((rel, tmp)) = STAGED.with(|s| s.borrow_mut().take()) else { return };
    set_status("downloading", format!("正在安装 {}…", rel.version));
    if let Err(e) = apply(&rel, &tmp) {
        log::warn!("update apply: {e}");
        set_status("error", format!("更新失败：{}", short(&e)));
    }
}

/// 「立即更新」：下载（已下载就跳过）并马上安装，成功后自动重启到新版。
pub fn install_bg() {
    if STAGED.with(|s| s.borrow().is_some()) {
        IDLE.with(|t| if let Some(t) = t.borrow().as_ref() { t.stop() });
        apply_staged();
        return;
    }
    let Some(rel) = LATEST.with(|l| l.borrow().clone()) else {
        check_bg(true);
        return;
    };
    if status().state == "downloading" {
        return;
    }
    set_status("downloading", format!("正在下载 {}…", rel.version));
    std::thread::spawn(move || {
        let r = download(&rel, false);
        let _ = slint::invoke_from_event_loop(move || match r {
            Ok(tmp) => {
                set_status("downloading", "更新完成，正在重启…".into());
                if let Err(e) = apply(&rel, &tmp) {
                    log::warn!("update install: {e}");
                    set_status("error", format!("更新失败：{}", short(&e)));
                }
            }
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
