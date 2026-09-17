//! Optional Cloudflare quick tunnel, so the phone can reach us off-LAN.
//!
//! Quick tunnels need no Cloudflare account, but the hostname is random and
//! changes every restart — which is exactly why the tray shows a QR code
//! instead of asking anyone to type it.

use anyhow::{bail, Result};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, RwLock};

pub const BIN: &str = "cloudflared";

pub fn available() -> bool {
    which(BIN).is_some()
}

fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|p| p.is_file())
}

/// Keep a tunnel up for as long as the switch says it should be.
///
/// cloudflared gives up and exits on its own — most often because the daemon
/// starts at login a moment before the network does — and nothing used to
/// notice: the process was left a zombie, and the tray went on offering an
/// address that had never worked. So watch the child, drop its URL the moment
/// it dies, and start another one after a widening pause.
pub async fn supervise(
    port: u16,
    slot: Arc<RwLock<Option<String>>>,
    start_enabled: bool,
    mut switch: mpsc::Receiver<bool>,
) {
    const FIRST_RETRY: Duration = Duration::from_secs(2);
    const LONGEST_RETRY: Duration = Duration::from_secs(60);

    let mut child: Option<Child> = None;
    let mut enabled = start_enabled;
    let mut retry = FIRST_RETRY;
    let mut pause: Option<Duration> = None;

    loop {
        if enabled && child.is_none() {
            // Sit out the backoff, but stay ready for the switch.
            if let Some(delay) = pause.take() {
                tokio::select! {
                    next = switch.recv() => match next {
                        Some(v) => {
                            enabled = v;
                            continue;
                        }
                        None => break,
                    },
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            match spawn(port, slot.clone()).await {
                Ok(c) => child = Some(c),
                // No cloudflared to run: retrying would only fill the journal.
                // Wait for someone to turn the switch again.
                Err(e) => {
                    eprintln!("  tunnel: {e}");
                    enabled = false;
                }
            }
        }

        if !enabled {
            if let Some(mut c) = child.take() {
                let _ = c.kill().await;
            }
            *slot.write().await = None;
            retry = FIRST_RETRY;
            pause = None;
        }

        let next = match child.as_mut() {
            Some(c) => tokio::select! {
                next = switch.recv() => next,
                gone = c.wait() => {
                    let how = match gone {
                        Ok(status) => status.to_string(),
                        Err(e) => e.to_string(),
                    };
                    child = None;
                    *slot.write().await = None;
                    eprintln!("  tunnel: {BIN} stopped ({how}) — retrying in {}s", retry.as_secs());
                    pause = Some(retry);
                    retry = (retry * 2).min(LONGEST_RETRY);
                    continue;
                }
            },
            None => switch.recv().await,
        };
        match next {
            Some(v) => enabled = v,
            // Nothing is left to flip the switch; kill_on_drop takes the
            // tunnel down with us.
            None => break,
        }
    }
}

/// Spawn `cloudflared` and publish the public URL into `slot` once it appears.
///
/// The URL is only printed on stderr, so we read the child's output rather
/// than polling anything.
pub async fn spawn(port: u16, slot: Arc<RwLock<Option<String>>>) -> Result<Child> {
    if !available() {
        bail!("cloudflared is not installed");
    }

    // Until this tunnel says otherwise there is no public address: the last
    // one died with the last child.
    *slot.write().await = None;

    let mut child = Command::new(BIN)
        .arg("tunnel")
        .arg("--no-autoupdate")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{port}"))
        // Everything worth reading is on stderr, and a pipe nobody drains
        // would eventually wedge the child.
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let stderr = child.stderr.take().expect("piped stderr");
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(url) = extract_url(&line) {
                *slot.write().await = Some(url.clone());
                eprintln!("  tunnel {url}");
            }
        }
    });

    Ok(child)
}

/// Pull `https://<something>.trycloudflare.com` out of a log line.
fn extract_url(line: &str) -> Option<String> {
    line.match_indices("https://")
        .find_map(|(at, scheme)| quick_tunnel_host(&line[at + scheme.len()..]))
        .map(|host| format!("https://{host}"))
}

/// The hostname at the head of `rest`, if it is a quick tunnel's own.
///
/// cloudflared names other Cloudflare addresses as it goes — the terms of
/// service, and `https://api.trycloudflare.com/tunnel` in the error it prints
/// when it cannot get a tunnel at all. Matching those hands the phone a QR
/// code for a page that was never ours, so insist on the real shape: one
/// label under trycloudflare.com, and no path.
fn quick_tunnel_host(rest: &str) -> Option<&str> {
    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, '|' | '"' | '\\' | '\'' | ',' | ')'))
        .unwrap_or(rest.len());
    let host = rest[..end].trim_end_matches('.');
    let label = host.strip_suffix(".trycloudflare.com")?;
    (!label.is_empty() && !label.contains(['.', '/', '@']) && label != "api").then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_url_in_banner_line() {
        let line = "|  https://neat-words-here.trycloudflare.com    |";
        assert_eq!(
            extract_url(line).as_deref(),
            Some("https://neat-words-here.trycloudflare.com")
        );
    }

    #[test]
    fn ignores_unrelated_urls() {
        assert_eq!(extract_url("see https://developers.cloudflare.com/x"), None);
        assert_eq!(extract_url("no url at all"), None);
    }

    /// The line that used to put an API endpoint on the tray and in the QR.
    #[test]
    fn ignores_the_api_endpoint_in_a_failure() {
        let line = r#"ERR Failed to request quick Tunnel error="Post \"https://api.trycloudflare.com/tunnel\": dial tcp: lookup api.trycloudflare.com: no such host""#;
        assert_eq!(extract_url(line), None);
    }

    #[test]
    fn ignores_a_tunnel_url_with_a_path() {
        assert_eq!(
            extract_url("GET https://neat-words-here.trycloudflare.com/api/status 200"),
            None
        );
    }

    /// The terms-of-service banner comes first and mentions two other URLs.
    #[test]
    fn skips_earlier_urls_on_the_same_line() {
        let line = "INF terms https://www.cloudflare.com/website-terms/ tunnel https://calm-river.trycloudflare.com done";
        assert_eq!(
            extract_url(line).as_deref(),
            Some("https://calm-river.trycloudflare.com")
        );
    }
}
