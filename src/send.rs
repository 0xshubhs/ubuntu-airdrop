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

/// How long to wait for the receiver to answer before giving up.
const APPROVAL_WAIT: Duration = Duration::from_secs(120);

pub async fn send(target: &str, files: &[PathBuf], pin: &str, wait: Duration) -> Result<()> {
    for f in files {
        if !f.is_file() {
            bail!("not a file: {}", f.display());
        }
    }

    let hostport = resolve(target, wait).await?;
    let me = crate::config::hostname();
    let client = reqwest::Client::new();

    // Announce first. The receiver sees "<device> wants to send N files" and
    // decides; nothing moves until they accept.
    let manifest: Vec<serde_json::Value> = files
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.file_name().map(|s| s.to_string_lossy().to_string())
                          .unwrap_or_else(|| "untitled".into()),
                "size": p.metadata().map(|m| m.len()).unwrap_or(0),
            })
        })
        .collect();

    let offered = client
        .post(format!("http://{hostport}/api/offer"))
        .header("X-Drop-Pin", pin)
        .header("X-Drop-Device", &me)
        .json(&serde_json::json!({"device": me, "files": manifest}))
        .send()
        .await
        .context("could not reach the receiver")?;

    let mut offer_id = None;
    if offered.status() == reqwest::StatusCode::UNAUTHORIZED {
        bail!("rejected: wrong PIN");
    }
    if offered.status().is_success() {
        let body: serde_json::Value = offered.json().await.unwrap_or_default();
        let id = body
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .context("receiver returned no offer id")?;

        println!("   waiting for {target} to accept…");
        let deadline = std::time::Instant::now() + APPROVAL_WAIT;
        loop {
            if std::time::Instant::now() > deadline {
                bail!("timed out waiting for a decision");
            }
            tokio::time::sleep(Duration::from_millis(700)).await;

            let status: serde_json::Value = client
                .get(format!("http://{hostport}/api/offer/{id}"))
                .header("X-Drop-Pin", pin)
                .send()
                .await?
                .json()
                .await
                .unwrap_or_default();

            match status.get("status").and_then(|v| v.as_str()) {
                Some("accepted") => break,
                Some("declined") => bail!("declined by the receiver"),
                Some("expired") => bail!("the offer expired without an answer"),
                _ => continue,
            }
        }
        offer_id = Some(id);
    }
    // A receiver too old to know about offers 404s; fall through and just send.

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

    let url = match &offer_id {
        Some(id) => format!("http://{hostport}/api/upload?offer={id}"),
        None => format!("http://{hostport}/api/upload"),
    };

    let res = client
        .post(url)
        .header("X-Drop-Pin", pin)
        .header("X-Drop-Device", &me)
        .multipart(form)
        .send()
        .await
        .context("upload failed")?;

    if res.status() == reqwest::StatusCode::UNAUTHORIZED {
        bail!("rejected: wrong PIN");
    }
    if res.status() == reqwest::StatusCode::ACCEPTED {
        println!("   sent — waiting for the receiver to accept");
        return Ok(());
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

/// Push a snippet instead of a file. The receiver saves it and puts it on
/// their clipboard, after the same accept prompt a file gets.
pub async fn send_text(target: &str, text: &str, pin: &str, wait: Duration) -> Result<()> {
    if text.trim().is_empty() {
        bail!("nothing to send");
    }

    let hostport = resolve(target, wait).await?;
    let me = crate::config::hostname();
    let client = reqwest::Client::new();

    let res = client
        .post(format!("http://{hostport}/api/text"))
        .header("X-Drop-Pin", pin)
        .header("X-Drop-Device", &me)
        .json(&serde_json::json!({"device": me, "text": text}))
        .send()
        .await
        .context("could not reach the receiver")?;

    match res.status() {
        reqwest::StatusCode::UNAUTHORIZED => bail!("rejected: wrong PIN"),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE => bail!("too long — send it as a file"),
        s if s == reqwest::StatusCode::OK => {
            println!("   sent");
            return Ok(());
        }
        s if s != reqwest::StatusCode::ACCEPTED => bail!("server said {s}"),
        _ => {}
    }

    let body: serde_json::Value = res.json().await.unwrap_or_default();
    let Some(id) = body.get("offer").and_then(|v| v.as_str()) else {
        bail!("receiver returned no offer id");
    };

    println!("   waiting for {target} to accept…");
    let deadline = std::time::Instant::now() + APPROVAL_WAIT;
    loop {
        if std::time::Instant::now() > deadline {
            bail!("timed out waiting for a decision");
        }
        tokio::time::sleep(Duration::from_millis(700)).await;

        let status: serde_json::Value = client
            .get(format!("http://{hostport}/api/offer/{id}"))
            .header("X-Drop-Pin", pin)
            .send()
            .await?
            .json()
            .await
            .unwrap_or_default();

        match status.get("status").and_then(|v| v.as_str()) {
            Some("accepted") | Some("complete") => {
                println!("   sent");
                return Ok(());
            }
            Some("declined") => bail!("declined by the receiver"),
            Some("expired") => bail!("the offer expired without an answer"),
            _ => continue,
        }
    }
}
