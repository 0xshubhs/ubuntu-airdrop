//! PIN auth: signed session cookies plus per-IP throttling.
//!
//! On a LAN a bare PIN is fine. Behind a Cloudflare tunnel it is the only
//! thing between the internet and the Drop folder, and six digits is a
//! million guesses — so failures back off hard.

use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

const MAX_STRIKES: u32 = 5;
const BASE_LOCKOUT: Duration = Duration::from_secs(30);
const MAX_LOCKOUT: Duration = Duration::from_secs(15 * 60);

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `<expiry>.<hmac>` — stateless, so restarting the daemon keeps sessions
/// alive as long as the secret in the config file survives.
pub fn issue(secret: &str, ttl: u64) -> String {
    let expiry = now() + ttl;
    let sig = sign(secret, expiry);
    format!("{expiry}.{sig}")
}

pub fn verify(secret: &str, token: &str) -> bool {
    let Some((exp_raw, sig)) = token.split_once('.') else {
        return false;
    };
    let Ok(expiry) = exp_raw.parse::<u64>() else {
        return false;
    };
    if expiry < now() {
        return false;
    }
    constant_eq(&sign(secret, expiry), sig)
}

fn sign(secret: &str, expiry: u64) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac accepts any key");
    mac.update(expiry.to_string().as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Compare without leaking where the mismatch is.
pub fn constant_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Debug, Default)]
struct Strike {
    count: u32,
    locked_until: Option<Instant>,
}

/// Per-IP failed-PIN tracker with exponential lockout.
#[derive(Debug, Default)]
pub struct Throttle {
    ips: HashMap<IpAddr, Strike>,
}

impl Throttle {
    /// How long this IP must wait, if it is currently locked out.
    pub fn locked_for(&self, ip: &IpAddr) -> Option<Duration> {
        let strike = self.ips.get(ip)?;
        let until = strike.locked_until?;
        until.checked_duration_since(Instant::now())
    }

    pub fn record_failure(&mut self, ip: IpAddr) {
        let strike = self.ips.entry(ip).or_default();
        strike.count += 1;
        if strike.count >= MAX_STRIKES {
            let over = strike.count - MAX_STRIKES;
            let backoff = BASE_LOCKOUT
                .saturating_mul(1u32 << over.min(5))
                .min(MAX_LOCKOUT);
            strike.locked_until = Some(Instant::now() + backoff);
        }
    }

    pub fn record_success(&mut self, ip: &IpAddr) {
        self.ips.remove(ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_token() {
        let secret = "abc123";
        let token = issue(secret, 60);
        assert!(verify(secret, &token));
    }

    #[test]
    fn rejects_wrong_secret_and_tamper() {
        let token = issue("abc123", 60);
        assert!(!verify("different", &token));
        assert!(!verify("abc123", "9999999999.deadbeef"));
        assert!(!verify("abc123", "garbage"));
    }

    #[test]
    fn rejects_expired_token() {
        let secret = "abc123";
        let expiry = now() - 1;
        let token = format!("{expiry}.{}", sign(secret, expiry));
        assert!(!verify(secret, &token));
    }

    #[test]
    fn locks_out_after_repeated_failures() {
        let mut t = Throttle::default();
        let ip: IpAddr = "10.0.0.9".parse().unwrap();
        for _ in 0..MAX_STRIKES - 1 {
            t.record_failure(ip);
        }
        assert!(t.locked_for(&ip).is_none());
        t.record_failure(ip);
        assert!(t.locked_for(&ip).is_some());

        t.record_success(&ip);
        assert!(t.locked_for(&ip).is_none());
    }
}
