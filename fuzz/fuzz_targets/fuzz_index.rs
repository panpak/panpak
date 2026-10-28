#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok((chunks, entries)) = panpak_core::parse_index(data) {
        // Roundtrip: what parses must re-encode and re-parse the same.
        let raw2 = panpak_core::encode_index(&chunks, &entries);
        let (c2, e2) = panpak_core::parse_index(&raw2).expect("index roundtrip");
        assert_eq!(chunks.len(), c2.len(), "unstable n_chunks");
        assert_eq!(entries.len(), e2.len(), "unstable n_entries");
        for (a, b) in chunks.iter().zip(c2.iter()) {
            assert_eq!(a.hash, b.hash);
            assert_eq!((a.off, a.comp, a.raw), (b.off, b.comp, b.raw));
        }
    }
});
