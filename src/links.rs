//! Links that arrive as files, and what is safe to open.
//!
//! An Apple device shares a link as a tiny file rather than as text: a
//! `.webloc` from macOS and iOS, a `.url` from Windows. omdrop opens those on
//! a click and refuses anything that is not http or https, because the
//! address was chosen by whoever sent it: a `file://` link in a notification
//! is a way to make someone open something on their own machine without
//! meaning to, and `smb://` reaches further than that.
//!
//! Drop takes the same line. A snippet that is nothing but a link counts too,
//! since that is how a phone usually sends one.

/// The link inside a shared file, if it holds one.
///
/// Three shapes for the price of one scan. A `.url` is an INI file with a
/// `URL=` line; a `.webloc` is a plist, XML or binary depending on the
/// version of macOS that wrote it. Rather than parse a binary plist for one
/// string, take the first http(s) run in the bytes: in a file whose entire
/// job is to hold one address, that is the address.
pub fn in_file(name: &str, body: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if !(lower.ends_with(".url") || lower.ends_with(".webloc")) {
        return None;
    }

    for line in body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("URL=").or_else(|| line.strip_prefix("url="))
            && let Some(url) = safe(rest)
        {
            return Some(url);
        }
        if let Some(inner) = between(line, "<string>", "</string>")
            && let Some(url) = safe(inner)
        {
            return Some(url);
        }
    }
    first_url(body)
}

fn between<'a>(haystack: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = haystack.find(open)? + open.len();
    let rest = &haystack[start..];
    let end = rest.find(close)?;
    Some(&rest[..end])
}

/// The first http(s) address in a blob, however it is wrapped.
fn first_url(body: &str) -> Option<String> {
    let at = body.find("https://").or_else(|| body.find("http://"))?;
    let rest = &body[at..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '\''))
        .unwrap_or(rest.len());
    safe(&rest[..end])
}

/// A snippet that is nothing but a link, which is how a phone sends one.
pub fn in_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.lines().count() != 1 {
        return None;
    }
    safe(trimmed)
}

/// `Some` only for an http or https address worth handing to a browser.
///
/// Everything else is refused by not being understood rather than by a list
/// of bad schemes, so a scheme nobody has thought of yet is refused too.
pub fn safe(raw: &str) -> Option<String> {
    let url = raw.trim().trim_end_matches(['.', ',', ')']);
    if url.len() > 2048 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    // A host, at least. "http://" on its own opens nothing.
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    (!host.is_empty()).then(|| url.to_string())
}

/// Just the host, for a notification that has one line to say it in.
pub fn host_of(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(url)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_windows_url_file() {
        let body = "[InternetShortcut]\r\nURL=https://example.com/page\r\nIconIndex=0\r\n";
        assert_eq!(
            in_file("Link.url", body).as_deref(),
            Some("https://example.com/page")
        );
    }

    #[test]
    fn reads_an_xml_webloc() {
        let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>URL</key><string>https://example.com/a</string></dict></plist>"#;
        assert_eq!(
            in_file("Shared.webloc", body).as_deref(),
            Some("https://example.com/a")
        );
    }

    /// A binary plist is mostly unreadable, but the address is still in there.
    #[test]
    fn digs_a_url_out_of_a_binary_webloc() {
        let body = "bplist00\u{1}\u{2}URL_\u{10}\u{1b}https://example.com/binary\u{8}\u{d}";
        assert_eq!(
            in_file("Shared.webloc", body).as_deref(),
            Some("https://example.com/binary")
        );
    }

    #[test]
    fn ignores_files_that_are_not_links() {
        assert_eq!(in_file("photo.jpeg", "https://example.com"), None);
        assert_eq!(in_file("notes.txt", "https://example.com"), None);
    }

    #[test]
    fn a_link_file_with_no_link_in_it_is_not_a_link() {
        assert_eq!(in_file("empty.url", "[InternetShortcut]\n"), None);
    }

    #[test]
    fn only_http_and_https_are_worth_opening() {
        assert_eq!(safe("https://example.com").as_deref(), Some("https://example.com"));
        assert_eq!(safe("http://192.168.0.5:8080/x").as_deref(), Some("http://192.168.0.5:8080/x"));
        assert_eq!(safe("file:///etc/passwd"), None);
        assert_eq!(safe("smb://nas/share"), None);
        assert_eq!(safe("javascript:alert(1)"), None);
        assert_eq!(safe("ftp://example.com"), None);
        assert_eq!(safe("https://"), None);
        assert_eq!(safe(""), None);
    }

    /// A `file://` inside a .webloc must not sneak past on the fallback scan.
    #[test]
    fn a_link_file_pointing_at_the_disk_is_refused() {
        let body = r#"<plist><dict><key>URL</key><string>file:///etc/shadow</string></dict></plist>"#;
        assert_eq!(in_file("nasty.webloc", body), None);
    }

    #[test]
    fn refuses_anything_with_whitespace_or_control_characters() {
        assert_eq!(safe("https://example.com/a b"), None);
        assert_eq!(safe("https://example.com/\u{1}"), None);
        assert_eq!(safe(&format!("https://example.com/{}", "a".repeat(3000))), None);
    }

    #[test]
    fn names_the_host_for_a_notification() {
        assert_eq!(host_of("https://example.com/a/b?c=1"), "example.com");
        assert_eq!(host_of("http://192.168.0.5:8080"), "192.168.0.5:8080");
    }

    #[test]
    fn a_snippet_that_is_only_a_link_counts() {
        assert_eq!(in_text("  https://example.com/x  ").as_deref(), Some("https://example.com/x"));
        assert_eq!(in_text("read this: https://example.com"), None);
        assert_eq!(in_text("https://example.com\nand more"), None);
    }

    #[test]
    fn trailing_sentence_punctuation_is_not_part_of_the_address() {
        assert_eq!(safe("https://example.com/x.").as_deref(), Some("https://example.com/x"));
    }
}
