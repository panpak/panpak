#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Accepted by safe_path => join never escapes the base, no control character.
        if panpak_core::safe_path(s).is_ok() {
            let base = std::path::Path::new("/base");
            let joined = base.join(s);
            assert!(joined.starts_with(base), "joined path escapes base: {s:?}");
            assert!(!s.chars().any(char::is_control), "control character accepted: {s:?}");
        }
        // Accepted app id => one path component under any base.
        if panpak_core::validate_app_id(s).is_ok() {
            let base = std::path::Path::new("/base");
            assert_eq!(base.join(s).parent(), Some(base), "app id is not one component: {s:?}");
            assert!(s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_'));
            assert!(s.split('.').all(|seg| !seg.is_empty() && !seg.as_bytes()[0].is_ascii_digit()), "segment starts with a digit: {s:?}");
        }
        let _ = panpak_core::target_escapes(s);
    }
});
