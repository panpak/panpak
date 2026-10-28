#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let size = u64::from_le_bytes(data[..8].try_into().unwrap());
    let body = &data[8..];
    // Exact length: first 80 bytes.
    if body.len() >= 80 {
        if let Ok(ft) = panpak_core::parse_footer(size, &body[..80]) {
            let _ = panpak_core::check_layout(size, &ft);
        }
    }
    // Wrong lengths: must fail with an error, never panic.
    let _ = panpak_core::parse_footer(size, body);
});
