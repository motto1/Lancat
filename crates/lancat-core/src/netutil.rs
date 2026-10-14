//! Network helpers: trust checks, interface enumeration and classification (physical LAN vs
//! overlay), overlay peer enumeration (Tailscale, EasyTier, NetBird).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Returns true for addresses that are allowed to talk to us:
/// loopback, RFC1918, link-local, CGNAT (100.64/10, used by Tailscale etc.), IPv6 ULA / link-local.
pub fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_v4(&v4);
            }
            is_private_v6(v6)
        }
    }
}

fn is_private_v4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        // 100.64.0.0/10 carrier-grade NAT (common for overlay LANs)
        || (o[0] == 100 && (o[1] & 0xC0) == 64)
}

fn is_private_v6(ip: &Ipv6Addr) -> bool {
    let s = ip.segments();
    ip.is_loopback()
        || (s[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
        || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 link local
}

/// Heuristic: virtual adapters where mDNS multicast is pointless (containers, VM host-only nets,
/// overlay VPNs without multicast). Only mDNS skips these; UDP probing covers every interface.
pub fn is_virtual_iface(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    const PREFIXES: &[&str] = &[
        "docker", "br-", "veth", "virbr", "vmnet", "vboxnet", "lxc", "lxd", "podman", "cni", "flannel",
        "kube", "tun", "tap", "zt", "wg", "utun", "llw", "awdl", "anpi", "bridge", "vnic", "ham",
    ];
    const CONTAINS: &[&str] = &["vmware", "virtualbox", "hyper-v", "vethernet", "wsl", "docker", "loopback", "npcap", "tailscale"];
    PREFIXES.iter().any(|p| n.starts_with(p)) || CONTAINS.iter().any(|c| n.contains(c))
}

/// One IPv4 address of a local interface that discovery probes on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProbeIface {
    pub name: String,
    pub ip: Ipv4Addr,
    pub prefix: u8,
    /// Directed broadcast address; `None` for point-to-point and /31–/32 links (e.g. Tailscale),
    /// where peers have to be enumerated and probed by unicast instead.
    pub broadcast: Option<Ipv4Addr>,
    /// Not a physical NIC: an overlay / VPN / VM / container adapter (see [`iface_kind`]).
    pub virt: bool,
    /// Layer-3 only (TUN, Wintun, WireGuard, point-to-point): broadcasts go nowhere, peers have to
    /// be listed by an overlay CLI or found by a unicast sweep of the subnet.
    pub l3: bool,
}

impl ProbeIface {
    /// Tailscale's interface (tailscale0 / utun* with a 100.64/10 address / "Tailscale" on Windows).
    pub fn is_tailscale(&self) -> bool {
        self.name.to_ascii_lowercase().contains("tailscale") || is_cgnat(&self.ip)
    }

    pub fn contains(&self, ip: &Ipv4Addr) -> bool {
        if self.prefix == 0 {
            return false;
        }
        let mask = u32::MAX << (32 - self.prefix as u32);
        u32::from(*ip) & mask == u32::from(self.ip) & mask
    }

    /// Every other host address of the subnet, for a unicast sweep; empty above /24 (LocalSend
    /// draws the same line: more than 254 probes per round is a port scan, not discovery).
    pub fn sweep_hosts(&self) -> Vec<Ipv4Addr> {
        if !(24..=30).contains(&self.prefix) {
            return vec![];
        }
        let mask = u32::MAX << (32 - self.prefix as u32);
        let net = u32::from(self.ip) & mask;
        let bcast = net | !mask;
        ((net + 1)..bcast).map(Ipv4Addr::from).filter(|ip| *ip != self.ip).collect()
    }
}

// ---- physical vs virtual -------------------------------------------------------------------
//
// Decided from what the OS says about the adapter, not from its address or name, so a VPN on
// 10.x / 192.168.x (EasyTier's 10.126.126.0/24) is not mistaken for the LAN and Hamachi's 25.x /
// Radmin's 26.x are recognised as overlays:
// * Windows: MIB_IF_ROW2.HardwareInterface (false for Wintun, TAP, ZeroTier, Hamachi, Hyper-V ...),
//   MediaType NdisMediumIP = layer-3 (Wintun: IfType 53, MediaType 19).
// * Linux: /sys/class/net/<if>/device exists only for real hardware; tun_flags marks TUN/TAP
//   (IFF_TUN = L3); a bridge or VLAN/bond over a physical NIC counts as physical; type 65534
//   (ARPHRD_NONE: WireGuard ...) is L3.
// * elsewhere (macOS, Android without sysfs access): the name heuristic.
// The user can override per adapter name (settings `iface_kinds`).

/// (virtual, layer-3) as the OS reports it; `None` = unknown, fall back to the name heuristic.
type Kind = Option<(bool, bool)>;

#[cfg(target_os = "linux")]
fn os_kinds(names: &[String]) -> std::collections::HashMap<String, (bool, bool)> {
    use std::path::Path;
    let root = Path::new("/sys/class/net");
    let hw = |n: &std::ffi::OsStr| root.join(n).join("device").exists();
    let mut out = std::collections::HashMap::new();
    for name in names {
        let base = root.join(name);
        if !base.exists() {
            continue;
        }
        let kind = if let Ok(f) = std::fs::read_to_string(base.join("tun_flags")) {
            let v = u32::from_str_radix(f.trim().trim_start_matches("0x"), 16).unwrap_or(0);
            (true, v & 0x0001 != 0)
        } else if base.join("device").exists() {
            (false, false)
        } else if base.join("bridge").exists() {
            let phys = std::fs::read_dir(base.join("brif")).map(|d| d.flatten().any(|e| hw(&e.file_name()))).unwrap_or(false);
            (!phys, false)
        } else if std::fs::read_dir(&base)
            .map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().starts_with("lower_") && e.path().join("device").exists()))
            .unwrap_or(false)
        {
            (false, false) // VLAN / bond / macvlan on top of a real NIC
        } else {
            let ty = std::fs::read_to_string(base.join("type")).ok().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(1);
            (true, ty == 65534 || ty == 776)
        };
        out.insert(name.clone(), kind);
    }
    out
}

#[cfg(windows)]
fn os_kinds_by_index() -> std::collections::HashMap<u32, (bool, bool)> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
    let mut out = std::collections::HashMap::new();
    unsafe {
        let mut t: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut t) != 0 || t.is_null() {
            return out;
        }
        let n = (*t).NumEntries as usize;
        let rows = std::slice::from_raw_parts((*t).Table.as_ptr(), n);
        for r in rows {
            let hardware = r.InterfaceAndOperStatusFlags._bitfield & 0x01 != 0;
            let l3 = r.MediaType == 19; // NdisMediumIP (Wintun, WireGuard)
            out.insert(r.InterfaceIndex, (!hardware, l3));
        }
        FreeMibTable(t as *const _);
    }
    out
}

static OVERRIDES: std::sync::RwLock<std::collections::BTreeMap<String, bool>> = std::sync::RwLock::new(std::collections::BTreeMap::new());

/// Per-adapter user overrides: name -> "lan" | "virtual" (anything else = automatic).
pub fn set_iface_overrides(m: &std::collections::BTreeMap<String, String>) {
    let mut o = OVERRIDES.write().unwrap();
    o.clear();
    for (k, v) in m {
        match v.as_str() {
            "lan" => {
                o.insert(k.clone(), false);
            }
            "virtual" => {
                o.insert(k.clone(), true);
            }
            _ => {}
        }
    }
    drop(o);
    *IF_CACHE.lock().unwrap() = None;
}

fn heuristic_kind(name: &str, ip: &Ipv4Addr, p2p: bool, prefix: u8) -> (bool, bool) {
    let n = name.to_ascii_lowercase();
    let virt = is_virtual_iface(name) || is_cgnat(ip);
    let l3 = p2p || prefix >= 31 || ["tailscale", "utun", "wg", "tun", "wintun", "wireguard"].iter().any(|k| n.contains(k));
    (virt, virt && l3)
}

fn resolve_kind(name: &str, os: Kind, ip: &Ipv4Addr, p2p: bool, prefix: u8) -> (bool, bool) {
    let (virt, l3) = os.unwrap_or_else(|| heuristic_kind(name, ip, p2p, prefix));
    let l3 = l3 || p2p || prefix >= 31;
    match OVERRIDES.read().unwrap().get(name) {
        Some(&v) => (v, v && l3),
        None => (virt, l3),
    }
}

/// 198.18.0.0/15 (RFC 2544 benchmarking): Clash / Mihomo / sing-box / FlClash TUN and fake-ip
/// range. A proxy's TUN, not a network with peers on it: never probed, never trusted.
pub fn is_proxy_fakeip(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 198 && (o[1] & 0xFE) == 18
}

pub fn is_cgnat(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (o[1] & 0xC0) == 64
}

/// Every non-loopback, not-down IPv4 interface with a private address, plus virtual adapters on
/// non-private ranges (Hamachi 25/8, Radmin 26/8, ZeroTier on a public block). A public address
/// on a physical NIC (a server on the internet) is never included.
pub fn probe_ifaces() -> Vec<ProbeIface> {
    let mut out = Vec::new();
    let Ok(list) = if_addrs::get_if_addrs() else { return out };
    #[cfg(target_os = "linux")]
    let kinds = os_kinds(&list.iter().map(|i| i.name.clone()).collect::<Vec<_>>());
    #[cfg(windows)]
    let kinds = os_kinds_by_index();
    for iface in list {
        if iface.is_loopback() || matches!(iface.oper_status, if_addrs::IfOperStatus::Down | if_addrs::IfOperStatus::NotPresent) {
            continue;
        }
        let if_addrs::IfAddr::V4(v4) = &iface.addr else { continue };
        if v4.ip.is_loopback() || v4.ip.is_unspecified() || is_proxy_fakeip(&v4.ip) {
            continue;
        }
        #[cfg(target_os = "linux")]
        let os: Kind = kinds.get(&iface.name).copied();
        #[cfg(windows)]
        let os: Kind = iface.index.and_then(|i| kinds.get(&i).copied());
        #[cfg(not(any(target_os = "linux", windows)))]
        let os: Kind = None;
        let (virt, l3) = resolve_kind(&iface.name, os, &v4.ip, iface.is_p2p(), v4.prefixlen);
        if !is_private_v4(&v4.ip) && !(virt && v4.prefixlen >= 8) {
            continue;
        }
        let broadcast = if l3 || iface.is_p2p() || v4.prefixlen >= 31 {
            None
        } else {
            Some(v4.broadcast.unwrap_or_else(|| Ipv4Addr::from(u32::from(v4.ip) | !u32::from(v4.netmask))))
        };
        out.push(ProbeIface { name: iface.name.clone(), ip: v4.ip, prefix: v4.prefixlen, broadcast, virt, l3 });
    }
    out.sort();
    out.dedup();
    out
}

static IF_CACHE: std::sync::Mutex<Option<(std::time::Instant, Vec<ProbeIface>)>> = std::sync::Mutex::new(None);

/// [`probe_ifaces`] cached for 3 s: for per-request checks (the HTTP guard, path ranking).
pub fn cached_ifaces() -> Vec<ProbeIface> {
    let mut c = IF_CACHE.lock().unwrap();
    if let Some((at, v)) = c.as_ref() {
        if at.elapsed() < std::time::Duration::from_secs(3) {
            return v.clone();
        }
    }
    let v = probe_ifaces();
    *c = Some((std::time::Instant::now(), v.clone()));
    v
}

/// May this source talk to us? Private ranges as before, plus any address inside the subnet of
/// one of our *virtual* adapters (the overlay itself vouches for its members: Hamachi, Radmin,
/// ZeroTier on public blocks). Manually added peers are allowed separately by the caller.
pub fn is_trusted_source(ip: &IpAddr) -> bool {
    if is_private(ip) {
        return true;
    }
    let v4 = match ip {
        IpAddr::V4(v4) => *v4,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4,
            None => return false,
        },
    };
    cached_ifaces().iter().any(|i| i.virt && i.contains(&v4))
}

/// Path class of a peer address: 0 = on one of our physical LANs, 1 = other IPv4 (routed),
/// 2 = on an overlay / VPN adapter (or CGNAT), 3 = IPv6.
pub fn path_rank(ip: &IpAddr) -> u8 {
    let IpAddr::V4(v4) = ip else { return 3 };
    let ifs = cached_ifaces();
    if let Some(i) = ifs.iter().filter(|i| i.contains(v4)).max_by_key(|i| i.prefix) {
        return if i.virt { 2 } else { 0 };
    }
    if is_cgnat(v4) {
        2
    } else {
        1
    }
}

/// Online Tailscale peers' IPv4 addresses from `tailscale status --json`.
pub fn parse_tailscale_status(json: &str) -> Vec<Ipv4Addr> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return vec![] };
    let mut out = vec![];
    if let Some(peers) = v.get("Peer").and_then(|p| p.as_object()) {
        for p in peers.values() {
            if !p.get("Online").and_then(|o| o.as_bool()).unwrap_or(false) {
                continue;
            }
            for ip in p.get("TailscaleIPs").and_then(|a| a.as_array()).into_iter().flatten() {
                if let Some(v4) = ip.as_str().and_then(|s| s.parse::<Ipv4Addr>().ok()) {
                    out.push(v4);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn tailscale_cli_candidates() -> Vec<std::path::PathBuf> {
    let mut v: Vec<std::path::PathBuf> = vec![];
    if cfg!(target_os = "windows") {
        for base in [std::env::var_os("ProgramFiles"), std::env::var_os("ProgramFiles(x86)")].into_iter().flatten() {
            v.push(std::path::Path::new(&base).join("Tailscale").join("tailscale.exe"));
        }
        v.push(r"C:\Program Files\Tailscale\tailscale.exe".into());
        v.push("tailscale.exe".into());
    } else {
        if cfg!(target_os = "macos") {
            v.push("/Applications/Tailscale.app/Contents/MacOS/Tailscale".into());
            v.push("/opt/homebrew/bin/tailscale".into());
            v.push("/usr/local/bin/tailscale".into());
        }
        v.push("tailscale".into());
        v.push("/usr/bin/tailscale".into());
        v.push("/usr/local/bin/tailscale".into());
    }
    v
}

async fn run_cli(exe: &std::path::Path, args: &[&str]) -> Option<Option<String>> {
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    match tokio::time::timeout(std::time::Duration::from_secs(3), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => Some(Some(String::from_utf8_lossy(&out.stdout).into_owned())),
        Ok(Ok(_)) => Some(None), // present but not running / logged out
        _ => None,               // not found / hung: try the next candidate
    }
}

async fn first_cli(cands: Vec<std::path::PathBuf>, args: &[&str]) -> Option<String> {
    for exe in cands {
        if exe.is_absolute() && !exe.exists() {
            continue;
        }
        if let Some(r) = run_cli(&exe, args).await {
            return r;
        }
    }
    None
}

/// Asks the local Tailscale CLI for online peers. Silent (empty) when there is no CLI, it is not
/// logged in, or it does not answer within 3 s.
pub async fn tailscale_peers() -> Vec<Ipv4Addr> {
    first_cli(tailscale_cli_candidates(), &["status", "--json"]).await.map(|j| parse_tailscale_status(&j)).unwrap_or_default()
}

fn cli_candidates(name: &str, win_dirs: &[&str]) -> Vec<std::path::PathBuf> {
    let mut v: Vec<std::path::PathBuf> = vec![];
    if cfg!(windows) {
        for base in [std::env::var_os("ProgramFiles"), std::env::var_os("ProgramFiles(x86)")].into_iter().flatten() {
            for d in win_dirs {
                v.push(std::path::Path::new(&base).join(d).join(format!("{name}.exe")));
            }
        }
        v.push(format!("{name}.exe").into());
    } else {
        v.push(name.into());
        for d in ["/usr/bin", "/usr/local/bin", "/opt/homebrew/bin", "/usr/sbin"] {
            v.push(std::path::Path::new(d).join(name));
        }
    }
    v
}

/// IPv4 strings found under any of `keys` anywhere in a JSON document ("10.0.0.2/24" accepted).
fn json_ipv4s(v: &serde_json::Value, keys: &[&str], keep: &dyn Fn(&serde_json::Map<String, serde_json::Value>) -> bool, out: &mut Vec<Ipv4Addr>) {
    match v {
        serde_json::Value::Object(m) => {
            if keep(m) {
                for k in keys {
                    if let Some(s) = m.get(*k).and_then(|x| x.as_str()) {
                        if let Ok(ip) = s.split('/').next().unwrap_or("").trim().parse::<Ipv4Addr>() {
                            out.push(ip);
                        }
                    }
                }
            }
            for x in m.values() {
                json_ipv4s(x, keys, keep, out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| json_ipv4s(x, keys, keep, out)),
        _ => {}
    }
}

/// `easytier-cli -o json peer list` (EasyTier ≥ 2.2): every peer row's `ipv4`.
pub fn parse_easytier_peers(json: &str) -> Vec<Ipv4Addr> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return vec![] };
    let mut out = vec![];
    // skip our own row (cost "Local")
    json_ipv4s(&v, &["ipv4"], &|m| m.get("cost").and_then(|c| c.as_str()) != Some("Local"), &mut out);
    out.sort();
    out.dedup();
    out
}

/// `netbird status --json`: peers' `netbirdIp` unless they are disconnected (lazy peers show
/// "Idle" and still answer).
pub fn parse_netbird_status(json: &str) -> Vec<Ipv4Addr> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return vec![] };
    let Some(peers) = v.get("peers") else { return vec![] };
    let mut out = vec![];
    json_ipv4s(peers, &["netbirdIp"], &|m| !m.get("status").and_then(|c| c.as_str()).is_some_and(|s| s.eq_ignore_ascii_case("disconnected")), &mut out);
    out.sort();
    out.dedup();
    out
}

/// Peers of every point-to-point overlay whose CLI can list them without root: Tailscale /
/// Headscale, EasyTier, NetBird. (ZeroTier, Hamachi, Radmin, n2n are layer-2 and are found by
/// broadcast; WireGuard / Nebula have no peer list: the /24 sweep, gossip or a manual address.)
pub async fn overlay_peers() -> Vec<Ipv4Addr> {
    let (ts, et, nb) = tokio::join!(
        tailscale_peers(),
        async { first_cli(cli_candidates("easytier-cli", &["EasyTier"]), &["-o", "json", "peer", "list"]).await.map(|j| parse_easytier_peers(&j)).unwrap_or_default() },
        async { first_cli(cli_candidates("netbird", &["Netbird", "NetBird"]), &["status", "--json"]).await.map(|j| parse_netbird_status(&j)).unwrap_or_default() },
    );
    let mut v: Vec<Ipv4Addr> = ts.into_iter().chain(et).chain(nb).collect();
    v.sort();
    v.dedup();
    v
}

pub fn os_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else {
        "unknown"
    }
}

pub fn host_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|h| h.trim_end_matches(".local").to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "Lancat".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ranges() {
        for s in ["127.0.0.1", "10.1.2.3", "172.16.0.1", "172.31.255.1", "192.168.1.1", "169.254.3.3", "100.124.103.26", "::1", "fd00::1", "fe80::1", "::ffff:192.168.1.2"] {
            assert!(is_private(&s.parse().unwrap()), "{s} should be private");
        }
        for s in ["8.8.8.8", "172.32.0.1", "1.1.1.1", "100.128.0.1", "2001:db8::1", "::ffff:8.8.8.8"] {
            assert!(!is_private(&s.parse().unwrap()), "{s} should be public");
        }
    }

    #[test]
    fn virtual_ifaces() {
        assert!(is_virtual_iface("docker0"));
        assert!(is_virtual_iface("vEthernet (WSL)"));
        assert!(is_virtual_iface("VMware Network Adapter VMnet8"));
        assert!(is_virtual_iface("veth12ab"));
        assert!(!is_virtual_iface("eth0"));
        assert!(!is_virtual_iface("wlan0"));
        assert!(!is_virtual_iface("en0"));
        assert!(!is_virtual_iface("Wi-Fi"));
    }

    #[test]
    fn tailscale_status() {
        let j = r#"{"Self":{"TailscaleIPs":["100.124.103.26"]},"Peer":{
            "k1":{"HostName":"laptop","Online":true,"TailscaleIPs":["100.101.1.2","fd7a:115c:a1e0::1"]},
            "k2":{"HostName":"phone","Online":false,"TailscaleIPs":["100.101.1.3"]},
            "k3":{"HostName":"nas","Online":true,"TailscaleIPs":["fd7a:115c:a1e0::9"]}}}"#;
        assert_eq!(parse_tailscale_status(j), vec!["100.101.1.2".parse::<Ipv4Addr>().unwrap()]);
        assert!(parse_tailscale_status("not json").is_empty());
        assert!(parse_tailscale_status(r#"{"Peer":null}"#).is_empty());
    }

    #[test]
    fn overlay_cli_parsers() {
        let et = r#"[{"cidr":"10.126.126.1/24","ipv4":"10.126.126.1","hostname":"me","cost":"Local"},
                     {"ipv4":"10.126.126.2/24","hostname":"b","cost":"p2p"},{"ipv4":"","hostname":"PublicServer","cost":"p2p"}]"#;
        assert_eq!(parse_easytier_peers(et), vec!["10.126.126.2".parse::<Ipv4Addr>().unwrap()]);
        let nb = r#"{"netbirdIp":"100.64.0.10/16","peers":{"total":3,"details":[
            {"fqdn":"a","netbirdIp":"100.64.0.20","status":"Connected"},
            {"fqdn":"b","netbirdIp":"100.64.0.21","status":"Idle"},
            {"fqdn":"c","netbirdIp":"100.64.0.22","status":"Disconnected"}]}}"#;
        assert_eq!(parse_netbird_status(nb), vec!["100.64.0.20".parse::<Ipv4Addr>().unwrap(), "100.64.0.21".parse().unwrap()]);
        assert!(parse_easytier_peers("Error: rpc").is_empty());
    }

    #[test]
    fn sweep_and_contains() {
        let i = ProbeIface { name: "tun0".into(), ip: "10.126.126.5".parse().unwrap(), prefix: 24, broadcast: None, virt: true, l3: true };
        let h = i.sweep_hosts();
        assert_eq!(h.len(), 253);
        assert!(!h.contains(&i.ip) && h.contains(&"10.126.126.1".parse().unwrap()) && h.contains(&"10.126.126.254".parse().unwrap()));
        assert!(i.contains(&"10.126.126.200".parse().unwrap()) && !i.contains(&"10.126.127.1".parse().unwrap()));
        let big = ProbeIface { prefix: 16, ..i.clone() };
        assert!(big.sweep_hosts().is_empty());
    }

    #[test]
    fn overrides_win() {
        let mut m = std::collections::BTreeMap::new();
        m.insert("vEthernet (External)".to_string(), "lan".to_string());
        set_iface_overrides(&m);
        assert_eq!(resolve_kind("vEthernet (External)", Some((true, false)), &"192.168.1.5".parse().unwrap(), false, 24), (false, false));
        assert_eq!(resolve_kind("eth0", Some((false, false)), &"10.126.126.5".parse().unwrap(), false, 24), (false, false));
        assert_eq!(resolve_kind("tun0", None, &"10.126.126.5".parse().unwrap(), false, 24), (true, true));
        set_iface_overrides(&Default::default());
    }

    #[test]
    fn dump_ifaces() {
        for i in probe_ifaces() {
            eprintln!("IFACE {} {}/{} virt={} l3={} bcast={:?}", i.name, i.ip, i.prefix, i.virt, i.l3, i.broadcast);
        }
    }

    #[test]
    fn probe_ifaces_sane() {
        for i in probe_ifaces() {
            assert!(!i.ip.is_loopback());
            assert!(is_private(&IpAddr::V4(i.ip)) || i.virt);
            if i.prefix >= 31 {
                assert!(i.broadcast.is_none());
            }
        }
    }
}
