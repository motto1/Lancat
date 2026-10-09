//! Network helpers: private-range checks, interface enumeration, Tailscale peer enumeration.

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
}

impl ProbeIface {
    /// Tailscale's interface (tailscale0 / utun* with a 100.64/10 address / "Tailscale" on Windows).
    pub fn is_tailscale(&self) -> bool {
        self.name.to_ascii_lowercase().contains("tailscale") || is_cgnat(&self.ip)
    }
}

pub fn is_cgnat(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (o[1] & 0xC0) == 64
}

/// Every non-loopback, not-down IPv4 interface with a private address — physical LANs and
/// virtual LANs (Tailscale, ZeroTier, WireGuard, VPNs, VM networks) alike.
pub fn probe_ifaces() -> Vec<ProbeIface> {
    let mut out = Vec::new();
    let Ok(list) = if_addrs::get_if_addrs() else { return out };
    for iface in list {
        if iface.is_loopback() || matches!(iface.oper_status, if_addrs::IfOperStatus::Down | if_addrs::IfOperStatus::NotPresent) {
            continue;
        }
        let if_addrs::IfAddr::V4(v4) = &iface.addr else { continue };
        if !is_private_v4(&v4.ip) || v4.ip.is_loopback() {
            continue;
        }
        let broadcast = if iface.is_p2p() || v4.prefixlen >= 31 {
            None
        } else {
            Some(v4.broadcast.unwrap_or_else(|| Ipv4Addr::from(u32::from(v4.ip) | !u32::from(v4.netmask))))
        };
        out.push(ProbeIface { name: iface.name.clone(), ip: v4.ip, prefix: v4.prefixlen, broadcast });
    }
    out.sort();
    out.dedup();
    out
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

/// Asks the local Tailscale CLI for online peers. Silent (empty) when there is no CLI, it is not
/// logged in, or it does not answer within 3 s.
pub async fn tailscale_peers() -> Vec<Ipv4Addr> {
    for exe in tailscale_cli_candidates() {
        if exe.is_absolute() && !exe.exists() {
            continue;
        }
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.args(["status", "--json"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        match tokio::time::timeout(std::time::Duration::from_secs(3), cmd.output()).await {
            Ok(Ok(out)) if out.status.success() => return parse_tailscale_status(&String::from_utf8_lossy(&out.stdout)),
            Ok(Ok(_)) => return vec![], // CLI present but not running / logged out
            _ => continue,
        }
    }
    vec![]
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
    fn probe_ifaces_sane() {
        for i in probe_ifaces() {
            assert!(!i.ip.is_loopback());
            assert!(is_private(&IpAddr::V4(i.ip)));
            if i.prefix >= 31 {
                assert!(i.broadcast.is_none());
            }
        }
    }
}
