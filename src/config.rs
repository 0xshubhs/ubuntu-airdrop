//! On-disk configuration, shared by the daemon, the tray and the CLI.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const SERVICE_TYPE: &str = "_mydrop._tcp.local.";
pub const DEFAULT_PORT: u16 = 8420;

/// Session cookie name and lifetime.
pub const COOKIE: &str = "drop_session";
pub const SESSION_SECS: u64 = 60 * 60 * 12;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Six digits. Guards every route, not just uploads.
    pub pin: String,
    pub port: u16,
    /// Where incoming files land.
    pub dir: PathBuf,
    /// Advertised device name.
    pub name: String,
    /// Move each file to `move_target` once it finishes, clearing `dir`.
    #[serde(default)]
    pub auto_move: bool,
    pub move_target: PathBuf,
    /// Expose over a Cloudflare tunnel. Off by default: it puts this on the
    /// public internet, where the PIN is the only thing standing in the way.
    #[serde(default)]
    pub tunnel: bool,
    /// Hold incoming files until they are accepted, instead of writing
    /// straight into `dir`. This is the AirDrop behaviour.
    #[serde(default = "yes")]
    pub require_approval: bool,
    /// HMAC key for session cookies. Regenerating it logs everyone out.
    pub secret: String,
}

impl Default for Config {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Config {
            pin: random_pin(),
            port: DEFAULT_PORT,
            dir: home.join("Drop"),
            name: hostname(),
            auto_move: false,
            move_target: dirs::download_dir().unwrap_or_else(|| home.join("Downloads")),
            tunnel: false,
            require_approval: true,
            secret: random_hex(32),
        }
    }
}

fn yes() -> bool {
    true
}

impl Config {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("drop")
            .join("config.json")
    }

    /// Read the config, creating it with fresh secrets on first run.
    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            let cfg = Config::default();
            cfg.save()?;
            return Ok(cfg);
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config = serde_json::from_str(&raw)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, body)?;
        // The PIN and the cookie key live here.
        restrict(&path);
        Ok(())
    }

    pub fn local_url(&self) -> String {
        format!("http://{}:{}", crate::net::lan_ip(), self.port)
    }
}

#[cfg(unix)]
fn restrict(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) {}

pub fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .and_then(|s| s.split('.').next().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "drop".into())
}

pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("system rng");
    hex::encode(buf)
}

pub fn random_pin() -> String {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf).expect("system rng");
    format!("{:06}", u32::from_le_bytes(buf) % 1_000_000)
}
