#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;
use panpak_core::ChunkDecoder;
use std::cell::RefCell;

thread_local! {
    // One decoder for the whole run: earlier inputs must not affect later ones.
    static REUSED: RefCell<ChunkDecoder> = RefCell::new(ChunkDecoder::new().unwrap());
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let limit = u64::from_le_bytes(data[..8].try_into().unwrap());
    let comp = &data[8..];
    // Enforce the cap: if Ok, len <= limit. Never panic/OOM beyond the cap.
    let fresh = panpak_core::decode_capped(comp, limit);
    if let Ok(out) = &fresh {
        assert!((out.len() as u64) <= limit, "cap exceeded");
    }
    // The reused decoder must agree with a fresh one.
    let mut out = Vec::new();
    let reused = REUSED.with(|d| d.borrow_mut().decode(comp, limit, &mut out));
    assert_eq!(reused.is_ok(), fresh.is_ok(), "reused decoder disagrees");
    if let Ok(f) = fresh {
        assert_eq!(out, f, "reused decoder output differs");
    }
});
