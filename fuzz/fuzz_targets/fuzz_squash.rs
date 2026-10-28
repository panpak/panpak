#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Parsing squashfs v4 from arbitrary offsets: never panic, only Result.
    // Find hsqs candidates as `panpak convert` does and try parsing them.
    let mut i = 0;
    while i + 4 <= data.len() {
        if &data[i..i + 4] == b"hsqs" {
            let cur = std::io::Cursor::new(&data[i..]);
            let reader = std::io::BufReader::new(cur);
            if let Ok(fs) = backhand::FilesystemReader::from_reader_with_offset(reader, 0) {
                // Iterate a few nodes: exercise inode/directory/fragment.
                for (n, node) in fs.files().enumerate() {
                    if n > 32 {
                        break;
                    }
                    let _ = node.fullpath.as_os_str().len();
                }
            }
            // Skip ahead to avoid retrying the same offset forever.
            i += 4;
        } else {
            i += 1;
        }
        if i > 1 << 20 {
            break;
        }
    }
});
