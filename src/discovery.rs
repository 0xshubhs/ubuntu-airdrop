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
                peers.write().await.insert(
                    info.get_fullname().to_string(),
                    Peer {
                        label,
                        host: addr.to_string(),
                        port: info.get_port(),
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
