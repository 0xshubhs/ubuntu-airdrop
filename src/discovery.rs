//! mDNS: advertise ourselves and keep a list of other Drop devices.

use crate::config::SERVICE_TYPE;
use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub label: String,
    pub host: String,
    pub port: u16,
    /// Round trip to its port, in milliseconds, when anyone has measured it.
    /// Drop has no radio signal to report, so this is what the radar places a
    /// device by: it is the closest honest thing to "how far away is it".
    #[serde(default)]
    pub ms: Option<u32>,
    /// When mDNS last resolved it, so a dot can fade rather than vanish.
    #[serde(default)]
    pub seen: u64,
}

pub type Peers = Arc<RwLock<HashMap<String, Peer>>>;

pub fn daemon() -> Result<ServiceDaemon> {
    Ok(ServiceDaemon::new()?)
}

/// Publish `_mydrop._tcp.local.`.
///
/// The host name is deliberately `<name>-drop.local.` and not `<name>.local.`:
/// Avahi already owns the plain hostname on most Linux boxes, and claiming it
/// again loses the conflict check.
pub fn advertise(mdns: &ServiceDaemon, name: &str, port: u16) -> Result<ServiceInfo> {
    let ip = crate::net::lan_ip();
    let safe: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    let props = [
        ("name".to_string(), name.to_string()),
        ("os".to_string(), "linux".to_string()),
        ("v".to_string(), "2".to_string()),
    ];

    let info = ServiceInfo::new(
        SERVICE_TYPE,
        name,
        &format!("{safe}-drop.local."),
        ip.as_str(),
        port,
        &props[..],
    )?;

    mdns.register(info.clone())?;
    Ok(info)
}

/// Take the advertisement down again.
///
/// A machine that has stopped receiving should stop appearing in other
/// devices' lists, or the window is only half observed: the name is still on
/// the phone when nothing will answer it.
pub fn unadvertise(mdns: &ServiceDaemon, info: &ServiceInfo) -> Result<()> {
    mdns.unregister(info.get_fullname())?;
    Ok(())
}

/// Watch for other devices, keeping `peers` current. Never returns.
pub async fn browse(mdns: ServiceDaemon, exclude: String, peers: Peers) -> Result<()> {
    let receiver = mdns.browse(SERVICE_TYPE)?;

    while let Ok(event) = receiver.recv_async().await {
        match event {
            ServiceEvent::ServiceResolved(info) => {
                let label = info
                    .get_property_val_str("name")
                    .unwrap_or_else(|| info.get_fullname())
                    .to_string();
                if label == exclude {
                    continue;
                }
                let Some(addr) = info.get_addresses().iter().find(|a| a.is_ipv4()) else {
                    continue;
                };
                let key = info.get_fullname().to_string();
                let mut live = peers.write().await;
                // Keep the measured round trip across a re-resolve; mDNS
                // repeats itself often and the dot should not jump out to the
                // rim every time it does.
                let ms = live.get(&key).and_then(|p| p.ms);
                live.insert(
                    key,
                    Peer {
                        label,
                        host: addr.to_string(),
                        port: info.get_port(),
                        ms,
                        seen: crate::auth::now(),
                    },
                );
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                peers.write().await.remove(&fullname);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Time a TCP connect to each peer, which is the only distance Drop has.
///
/// A connect rather than a ping: ICMP is often filtered, and what matters is
/// whether the port answers, which is the same question a transfer asks.
pub async fn measure(peers: &Peers) {
    use std::time::Instant;
    let snapshot: Vec<(String, String, u16)> = peers
        .read()
        .await
        .iter()
        .map(|(key, p)| (key.clone(), p.host.clone(), p.port))
        .collect();

    for (key, host, port) in snapshot {
        let started = Instant::now();
        let reached = tokio::time::timeout(
            std::time::Duration::from_millis(1200),
            tokio::net::TcpStream::connect((host.as_str(), port)),
        )
        .await
        .ok()
        .and_then(|r| r.ok())
        .is_some();
        let ms = reached.then(|| started.elapsed().as_millis().min(9_999) as u32);
        if let Some(peer) = peers.write().await.get_mut(&key) {
            peer.ms = ms;
        }
    }
}

/// Browse for `wait` and return whatever answered — used by `drop peers`
/// and by `drop send <name>`.
pub async fn snapshot(exclude: String, wait: std::time::Duration) -> Result<Vec<Peer>> {
    let mdns = daemon()?;
    let peers: Peers = Default::default();
    let watch = tokio::spawn(browse(mdns, exclude, peers.clone()));
    tokio::time::sleep(wait).await;
    watch.abort();

    let mut found: Vec<Peer> = peers.read().await.values().cloned().collect();
    found.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    Ok(found)
}
