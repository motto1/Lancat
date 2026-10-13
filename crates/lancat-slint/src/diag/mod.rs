//! "权限与诊断"（移植自 src-tauri/src/diag/mod.rs，去掉 Tauri）：检查其他设备为什么连不进本机
//! （防火墙、网络类别、端口、接收目录、对端报告）并一键修复（Windows：一次 UAC）。
//! * `refresh()` 跑全部检查（阻塞工作放到 tokio 的 blocking 线程）。
//! * `run_fix(id, dry_run)`：修复后再检查一次；dry_run（或 LANCAT_FIX_DRY_RUN=1）只返回将要执行的命令。
//! * 面板横幅 / 齿轮红点由 `status()` 驱动，变化时推给 UI 线程（crate::diag_ui_update）。

#[cfg(windows)]
mod win;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use lancat_core::{InboundReport, Node};

#[derive(Debug, Clone)]
pub struct FixInfo {
    pub id: String,
    pub label: String,
    /// Needs administrator rights (Windows: one UAC prompt).
    pub admin: bool,
    /// Ask the user first, with this text (changing the network type).
    pub confirm: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub id: String,
    pub title: String,
    /// "ok" | "warn" | "error" | "unknown"
    pub status: String,
    pub summary: String,
    pub detail: Vec<String>,
    pub help: Option<String>,
    pub fix: Option<FixInfo>,
    /// A problem here means other devices cannot connect to this one (drives the banner).
    pub inbound: bool,
}

impl Check {
    fn new(id: &str, title: &str, status: &str, summary: impl Into<String>) -> Self {
        Check { id: id.into(), title: title.into(), status: status.into(), summary: summary.into(), detail: vec![], help: None, fix: None, inbound: false }
    }
    fn problem(&self) -> bool {
        self.status == "error" || self.status == "warn"
    }
}

#[derive(Debug, Clone)]
pub struct Diagnostics {
    #[allow(dead_code)]
    pub os: String,
    pub checks: Vec<Check>,
    pub problems: usize,
    pub inbound_problem: bool,
    pub dry_run_forced: bool,
    pub ts: i64,
}

#[derive(Debug, Clone)]
pub struct FixResult {
    pub ok: bool,
    pub dry_run: bool,
    #[allow(dead_code)]
    pub cancelled: bool,
    pub message: String,
    pub commands: Vec<String>,
    #[allow(dead_code)]
    pub script: Option<String>,
    pub diagnostics: Option<Diagnostics>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    /// "inbound_blocked" | "recv_dir"
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub ts: i64,
}

#[derive(Debug, Clone)]
pub struct DiagStatus {
    pub problems: usize,
    pub alert: Option<Alert>,
}

struct DiagState {
    node: Arc<Node>,
    rt: tokio::runtime::Handle,
    last: Mutex<Option<Diagnostics>>,
    alert: Mutex<Option<Alert>>,
    dismissed: AtomicBool,
    /// The TCP port the core was configured for (it falls back to a random one when taken).
    wanted_port: u16,
    running_fix: AtomicBool,
}

static ST: OnceLock<DiagState> = OnceLock::new();

fn st() -> &'static DiagState {
    ST.get().expect("diag::init")
}

pub fn init(node: Arc<Node>, rt: tokio::runtime::Handle, wanted_port: u16) {
    let _ = ST.set(DiagState { node, rt: rt.clone(), last: Mutex::new(None), alert: Mutex::new(None), dismissed: AtomicBool::new(false), wanted_port, running_fix: AtomicBool::new(false) });
    // first check shortly after start (off the startup path); a problem raises the banner
    rt.spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        let _ = refresh().await;
    });
}

pub fn runtime() -> tokio::runtime::Handle {
    st().rt.clone()
}

pub fn last() -> Option<Diagnostics> {
    st().last.lock().unwrap().clone()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn ago(ts: i64) -> String {
    let s = ((now_ms() - ts) / 1000).max(0);
    if s < 60 {
        "刚刚".into()
    } else if s < 3600 {
        format!("{} 分钟前", s / 60)
    } else {
        format!("{} 小时前", s / 3600)
    }
}

pub fn dry_run_forced() -> bool {
    std::env::var("LANCAT_FIX_DRY_RUN").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Runs every check (blocking: COM / child processes / file writes).
fn run_checks(node: &Node, wanted_port: u16) -> Diagnostics {
    let mut checks = vec![];
    let port = node.port();
    #[cfg(windows)]
    checks.extend(windows_checks(port, wanted_port));
    #[cfg(target_os = "linux")]
    checks.extend(linux_checks(wanted_port));
    checks.push(port_check(port, wanted_port));
    checks.push(recv_dir_check(node));
    checks.push(runtime_check(node));
    let problems = checks.iter().filter(|c| c.problem()).count();
    let inbound_problem = checks.iter().any(|c| c.inbound && c.problem());
    Diagnostics { os: lancat_core::netutil::os_name().into(), checks, problems, inbound_problem, dry_run_forced: dry_run_forced(), ts: now_ms() }
}

/// Checks again, stores the result and updates the banner / red dot.
pub async fn refresh() -> Result<Diagnostics, String> {
    let st = st();
    let n = st.node.clone();
    let wanted = st.wanted_port;
    let d = tokio::task::spawn_blocking(move || run_checks(&n, wanted)).await.map_err(|e| e.to_string())?;
    *st.last.lock().unwrap() = Some(d.clone());
    {
        let mut alert = st.alert.lock().unwrap();
        if d.inbound_problem {
            if alert.as_ref().map_or(true, |a| a.kind != "inbound_blocked") {
                let first = d.checks.iter().find(|c| c.inbound && c.problem());
                *alert = Some(Alert {
                    kind: "inbound_blocked".into(),
                    title: "其他设备可能连不进本机".into(),
                    detail: first.map(|c| format!("{}：{}", c.title, c.summary)).unwrap_or_default(),
                    ts: now_ms(),
                });
            }
        } else if alert.as_ref().is_some_and(|a| a.kind == "inbound_blocked") {
            *alert = None;
        }
        let recv_ok = d.checks.iter().any(|c| c.id == "recv_dir" && c.status == "ok");
        if recv_ok && alert.as_ref().is_some_and(|a| a.kind == "recv_dir") {
            *alert = None;
        }
    }
    emit_status();
    Ok(d)
}

pub fn status() -> DiagStatus {
    let Some(st) = ST.get() else { return DiagStatus { problems: 0, alert: None } };
    let problems = st.last.lock().unwrap().as_ref().map(|d| d.problems).unwrap_or(0);
    let alert = if st.dismissed.load(Ordering::SeqCst) { None } else { st.alert.lock().unwrap().clone() };
    // a runtime alert (peer report, receive folder) before the first check also counts
    let problems = if problems == 0 && alert.is_some() { 1 } else { problems };
    DiagStatus { problems, alert }
}

fn emit_status() {
    let s = status();
    let last = last();
    crate::post(move |app| app.diag_status(s, last));
}

/// A core alert: a peer cannot connect to us, or the receive folder failed. Shows the banner
/// again even when it was dismissed.
pub fn on_core_alert(kind: &str, report: Option<&InboundReport>, detail: &str) {
    let Some(st) = ST.get() else { return };
    let alert = match kind {
        "inbound_blocked" => {
            let who = report.map(|r| r.peer_name.clone()).unwrap_or_else(|| "其他设备".into());
            let at = report.filter(|r| !r.addr.is_empty()).map(|r| format!("（{}:{}）", r.addr, r.port)).unwrap_or_default();
            Alert { kind: kind.into(), title: "其他设备连不进本机".into(), detail: format!("{who} 连不上本机{at}，发给本机的文件只能绕道或失败"), ts: now_ms() }
        }
        "recv_dir" => Alert { kind: kind.into(), title: "接收目录不可写".into(), detail: detail.into(), ts: now_ms() },
        _ => return,
    };
    let changed = {
        let mut a = st.alert.lock().unwrap();
        let same = a.as_ref().is_some_and(|x| x.kind == alert.kind && x.detail == alert.detail && alert.ts - x.ts < 60_000);
        *a = Some(alert);
        !same || st.dismissed.load(Ordering::SeqCst)
    };
    st.dismissed.store(false, Ordering::SeqCst);
    if changed {
        emit_status();
    }
}

pub fn dismiss_alert() {
    if let Some(st) = ST.get() {
        st.dismissed.store(true, Ordering::SeqCst);
        emit_status();
    }
}

pub async fn run_fix(id: String, dry_run: bool) -> Result<FixResult, String> {
    let dry = dry_run || dry_run_forced();
    let st = st();
    let n = st.node.clone();
    let wanted = st.wanted_port;
    // receive folder: plain settings change, no rights needed
    if id == "recv_dir_default" {
        let mut s = n.settings();
        let commands = vec![format!("设置 → 接收目录：{} → 默认（下载）", if s.receive_dir.is_empty() { "默认" } else { s.receive_dir.as_str() })];
        if dry {
            return Ok(FixResult { ok: true, dry_run: true, cancelled: false, message: "预览：不会做任何修改".into(), commands, script: None, diagnostics: None });
        }
        s.receive_dir.clear();
        n.set_settings(s).map_err(|e| e.to_string())?;
        let d = refresh().await?;
        return Ok(FixResult { ok: true, dry_run: false, cancelled: false, message: "已恢复为默认接收目录".into(), commands, script: None, diagnostics: Some(d) });
    }
    #[cfg(not(windows))]
    {
        let _ = (wanted, dry);
        Err(format!("这一项（{id}）需要手动处理，请按说明操作"))
    }
    #[cfg(windows)]
    {
    let plan = tokio::task::spawn_blocking(move || windows_fix_plan(&id, wanted)).await.map_err(|e| e.to_string())??;
    if dry {
        return Ok(FixResult { ok: true, dry_run: true, cancelled: false, message: "预览：以下命令会在一次管理员授权（UAC）后执行，现在不会做任何修改".into(), commands: plan.commands, script: Some(plan.script), diagnostics: None });
    }
    if st.running_fix.swap(true, Ordering::SeqCst) {
        return Err("修复正在进行".into());
    }
    let script = plan.script.clone();
    let r = tokio::task::spawn_blocking(move || win::run_elevated_ps(&script, std::time::Duration::from_secs(90))).await;
    st.running_fix.store(false, Ordering::SeqCst);
    let r = r.map_err(|e| e.to_string())?;
    let (ok, cancelled, message) = match r {
        Ok(0) => (true, false, plan.done_msg.clone()),
        Ok(code) => (false, false, format!("修复没有完成（退出码 {code}）")),
        Err(win::ElevateError::Cancelled) => (false, true, "已取消：没有获得管理员授权".into()),
        Err(win::ElevateError::Failed(e)) => (false, false, format!("修复失败：{e}")),
    };
    if ok {
        n.clear_inbound_reports();
    }
    log::info!("diagnostics fix {}: ok={ok} cancelled={cancelled} {message}", plan.id);
    let d = refresh().await?;
    Ok(FixResult { ok, dry_run: false, cancelled, message, commands: plan.commands, script: None, diagnostics: Some(d) })
    }
}


fn port_check(port: u16, wanted: u16) -> Check {
    if wanted == 0 || port == wanted {
        let mut c = Check::new("port", "服务端口", "ok", format!("TCP {port} 由本程序监听"));
        c.inbound = true;
        return c;
    }
    let owner = port_owner(wanted);
    let mut c = Check::new(
        "port",
        "服务端口",
        "error",
        format!("端口 {wanted} 被{}占用，本程序改用了随机端口 {port}", owner.as_deref().map(|o| format!("「{o}」")).unwrap_or_else(|| "其他程序".into())),
    );
    c.inbound = true;
    c.detail.push("其他设备按默认端口找不到本机，防火墙放行规则也对不上这个随机端口".into());
    if owner.as_deref().is_some_and(|o| o.to_lowercase().contains("lancat")) {
        c.detail.push("占用者看起来是另一个 Lancat（例如开发版或命令行版）".into());
    }
    c.help = Some(format!("关闭占用端口 {wanted} 的程序后重启 Lancat"));
    c
}

#[cfg(windows)]
fn port_owner(port: u16) -> Option<String> {
    let pid = win::tcp_listener_pid(port)?;
    Some(match win::process_path(pid) {
        Some(p) => format!("{p}（PID {pid}）"),
        None => format!("PID {pid}"),
    })
}

#[cfg(not(windows))]
fn port_owner(port: u16) -> Option<String> {
    // lsof prints p<pid> / c<command> lines; only processes of this user are visible without root
    let out = std::process::Command::new("lsof").args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpc"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let pid = text.lines().find_map(|l| l.strip_prefix('p'))?.to_string();
    let cmd = text.lines().find_map(|l| l.strip_prefix('c')).unwrap_or("?").to_string();
    Some(format!("{cmd}（PID {pid}）"))
}

fn recv_dir_check(node: &Node) -> Check {
    let custom = !node.settings().receive_dir.trim().is_empty();
    match node.check_receive_dir() {
        Ok(dir) => Check::new("recv_dir", "接收目录", "ok", format!("可写：{}", dir.display())),
        Err(e) => {
            let mut c = Check::new("recv_dir", "接收目录", "error", "接收目录不可写，收到的文件无法保存");
            c.detail.push(e);
            c.help = Some("在设置 →「接收」里换一个有写入权限的目录（也检查磁盘是否已满）".into());
            if custom {
                c.fix = Some(FixInfo { id: "recv_dir_default".into(), label: "恢复默认目录".into(), admin: false, confirm: None });
            }
            c
        }
    }
}

fn runtime_check(node: &Node) -> Check {
    let reports = node.inbound_reports();
    let unreachable = node.unreachable_peers();
    let mut c = if !reports.is_empty() {
        let mut c = Check::new("runtime", "连接情况", "error", format!("{} 台设备最近连不进本机", reports.len()));
        for r in &reports {
            let at = if r.addr.is_empty() { String::new() } else { format!("（{}:{}）", r.addr, r.port) };
            c.detail.push(format!("{} 连不上本机{at}，{}", r.peer_name, ago(r.ts)));
        }
        c.help = Some("先处理上面的防火墙和网络类型；如果它们都正常，可能是路由器开了 AP 隔离 / 访客网络，或者其他安全软件拦截了 Lancat".into());
        c.inbound = true;
        c
    } else if let Some((name, ts)) = node.last_inbound() {
        Check::new("runtime", "连接情况", "ok", format!("{name} {}成功连入本机", ago(ts)))
    } else {
        Check::new("runtime", "连接情况", "unknown", "启动后还没有其他设备连入过本机")
    };
    for (name, ip) in unreachable {
        c.detail.push(format!("本机连不进 {name}（{ip}）：对方的防火墙拦截了入站连接，需要在对方电脑上修复"));
    }
    c
}

// ------------------------------------------------------------------ Windows

#[cfg(windows)]
const PROFILE_NAMES: [(i32, &str); 3] = [(win::PROFILE_DOMAIN, "域"), (win::PROFILE_PRIVATE, "专用"), (win::PROFILE_PUBLIC, "公用")];

#[cfg(windows)]
fn profile_names(mask: i32) -> String {
    let v: Vec<&str> = PROFILE_NAMES.iter().filter(|(p, _)| mask & p != 0).map(|(_, n)| *n).collect();
    if v.is_empty() {
        "无".into()
    } else {
        v.join("、")
    }
}

#[cfg(windows)]
fn fw_enabled(snap: &win::FwSnapshot, profile: i32) -> bool {
    match profile {
        p if p == win::PROFILE_DOMAIN => snap.enabled[0],
        p if p == win::PROFILE_PRIVATE => snap.enabled[1],
        _ => snap.enabled[2],
    }
}

#[cfg(windows)]
fn covers_tcp(r: &win::FwRule, port: u16) -> bool {
    (r.protocol == win::PROTO_TCP || r.protocol == win::PROTO_ANY) && win::ports_cover(&r.ports, port)
}

/// Old Lancat rules whose program no longer exists (dev builds, moved installs).
#[cfg(windows)]
fn stale_rules(snap: &win::FwSnapshot, exe: &str) -> Vec<win::FwRule> {
    let mut v: Vec<win::FwRule> = snap
        .rules
        .iter()
        .filter(|r| !r.app.is_empty() && !win::same_path(&r.app, exe) && r.name.to_lowercase().contains("lancat") && !std::path::Path::new(&r.app).exists())
        .cloned()
        .collect();
    v.dedup_by(|a, b| a.name == b.name && win::same_path(&a.app, &b.app));
    v
}

#[cfg(windows)]
fn windows_checks(port: u16, wanted: u16) -> Vec<Check> {
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    let mut out = vec![];
    let snap = win::firewall_snapshot(std::path::Path::new(&exe), &[port, wanted]);
    let nets = win::network_profiles();
    let fw_fix = FixInfo { id: "firewall".into(), label: "修复防火墙规则".into(), admin: true, confirm: None };

    // -------- firewall
    match &snap {
        Err(e) => {
            let mut c = Check::new("firewall", "Windows 防火墙", "unknown", "无法读取防火墙设置");
            c.detail.push(e.clone());
            c.help = Some("可以在「Windows 安全中心 → 防火墙和网络保护 → 允许应用通过防火墙」里手动放行 Lancat（专用网络）".into());
            c.fix = Some(fw_fix.clone());
            c.inbound = true;
            out.push(c);
        }
        Ok(snap) => {
            let mine: Vec<&win::FwRule> = snap.rules.iter().filter(|r| win::same_path(&r.app, &exe)).collect();
            let blocks: Vec<&win::FwRule> = snap
                .rules
                .iter()
                .filter(|r| r.enabled && !r.allow && (win::same_path(&r.app, &exe) || (r.app.is_empty() && covers_tcp(r, port))))
                .collect();
            let allows: Vec<&win::FwRule> = mine.iter().copied().filter(|r| r.enabled && r.allow && covers_tcp(r, port)).collect();
            let stale = stale_rules(snap, &exe);
            let active: Vec<i32> = PROFILE_NAMES.iter().map(|(p, _)| *p).filter(|p| snap.current & p != 0).collect();
            let on: Vec<i32> = active.iter().copied().filter(|p| fw_enabled(snap, *p)).collect();
            // per active profile with the firewall on: blocked by a rule, allowed, or no rule (dropped)
            let blocked_by_rule: Vec<i32> = on.iter().copied().filter(|p| blocks.iter().any(|r| r.profiles & p != 0)).collect();
            let no_allow: Vec<i32> = on.iter().copied().filter(|p| !blocked_by_rule.contains(p) && !allows.iter().any(|r| r.profiles & p != 0)).collect();
            let state_line = format!(
                "防火墙：域 {} · 专用 {} · 公用 {}（当前网络：{}）",
                if snap.enabled[0] { "开" } else { "关" },
                if snap.enabled[1] { "开" } else { "关" },
                if snap.enabled[2] { "开" } else { "关" },
                profile_names(snap.current)
            );
            let mut c = if on.is_empty() {
                let mut c = Check::new("firewall", "Windows 防火墙", "ok", format!("当前网络（{}）的防火墙已关闭，不拦截入站连接", profile_names(snap.current)));
                if allows.is_empty() {
                    c.detail.push("本程序还没有放行规则：以后打开防火墙，其他设备就会连不进来，可以先修复".into());
                }
                c
            } else if !blocked_by_rule.is_empty() {
                Check::new("firewall", "Windows 防火墙", "error", format!("有阻止规则拦截本程序（{}网络），其他设备连不进来", profile_names(blocked_by_rule.iter().fold(0, |a, p| a | p))))
            } else if !no_allow.is_empty() {
                let public_only = no_allow.iter().all(|p| *p == win::PROFILE_PUBLIC) && !allows.is_empty();
                if public_only {
                    Check::new("firewall", "Windows 防火墙", "error", "当前是公用网络，放行规则只对专用/域网络生效，入站连接会被拦截")
                } else {
                    Check::new("firewall", "Windows 防火墙", "error", format!("本程序在{}网络上没有放行规则，Windows 会静默拦截入站连接", profile_names(no_allow.iter().fold(0, |a, p| a | p))))
                }
            } else {
                Check::new("firewall", "Windows 防火墙", "ok", format!("已放行（{}网络）", profile_names(allows.iter().fold(0, |a, r| a | r.profiles) & 7)))
            };
            c.detail.insert(0, state_line);
            c.detail.push(format!("程序：{exe}"));
            for r in &blocks {
                let scope = if r.app.is_empty() { format!("所有程序，端口 {}", r.ports) } else { "本程序".into() };
                c.detail.push(format!("阻止规则「{}」（{scope}，{}网络）{}", r.name, profile_names(r.profiles & 7), if on.is_empty() { "：防火墙打开后生效" } else { "" }));
            }
            for r in &allows {
                c.detail.push(format!("放行规则「{}」（{}网络）", r.name, profile_names(r.profiles & 7)));
            }
            if !stale.is_empty() {
                c.detail.push(format!("另有 {} 条指向已不存在路径的旧 Lancat 规则（修复时一并删除）", stale.len()));
                for r in stale.iter().take(4) {
                    c.detail.push(format!("  「{}」{} {}", r.name, if r.allow { "放行" } else { "阻止" }, r.app));
                }
            }
            if blocks.iter().any(|r| r.app.is_empty()) {
                c.help = Some("有针对所有程序、拦截 Lancat 端口的阻止规则，自动修复不会删除它：请在「高级安全 Windows Defender 防火墙 → 入站规则」里检查".into());
            }
            c.inbound = true;
            c.fix = Some(fw_fix.clone());
            out.push(c);
        }
    }

    // -------- network type
    match &nets {
        Err(e) => {
            let mut c = Check::new("network", "网络类型", "unknown", "无法读取网络类型");
            c.detail.push(e.clone());
            c.help = Some("在「设置 → 网络和 Internet → 属性」里把家里/公司的网络设为「专用网络」".into());
            out.push(c);
        }
        Ok(nets) if nets.is_empty() => out.push(Check::new("network", "网络类型", "unknown", "当前没有连接的网络")),
        Ok(nets) => {
            let public: Vec<&win::NetProfile> = nets.iter().filter(|n| n.category == 0).collect();
            let names = |v: &[&win::NetProfile]| v.iter().map(|n| format!("「{}」", n.name)).collect::<Vec<_>>().join("、");
            let public_fw_on = snap.as_ref().map(|s| s.enabled[2]).unwrap_or(true);
            let mut c = if public.is_empty() {
                Check::new("network", "网络类型", "ok", format!("专用/域网络：{}", names(&nets.iter().collect::<Vec<_>>())))
            } else if !public_fw_on {
                let mut c = Check::new("network", "网络类型", "ok", format!("{} 是公用网络，但公用网络的防火墙已关闭，不影响连接", names(&public)));
                c.detail.push("打开防火墙后，公用网络上的入站连接会被拦截".into());
                c
            } else {
                let mut c = Check::new("network", "网络类型", "error", format!("{} 是公用网络：Windows 会拦截其他设备连入", names(&public)));
                c.detail.push("Lancat 只在专用/域网络上放行；家里或公司的可信网络可以改为「专用」".into());
                c.inbound = true;
                c
            };
            for n in nets.iter() {
                let kind = match n.category {
                    0 => "公用",
                    1 => "专用",
                    2 => "域",
                    _ => "未知",
                };
                c.detail.push(format!("{}：{kind}", n.name));
            }
            if !public.is_empty() {
                c.fix = Some(FixInfo {
                    id: "network".into(),
                    label: "改为专用网络".into(),
                    admin: true,
                    confirm: Some(format!(
                        "将把 {} 设为「专用网络」。只应对家里、公司等可信网络这样做；在咖啡店、机场、酒店等公共 Wi-Fi 上请不要改（同一网络里的其他人将能发现并连接本机）。确定修改吗？",
                        names(&public)
                    )),
                });
            }
            c.help = Some("也可以在「设置 → 网络和 Internet → 属性 → 网络配置文件类型」里手动修改".into());
            out.push(c);
        }
    }
    out
}

#[cfg(windows)]
struct FixPlan {
    id: String,
    commands: Vec<String>,
    script: String,
    done_msg: String,
}

/// PowerShell single-quoted string.
#[cfg(windows)]
fn psq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[cfg(windows)]
fn windows_fix_plan(id: &str, wanted: u16) -> Result<FixPlan, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?.to_string_lossy().to_string();
    let port = if wanted == 0 { lancat_core::protocol::DEFAULT_PORT } else { wanted };
    match id {
        "firewall" => {
            // same rules as the installer (src-tauri/windows/hooks.nsh): delete every rule for this
            // exe (incl. "Block" rules left by a dismissed prompt), then allow TCP + UDP on private
            // and domain networks only. Plus stale Lancat rules for programs that no longer exist.
            let snap = win::firewall_snapshot(std::path::Path::new(&exe), &[port]).ok();
            let stale = snap.as_ref().map(|s| stale_rules(s, &exe)).unwrap_or_default();
            // (netsh args, must succeed)
            let mut steps: Vec<(String, bool)> = vec![(format!("advfirewall firewall delete rule name=all program=\"{exe}\""), false)];
            for r in &stale {
                steps.push((format!("advfirewall firewall delete rule name=\"{}\" program=\"{}\"", r.name, r.app), false));
            }
            steps.push((format!("advfirewall firewall add rule name=\"Lancat\" dir=in action=allow program=\"{exe}\" protocol=TCP localport={port} profile=private,domain"), true));
            steps.push((format!("advfirewall firewall add rule name=\"Lancat\" dir=in action=allow program=\"{exe}\" protocol=UDP localport={port},5353 profile=private,domain"), true));
            let commands = steps.iter().map(|(a, _)| format!("netsh {a}")).collect();
            // Start-Process passes the argument string verbatim (PowerShell's own native-argument
            // quoting would mangle program="C:\path with spaces\lancat.exe")
            let mut script = String::from("$ErrorActionPreference = 'Continue'\n$fail = 0\n");
            for (args, must) in &steps {
                script.push_str(&format!(
                    "$p = Start-Process -FilePath \"$env:SystemRoot\\System32\\netsh.exe\" -ArgumentList {} -WindowStyle Hidden -Wait -PassThru\n",
                    psq(args)
                ));
                if *must {
                    script.push_str("if ($p.ExitCode -ne 0) { $fail = 1 }\n");
                }
            }
            script.push_str("exit $fail\n");
            Ok(FixPlan { id: id.into(), commands, script, done_msg: "已更新防火墙规则：本程序在专用和域网络上放行".into() })
        }
        "network" => {
            let nets = win::network_profiles()?;
            let public: Vec<String> = nets.into_iter().filter(|n| n.category == 0).map(|n| n.name).collect();
            if public.is_empty() {
                return Err("当前没有公用网络，不需要修改".into());
            }
            let list = public.iter().map(|n| psq(n)).collect::<Vec<_>>().join(",");
            let cmd = format!("Get-NetConnectionProfile | Where-Object {{ $_.NetworkCategory -eq 'Public' -and @({list}) -contains $_.Name }} | Set-NetConnectionProfile -NetworkCategory Private");
            let script = format!("$ErrorActionPreference = 'Stop'\ntry {{\n  {cmd}\n  exit 0\n}} catch {{\n  exit 1\n}}\n");
            Ok(FixPlan { id: id.into(), commands: vec![cmd], script, done_msg: "已改为专用网络".into() })
        }
        _ => Err(format!("未知的修复项：{id}")),
    }
}

// ------------------------------------------------------------------ Linux（旧版 src-tauri/src/diag/mod.rs 原样移植）

#[cfg(target_os = "linux")]
fn linux_checks(port: u16) -> Vec<Check> {
    let port = if port == 0 { lancat_core::protocol::DEFAULT_PORT } else { port };
    let run = |cmd: &str, args: &[&str]| -> Option<(bool, String)> {
        let o = std::process::Command::new(cmd).args(args).output().ok()?;
        Some((o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).to_lowercase()))
    };
    let help = format!(
        "ufw：sudo ufw allow {port}/tcp && sudo ufw allow {port},5353/udp\nfirewalld：sudo firewall-cmd --permanent --add-port={port}/tcp --add-port={port}/udp --add-port=5353/udp && sudo firewall-cmd --reload"
    );
    let mut c = match run("firewall-cmd", &["--state"]) {
        Some((true, s)) if s.contains("running") && !s.contains("not running") => match run("firewall-cmd", &["--list-ports"]) {
            Some((true, ports)) if ports.split_whitespace().any(|p| p == format!("{port}/tcp")) => Check::new("firewall", "防火墙", "ok", format!("firewalld 已放行 {port}/tcp")),
            Some((true, _)) => Check::new("firewall", "防火墙", "warn", format!("firewalld 正在运行，当前区域没有放行 {port}/tcp")),
            _ => Check::new("firewall", "防火墙", "unknown", "firewalld 正在运行，无法读取放行端口"),
        },
        _ => match run("ufw", &["status"]) {
            Some((true, s)) if s.contains("inactive") => Check::new("firewall", "防火墙", "ok", "ufw 未启用"),
            Some((true, s)) if s.contains(&format!("{port}/tcp")) || s.contains(&format!("{port} ")) => Check::new("firewall", "防火墙", "ok", format!("ufw 已放行 {port}")),
            Some((true, _)) => Check::new("firewall", "防火墙", "warn", format!("ufw 已启用，没有放行 {port}/tcp")),
            // ufw needs root to read its status; nftables / iptables too
            _ => Check::new("firewall", "防火墙", "unknown", "无法读取防火墙状态（需要管理员权限）"),
        },
    };
    c.help = Some(help);
    c.inbound = true;
    let mut n = Check::new("network", "网络类型", "unknown", "Linux 没有公用/专用网络之分");
    n.help = Some("如果其他设备仍连不进来，检查路由器是否开启了 AP 隔离 / 访客网络".into());
    vec![c, n]
}

// ------------------------------------------------------------------ macOS
