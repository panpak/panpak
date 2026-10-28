#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 24 {
        return;
    }
    let start = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let end = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let body_len = u64::from_le_bytes(data[16..24].try_into().unwrap()) as usize;
    let header = match std::str::from_utf8(&data[24..]) {
        Ok(s) => s,
        Err(_) => return,
    };
    // Property: if Ok, body_len was exactly end-start+1 (checked, no wrap).
    if panpak_core::check_range_parts(start, end, header, body_len).is_ok() {
        match end.checked_sub(start).and_then(|n| n.checked_add(1)) {
            Some(exp) => assert_eq!(body_len as u64, exp),
            None => assert_eq!(body_len as u64, u64::MAX),
        }
    }
});
