//! QR rendering, shared by the tray (writes a file) and the window (serves it).

use anyhow::Result;
use qrcode::{Color, QrCode};

/// Draw the QR with the PIN and URL beneath it.
///
/// Built from the module matrix rather than `qrcode`'s SVG renderer so there
/// is room for a caption — scanning and typing then happen in one glance.
pub fn svg(url: &str, pin: &str, module: usize) -> Result<String> {
    const QUIET: usize = 4;
    const PAD: usize = 20;

    let code = QrCode::new(url.as_bytes())?;
    let modules = code.width();
    let colors = code.to_colors();

    let qr_px = (modules + QUIET * 2) * module;
    let width = qr_px + PAD * 2;
    let caption = 84;
    let height = qr_px + PAD * 2 + caption;

    let mut out = String::with_capacity(modules * modules * 48);
    out.push_str(&format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">"##
    ));
    out.push_str(&format!(
        r##"<rect width="{width}" height="{height}" rx="10" fill="#ffffff"/>"##
    ));

    for (i, color) in colors.iter().enumerate() {
        if *color != Color::Dark {
            continue;
        }
        let x = (i % modules + QUIET) * module + PAD;
        let y = (i / modules + QUIET) * module + PAD;
        out.push_str(&format!(
            r##"<rect x="{x}" y="{y}" width="{module}" height="{module}" fill="#000000"/>"##
        ));
    }

    let mid = width / 2;
    let pin_y = qr_px + PAD + 28;
    out.push_str(&format!(
        r##"<text x="{mid}" y="{pin_y}" text-anchor="middle" font-family="monospace" font-size="32" font-weight="bold" letter-spacing="6" fill="#16181A">{}</text>"##,
        escape(pin)
    ));
    out.push_str(&format!(
        r##"<text x="{mid}" y="{}" text-anchor="middle" font-family="monospace" font-size="12" fill="#6E6C66">{}</text>"##,
        pin_y + 24,
        escape(url)
    ));
    out.push_str("</svg>");
    Ok(out)
}

fn escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_contains_the_pin() {
        let out = svg("http://192.168.0.1:8420", "481902", 8).unwrap();
        assert!(out.starts_with("<svg"));
        assert!(out.ends_with("</svg>"));
        assert!(out.contains("481902"));
    }

    #[test]
    fn escapes_a_url_with_query_chars() {
        let out = svg("http://x/?a=1&b=2", "000000", 6).unwrap();
        assert!(out.contains("&amp;"));
    }
}
