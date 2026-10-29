// SPDX-License-Identifier: MIT OR Apache-2.0
//! What an app may ask the outside to open (`xdg-open` inside the isolated view), and the names of the
//! URL schemes an app may register. The checks are strict and simple: nothing here parses a URL
//! in full, it only decides what is not passed on. Rules: PANPAK-FORMAT.md §9.3 and THREAT-MODEL.md §3.
use anyhow::{bail, ensure, Result};

/// The longest address passed on.
pub const MAX_URL: usize = 2048;

/// What kind of address was accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlKind {
    Http,
    Https,
    Mailto,
}

/// Accept `http:`, `https:` and `mailto:` addresses and nothing else. Never `file:` (it would make an
/// application outside the sandbox open a file of the system), never a path, never a program.
/// The address is printable ASCII without a space, a quote or a backslash, at most `MAX_URL` long,
/// and an `http` or `https` one has a host.
pub fn check_url(url: &str) -> Result<UrlKind> {
    ensure!(!url.is_empty() && url.len() <= MAX_URL, "address empty or longer than {MAX_URL} bytes");
    ensure!(
        url.bytes().all(|b| (0x21..=0x7e).contains(&b) && !matches!(b, b'"' | b'\\' | b'<' | b'>' | b'`')),
        "address has a space, a control character or a character that is not allowed"
    );
    let (scheme, rest) = url.split_once(':').ok_or_else(|| anyhow::anyhow!("not an address (no scheme)"))?;
    let kind = match scheme.to_ascii_lowercase().as_str() {
        "http" => UrlKind::Http,
        "https" => UrlKind::Https,
        "mailto" => UrlKind::Mailto,
        other => bail!("scheme {other} is not allowed (only http, https and mailto)"),
    };
    match kind {
        UrlKind::Mailto => check_mailto(rest)?,
        UrlKind::Http | UrlKind::Https => {
            let after = rest.strip_prefix("//").ok_or_else(|| anyhow::anyhow!("no // after the scheme"))?;
            let host = after.split(['/', '?', '#']).next().unwrap_or("");
            let host = host.rsplit('@').next().unwrap_or("");
            ensure!(!host.is_empty() && !host.starts_with(':'), "no host");
        }
    }
    Ok(kind)
}

/// The only fields of a `mailto:` address that are passed on. Mail clients take other fields (`attach`,
/// `attachment`, `x-...`) as a request to attach a local file, so every other field is refused.
const MAILTO_FIELDS: &[&str] = &["to", "cc", "bcc", "subject", "body"];

/// What follows `mailto:`: addresses, then optionally `?field=value&field=value` with only the fields
/// above. A field name is compared as written (no percent-decoding), so `%61ttach` is not `attach`.
/// A `%` must start a valid escape, `%00` is refused, and a line break may only be in the body.
fn check_mailto(rest: &str) -> Result<()> {
    ensure!(!rest.starts_with('/'), "mail address starts with /");
    let (addr, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };
    ensure!(!addr.is_empty() || query.is_some(), "empty mail address");
    ensure!(!addr.contains(['#', ';']), "mail address has a separator that is not allowed");
    escapes_ok(addr, false)?;
    if let Some(q) = query {
        ensure!(!q.is_empty() && !q.contains(['?', '#', ';']), "mail fields have a separator that is not allowed");
        let mut seen: Vec<String> = Vec::new();
        for pair in q.split('&') {
            let (name, value) = pair.split_once('=').ok_or_else(|| anyhow::anyhow!("mail field without ="))?;
            let name = name.to_ascii_lowercase();
            ensure!(MAILTO_FIELDS.contains(&name.as_str()), "mail field {name} is not allowed (only to, cc, bcc, subject, body)");
            ensure!(!seen.contains(&name), "mail field {name} twice");
            escapes_ok(value, name == "body")?;
            seen.push(name);
        }
    }
    Ok(())
}

/// Every `%` is followed by two hex digits; `%00` never; `%0A` and `%0D` only if `newlines`.
fn escapes_ok(text: &str, newlines: bool) -> Result<()> {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b.get(i + 1..i + 3).filter(|h| h.iter().all(u8::is_ascii_hexdigit));
            let Some(h) = hex else { bail!("bad percent escape") };
            let v = u8::from_str_radix(std::str::from_utf8(h)?, 16)?;
            ensure!(v != 0 && (newlines || (v != b'\n' && v != b'\r')), "control character in a mail field");
            ensure!(v >= 0x20 || v == b'\n' || v == b'\r' || v == b'\t', "control character in a mail field");
            i += 3;
        } else {
            i += 1;
        }
    }
    Ok(())
}

/// Schemes an app must not register: they have a meaning of their own, or lead to files, scripts or
/// programs.
const RESERVED: &[&str] = &[
    "http", "https", "file", "ftp", "ftps", "sftp", "ssh", "mailto", "javascript", "data", "about", "blob", "ws", "wss", "chrome",
    "view-source", "vbscript", "ms-msdt", "search-ms", "x-scheme-handler",
];

/// A scheme name the app may register (`x-scheme-handler/<name>`): lowercase letters, digits, `+`,
/// `.` and `-`, starting with a letter, at most 32 characters, and not a reserved one.
pub fn check_scheme_name(s: &str) -> std::result::Result<(), String> {
    if s.is_empty() || s.len() > 32 {
        return Err("a scheme has 1 to 32 characters".to_string());
    }
    let mut chars = s.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase()) {
        return Err("a scheme starts with a lowercase letter".to_string());
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '.' | '-')) {
        return Err("a scheme has only lowercase letters, digits, +, . and -".to_string());
    }
    if RESERVED.contains(&s) {
        return Err(format!("{s} is a reserved scheme"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_and_mail_addresses_pass() {
        for (u, k) in [
            ("https://example.org/", UrlKind::Https),
            ("http://example.org:8080/a/b?c=d#e", UrlKind::Http),
            ("HTTPS://EXAMPLE.ORG", UrlKind::Https),
            ("https://user@example.org/", UrlKind::Https),
            ("https://[::1]:3000/x", UrlKind::Https),
            ("mailto:someone@example.org", UrlKind::Mailto),
            ("mailto:someone@example.org?subject=Hi%20there", UrlKind::Mailto),
            ("mailto:a@b.org,c@d.org?cc=e@f.org&bcc=g@h.org&subject=S&body=Line1%0ALine2", UrlKind::Mailto),
            ("mailto:?to=a@b.org&subject=x", UrlKind::Mailto),
            ("MAILTO:a@b.org?Subject=x", UrlKind::Mailto),
        ] {
            assert_eq!(check_url(u).unwrap(), k, "{u}");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        for bad in [
            "",
            "file:///etc/passwd",
            "FILE:///etc/passwd",
            "file:/etc/passwd",
            "/etc/passwd",
            "../x",
            "example.org",
            "ftp://example.org/",
            "javascript:alert(1)",
            "data:text/html,hi",
            "ssh://host",
            "x-scheme-handler/https",
            "https:example.org",
            "https:///path",
            "https://",
            "https://:80/",
            "mailto:",
            "mailto:/etc",
            "https://example.org/a b",
            "https://example.org/\u{1}",
            "https://example.org/\n",
            "https://example.org/\t",
            "https://example.org/\"x",
            "https://example.org/\\x",
            "https://exämple.org/",
            "-https://example.org/",
            "--help",
            " https://example.org/",
            "https://example.org/ ",
        ] {
            assert!(check_url(bad).is_err(), "{bad:?}");
        }
        assert!(check_url(&format!("https://example.org/{}", "a".repeat(MAX_URL))).is_err());
        assert!(check_url(&format!("https://e.org/{}", "a".repeat(MAX_URL - 14))).is_ok());
    }

    #[test]
    fn mailto_takes_only_the_plain_fields() {
        for bad in [
            "mailto:a@b.org?attach=/etc/passwd",
            "mailto:a@b.org?attachment=/home/u/.ssh/id_ed25519",
            "mailto:a@b.org?subject=x&attach=file:///etc/passwd",
            "mailto:a@b.org?Attach=/etc/passwd",
            "mailto:a@b.org?%61ttach=/etc/passwd",
            "mailto:a@b.org?x-attach=/etc/passwd",
            "mailto:a@b.org?in-reply-to=x",
            "mailto:a@b.org?subject",
            "mailto:a@b.org?",
            "mailto:a@b.org?subject=x?attach=/etc/passwd",
            "mailto:a@b.org?subject=x;attach=/etc/passwd",
            "mailto:a@b.org;attach=/etc/passwd",
            "mailto:a@b.org?subject=x&subject=y",
            "mailto:a@b.org?subject=x#attach=/etc/passwd",
            "mailto:a@b.org?subject=a%0ABcc:evil@x.org",
            "mailto:a%0D%0ABcc:evil@x.org",
            "mailto:a@b.org?subject=%00",
            "mailto:a@b.org?body=%01",
            "mailto:a@b.org?body=%zz",
            "mailto:a@b.org?body=%4",
            "mailto:a@b.org?to=x&&cc=y",
        ] {
            assert!(check_url(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn scheme_names_are_strict() {
        for ok in ["audacity", "my-app", "a", "x+y.z-1", "panpaktest"] {
            assert!(check_scheme_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "1a", "-a", "A", "a b", "a_b", "a/b", "http", "https", "file", "mailto", "javascript", &"a".repeat(33), "ä"] {
            assert!(check_scheme_name(bad).is_err(), "{bad:?}");
        }
    }
}
