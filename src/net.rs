//! Small networking and filename helpers.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};

/// Best-guess outward-facing IPv4. Opens a UDP socket but sends nothing.
pub fn lan_ip() -> String {
    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(_) => return "127.0.0.1".into(),
    };
    // TEST-NET-1. Never routed, so nothing leaves the machine; connect() just
    // makes the kernel pick the interface it would use.
    if sock.connect("192.0.2.1:53").is_err() {
        return "127.0.0.1".into();
    }
    match sock.local_addr() {
        Ok(SocketAddr::V4(a)) => a.ip().to_string(),
        _ => "127.0.0.1".into(),
    }
}

pub fn is_loopback(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(a) => a.is_loopback(),
        IpAddr::V6(a) => a.is_loopback(),
    }
}

/// Strip directories, drop anything exotic, and never overwrite.
pub fn safe_name(raw: &str, folder: &Path) -> PathBuf {
    let base = Path::new(raw)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");

    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " .-_()[]".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(['.', ' ']).to_string();
    let cleaned = if cleaned.is_empty() {
        "untitled".to_string()
    } else {
        cleaned
    };

    let target = folder.join(&cleaned);
    if !target.exists() {
        return target;
    }

    let path = Path::new(&cleaned);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    let ext = path.extension().and_then(|s| s.to_str());

    for n in 2..10_000 {
        let candidate = match ext {
            Some(e) => folder.join(format!("{stem} ({n}).{e}")),
            None => folder.join(format!("{stem} ({n})")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    folder.join(format!("{stem}-{}", crate::config::random_hex(4)))
}

pub fn human_size(n: u64) -> String {
    const KB: f64 = 1024.0;
    let n = n as f64;
    if n < KB {
        return format!("{n:.0} B");
    }
    for (limit, unit) in [(KB * KB, "KB"), (KB * KB * KB, "MB")] {
        if n < limit {
            return format!("{:.1} {}", n / (limit / KB), unit);
        }
    }
    format!("{:.1} GB", n / (KB * KB * KB))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_scales() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn safe_name_strips_traversal() {
        let dir = std::env::temp_dir();
        let got = safe_name("../../etc/passwd", &dir);
        assert_eq!(got.parent().unwrap(), dir);
        assert_eq!(got.file_name().unwrap(), "passwd");
    }

    #[test]
    fn safe_name_rejects_empty() {
        let dir = std::env::temp_dir();
        assert_eq!(safe_name("...", &dir).file_name().unwrap(), "untitled");
        assert_eq!(safe_name("", &dir).file_name().unwrap(), "untitled");
    }
}
