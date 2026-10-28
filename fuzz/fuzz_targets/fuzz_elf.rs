#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // elf_needs_bytes must never panic on arbitrary input.
    let _ = panpak_core::elf_needs_bytes(data);
});
