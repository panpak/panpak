#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Accepted by safe_path => join never escapes the base.
        if panpak_core::safe_path(s).is_ok() {
            let base = std::path::Path::new("/base");
            let joined = base.join(s);
            assert!(joined.starts_with(base), "joined path escapes base: {s:?}");
        }
        let _ = panpak_core::target_escapes(s);
    }
});
