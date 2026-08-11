//! `drop send` — push files to another Drop device.

use anyhow::{bail, Context, Result};
use reqwest::multipart::{Form, Part};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Accept `host:port`, a bare IP, a URL, or a peer name to resolve over mDNS.
pub async fn resolve(target: &str, wait: Duration) -> Result<String> {
    let looks_numeric = target
        .split(':')
        .next()
        .map(|h| h.split('.').count() == 4 && h.split('.').all(|o| o.parse::<u8>().is_ok()))
        .unwrap_or(false);

    if looks_numeric || target.starts_with("http") {
        let hostport = target.rsplit("//").next().unwrap_or(target).to_string();
        return Ok(if hostport.contains(':') {
            hostport
        } else {
            format!("{hostport}:{}", crate::config::DEFAULT_PORT)
        });
    }

    let me = crate::config::hostname();
    let peers = crate::discovery::snapshot(me, wait).await?;
    let needle = target.to_lowercase();

    let found = peers
        .iter()
        .find(|p| p.label.to_lowercase() == needle)
        .or_else(|| peers.iter().find(|p| p.label.to_lowercase().contains(&needle)));

    match found {
        Some(p) => {
            println!("-> {} at {}:{}", p.label, p.host, p.port);
            Ok(format!("{}:{}", p.host, p.port))
        }
        None => bail!("no peer matching '{target}'. try: drop peers"),
    }
}

pub async fn send(target: &str, files: &[PathBuf], pin: &str, wait: Duration) -> Result<()> {
    for f in files {
        if !f.is_file() {
            bail!("not a file: {}", f.display());
        }
    }

    let hostport = resolve(target, wait).await?;
    let mut form = Form::new();

    for path in files {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled".into());
        let bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        form = form.part(
            "files",
            Part::bytes(bytes)
                .file_name(name)
                .mime_str("application/octet-stream")?,
        );
    }

    let res = reqwest::Client::new()
        .post(format!("http://{hostport}/api/upload"))
        .header("X-Drop-Pin", pin)
        .multipart(form)
        .send()
        .await
        .context("upload failed")?;

    if res.status() == reqwest::StatusCode::UNAUTHORIZED {
        bail!("rejected: wrong PIN");
    }
    if !res.status().is_success() {
        bail!("server said {}", res.status());
    }

    let body: serde_json::Value = res.json().await.unwrap_or_default();
    if let Some(saved) = body.get("saved").and_then(|v| v.as_array()) {
        for name in saved {
            println!("   sent {}", name.as_str().unwrap_or("?"));
        }
    }
    Ok(())
}

pub fn expand(raw: &[String]) -> Vec<PathBuf> {
    raw.iter().map(|s| Path::new(s).to_path_buf()).collect()
}
