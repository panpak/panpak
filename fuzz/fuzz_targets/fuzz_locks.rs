#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The recognisers of stale lock files must never panic, and must refuse what is not a lock.
    let _ = panpak_core::stalelocks::is_chromium_lock_target(data);
    let _ = panpak_core::stalelocks::is_firefox_lock_target(data);
    if panpak_core::stalelocks::is_qt_lock_content(data) {
        assert!(!data.contains(&0), "NUL accepted in a lock file");
    }
});
