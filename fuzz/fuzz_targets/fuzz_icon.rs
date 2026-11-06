#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

// The icon check reads a file that a package supplies. Whatever the bytes are, it neither panics nor reads
// more than it should, and a file it accepts is under the size limit, starts with the PNG signature and has a side
// inside the limits.
fuzz_target!(|data: &[u8]| {
    if let Ok(icon) = panpak_icon::check(data) {
        assert!(data.len() <= panpak_icon::MAX_FILE);
        assert!(data.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!((panpak_icon::MIN_SIDE..=panpak_icon::MAX_SIDE).contains(&icon.side));
        // The text chunks are never accepted.
        assert!(!data.windows(4).any(|w| w == b"tEXt" || w == b"iCCP" || w == b"zTXt" || w == b"iTXt"));
    }
});
