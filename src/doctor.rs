//! `drop doctor`: what is in the way, most fundamental first.
//!
//! Taken from omdrop's `omdrop-discoverable probe`, which answers the only
//! question that matters when a phone cannot see your computer: which link
//! in the chain is broken. Every check here runs as you, without root, and
//! says what to do rather than only that something is wrong.

use crate::config::Config;
use std::fmt;
use std::path::Path;
use std::process::{Command, Stdio};

/// One thing that was looked at.
pub struct Finding {
    pub name: &'static str,
    pub verdict: Verdict,
    pub say: String,
    /// What to run, when there is something to run.
    pub fix: Option<String>,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Verdict {
    /// Working.
    Good,
    /// Not working, and it stops Drop doing its job.
    Blocked,
    /// Worth knowing, but Drop still works.
    Note,
}

impl Verdict {
    fn mark(&self) -> &'static str {
        match self {
            Verdict::Good => "ok  ",
            Verdict::Blocked => "no  ",
            Verdict::Note => "hm  ",
        }
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "  {}{:<10} {}",
            self.verdict.mark(),
            self.name,
            self.say
        )?;
        if let Some(fix) = &self.fix {
            write!(f, "\n                 {fix}")?;
        }
        Ok(())
    }
}

fn good(name: &'static str, say: impl Into<String>) -> Finding {
    Finding {
        name,
        verdict: Verdict::Good,
        say: say.into(),
        fix: None,
    }
}

fn blocked(name: &'static str, say: impl Into<String>, fix: Option<&str>) -> Finding {
    Finding {
        name,
        verdict: Verdict::Blocked,
        say: say.into(),
        fix: fix.map(|f| f.to_string()),
    }
}

fn note(name: &'static str, say: impl Into<String>, fix: Option<&str>) -> Finding {
    Finding {
        name,
        verdict: Verdict::Note,
        say: say.into(),
        fix: fix.map(|f| f.to_string()),
    }
}

fn have(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|dir| {
                dir.join(bin)
                    .metadata()
                    .map(|m| m.is_file() || m.is_symlink())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Is a unit running? `is-active` answers for a user service without root.
fn unit_active(scope: &str, unit: &str) -> bool {
    Command::new("systemctl")
        .args([scope, "is-active", "--quiet", unit])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn tray_running() -> bool {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let Ok(pid) = std::fs::read_to_string(Path::new(&dir).join("drop-tray.pid")) else {
        return false;
    };
    let Ok(pid) = pid.trim().parse::<u32>() else {
        return false;
    };
    std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
        .map(|c| c.contains("drop") && c.contains("tray"))
        .unwrap_or(false)
}

/// Can we write where files are meant to land?
fn writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".drop-doctor-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"x").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Everything, in the order a transfer needs it.
pub async fn run(cfg: &Config) -> Vec<Finding> {
    let mut out = Vec::new();

    // 1. The daemon. Nothing else matters if it is not up.
    let status = crate::tray::status(cfg.port).await;
    match &status {
        Some(_) => out.push(good("daemon", format!("answering on 127.0.0.1:{}", cfg.port))),
        None => out.push(blocked(
            "daemon",
            format!("nothing is answering on 127.0.0.1:{}", cfg.port),
            Some("systemctl --user start drop.service"),
        )),
    }

    // 2. Somewhere to put what arrives.
    if writable(&cfg.dir) {
        let free = crate::disk::free_bytes(&cfg.dir)
            .map(|b| format!(", {} free", crate::net::human_size(b)))
            .unwrap_or_default();
        out.push(good("folder", format!("{}{free}", cfg.dir.display())));
    } else {
        out.push(blocked(
            "folder",
            format!("{} cannot be written to", cfg.dir.display()),
            Some(&format!("mkdir -p {} && chmod u+w $_", cfg.dir.display())),
        ));
    }

    // 3. An address a phone could type.
    let ip = crate::net::lan_ip();
    if ip == "127.0.0.1" {
        out.push(blocked(
            "address",
            "no network address, so only this machine can reach Drop",
            Some("connect to Wi-Fi or plug in a cable"),
        ));
    } else {
        out.push(good("address", format!("http://{ip}:{}", cfg.port)));
    }

    // 4. Whether the window is open at all. The commonest reason a phone
    //    sees nothing, now that there is a window to close.
    match &status {
        // A daemon from before the window says nothing about one, and serde
        // reads that silence as "off". Say what is actually true instead.
        Some(s) if s.visible_for.is_empty() => out.push(note(
            "receiving",
            "this daemon is older than the receive window, so it is always on",
            Some("systemctl --user restart drop.service"),
        )),
        Some(s) if s.receiving => {
            let how = match s.remaining {
                Some(left) => format!("on, {} left", crate::window::countdown(left)),
                None if s.once => "on, until one file arrives".into(),
                None => "on".into(),
            };
            out.push(good("receiving", how));
        }
        Some(s) => out.push(blocked(
            "receiving",
            if s.off_because.is_empty() || s.off_because == "off" {
                "off, so arrivals are refused and nothing is advertised".into()
            } else {
                format!("off, {}", s.off_because)
            },
            Some("drop on"),
        )),
        None => {}
    }

    // 5. A firewall will not say so itself. We cannot read ufw's rules
    //    without root, so this is a caution rather than a verdict.
    if unit_active("--system", "ufw.service") {
        out.push(note(
            "firewall",
            "ufw is running; if the phone cannot reach you, the port is the first suspect",
            Some(&format!("sudo ufw allow {}/tcp", cfg.port)),
        ));
    } else {
        out.push(good("firewall", "no ufw in the way"));
    }

    // 6. mDNS is how the other machines find this one.
    if unit_active("--system", "avahi-daemon.service") {
        out.push(good("mdns", "avahi is running, so names resolve on the LAN"));
    } else {
        out.push(note(
            "mdns",
            "avahi is not running; Drop still advertises, but .local names may not resolve",
            Some("sudo systemctl enable --now avahi-daemon"),
        ));
    }

    // 7. The indicator, which is where most people drive this from.
    if tray_running() {
        out.push(good("tray", "the indicator is up"));
    } else {
        out.push(note("tray", "no indicator running", Some("drop tray &")));
    }

    // 8. The tunnel, only when it is meant to be on.
    if cfg.tunnel {
        match (have(crate::tunnel::BIN), status.as_ref().and_then(|s| s.tunnel_url.clone())) {
            (false, _) => out.push(blocked(
                "tunnel",
                "turned on, but cloudflared is not installed",
                Some("sudo apt install cloudflared"),
            )),
            (true, Some(url)) => out.push(good("tunnel", url)),
            (true, None) => out.push(note(
                "tunnel",
                "turned on, waiting for an address",
                Some("journalctl --user -u drop -n 20"),
            )),
        }
    }

    // 9. The optional helpers, in one line rather than four.
    let missing: Vec<&str> = [
        ("notify-send", "notifications"),
        ("zenity", "the file picker"),
        ("xdg-open", "opening what arrives"),
    ]
    .iter()
    .filter(|(bin, _)| !have(bin))
    .map(|(_, what)| *what)
    .collect();
    let clipboard = have("wl-copy") || have("xclip");
    if missing.is_empty() && clipboard {
        out.push(good("helpers", "notifications, picker and clipboard all present"));
    } else {
        let mut lacking = missing;
        if !clipboard {
            lacking.push("the clipboard");
        }
        out.push(note(
            "helpers",
            format!("no {}", lacking.join(", no ")),
            Some("sudo apt install libnotify-bin zenity wl-clipboard"),
        ));
    }

    out
}

/// What to print under the list: the first thing worth doing, or nothing.
pub fn verdict(findings: &[Finding]) -> String {
    let blocked = findings
        .iter()
        .find(|f| f.verdict == Verdict::Blocked);
    match blocked {
        Some(f) => format!("\n  {} is what to fix first.\n", f.name),
        None if findings.iter().any(|f| f.verdict == Verdict::Note) => {
            "\n  Nothing is in the way.\n".to_string()
        }
        None => "\n  All clear.\n".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &'static str, verdict: Verdict) -> Finding {
        Finding {
            name,
            verdict,
            say: "whatever".into(),
            fix: None,
        }
    }

    #[test]
    fn names_the_first_blocker() {
        let list = vec![
            f("daemon", Verdict::Good),
            f("folder", Verdict::Blocked),
            f("address", Verdict::Blocked),
        ];
        assert!(verdict(&list).contains("folder is what to fix first"));
    }

    #[test]
    fn a_note_is_not_a_blocker() {
        let list = vec![f("daemon", Verdict::Good), f("firewall", Verdict::Note)];
        assert!(verdict(&list).contains("Nothing is in the way"));
    }

    #[test]
    fn all_good_says_so() {
        let list = vec![f("daemon", Verdict::Good)];
        assert!(verdict(&list).contains("All clear"));
    }

    #[test]
    fn a_finding_prints_its_fix_under_itself() {
        let found = Finding {
            name: "daemon",
            verdict: Verdict::Blocked,
            say: "nothing is answering".into(),
            fix: Some("systemctl --user start drop.service".into()),
        };
        let shown = found.to_string();
        assert!(shown.contains("no  daemon"), "{shown}");
        assert!(shown.lines().count() == 2, "the fix goes on its own line");
    }

    #[test]
    fn finds_a_binary_that_is_certainly_there() {
        assert!(have("sh"), "/bin/sh is on every PATH worth having");
        assert!(!have("drop-definitely-not-a-real-binary"));
    }

    #[test]
    fn a_writable_folder_reports_itself_writable() {
        let dir = std::env::temp_dir().join(format!("drop-doctor-test-{}", std::process::id()));
        assert!(writable(&dir));
        assert!(!writable(Path::new("/proc/nope/not/here")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
