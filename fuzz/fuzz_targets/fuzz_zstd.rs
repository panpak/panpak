#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let limit = u64::from_le_bytes(data[..8].try_into().unwrap());
    let comp = &data[8..];
    // Enforce the cap: if Ok, len <= limit. Never panic/OOM beyond the cap.
    if let Ok(out) = panpak_core::decode_capped(comp, limit) {
        assert!((out.len() as u64) <= limit, "cap exceeded");
    }
});
