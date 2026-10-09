//! Discovery: mDNS (`_lancat._tcp.local.`, TXT id/name/os/ver) plus a UDP broadcast
//! heartbeat every 5 s on every physical interface as a fallback. Peers that are not heard
//! from for 15 s are marked offline by the node's housekeeping loop.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent, ServiceInfo};
use socket2::{Domain, Protocol, Socket, Type};

use crate::netutil;
use crate::node::Node;
use crate::protocol::*;

pub(crate) struct Mdns {
    daemon: ServiceDaemon,
    fullname: Option<String>,
}

pub(crate) fn start(node: Arc<Node>) {
    if node.cfg.enable_mdns {
        match ServiceDaemon::new() {
            Ok(daemon) => {
                // never announce on virtual adapters (Docker, VMware, WSL ...)
                if let Ok(list) = if_addrs::get_if_addrs() {
                    for i in list {
                        if netutil::is_virtual_iface(&i.name) {
                            let _ = daemon.disable_interface(IfKind::Name(i.name.clone()));
                        }
                    }
                }
                let _ = daemon.disable_interface(IfKind::IPv6);
                *node.mdns.lock().unwrap() = Some(Mdns { daemon, fullname: None });
                announce(&node);
                browse(node.clone());
            }
            Err(e) => log::warn!("mDNS unavailable: {e}"),
        }
    }
    if node.cfg.enable_udp {
        match udp_socket(node.cfg.udp_port) {
            Ok(sock) => {
                let sock = Arc::new(sock);
                tokio::spawn(udp_send_loop(node.clone(), sock.clone()));
                tokio::spawn(udp_recv_loop(node.clone(), sock));
            }
            Err(e) => log::warn!("UDP heartbeat unavailable: {e}"),
        }
    }
}

fn announce(node: &Node) {
    let me = node.me();
    let mut guard = node.mdns.lock().unwrap();
    let Some(m) = guard.as_mut() else { return };
    let mut props = HashMap::new();
    props.insert("id".to_string(), me.id.clone());
    props.insert("name".to_string(), me.name.clone());
    props.insert("os".to_string(), me.os.clone());
    props.insert("ver".to_string(), me.ver.clone());
    let short: String = me.id.chars().filter(|c| c.is_ascii_alphanumeric()).take(12).collect();
    let instance = format!("lancat-{short}");
    let host = format!("lancat-{short}.local.");
    match ServiceInfo::new(SERVICE_TYPE, &instance, &host, (), me.port, props) {
        Ok(info) => {
            let info = info.enable_addr_auto();
            let fullname = info.get_fullname().to_string();
            if let Err(e) = m.daemon.register(info) {
                log::warn!("mDNS register failed: {e}");
            } else {
                m.fullname = Some(fullname);
            }
        }
        Err(e) => log::warn!("mDNS service info: {e}"),
    }
}

/// Re-registers after a rename.
pub(crate) fn reannounce(node: &Node) {
    {
        let guard = node.mdns.lock().unwrap();
        if let Some(m) = guard.as_ref() {
            if let Some(f) = &m.fullname {
                let _ = m.daemon.unregister(f);
            }
        }
    }
    announce(node);
}

pub(crate) fn stop(node: &Node) {
    if let Some(m) = node.mdns.lock().unwrap().take() {
        if let Some(f) = &m.fullname {
            let _ = m.daemon.unregister(f);
        }
        let _ = m.daemon.shutdown();
    }
}

fn browse(node: Arc<Node>) {
    let rx = {
        let guard = node.mdns.lock().unwrap();
        match guard.as_ref().map(|m| m.daemon.browse(SERVICE_TYPE)) {
            Some(Ok(rx)) => rx,
            Some(Err(e)) => {
                log::warn!("mDNS browse failed: {e}");
                return;
            }
            None => return,
        }
    };
    let mut stop = node.shutdown_rx();
    tokio::spawn(async move {
        loop {
            let ev = tokio::select! {
                ev = rx.recv_async() => match ev { Ok(ev) => ev, Err(_) => break },
                _ = stop.changed() => break,
            };
            if let ServiceEvent::ServiceResolved(svc) = ev {
                let get = |k: &str| svc.get_property_val_str(k).unwrap_or("").to_string();
                let info = DeviceInfo { id: get("id"), name: get("name"), os: get("os"), ver: get("ver"), port: svc.get_port() };
                if info.id.is_empty() || info.id == node.me().id {
                    continue;
                }
                // prefer a private IPv4 address
                let mut addrs: Vec<IpAddr> = svc.get_addresses().iter().map(|a| a.to_ip_addr()).filter(netutil::is_private).collect();
                addrs.sort_by_key(|a| (!a.is_ipv4(), a.is_loopback()));
                if let Some(ip) = addrs.first() {
                    node.peer_seen(info, *ip);
                }
            }
        }
    });
}

fn udp_socket(port: u16) -> std::io::Result<tokio::net::UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    s.set_reuse_port(true)?;
    s.set_broadcast(true)?;
    s.set_nonblocking(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
    tokio::net::UdpSocket::from_std(s.into())
}

async fn udp_send_loop(node: Arc<Node>, sock: Arc<tokio::net::UdpSocket>) {
    let mut stop = node.shutdown_rx();
    let mut tick = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = stop.changed() => break,
        }
        let hb = Heartbeat { lancat: 1, device: node.me() };
        let Ok(payload) = serde_json::to_vec(&hb) else { continue };
        let mut targets: Vec<Ipv4Addr> = netutil::lan_ifaces().into_iter().map(|i| i.broadcast).collect();
        targets.push(Ipv4Addr::BROADCAST);
        targets.dedup();
        for t in targets {
            let _ = sock.send_to(&payload, SocketAddr::from((t, node.cfg.udp_port))).await;
        }
    }
}

async fn udp_recv_loop(node: Arc<Node>, sock: Arc<tokio::net::UdpSocket>) {
    let mut stop = node.shutdown_rx();
    let mut buf = vec![0u8; 2048];
    loop {
        let (n, from) = tokio::select! {
            r = sock.recv_from(&mut buf) => match r { Ok(v) => v, Err(_) => { tokio::time::sleep(Duration::from_millis(200)).await; continue } },
            _ = stop.changed() => break,
        };
        if !netutil::is_private(&from.ip()) || node.is_blocked_addr(&from.ip()) {
            continue;
        }
        if let Ok(hb) = serde_json::from_slice::<Heartbeat>(&buf[..n]) {
            if hb.lancat == 1 && hb.device.id != node.me().id && !node.store.is_blocked(&hb.device.id) {
                node.peer_seen(hb.device, from.ip());
            }
        }
    }
}
