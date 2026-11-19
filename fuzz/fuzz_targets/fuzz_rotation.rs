#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;
use panpak_core::trust::{follow, Pin, Step, Trust};

fuzz_target!(|data: &[u8]| {
    // \0-separated strings: app id, pinned root, pinned recovery, then a new root, a new recovery and up to 9 steps of five fields each (by, from, to, recovery, sig).
    let mut parts: Vec<&str> = Vec::new();
    for chunk in data.split(|b| *b == 0) {
        match std::str::from_utf8(chunk) {
            Ok(s) => parts.push(s),
            Err(_) => return,
        }
        if parts.len() == 50 {
            break;
        }
    }
    while parts.len() < 5 {
        parts.push("");
    }
    let chain: Vec<Step> = parts[5..]
        .chunks_exact(5)
        .map(|c| Step { by: c[0].to_string(), from: c[1].to_string(), to: c[2].to_string(), recovery: c[3].to_string(), sig: c[4].to_string() })
        .collect();
    let new = Trust { root: vec![parts[3].to_string()], threshold: 1, recovery: parts[4].to_string(), domain: None, chain };
    let pin = Pin { root: parts[1].to_string(), recovery: parts[2].to_string() };
    let _ = follow(parts[0], &pin, &new);
});
