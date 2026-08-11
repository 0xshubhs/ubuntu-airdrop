//! Optional Cloudflare quick tunnel, so the phone can reach us off-LAN.
//!
//! Quick tunnels need no Cloudflare account, but the hostname is random and
//! changes every restart — which is exactly why the tray shows a QR code
//! instead of asking anyone to type it.

use anyhow::{bail, Result};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::RwLock;

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

/// Spawn `cloudflared` and publish the public URL into `slot` once it appears.
///
/// The URL is only printed on stderr, so we read the child's output rather
/// than polling anything.
pub async fn spawn(port: u16, slot: Arc<RwLock<Option<String>>>) -> Result<Child> {
    if !available() {
        bail!("cloudflared is not installed");
    }

    let mut child = Command::new(BIN)
        .arg("tunnel")
        .arg("--no-autoupdate")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{port}"))
        .stdout(Stdio::piped())
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
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '|' || c == '"')
        .unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches(['.', ',']).to_string();
    url.contains(".trycloudflare.com").then_some(url)
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
}
