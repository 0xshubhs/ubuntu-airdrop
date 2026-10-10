//! Drop — AirDrop-ish file transfer for your own LAN.

mod auth;
mod config;
mod discovery;
mod disk;
mod doctor;
mod net;
mod offers;
mod page;
mod panel;
mod popover;
mod qr;
mod send;
mod server;
mod sessions;
mod tray;
mod tunnel;
mod window;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::Config;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "drop", version, about = "AirDrop-ish file transfer over your LAN")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Receive files (this is what the systemd service runs)
    Serve {
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
        /// Fixed PIN instead of the stored one
        #[arg(long)]
        pin: Option<String>,
    },
    /// Start receiving: for a while ("15m"), for one file ("once"), or
    /// until you say otherwise ("always"). Bare numbers are minutes.
    On {
        /// How long, just this once. Without it, the stored choice is used.
        span: Option<String>,
    },
    /// Stop receiving now
    Off,
    /// Set the name other devices see
    Name {
        name: String,
    },
    /// Set where received files land
    Dir {
        path: String,
    },
    /// Largest transfer, as a percentage of the free space (0 lifts it)
    Limit {
        pct: u8,
    },
    /// Set how long receiving stays on from now on
    Visible {
        /// "15m", "once", "always", or a bare number of minutes
        span: String,
    },
    /// Show the indicator in the top-right of the desktop
    Tray,
    /// List devices advertising right now
    Peers {
        #[arg(long, default_value = "2")]
        wait: f32,
    },
    /// Push files, or a snippet with --text, to a peer
    Send {
        /// Peer name, IP, or host:port
        to: String,
        files: Vec<String>,
        /// Send this text instead of files. "-" reads stdin.
        #[arg(long, conflicts_with = "files")]
        text: Option<String>,
        #[arg(long)]
        pin: Option<String>,
        #[arg(long, default_value = "2")]
        wait: f32,
    },
    /// Open the Drop window
    Panel,
    /// Open the small popover under the tray icon
    Popover,
    /// Show the QR code and PIN for pairing a phone
    Qr,
    /// Open the Drop folder in the file manager
    Open,
    /// Print this device's PIN and address
    Status,
    /// Check what is standing in the way of a transfer
    Doctor,
    /// Generate a new PIN, invalidating existing sessions
    Pin,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Serve {
            port,
            name,
            dir,
            pin,
        } => {
            let mut cfg = Config::load()?;
            if let Some(p) = port {
                cfg.port = p;
            }
            if let Some(n) = name {
                cfg.name = n;
            }
            if let Some(d) = dir {
                cfg.dir = d;
            }
            if let Some(p) = pin {
                cfg.pin = p;
            }
            cfg.save()?;
            server::serve(cfg).await
        }

        Cmd::On { span } => {
            let cfg = Config::load()?;
            tray::ensure_daemon();
            let body = match span.as_deref() {
                Some(raw) => {
                    let span = window::Span::parse(raw).with_context(|| {
                        format!("{raw:?} is not a length of time. Try 15m, 90s, 2h, once or always.")
                    })?;
                    serde_json::json!({"enabled": true, "span": span.spec()})
                }
                None => serde_json::json!({"enabled": true}),
            };
            control(cfg.port, "/api/control/receiving", body).await?;
            report(cfg.port).await
        }

        Cmd::Off => {
            let cfg = Config::load()?;
            control(
                cfg.port,
                "/api/control/receiving",
                serde_json::json!({"enabled": false}),
            )
            .await?;
            report(cfg.port).await
        }

        Cmd::Name { name } => {
            let cfg = Config::load()?;
            tray::ensure_daemon();
            control(cfg.port, "/api/control/name", serde_json::json!({"name": name})).await?;
            println!("  they see you as {}", name.trim());
            Ok(())
        }

        Cmd::Dir { path } => {
            let cfg = Config::load()?;
            tray::ensure_daemon();
            let out = control(cfg.port, "/api/control/dir", serde_json::json!({"path": path})).await?;
            println!(
                "  saving to {}",
                out.get("dir").and_then(|d| d.as_str()).unwrap_or(&path)
            );
            Ok(())
        }

        Cmd::Limit { pct } => {
            let cfg = Config::load()?;
            tray::ensure_daemon();
            let out = control(cfg.port, "/api/control/limit", serde_json::json!({"pct": pct})).await?;
            match out.get("limit_bytes").and_then(|b| b.as_u64()) {
                Some(bytes) => println!(
                    "  largest transfer {pct}% of what is free ({})",
                    net::human_size(bytes)
                ),
                None => println!("  largest transfer: no cap"),
            }
            Ok(())
        }

        Cmd::Visible { span } => {
            let cfg = Config::load()?;
            let span = window::Span::parse(&span).with_context(|| {
                format!("{span:?} is not a length of time. Try 15m, 90s, 2h, once or always.")
            })?;
            tray::ensure_daemon();
            control(
                cfg.port,
                "/api/control/visible-for",
                serde_json::json!({"spec": span.spec()}),
            )
            .await?;
            println!("  stay visible for: {span}");
            report(cfg.port).await
        }

        Cmd::Tray => {
            let cfg = Config::load()?;
            tray::run(cfg.port).await
        }

        Cmd::Peers { wait } => {
            let me = config::hostname();
            println!("listening for {wait:.0}s ...");
            let peers = discovery::snapshot(me, secs(wait)).await?;
            if peers.is_empty() {
                println!("nothing found. is drop running on the other machine?");
            }
            for p in peers {
                println!("  {:<22} {}:{}", p.label, p.host, p.port);
            }
            Ok(())
        }

        Cmd::Send {
            to,
            files,
            text,
            pin,
            wait,
        } => {
            if files.is_empty() && text.is_none() {
                anyhow::bail!("give me at least one file, or --text");
            }
            // Without --pin, assume the peer shares our PIN (common when both
            // ends are yours); otherwise the receiver rejects it and says so.
            let pin = match pin {
                Some(p) => p,
                None => Config::load()?.pin,
            };

            match text {
                Some(t) => {
                    let body = if t == "-" {
                        std::io::read_to_string(std::io::stdin())?
                    } else {
                        t
                    };
                    send::send_text(&to, &body, &pin, secs(wait)).await
                }
                None => send::send(&to, &send::expand(&files), &pin, secs(wait)).await,
            }
        }

        Cmd::Panel => {
            let cfg = Config::load()?;
            tray::ensure_daemon();
            panel::open(cfg.port)
        }

        Cmd::Popover => {
            let cfg = Config::load()?;
            tray::ensure_daemon();
            popover::open(cfg.port, false)
        }

        Cmd::Qr => {
            let cfg = Config::load()?;
            // Ask the daemon for the tunnel URL; fall back to the LAN address
            // when it is off or unreachable.
            let target = tray::public_url(cfg.port)
                .await
                .unwrap_or_else(|| cfg.local_url());
            println!("  {target}");
            println!("  PIN {}", cfg.pin);
            tray::show_qr(&target, &cfg.pin)
        }

        Cmd::Open => {
            let cfg = Config::load()?;
            std::fs::create_dir_all(&cfg.dir)?;
            std::process::Command::new("xdg-open").arg(&cfg.dir).spawn()?;
            Ok(())
        }

        Cmd::Status => {
            let cfg = Config::load()?;
            println!("  {}", cfg.name);
            println!("  {}", cfg.local_url());
            println!("  saving to {}", cfg.dir.display());
            println!("  PIN {}", cfg.pin);
            print!("  receiving  ");
            match tray::status(cfg.port).await {
                Some(s) => println!("{}", receiving_line(&s)),
                None => println!("daemon not running"),
            }
            println!("  visible for: {}", cfg.span());
            match disk::budget_for(&cfg.dir, cfg.limit_pct) {
                Some(bytes) => println!(
                    "  largest transfer  {}% of what is free ({})",
                    cfg.limit_pct,
                    net::human_size(bytes)
                ),
                None => println!("  largest transfer  no cap"),
            }
            println!(
                "  auto-move  {}",
                if cfg.auto_move {
                    format!("on -> {}", cfg.move_target.display())
                } else {
                    "off".into()
                }
            );
            println!(
                "  tunnel     {}",
                if cfg.tunnel { "on" } else { "off" }
            );
            Ok(())
        }

        Cmd::Doctor => {
            let cfg = Config::load()?;
            println!("\n  Drop {}  on  {}\n", env!("CARGO_PKG_VERSION"), cfg.name);
            let findings = doctor::run(&cfg).await;
            for f in &findings {
                println!("{f}");
            }
            print!("{}", doctor::verdict(&findings));
            Ok(())
        }

        Cmd::Pin => {
            let mut cfg = Config::load()?;
            cfg.pin = config::random_pin();
            cfg.secret = config::random_hex(32);
            cfg.save()?;
            println!("  PIN {}", cfg.pin);
            println!("  restart the daemon: systemctl --user restart drop");
            Ok(())
        }
    }
}

/// Post to the daemon's own control API, which is where the window lives:
/// the tray and the panel go through the same routes.
async fn control(port: u16, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
    let reply = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}{path}"))
        .json(&body)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .with_context(|| "the Drop daemon is not answering (systemctl --user start drop)")?;
    let ok = reply.status().is_success();
    let value: serde_json::Value = reply.json().await.unwrap_or_default();
    if !ok {
        let say = value
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("the daemon refused that");
        anyhow::bail!("{say}");
    }
    Ok(value)
}

/// "on, 14:32 left" and the like, for the CLI.
fn receiving_line(s: &tray::Status) -> String {
    match (s.receiving, s.remaining) {
        (true, Some(left)) => format!("on, {} left", window::countdown(left)),
        (true, None) if s.once => "on, until one file arrives".into(),
        (true, None) => "on".into(),
        // "off (off)" says nothing twice; a window that ran out does.
        (false, _) if !s.off_because.is_empty() && s.off_because != "off" => {
            format!("off, {}", s.off_because)
        }
        (false, _) => "off".into(),
    }
}

/// What the window looks like after a change, read back from the daemon so
/// the CLI reports what is true rather than what it asked for.
async fn report(port: u16) -> Result<()> {
    match tray::status(port).await {
        Some(s) => println!("  receiving {}", receiving_line(&s)),
        None => println!("  the daemon is not answering"),
    }
    Ok(())
}

fn secs(v: f32) -> Duration {
    Duration::from_millis((v.max(0.1) * 1000.0) as u64)
}
