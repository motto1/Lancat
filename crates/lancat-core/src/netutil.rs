//! Network helpers: private-range checks, interface enumeration, virtual NIC filtering.

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

/// Heuristic: names of virtual adapters we do not want to announce on / broadcast to.
pub fn is_virtual_iface(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    const PREFIXES: &[&str] = &[
        "docker", "br-", "veth", "virbr", "vmnet", "vboxnet", "lxc", "lxd", "podman", "cni", "flannel",
        "kube", "tun", "tap", "zt", "wg", "utun", "llw", "awdl", "anpi", "bridge", "vnic", "ham",
    ];
    const CONTAINS: &[&str] = &["vmware", "virtualbox", "hyper-v", "vethernet", "wsl", "docker", "loopback", "npcap", "tailscale"];
    PREFIXES.iter().any(|p| n.starts_with(p)) || CONTAINS.iter().any(|c| n.contains(c))
}

#[derive(Debug, Clone)]
pub struct LanIface {
    pub name: String,
    pub ip: Ipv4Addr,
    pub broadcast: Ipv4Addr,
}

/// Physical-looking IPv4 interfaces with a private address.
pub fn lan_ifaces() -> Vec<LanIface> {
    let mut out = Vec::new();
    let Ok(list) = if_addrs::get_if_addrs() else { return out };
    for iface in list {
        if iface.is_loopback() || is_virtual_iface(&iface.name) {
            continue;
        }
        if let if_addrs::IfAddr::V4(v4) = &iface.addr {
            if !is_private_v4(&v4.ip) || v4.ip.is_loopback() {
                continue;
            }
            let bc = v4.broadcast.unwrap_or_else(|| {
                let ip = u32::from(v4.ip);
                let mask = u32::from(v4.netmask);
                Ipv4Addr::from(ip | !mask)
            });
            out.push(LanIface { name: iface.name.clone(), ip: v4.ip, broadcast: bc });
        }
    }
    out
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
}
