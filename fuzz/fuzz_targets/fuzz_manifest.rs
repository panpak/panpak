#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let Ok(man) = toml::from_str::<panpak_core::Manifest>(s) else { return };
        // Whatever the manifest says, an accepted one has fields that are safe as
        // file names, paths, .desktop text and terminal text.
        if panpak_core::validate_manifest(&man).is_ok() {
            let id = &man.app.id;
            assert!(id.len() <= panpak_core::MAX_APP_ID_LEN && id.is_ascii() && id.contains('.'));
            assert!(!id.starts_with('.') && !id.ends_with('.') && !id.contains("..") && !id.contains('/'));
            assert!(id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_'));
            assert!(id.split('.').all(|s| !s.is_empty() && !s.as_bytes()[0].is_ascii_digit()), "segment starts with a digit: {id:?}");
            let base = std::path::Path::new("/base");
            assert_eq!(base.join(id).parent(), Some(base), "id is not one path component: {id:?}");
            assert!(!man.app.name.chars().any(char::is_control), "control character in name");
            assert!(!man.app.version.is_empty() && !man.app.version.contains('/'));
            let entry = panpak_core::safe_path(&man.app.entry).expect("accepted entry must be a safe path");
            assert!(base.join(entry).starts_with(base));
        }
        // The id rule alone agrees with the manifest rule.
        if panpak_core::validate_app_id(&man.app.id).is_err() {
            assert!(panpak_core::validate_manifest(&man).is_err());
        }
    }
});
