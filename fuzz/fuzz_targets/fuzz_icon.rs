#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

// The icon check and the cleaning read a file that a package or a packager supplies. Whatever the bytes are, it neither panics nor reads
// more than it should, and a file it accepts is under the size limit, starts with the PNG signature and has a side
// inside the limits.
fuzz_target!(|data: &[u8]| {
    if let Ok(icon) = panpak_icon::check(data) {
        assert!(data.len() <= panpak_icon::MAX_FILE);
        assert!(data.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!((panpak_icon::MIN_SIDE..=panpak_icon::MAX_SIDE).contains(&icon.side));
        // The text chunks are never accepted.
        assert!(!data.windows(4).any(|w| w == b"tEXt" || w == b"iCCP" || w == b"zTXt" || w == b"iTXt"));
        // A file that is acceptable already is its own normalization.
        let n = panpak_icon::normalize(data).expect("an acceptable file normalizes");
        assert!(n.data == data && n.dropped.is_empty());
    }
    // The cleaning: whatever it makes is acceptable, is stable under a second pass, and holds only the five kinds of chunk.
    if let Ok(n) = panpak_icon::normalize(data) {
        assert!(panpak_icon::check(&n.data).is_ok());
        let again = panpak_icon::normalize(&n.data).expect("a normalized file normalizes");
        assert!(again.data == n.data && again.dropped.is_empty());
        let mut pos = 8;
        while pos + 12 <= n.data.len() {
            let len = u32::from_be_bytes(n.data[pos..pos + 4].try_into().unwrap()) as usize;
            assert!(matches!(&n.data[pos + 4..pos + 8], b"IHDR" | b"PLTE" | b"tRNS" | b"IDAT" | b"IEND"));
            pos += 12 + len;
        }
        assert_eq!(pos, n.data.len());
    }
});
