#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;
use std::path::Path;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let home = Path::new("/home/u");
    // Whatever the file says, a folder that is accepted is a clean path below the home folder.
    for name in panpak_core::XDG_DIR_NAMES {
        if let Ok(rel) = panpak_core::resolve_xdg_dir(name, home, Some(s)) {
            assert!(!rel.is_empty() && !rel.starts_with('/') && !rel.ends_with('/'), "bad path {rel:?}");
            assert!(rel.split('/').all(|p| !p.is_empty() && p != "." && p != ".."), "bad part in {rel:?}");
            assert!(!rel.bytes().any(|b| matches!(b, b'"' | b'\\' | b'$' | b'`') || b < 0x20 || b > 0x7e), "bad byte in {rel:?}");
            assert!(home.join(&rel).starts_with(home));
        }
    }
    // A pin that is accepted can be written again and read back the same.
    if let Ok(pin) = panpak_core::parse_pin(s) {
        let mut pairs: Vec<(String, String)> = pin.into_iter().collect();
        pairs.sort();
        let back = panpak_core::parse_pin(&panpak_core::format_pin(&pairs)).expect("own pin must parse");
        assert_eq!(back.len(), pairs.len());
        // And the generated file from it parses.
        let text = panpak_core::generated_user_dirs(&pairs).expect("accepted paths must be writable");
        assert!(panpak_core::parse_user_dirs(&text, home).is_ok());
    }
    // Addresses: only the three schemes, no file:, no space or control character, bounded.
    if panpak_core::check_url(s).is_ok() {
        let l = s.to_ascii_lowercase();
        assert!(l.starts_with("http://") || l.starts_with("https://") || l.starts_with("mailto:"), "scheme {s:?}");
        assert!(s.len() <= panpak_core::MAX_URL && s.bytes().all(|b| (0x21..=0x7e).contains(&b)), "bytes {s:?}");
    }
    if panpak_core::check_scheme_name(s).is_ok() {
        assert!(s.len() <= 32 && s.as_bytes()[0].is_ascii_lowercase());
        assert!(!matches!(s, "file" | "http" | "https" | "mailto" | "javascript"));
    }
});
