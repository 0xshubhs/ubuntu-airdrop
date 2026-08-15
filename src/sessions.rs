//! Who is signed in from a browser right now.
//!
//! Separate from transfers on purpose: a phone is "connected" the moment it
//! gets past the PIN, not when it sends something. That is what makes it
//! possible to push files at it from this end.

use serde::Serialize;
use std::collections::HashMap;
use tokio::sync::RwLock;

/// A device stays listed while it keeps making authorised requests. The page
/// polls every 2.5s, so this is many missed polls, not a tight timeout.
const STALE_SECS: u64 = 75;

#[derive(Debug, Clone, Serialize)]
pub struct Session {
    /// What the browser calls itself — "iPhone", "Mac", and so on.
    pub device: String,
    pub ip: String,
    /// When the PIN was accepted.
    pub since: u64,
    /// Last authorised request.
    pub seen: u64,
}

#[derive(Default)]
pub struct Registry {
    map: RwLock<HashMap<String, Session>>,
}

impl Registry {
    /// The PIN was accepted. Returns true if this is a device we were not
    /// already showing, so the caller can announce it once.
    pub async fn open(&self, ip: &str, device: Option<String>, now: u64) -> bool {
        let name = name_for(device, ip);
        let mut map = self.map.write().await;

        match map.get_mut(ip) {
            // Re-entering the PIN on a device already listed is not a new
            // arrival, so do not announce it again.
            Some(existing) if now.saturating_sub(existing.seen) <= STALE_SECS => {
                existing.seen = now;
                existing.device = name;
                false
            }
            _ => {
                map.insert(
                    ip.to_string(),
                    Session {
                        device: name,
                        ip: ip.to_string(),
                        since: now,
                        seen: now,
                    },
                );
                true
            }
        }
    }

    /// Keep a session alive. Called on the page's own polling.
    pub async fn touch(&self, ip: &str, device: Option<String>, now: u64) {
        let mut map = self.map.write().await;
        if let Some(s) = map.get_mut(ip) {
            s.seen = now;
            if let Some(d) = device.filter(|d| !d.trim().is_empty()) {
                s.device = d;
            }
        }
    }

    /// Everyone still connected, most recently seen first.
    pub async fn live(&self, now: u64) -> Vec<Session> {
        let mut out: Vec<_> = self
            .map
            .read()
            .await
            .values()
            .filter(|s| now.saturating_sub(s.seen) <= STALE_SECS)
            .cloned()
            .collect();
        out.sort_by(|a, b| b.seen.cmp(&a.seen));
        out
    }

    /// Forget anything long gone, so the map does not grow forever.
    pub async fn sweep(&self, now: u64) {
        self.map
            .write()
            .await
            .retain(|_, s| now.saturating_sub(s.seen) <= STALE_SECS * 4);
    }
}

fn name_for(device: Option<String>, ip: &str) -> String {
    device
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| format!("Device at {ip}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn opening_lists_the_device() {
        let r = Registry::default();
        assert!(r.open("10.0.0.5", Some("iPhone".into()), 1000).await);

        let live = r.live(1000).await;
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].device, "iPhone");
        assert_eq!(live[0].ip, "10.0.0.5");
    }

    #[tokio::test]
    async fn nameless_devices_fall_back_to_their_address() {
        let r = Registry::default();
        r.open("10.0.0.9", None, 5).await;
        assert_eq!(r.live(5).await[0].device, "Device at 10.0.0.9");

        // An empty string is not a name either.
        r.open("10.0.0.9", Some("   ".into()), 6).await;
        assert_eq!(r.live(6).await[0].device, "Device at 10.0.0.9");
    }

    #[tokio::test]
    async fn re_entering_the_pin_is_not_a_new_arrival() {
        let r = Registry::default();
        assert!(r.open("10.0.0.5", Some("iPhone".into()), 100).await);
        assert!(!r.open("10.0.0.5", Some("iPhone".into()), 120).await);

        // Coming back is new again only once it has been quiet for the whole
        // window — measured from that second entry, which refreshed it.
        assert!(!r.open("10.0.0.5", Some("iPhone".into()), 120 + STALE_SECS).await);
        assert!(
            r.open("10.0.0.5", Some("iPhone".into()), 120 + STALE_SECS * 2 + 1)
                .await
        );
    }

    #[tokio::test]
    async fn silence_drops_it_from_the_list() {
        let r = Registry::default();
        r.open("10.0.0.5", Some("iPhone".into()), 0).await;
        assert_eq!(r.live(STALE_SECS).await.len(), 1);
        assert_eq!(r.live(STALE_SECS + 1).await.len(), 0);
    }

    #[tokio::test]
    async fn polling_keeps_it_alive() {
        let r = Registry::default();
        r.open("10.0.0.5", Some("iPhone".into()), 0).await;
        for t in (0..300).step_by(10) {
            r.touch("10.0.0.5", None, t).await;
        }
        assert_eq!(r.live(295).await.len(), 1);
    }

    #[tokio::test]
    async fn touch_updates_a_name_learned_later() {
        let r = Registry::default();
        r.open("10.0.0.5", None, 0).await;
        r.touch("10.0.0.5", Some("Shubham's iPhone".into()), 1).await;
        assert_eq!(r.live(1).await[0].device, "Shubham's iPhone");
    }

    #[tokio::test]
    async fn most_recent_first() {
        let r = Registry::default();
        r.open("10.0.0.1", Some("Mac".into()), 10).await;
        r.open("10.0.0.2", Some("iPhone".into()), 20).await;

        let live = r.live(20).await;
        assert_eq!(live[0].device, "iPhone");
        assert_eq!(live[1].device, "Mac");
    }

    #[tokio::test]
    async fn sweep_only_clears_the_long_gone() {
        let r = Registry::default();
        r.open("10.0.0.5", Some("iPhone".into()), 0).await;

        r.sweep(STALE_SECS * 2).await;
        assert_eq!(r.live(0).await.len(), 1, "recently stale is still tracked");

        r.sweep(STALE_SECS * 4 + 1).await;
        assert_eq!(r.live(0).await.len(), 0);
    }
}
