//! Transfer offers — the "iPhone wants to send 3 files. Accept / Decline?" step.
//!
//! Two ways in, because a sender that can only make one HTTP request (the iOS
//! Shortcut) cannot offer, poll, then upload:
//!
//!   negotiated  POST /api/offer -> wait for Accept -> POST /api/upload?offer=id
//!               Nothing is transferred until the receiver says yes.
//!
//!   staged      POST /api/upload with no offer. The bytes stream into a hidden
//!               staging folder and an offer is raised for them. They only reach
//!               the Drop folder on Accept, and are deleted on Decline.
//!
//! Either way nothing lands in the Drop folder without a decision.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;

/// How long a pending offer waits for a decision before it is dropped.
pub const TTL_SECS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Accepted,
    Declined,
    Expired,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferFile {
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Offer {
    pub id: String,
    pub device: String,
    pub files: Vec<OfferFile>,
    pub total: u64,
    pub status: Status,
    pub created: u64,
    /// Set when the bytes are already on disk waiting for a verdict.
    pub staged: Option<PathBuf>,
    /// A pasted snippet rather than a file. Held here so that accepting it
    /// can put it on the clipboard as well as saving it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl Offer {
    pub fn expired(&self, now: u64) -> bool {
        self.status == Status::Pending && now.saturating_sub(self.created) > TTL_SECS
    }
}

#[derive(Default)]
pub struct Registry {
    map: RwLock<HashMap<String, Offer>>,
}

impl Registry {
    pub async fn create(
        &self,
        device: String,
        files: Vec<OfferFile>,
        staged: Option<PathBuf>,
    ) -> Offer {
        self.create_inner(device, files, staged, None).await
    }

    /// A text snippet, offered the same way a file is so that it goes through
    /// the same accept prompt.
    pub async fn create_text(
        &self,
        device: String,
        file: OfferFile,
        staged: PathBuf,
        text: String,
    ) -> Offer {
        self.create_inner(device, vec![file], Some(staged), Some(text))
            .await
    }

    async fn create_inner(
        &self,
        device: String,
        files: Vec<OfferFile>,
        staged: Option<PathBuf>,
        text: Option<String>,
    ) -> Offer {
        let total = files.iter().map(|f| f.size).sum();
        let offer = Offer {
            id: crate::config::random_hex(8),
            device,
            files,
            total,
            status: Status::Pending,
            created: crate::auth::now(),
            staged,
            text,
        };
        self.map.write().await.insert(offer.id.clone(), offer.clone());
        offer
    }

    pub async fn get(&self, id: &str) -> Option<Offer> {
        let now = crate::auth::now();
        let mut map = self.map.write().await;
        let offer = map.get_mut(id)?;
        if offer.expired(now) {
            offer.status = Status::Expired;
        }
        Some(offer.clone())
    }

    /// Record a verdict. Returns the offer as it now stands.
    pub async fn decide(&self, id: &str, accept: bool) -> Option<Offer> {
        let mut map = self.map.write().await;
        let offer = map.get_mut(id)?;
        if offer.status != Status::Pending {
            return Some(offer.clone());
        }
        offer.status = if accept {
            Status::Accepted
        } else {
            Status::Declined
        };
        Some(offer.clone())
    }

    pub async fn set_status(&self, id: &str, status: Status) {
        if let Some(offer) = self.map.write().await.get_mut(id) {
            offer.status = status;
        }
    }

    /// Offers still awaiting a decision, oldest first.
    pub async fn pending(&self) -> Vec<Offer> {
        let now = crate::auth::now();
        let mut out: Vec<Offer> = self
            .map
            .read()
            .await
            .values()
            .filter(|o| o.status == Status::Pending && !o.expired(now))
            .cloned()
            .collect();
        out.sort_by_key(|o| o.created);
        out
    }

    /// Drop stale entries. Returns staging folders whose offers died
    /// undecided, so the caller can delete the bytes.
    pub async fn sweep(&self) -> Vec<PathBuf> {
        let now = crate::auth::now();
        let mut orphans = Vec::new();
        let mut map = self.map.write().await;

        for offer in map.values_mut() {
            if offer.expired(now) {
                offer.status = Status::Expired;
            }
        }
        map.retain(|_, offer| {
            let done = matches!(
                offer.status,
                Status::Declined | Status::Expired | Status::Complete
            );
            // Keep finished offers around briefly so a polling sender can see
            // the verdict rather than a 404.
            let stale = now.saturating_sub(offer.created) > TTL_SECS * 2;
            if done && stale {
                if let Some(dir) = &offer.staged {
                    orphans.push(dir.clone());
                }
                return false;
            }
            if matches!(offer.status, Status::Declined | Status::Expired) {
                if let Some(dir) = offer.staged.take() {
                    orphans.push(dir);
                }
            }
            true
        });
        orphans
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<OfferFile> {
        vec![
            OfferFile { name: "a.jpg".into(), size: 100 },
            OfferFile { name: "b.jpg".into(), size: 200 },
        ]
    }

    #[tokio::test]
    async fn create_then_accept() {
        let reg = Registry::default();
        let o = reg.create("iPhone".into(), files(), None).await;
        assert_eq!(o.status, Status::Pending);
        assert_eq!(o.total, 300);

        let decided = reg.decide(&o.id, true).await.unwrap();
        assert_eq!(decided.status, Status::Accepted);
        assert!(reg.pending().await.is_empty());
    }

    #[tokio::test]
    async fn decline_is_final() {
        let reg = Registry::default();
        let o = reg.create("Mac".into(), files(), None).await;
        reg.decide(&o.id, false).await;
        // A second verdict must not flip a decided offer.
        let again = reg.decide(&o.id, true).await.unwrap();
        assert_eq!(again.status, Status::Declined);
    }

    #[tokio::test]
    async fn declining_releases_staged_bytes() {
        let reg = Registry::default();
        let dir = PathBuf::from("/tmp/drop-test-stage");
        let o = reg.create("iPhone".into(), files(), Some(dir.clone())).await;
        reg.decide(&o.id, false).await;
        assert_eq!(reg.sweep().await, vec![dir]);
    }

    #[tokio::test]
    async fn pending_lists_only_undecided() {
        let reg = Registry::default();
        let a = reg.create("A".into(), files(), None).await;
        let _b = reg.create("B".into(), files(), None).await;
        reg.decide(&a.id, true).await;
        let pending = reg.pending().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].device, "B");
    }
}
