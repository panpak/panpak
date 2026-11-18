#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // 5 \0-separated strings: app id, pinned, new, rotation_old, rotation_sig.
    let mut parts: Vec<&str> = Vec::new();
    for chunk in data.split(|b| *b == 0) {
        match std::str::from_utf8(chunk) {
            Ok(s) => parts.push(s),
            Err(_) => return,
        }
        if parts.len() == 5 {
            break;
        }
    }
    while parts.len() < 5 {
        parts.push("");
    }
    let _ = panpak_core::verify_rotation(parts[0], parts[1], parts[2], Some(parts[3]), Some(parts[4]));
    let _ = panpak_core::verify_rotation(parts[0], parts[1], parts[2], None, None);
});
