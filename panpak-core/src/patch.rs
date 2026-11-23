// SPDX-License-Identifier: MIT OR Apache-2.0
//! Patches: a file of the new version written as a zstd frame that uses the same file of the previous version as its dictionary (PANPAK-FORMAT.md §4.1).
//!
//! The index of the new version signs the hash of the patch, the hash of the file it starts from and the place of the patch in the patches file, so the
//! bytes that reach the decompressor have a signed hash. The rebuilt file is checked against the chunks of the index in any case.

use anyhow::{ensure, Context, Result};
use zstd::stream::raw::{CParameter, DParameter, Decoder, Encoder, InBuffer, Operation, OutBuffer};

/// A file smaller than this is not worth a patch.
pub const PATCH_MIN_FILE: u64 = 64 << 10;
/// The window of a patch is at most 2^28 bytes: the file it starts from and the file it makes, together, fit in it.
pub const PATCH_WINDOW_LOG_MAX: u32 = 28;
/// A longer patch is refused.
pub const MAX_PATCH_LEN: u32 = 1 << 30;

/// Where a patch is and what it starts from (the fields of the file entry, PANPAK-FORMAT.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchRef {
    /// BLAKE3 of the bytes of the file of the previous version that the patch starts from.
    pub base: [u8; 32],
    /// Where the patch is in the patches file, and its length.
    pub off: u64,
    pub len: u32,
    /// BLAKE3 of the bytes of the patch, as they are in the patches file.
    pub hash: [u8; 32],
}

/// The window log that a patch from `base` bytes to `out` bytes needs, or `None` when it would be above the limit (the file is then sent as chunks).
pub fn window_log(base: u64, out: u64) -> Option<u32> {
    let total = base.checked_add(out)?;
    let log = (64 - total.saturating_sub(1).leading_zeros()).max(10);
    (log <= PATCH_WINDOW_LOG_MAX).then_some(log)
}

/// The memory that this process can still use: what the system says is available, or less when the control group of the process has a limit (a scope, a container).
pub fn available_memory() -> Option<u64> {
    let kib = |s: &str, key: &str| -> Option<u64> { s.lines().find_map(|l| l.strip_prefix(key)).and_then(|r| r.trim().trim_end_matches("kB").trim().parse::<u64>().ok()).map(|k| k << 10) };
    let sys = kib(&std::fs::read_to_string("/proc/meminfo").ok()?, "MemAvailable:");
    let cg = std::fs::read_to_string("/proc/self/cgroup").ok().and_then(|c| {
        let rel = c.lines().find_map(|l| l.strip_prefix("0::"))?.trim().to_string();
        let dir = std::path::Path::new("/sys/fs/cgroup").join(rel.trim_start_matches('/'));
        let max = std::fs::read_to_string(dir.join("memory.max")).ok()?.trim().parse::<u64>().ok()?;
        let cur = std::fs::read_to_string(dir.join("memory.current")).ok()?.trim().parse::<u64>().ok()?;
        Some(max.saturating_sub(cur))
    });
    match (sys, cg) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Is there room to make or use a patch between files of these sizes? A system that does not say how much is free is trusted.
pub fn enough_memory(base: u64, out: u64, available: Option<u64>) -> bool {
    available.is_none_or(|a| a >= memory_needed(base, out).saturating_add(256 << 20))
}

/// The memory that making or using a patch needs, with a margin: the base, the output of the decoder and the window, twice over.
pub fn memory_needed(base: u64, out: u64) -> u64 {
    (base + out).saturating_mul(2)
}

/// The patch that makes `new` from `base`.
pub fn make(base: &[u8], new: &[u8], level: i32) -> Result<Vec<u8>> {
    let log = window_log(base.len() as u64, new.len() as u64).context("the files are too large for a patch")?;
    let mut enc = Encoder::with_ref_prefix(level, base)?;
    enc.set_parameter(CParameter::WindowLog(log))?;
    enc.set_parameter(CParameter::EnableLongDistanceMatching(true))?;
    enc.set_pledged_src_size(Some(new.len() as u64))?;
    let mut out = vec![0u8; 1 << 16];
    let mut filled = 0usize;
    let mut input = InBuffer::around(new);
    while input.pos < new.len() {
        if out.len() - filled < 1 << 16 {
            out.resize(out.len() * 2, 0);
        }
        let mut dst = OutBuffer::around_pos(&mut out[..], filled);
        enc.run(&mut input, &mut dst)?;
        filled = dst.pos();
    }
    loop {
        if out.len() - filled < 1 << 16 {
            out.resize(out.len() * 2, 0);
        }
        let mut dst = OutBuffer::around_pos(&mut out[..], filled);
        let left = enc.finish(&mut dst, true)?;
        filled = dst.pos();
        if left == 0 {
            break;
        }
    }
    out.truncate(filled);
    Ok(out)
}

/// Decompress `patch` (a zstd frame that starts from `base`) and give the bytes to `sink`, in pieces. The output is exactly `out_len` bytes: more is refused as it
/// comes, less is an error at the end. The caller has checked the hash of the patch and the hash of `base`.
pub fn apply(base: &[u8], patch: &[u8], out_len: u64, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
    window_log(base.len() as u64, out_len).context("the files are too large for a patch")?;
    let mut dec = Decoder::with_ref_prefix(base).context("start the patch decoder")?;
    dec.set_parameter(DParameter::WindowLogMax(PATCH_WINDOW_LOG_MAX))?;
    let mut buf = vec![0u8; 1 << 17];
    let mut input = InBuffer::around(patch);
    let mut made = 0u64;
    let mut ended = false;
    while !ended {
        let mut dst = OutBuffer::around(&mut buf[..]);
        let hint = dec.run(&mut input, &mut dst).context("the patch is damaged")?;
        let n = dst.pos();
        made += n as u64;
        ensure!(made <= out_len, "the patch makes more than the {out_len} bytes of the file");
        if n > 0 {
            sink(&buf[..n])?;
        }
        if hint == 0 {
            ended = true;
        } else if n == 0 && input.pos >= patch.len() {
            anyhow::bail!("the patch is cut short");
        }
    }
    ensure!(input.pos == patch.len(), "the patch has bytes after its frame");
    ensure!(made == out_len, "the patch makes {made} bytes and the file has {out_len}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (s >> 33) as u8
            })
            .collect()
    }

    fn rebuild(base: &[u8], patch: &[u8], n: u64) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        apply(base, patch, n, &mut |b| {
            out.extend_from_slice(b);
            Ok(())
        })?;
        Ok(out)
    }

    #[test]
    fn a_patch_rebuilds_the_new_file_and_is_much_smaller_than_it() {
        let base = data(3 << 20, 1);
        let mut new = base.clone();
        new.splice(1 << 20..(1 << 20) + 10, [7u8; 40]);
        for i in (0..new.len()).step_by(100_000) {
            new[i] ^= 0x55;
        }
        let p = make(&base, &new, 19).unwrap();
        assert!(p.len() < 4000, "{}", p.len());
        assert_eq!(rebuild(&base, &p, new.len() as u64).unwrap(), new);
    }

    #[test]
    fn a_patch_with_another_size_or_another_base_or_cut_short_is_an_error() {
        let base = data(300_000, 2);
        let mut new = base.clone();
        new[1000] ^= 1;
        let p = make(&base, &new, 9).unwrap();
        assert!(rebuild(&base, &p, new.len() as u64 - 1).is_err(), "more than declared");
        assert!(rebuild(&base, &p, new.len() as u64 + 1).is_err(), "less than declared");
        assert!(rebuild(&base, &p[..p.len() - 3], new.len() as u64).is_err(), "cut short");
        let mut longer = p.clone();
        longer.push(0);
        assert!(rebuild(&base, &longer, new.len() as u64).is_err(), "bytes after the frame");
        let other = data(300_000, 3);
        let wrong = rebuild(&other, &p, new.len() as u64);
        assert!(wrong.is_err() || wrong.unwrap() != new, "another base must not give the file");
    }

    #[test]
    fn the_window_is_capped() {
        assert_eq!(window_log(1 << 20, 1 << 20), Some(21));
        assert_eq!(window_log(1 << 27, 1 << 27), Some(28));
        assert_eq!(window_log((1 << 27) + 1, 1 << 27), None);
    }
}
