// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the background job keeps for one app: `apps/<id>/update-state`. PANPAK-FORMAT.md section 6.7.
//!
//! A text file of `key=value` lines. Every value is a number or a short word, so a damaged or hand-edited file can at worst make the job run
//! earlier or later: it is never trusted for anything else (an unknown line is ignored, a bad number is zero).
use anyhow::Result;
use std::path::{Path, PathBuf};

pub const FILE: &str = "update-state";
/// Seconds in a day: the interval of the check and of the budget of the verification.
pub const DAY: u64 = 86_400;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BgState {
    /// When the last check of the update URL ended well (seconds since 1970).
    pub last_check: u64,
    /// The job is not due before this time.
    pub next_check: u64,
    /// Checks that failed one after the other (the backoff grows with it).
    pub failures: u32,
    /// The verification goes round the files of the installed version in the order of the signed index: the next one to check.
    pub cursor: u64,
    /// The day (seconds since 1970 divided by `DAY`) of `verified_bytes`.
    pub verify_day: u64,
    /// What the verification read on `verify_day`.
    pub verified_bytes: u64,
    /// When the verification came to the end of the files (a whole round).
    pub last_round: u64,
    /// Files found damaged and not repaired yet, and the first of them.
    pub damaged: u32,
    pub damaged_first: String,
    /// The root (12 hex digits) of the staged version that the user was told about, so that the notice is given once.
    pub notified: String,
}

pub fn path(appdir: &Path) -> PathBuf {
    appdir.join(FILE)
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub fn parse(text: &str) -> BgState {
    let mut s = BgState::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let n = || v.trim().parse::<u64>().unwrap_or(0);
        match k.trim() {
            "last_check" => s.last_check = n(),
            "next_check" => s.next_check = n(),
            "failures" => s.failures = n().min(1000) as u32,
            "cursor" => s.cursor = n(),
            "verify_day" => s.verify_day = n(),
            "verified_bytes" => s.verified_bytes = n(),
            "last_round" => s.last_round = n(),
            "damaged" => s.damaged = n().min(u32::MAX as u64) as u32,
            // A path of the tree, kept to say what is damaged: one line, no control characters.
            "damaged_first" => s.damaged_first = v.trim().chars().filter(|c| !c.is_control()).take(200).collect(),
            "notified" => s.notified = v.trim().chars().filter(char::is_ascii_hexdigit).take(12).collect(),
            _ => {}
        }
    }
    s
}

pub fn render(s: &BgState) -> String {
    let first: String = s.damaged_first.chars().filter(|c| !c.is_control()).collect();
    format!(
        "last_check={}\nnext_check={}\nfailures={}\ncursor={}\nverify_day={}\nverified_bytes={}\nlast_round={}\ndamaged={}\ndamaged_first={}\nnotified={}\n",
        s.last_check, s.next_check, s.failures, s.cursor, s.verify_day, s.verified_bytes, s.last_round, s.damaged, first, s.notified
    )
}

/// The state, or the empty one (never checked) when there is none or it cannot be read.
pub fn read(appdir: &Path) -> BgState {
    std::fs::read_to_string(path(appdir)).map(|t| parse(&t)).unwrap_or_default()
}

pub fn write(appdir: &Path, s: &BgState) -> Result<()> {
    crate::durable::write_file(&path(appdir), render(s).as_bytes(), 0o644)
}

/// Is the job due for this app at `now`? The stub asks this before it starts one, so it reads one small file and nothing else.
pub fn due(appdir: &Path, now: u64) -> bool {
    match std::fs::read_to_string(path(appdir)) {
        Ok(t) => parse(&t).next_check <= now,
        Err(_) => true,
    }
}

/// When to try again after `failures` failures in a row: an hour, doubling, up to a day.
pub fn backoff(failures: u32) -> u64 {
    (3600u64 << failures.saturating_sub(1).min(5)).min(DAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_goes_round_and_a_damaged_file_is_harmless() {
        let s = BgState { last_check: 5, next_check: 9, failures: 2, cursor: 77, verify_day: 3, verified_bytes: 1 << 20, last_round: 4, damaged: 1, damaged_first: "lib/a.so".into(), notified: "0123456789ab".into() };
        assert_eq!(parse(&render(&s)), s);
        let junk = parse("next_check=banana\n\0\nfailures=99999999999\ndamaged_first=a\x1b[31mb\nnotified=zz12\n=\nx");
        assert_eq!(junk.next_check, 0);
        assert_eq!(junk.failures, 1000, "a huge number is capped");
        assert_eq!(junk.damaged_first, "a[31mb");
        assert_eq!(junk.notified, "12");
    }

    #[test]
    fn the_backoff_doubles_to_a_day() {
        assert_eq!([backoff(1), backoff(2), backoff(3), backoff(6), backoff(40)], [3600, 7200, 14400, DAY, DAY]);
    }
}
