//! The top-right indicator.
//!
//! Runs as its own process, started with the desktop session, and talks to
//! the daemon over loopback. Keeping it separate means the daemon can start
//! at boot — long before there is a session bus for a tray to live on.

use anyhow::Result;
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{MenuItem, Tray, TrayMethods};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Status {
    pub name: String,
    pub pin: String,
    pub dir: PathBuf,
    pub local_url: String,
    pub tunnel_enabled: bool,
    pub tunnel_url: Option<String>,
    pub auto_move: bool,
    pub move_target: PathBuf,
    pub received: u64,
    pub peers: usize,
    pub files_waiting: usize,
}

#[derive(Debug)]
pub struct DropTray {
    pub port: u16,
    pub status: Option<Status>,
}

impl DropTray {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }
}

impl Tray for DropTray {
    fn id(&self) -> String {
        "drop".into()
    }

    fn title(&self) -> String {
        match &self.status {
            Some(s) => format!("Drop — {}", s.name),
            None => "Drop — not running".into(),
        }
    }

    fn icon_name(&self) -> String {
        match &self.status {
            Some(s) if s.files_waiting > 0 => "folder-download".into(),
            Some(_) => "network-transmit-receive".into(),
            None => "network-offline".into(),
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let description = match &self.status {
            Some(s) => format!("{}\nPIN {}\n{} waiting", s.local_url, s.pin, s.files_waiting),
            None => "Daemon not reachable".into(),
        };
        ksni::ToolTip {
            title: "Drop".into(),
            description,
            icon_name: self.icon_name(),
            icon_pixmap: Vec::new(),
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let Some(s) = self.status.clone() else {
            return vec![
                StandardItem {
                    label: "Daemon not running".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: "Start it".into(),
                    activate: Box::new(|_: &mut Self| {
                        let _ = Command::new("systemctl")
                            .args(["--user", "start", "drop.service"])
                            .status();
                    }),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: "Quit".into(),
                    activate: Box::new(|_: &mut Self| std::process::exit(0)),
                    ..Default::default()
                }
                .into(),
            ];
        };

        let mut items: Vec<MenuItem<Self>> = Vec::new();

        items.push(
            StandardItem {
                label: format!("Receiving as “{}”", s.name),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);

        // The PIN, which is the whole point of clicking here.
        let pin = s.pin.clone();
        items.push(
            StandardItem {
                label: format!("PIN  {pin}"),
                activate: Box::new(move |_: &mut Self| copy(&pin)),
                ..Default::default()
            }
            .into(),
        );

        let local = s.local_url.clone();
        items.push(
            StandardItem {
                label: local.clone(),
                activate: Box::new(move |_: &mut Self| copy(&local)),
                ..Default::default()
            }
            .into(),
        );

        match &s.tunnel_url {
            Some(url) => {
                let u = url.clone();
                items.push(
                    StandardItem {
                        label: format!("Internet: {}", short(&u)),
                        activate: Box::new(move |_: &mut Self| copy(&u)),
                        ..Default::default()
                    }
                    .into(),
                );
            }
            None if s.tunnel_enabled => items.push(
                StandardItem {
                    label: "Internet: starting…".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            ),
            None => {}
        }

        let qr_target = s.tunnel_url.clone().unwrap_or_else(|| s.local_url.clone());
        items.push(
            StandardItem {
                label: "Show QR code…".into(),
                activate: Box::new(move |_: &mut Self| {
                    if let Err(e) = show_qr(&qr_target) {
                        eprintln!("qr: {e}");
                    }
                }),
                ..Default::default()
            }
            .into(),
        );

        items.push(MenuItem::Separator);

        items.push(
            StandardItem {
                label: match s.files_waiting {
                    0 => "Drop folder is empty".into(),
                    1 => "1 file waiting".to_string(),
                    n => format!("{n} files waiting"),
                },
                enabled: false,
                ..Default::default()
            }
            .into(),
        );

        if s.peers > 0 {
            items.push(
                StandardItem {
                    label: match s.peers {
                        1 => "1 other device nearby".to_string(),
                        n => format!("{n} other devices nearby"),
                    },
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }

        let dir = s.dir.clone();
        items.push(
            StandardItem {
                label: "Open Drop folder".into(),
                activate: Box::new(move |_: &mut Self| {
                    let _ = Command::new("xdg-open").arg(&dir).spawn();
                }),
                ..Default::default()
            }
            .into(),
        );

        let target_name = s
            .move_target
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Downloads".into());

        items.push(
            StandardItem {
                label: format!("Move all to {target_name}"),
                enabled: s.files_waiting > 0,
                activate: Box::new(move |this: &mut Self| {
                    let url = this.url("/api/control/move-all");
                    tokio::spawn(async move {
                        let _ = reqwest::Client::new().post(url).send().await;
                    });
                }),
                ..Default::default()
            }
            .into(),
        );

        items.push(
            CheckmarkItem {
                label: format!("Always move to {target_name}"),
                checked: s.auto_move,
                activate: Box::new(move |this: &mut Self| {
                    let next = !this.status.as_ref().map(|s| s.auto_move).unwrap_or(false);
                    if let Some(st) = this.status.as_mut() {
                        st.auto_move = next;
                    }
                    let url = this.url("/api/control/auto-move");
                    tokio::spawn(async move {
                        let _ = reqwest::Client::new()
                            .post(url)
                            .json(&serde_json::json!({"enabled": next}))
                            .send()
                            .await;
                    });
                }),
                ..Default::default()
            }
            .into(),
        );

        items.push(MenuItem::Separator);

        items.push(
            CheckmarkItem {
                label: "Reachable from the internet".into(),
                checked: s.tunnel_enabled,
                activate: Box::new(move |this: &mut Self| {
                    let next = !this
                        .status
                        .as_ref()
                        .map(|s| s.tunnel_enabled)
                        .unwrap_or(false);
                    if let Some(st) = this.status.as_mut() {
                        st.tunnel_enabled = next;
                    }
                    let url = this.url("/api/control/tunnel");
                    tokio::spawn(async move {
                        let _ = reqwest::Client::new()
                            .post(url)
                            .json(&serde_json::json!({"enabled": next}))
                            .send()
                            .await;
                    });
                }),
                ..Default::default()
            }
            .into(),
        );

        items.push(
            StandardItem {
                label: "New PIN".into(),
                activate: Box::new(move |this: &mut Self| {
                    let url = this.url("/api/control/pin");
                    tokio::spawn(async move {
                        let _ = reqwest::Client::new().post(url).send().await;
                    });
                }),
                ..Default::default()
            }
            .into(),
        );

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|_: &mut Self| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        );

        items
    }
}

fn short(url: &str) -> String {
    url.trim_start_matches("https://").to_string()
}

/// Wayland first, then X11. Both are optional packages, so failing is fine —
/// the PIN is legible in the menu regardless.
fn copy(text: &str) {
    use std::io::Write;
    use std::process::Stdio;

    for cmd in [("wl-copy", vec![]), ("xclip", vec!["-selection", "clipboard"])] {
        let Ok(mut child) = Command::new(cmd.0)
            .args(cmd.1)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
        return;
    }
}

/// Render the URL as an SVG QR and hand it to the desktop's image viewer.
fn show_qr(url: &str) -> Result<()> {
    use qrcode::render::svg;
    use qrcode::QrCode;

    let code = QrCode::new(url.as_bytes())?;
    let svg = code
        .render()
        .min_dimensions(320, 320)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();

    let path = std::env::temp_dir().join("drop-qr.svg");
    std::fs::write(&path, svg)?;
    Command::new("xdg-open").arg(&path).spawn()?;
    Ok(())
}

pub async fn run(port: u16) -> Result<()> {
    let tray = DropTray {
        port,
        status: None,
    };
    let handle = tray.spawn().await?;
    let client = reqwest::Client::new();
    let endpoint = format!("http://127.0.0.1:{port}/api/status");

    // Seed from the daemon's current count so we don't announce a backlog
    // that arrived before the tray started.
    let mut seen: Option<u64> = None;

    loop {
        let fetched: Option<Status> = match client
            .get(&endpoint)
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r.json().await.ok(),
            _ => None,
        };

        if let Some(s) = &fetched {
            match seen {
                None => seen = Some(s.received),
                Some(prev) if s.received > prev => {
                    notify(s.received - prev, &s.dir);
                    seen = Some(s.received);
                }
                _ => {}
            }
        }

        handle
            .update(move |t: &mut DropTray| {
                t.status = fetched.clone();
            })
            .await;

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Shell out to `notify-send` rather than link a notification library:
/// the blocking D-Bus clients panic when called from inside a tokio runtime.
fn notify(count: u64, dir: &std::path::Path) {
    let body = match count {
        1 => "1 new file in ".to_string() + &dir.display().to_string(),
        n => format!("{n} new files in {}", dir.display()),
    };
    let _ = Command::new("notify-send")
        .args(["--app-name=Drop", "--icon=folder-download", "Drop", &body])
        .spawn();
}
