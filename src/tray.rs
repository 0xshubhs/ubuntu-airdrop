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
pub struct OfferFile {
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PendingOffer {
    pub id: String,
    pub device: String,
    #[serde(default)]
    pub files: Vec<OfferFile>,
    #[serde(default)]
    pub total: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Status {
    pub name: String,
    pub pin: String,
    pub dir: PathBuf,
    #[serde(default)]
    pub offers: Vec<PendingOffer>,
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

        // Anything waiting on a decision goes first — it is the only thing in
        // this menu that is time-sensitive.
        for offer in &s.offers {
            let n = offer.files.len();
            items.push(
                StandardItem {
                    label: format!(
                        "{} wants to send {} {}{}",
                        offer.device,
                        n,
                        if n == 1 { "file" } else { "files" },
                        if offer.total > 0 {
                            format!("  ({})", crate::net::human_size(offer.total))
                        } else {
                            String::new()
                        }
                    ),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );

            let accept_id = offer.id.clone();
            items.push(
                StandardItem {
                    label: "    Accept".into(),
                    activate: Box::new(move |this: &mut Self| {
                        let url = this.url(&format!("/api/control/offer/{accept_id}"));
                        tokio::spawn(async move { decide(url, true).await });
                    }),
                    ..Default::default()
                }
                .into(),
            );

            let decline_id = offer.id.clone();
            items.push(
                StandardItem {
                    label: "    Decline".into(),
                    activate: Box::new(move |this: &mut Self| {
                        let url = this.url(&format!("/api/control/offer/{decline_id}"));
                        tokio::spawn(async move { decide(url, false).await });
                    }),
                    ..Default::default()
                }
                .into(),
            );
            items.push(MenuItem::Separator);
        }

        items.push(
            StandardItem {
                label: format!("Receiving as “{}”", s.name),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Open Drop window".into(),
                activate: Box::new(move |this: &mut Self| {
                    let _ = crate::panel::open(this.port);
                }),
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

        // Whichever address actually works from where the phone is wins the
        // top slot. A LAN IP is useless on mobile data.
        match &s.tunnel_url {
            Some(url) => {
                let u = url.clone();
                items.push(
                    StandardItem {
                        label: format!("Anywhere:  {}", short(&u)),
                        activate: Box::new(move |_: &mut Self| copy(&u)),
                        ..Default::default()
                    }
                    .into(),
                );
            }
            None if s.tunnel_enabled => items.push(
                StandardItem {
                    label: "Anywhere:  starting…".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            ),
            None => {}
        }

        let local = s.local_url.clone();
        items.push(
            StandardItem {
                label: format!("This network only:  {local}"),
                activate: Box::new(move |_: &mut Self| copy(&local)),
                ..Default::default()
            }
            .into(),
        );

        let qr_target = s.tunnel_url.clone().unwrap_or_else(|| s.local_url.clone());
        let qr_pin = s.pin.clone();
        let qr_is_public = s.tunnel_url.is_some();
        items.push(
            StandardItem {
                label: if qr_is_public {
                    "Show QR code…".into()
                } else {
                    "Show QR code (this network)…".to_string()
                },
                activate: Box::new(move |_: &mut Self| {
                    if let Err(e) = show_qr(&qr_target, &qr_pin) {
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

/// Draw the QR, the PIN and the URL into one image.
///
/// Built by hand rather than with `qrcode`'s SVG renderer so the PIN can sit
/// under the code — scanning and typing happen in the same glance.
///
/// This opens in whatever views images. gnome-shell draws the tray menu over
/// D-Bus, which carries labels and checkboxes but not bitmaps, and Wayland
/// won't let a client place a window next to the panel — so a popup anchored
/// to the icon isn't on the table.
pub fn show_qr(url: &str, pin: &str) -> Result<()> {
    let svg = crate::qr::svg(url, pin, 8)?;
    let path = std::env::temp_dir().join("drop-qr.svg");
    std::fs::write(&path, svg)?;
    Command::new("xdg-open").arg(&path).spawn()?;
    Ok(())
}

/// The tunnel URL, if the daemon is up and the tunnel is running.
pub async fn public_url(port: u16) -> Option<String> {
    let status: Status = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/api/status"))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    status.tunnel_url
}

/// Only one tray may run, or the panel shows two icons.
///
/// Launching Drop from the app grid when it is already running is not an
/// error — it means "show me the code", so the caller falls through to the QR.
fn claim_singleton() -> bool {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let path = std::path::Path::new(&dir).join("drop-tray.pid");

    if let Ok(existing) = std::fs::read_to_string(&path) {
        if let Ok(pid) = existing.trim().parse::<u32>() {
            // A stale PID file outlives a crash, so confirm the process is
            // both alive and actually us.
            if let Ok(cmdline) = std::fs::read_to_string(format!("/proc/{pid}/cmdline")) {
                if cmdline.contains("drop") && cmdline.contains("tray") {
                    return false;
                }
            }
        }
    }
    let _ = std::fs::write(&path, std::process::id().to_string());
    true
}

/// Bring the daemon up if it isn't. Launching the tray from the app grid
/// should get you a working Drop, not an icon reporting a dead daemon.
pub fn ensure_daemon() {
    let active = Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", "drop.service"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !active {
        let _ = Command::new("systemctl")
            .args(["--user", "start", "drop.service"])
            .status();
    }
}

pub async fn run(port: u16) -> Result<()> {
    if !claim_singleton() {
        // Already in the panel. Treat the launch as "show me the QR".
        let cfg = crate::config::Config::load()?;
        let target = public_url(port).await.unwrap_or_else(|| cfg.local_url());
        return show_qr(&target, &cfg.pin);
    }
    ensure_daemon();

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
    let mut announced: std::collections::HashSet<String> = Default::default();

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

            // An offer expires in five minutes, so it has to be noticed
            // without the menu being open.
            for offer in &s.offers {
                if announced.insert(offer.id.clone()) {
                    notify_offer(offer, port);
                }
            }
            announced.retain(|id| s.offers.iter().any(|o| &o.id == id));
        }

        handle
            .update(move |t: &mut DropTray| {
                t.status = fetched.clone();
            })
            .await;

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn decide(url: String, accept: bool) {
    let _ = reqwest::Client::new()
        .post(url)
        .json(&serde_json::json!({"accept": accept}))
        .send()
        .await;
}

/// Raise the accept/decline prompt as a desktop notification with buttons.
///
/// `notify-send --action` needs a notification daemon that supports actions
/// and blocks until one is chosen, so it runs detached; if the desktop
/// ignores the actions the menu and the window still carry the decision.
fn notify_offer(offer: &PendingOffer, port: u16) {
    let n = offer.files.len();
    let body = format!(
        "{} {} · {}",
        n,
        if n == 1 { "file" } else { "files" },
        offer
            .files
            .iter()
            .map(|f| {
                if f.size > 0 {
                    format!("{} ({})", f.name, crate::net::human_size(f.size))
                } else {
                    f.name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
            .chars()
            .take(120)
            .collect::<String>()
    );
    let summary = format!("{} wants to send you files", offer.device);
    let id = offer.id.clone();

    std::thread::spawn(move || {
        let out = Command::new("notify-send")
            .args([
                "--app-name=Drop",
                "--icon=folder-download",
                "--urgency=critical",
                "--action=accept=Accept",
                "--action=decline=Decline",
                &summary,
                &body,
            ])
            .output();

        let Ok(out) = out else { return };
        let choice = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if choice.is_empty() {
            return; // dismissed, or the desktop does not do actions
        }
        let url = format!("http://127.0.0.1:{port}/api/control/offer/{id}");
        let body = format!("{{\"accept\":{}}}", choice == "accept");
        let _ = Command::new("curl")
            .args([
                "-s", "-o", "/dev/null", "-X", "POST",
                "-H", "Content-Type: application/json",
                "-d", &body, &url,
            ])
            .status();
    });
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
