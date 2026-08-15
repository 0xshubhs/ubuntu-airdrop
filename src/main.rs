//! Drop — AirDrop-ish file transfer for your own LAN.

mod auth;
mod config;
mod discovery;
mod net;
mod offers;
mod page;
mod panel;
mod popover;
mod qr;
mod send;
mod server;
mod tray;
mod tunnel;

use anyhow::Result;
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

fn secs(v: f32) -> Duration {
    Duration::from_millis((v.max(0.1) * 1000.0) as u64)
}
