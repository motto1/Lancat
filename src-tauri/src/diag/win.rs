//! Windows-only probes for the diagnostics page: firewall rules (INetFwPolicy2), network
//! category (INetworkListManager), who owns a TCP port (GetExtendedTcpTable) and running an
//! elevated, hidden PowerShell script (ShellExecuteExW "runas", one UAC prompt).
//! Nothing here opens a console window.

use std::path::Path;
use std::time::Duration;

use windows::core::{BSTR, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, WAIT_OBJECT_0};
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows::Win32::NetworkManagement::WindowsFirewall::{
    INetFwPolicy2, INetFwRule, NetFwPolicy2, NET_FW_ACTION_ALLOW, NET_FW_PROFILE2_DOMAIN,
    NET_FW_PROFILE2_PRIVATE, NET_FW_PROFILE2_PUBLIC, NET_FW_RULE_DIR_IN,
};
use windows::Win32::Networking::NetworkListManager::{
    INetwork, INetworkListManager, NetworkListManager, NLM_ENUM_NETWORK_CONNECTED,
};
use windows::Win32::Networking::WinSock::AF_INET;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Ole::IEnumVARIANT;
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SEE_MASK_NO_CONSOLE,
    SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::Interface;

pub const PROFILE_DOMAIN: i32 = NET_FW_PROFILE2_DOMAIN.0;
pub const PROFILE_PRIVATE: i32 = NET_FW_PROFILE2_PRIVATE.0;
pub const PROFILE_PUBLIC: i32 = NET_FW_PROFILE2_PUBLIC.0;
/// NET_FW_IP_PROTOCOL_ANY
pub const PROTO_ANY: i32 = 256;
pub const PROTO_TCP: i32 = 6;

#[derive(Debug, Clone)]
pub struct FwRule {
    pub name: String,
    /// Program path with environment variables expanded; empty = any program.
    pub app: String,
    pub allow: bool,
    pub enabled: bool,
    pub protocol: i32,
    pub ports: String,
    pub profiles: i32,
}

#[derive(Debug, Clone)]
pub struct FwSnapshot {
    /// Firewall on/off for (domain, private, public).
    pub enabled: [bool; 3],
    /// Bitmask of the profiles currently active (NET_FW_PROFILE2_*).
    pub current: i32,
    /// Inbound rules that can matter for us: rules for our exe, any-program rules naming one of
    /// our ports, and anything called "Lancat".
    pub rules: Vec<FwRule>,
}

/// Runs `f` on a fresh MTA thread so COM state never leaks into tokio / the UI thread.
fn com_thread<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    std::thread::spawn(move || unsafe {
        let init = CoInitializeEx(None, COINIT_MULTITHREADED);
        let r = f();
        if init.is_ok() {
            CoUninitialize();
        }
        r
    })
    .join()
    .map_err(|_| "COM 线程异常退出".to_string())?
}

fn expand_env(s: &str) -> String {
    // %VAR% expansion, enough for rule paths like %ProgramFiles%\Lancat\lancat.exe
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('%') {
            Some(j) => {
                let var = &after[..j];
                match std::env::var(var) {
                    Ok(v) if !var.is_empty() => out.push_str(&v),
                    _ => {
                        out.push('%');
                        out.push_str(var);
                        out.push('%');
                    }
                }
                rest = &after[j + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn same_path(a: &str, b: &str) -> bool {
    let n = |s: &str| s.trim().trim_matches('"').replace('/', "\\").to_lowercase();
    !a.trim().is_empty() && n(a) == n(b)
}

/// Does a LocalPorts string ("*", "47820", "1000-2000,5353", "") include `port`?
pub fn ports_cover(spec: &str, port: u16) -> bool {
    let spec = spec.trim();
    if spec.is_empty() || spec == "*" {
        return true;
    }
    spec.split(',').any(|p| {
        let p = p.trim();
        if let Some((a, b)) = p.split_once('-') {
            matches!((a.trim().parse::<u16>(), b.trim().parse::<u16>()), (Ok(a), Ok(b)) if a <= port && port <= b)
        } else {
            p.parse::<u16>() == Ok(port)
        }
    })
}

pub fn firewall_snapshot(exe: &Path, ports: &[u16]) -> Result<FwSnapshot, String> {
    let exe = exe.to_string_lossy().to_string();
    let ports = ports.to_vec();
    com_thread(move || unsafe {
        let policy: INetFwPolicy2 = CoCreateInstance(&NetFwPolicy2, None, CLSCTX_ALL)
            .map_err(|e| format!("无法读取防火墙设置：{e}"))?;
        let current = policy.CurrentProfileTypes().map_err(|e| e.to_string())?;
        let mut enabled = [true; 3];
        for (i, p) in [NET_FW_PROFILE2_DOMAIN, NET_FW_PROFILE2_PRIVATE, NET_FW_PROFILE2_PUBLIC].into_iter().enumerate() {
            if let Ok(v) = policy.get_FirewallEnabled(p) {
                enabled[i] = v.as_bool();
            }
        }
        let rules = policy.Rules().map_err(|e| e.to_string())?;
        let en: IEnumVARIANT = rules._NewEnum().map_err(|e| e.to_string())?.cast().map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        loop {
            let mut v = [VARIANT::default()];
            let mut fetched = 0u32;
            if en.Next(&mut v, &mut fetched).is_err() || fetched == 0 {
                break;
            }
            let Ok(disp) = windows::Win32::System::Com::IDispatch::try_from(&v[0]) else { continue };
            let Ok(rule) = disp.cast::<INetFwRule>() else { continue };
            let Ok(dir) = rule.Direction() else { continue };
            if dir != NET_FW_RULE_DIR_IN {
                continue;
            }
            let name = rule.Name().map(|b| b.to_string()).unwrap_or_default();
            let app = expand_env(&rule.ApplicationName().map(|b: BSTR| b.to_string()).unwrap_or_default());
            let lports = rule.LocalPorts().map(|b| b.to_string()).unwrap_or_default();
            let mine = same_path(&app, &exe);
            let any_app = app.trim().is_empty();
            let named = name.to_lowercase().contains("lancat");
            let names_port = !lports.trim().is_empty()
                && lports.trim() != "*"
                && ports.iter().any(|p| ports_cover(&lports, *p));
            if !(mine || named || (any_app && names_port)) {
                continue;
            }
            out.push(FwRule {
                name,
                app,
                allow: rule.Action().map(|a| a == NET_FW_ACTION_ALLOW).unwrap_or(false),
                enabled: rule.Enabled().map(|b| b.as_bool()).unwrap_or(false),
                protocol: rule.Protocol().unwrap_or(PROTO_ANY),
                ports: lports,
                profiles: rule.Profiles().unwrap_or(0x7fffffff),
            });
        }
        Ok(FwSnapshot { enabled, current, rules: out })
    })
}

#[derive(Debug, Clone)]
pub struct NetProfile {
    pub name: String,
    /// 0 = public, 1 = private, 2 = domain (NLM_NETWORK_CATEGORY)
    pub category: i32,
}

pub fn network_profiles() -> Result<Vec<NetProfile>, String> {
    com_thread(|| unsafe {
        let nlm: INetworkListManager = CoCreateInstance(&NetworkListManager, None, CLSCTX_ALL)
            .map_err(|e| format!("无法读取网络列表：{e}"))?;
        let en = nlm.GetNetworks(NLM_ENUM_NETWORK_CONNECTED).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        loop {
            let mut item: [Option<INetwork>; 1] = [None];
            let mut fetched = 0u32;
            if en.Next(&mut item, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            if let Some(n) = item[0].take() {
                out.push(NetProfile {
                    name: n.GetName().map(|b| b.to_string()).unwrap_or_default(),
                    category: n.GetCategory().map(|c| c.0).unwrap_or(-1),
                });
            }
        }
        Ok(out)
    })
}

/// Pid listening on 0.0.0.0/x.x.x.x:`port` (IPv4 TCP), if any.
pub fn tcp_listener_pid(port: u16) -> Option<u32> {
    unsafe {
        let mut size = 0u32;
        let _ = GetExtendedTcpTable(None, &mut size, false, AF_INET.0 as u32, TCP_TABLE_OWNER_PID_LISTENER, 0);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize + 1024];
        size = buf.len() as u32;
        if GetExtendedTcpTable(Some(buf.as_mut_ptr() as *mut _), &mut size, false, AF_INET.0 as u32, TCP_TABLE_OWNER_PID_LISTENER, 0) != 0 {
            return None;
        }
        let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
        let rows = std::slice::from_raw_parts(table.table.as_ptr() as *const MIB_TCPROW_OWNER_PID, table.dwNumEntries as usize);
        rows.iter()
            .find(|r| u16::from_be((r.dwLocalPort & 0xffff) as u16) == port)
            .map(|r| r.dwOwningPid)
    }
}

pub fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

#[derive(Debug)]
pub enum ElevateError {
    /// The user clicked "No" on the UAC prompt.
    Cancelled,
    Failed(String),
}

/// PowerShell's -EncodedCommand: base64 of the UTF-16LE script.
pub fn encode_ps(script: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut s = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

pub fn powershell_exe() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    format!("{root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe")
}

/// Runs `script` in an elevated, hidden PowerShell (one UAC prompt) and waits for it.
/// Returns the script's exit code.
pub fn run_elevated_ps(script: &str, timeout: Duration) -> Result<u32, ElevateError> {
    let file = HSTRING::from(powershell_exe());
    let params = HSTRING::from(format!(
        "-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -EncodedCommand {}",
        encode_ps(script)
    ));
    let verb = HSTRING::from("runas");
    unsafe {
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE | SEE_MASK_FLAG_NO_UI,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        if let Err(e) = ShellExecuteExW(&mut info) {
            if e.code() == ERROR_CANCELLED.to_hresult() {
                return Err(ElevateError::Cancelled);
            }
            return Err(ElevateError::Failed(e.message().to_string()));
        }
        if info.hProcess.is_invalid() {
            return Err(ElevateError::Failed("没有拿到进程句柄".into()));
        }
        let w = WaitForSingleObject(info.hProcess, timeout.as_millis().min(u32::MAX as u128) as u32);
        let mut code = 0u32;
        let ok = w == WAIT_OBJECT_0 && GetExitCodeProcess(info.hProcess, &mut code).is_ok();
        let _ = CloseHandle(info.hProcess);
        if !ok {
            return Err(ElevateError::Failed("修复脚本超时".into()));
        }
        Ok(code)
    }
}
