#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The signature block of a package: any bytes give a signature or an error, never a panic.
    let _ = panpak_core::parse_sig_block(data);
    // A block that is one good entry followed by the input: the first signature is the one that counts.
    let mut b = panpak_core::encode_sig_block(&[7u8; 64]);
    b.extend_from_slice(data);
    let _ = panpak_core::parse_sig_block(&b);
});
