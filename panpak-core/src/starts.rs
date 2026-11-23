// SPDX-License-Identifier: MIT OR Apache-2.0
//! How the starts of an installed version went, and the version that is known to work: `apps/<id>/starts` and the link `apps/<id>/last_good`.
//! PANPAK-FORMAT.md section 6.10.
//!
//! The outcome of a start never leaves the computer: it is a few numbers in a file of the store, read by the stub and by `panpak`, and nothing sends it anywhere.
//! A damaged or hand-edited file can at worst make the program ask a question that it would not ask, or not ask one that it would: it never changes a version by
//! itself. Nothing here rolls back: two failed starts in a row end in a question to the person.
use anyhow::Result;
use std::path::{Path, PathBuf};

pub const FILE: &str = "starts";
/// The link to the record of the last version that worked.
pub const LAST_GOOD: &str = "last_good";
/// The file that the collection writes when it takes out a previous version that is below the floor of its author (`floor=<n>`): a rollback then says why there is none.
pub const WITHDRAWN: &str = "withdrawn";
/// The file whose presence says that the previous version is always kept (the setting of `panpak keep-previous`).
pub const KEEP_PREVIOUS: &str = "keep_previous";
/// A start that ends within this many seconds with a failure is a failed start; one that lasts longer is a good start.
pub const QUICK_SECS: f64 = 20.0;
/// Good starts of one version after which it is the version that is known to work.
pub const PROVE_AFTER: u32 = 3;
/// Failed starts in a row after which the person is asked.
pub const ASK_AFTER: u32 = 2;

/// What one start counts as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Good,
    Failed,
    /// Neither: a quick end with status 0 (a program that hands over to a copy that runs, `--version`), a stop by the system or the user (SIGKILL, a signal that
    /// the stub passed on, the end of the session).
    Neither,
}

/// How a start ended: the status of the process, whether a stop signal was passed on by the stub, how long it ran, and whether the arguments were of a kind
/// that is not a start (`--version`, `--help`).
pub fn classify(code: Option<i32>, signal: Option<i32>, forwarded: bool, secs: f64, a_start: bool) -> Outcome {
    classify_after(code, signal, forwarded, secs, a_start, QUICK_SECS)
}

/// The seconds that a start must last to be a good one: `QUICK_SECS`, or the value of `PANPAK_QUICK_SECS` (for the tests, which cannot wait 20 s).
pub fn quick_secs() -> f64 {
    std::env::var("PANPAK_QUICK_SECS").ok().and_then(|v| v.parse::<f64>().ok()).filter(|v| (0.0..=3600.0).contains(v)).unwrap_or(QUICK_SECS)
}

pub fn classify_after(code: Option<i32>, signal: Option<i32>, forwarded: bool, secs: f64, a_start: bool, quick: f64) -> Outcome {
    if !a_start {
        return Outcome::Neither;
    }
    if secs >= quick {
        // It ran: whatever came at its end is not a failure to start. A stop that the system or the person asked for is not a good start either.
        return if signal.is_some() && (forwarded || signal == Some(libc_sigkill())) { Outcome::Neither } else { Outcome::Good };
    }
    match (code, signal) {
        (Some(0), _) => Outcome::Neither,
        (Some(_), _) => Outcome::Failed,
        (None, Some(s)) if forwarded || s == libc_sigkill() => Outcome::Neither,
        // A fatal signal of the program itself: SEGV, ABRT, BUS, ILL, FPE, SYS, and the other signals that end a process.
        (None, Some(_)) => Outcome::Failed,
        (None, None) => Outcome::Neither,
    }
}

fn libc_sigkill() -> i32 {
    9
}

/// What the file says about the version that is installed now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// The root (hex) of the version that the counters are about; a different root starts them again.
    pub root: String,
    /// Good starts of that version.
    pub good: u32,
    /// Failed starts in a row (a good start, or a new version, sets it to 0).
    pub failed: u32,
}

/// What a start that ended changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Nothing,
    /// This version has had `PROVE_AFTER` good starts: it is the version that is known to work.
    Proven,
    /// `ASK_AFTER` failed starts in a row: the person is asked whether to go back.
    Ask,
}

impl State {
    pub fn parse(text: &str) -> State {
        let mut s = State::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k {
                "root" if v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()) => s.root = v.to_string(),
                "good" => s.good = v.parse().unwrap_or(0),
                "failed" => s.failed = v.parse().unwrap_or(0),
                _ => {}
            }
        }
        s
    }

    pub fn text(&self) -> String {
        format!("root={}\ngood={}\nfailed={}\n", self.root, self.good, self.failed)
    }

    /// Count one start of the version `root`. Returns the event.
    pub fn count(&mut self, root: &str, outcome: Outcome) -> Event {
        if self.root != root {
            *self = State { root: root.to_string(), good: 0, failed: 0 };
        }
        match outcome {
            Outcome::Neither => Event::Nothing,
            Outcome::Good => {
                self.failed = 0;
                if self.good < PROVE_AFTER {
                    self.good += 1;
                    if self.good == PROVE_AFTER {
                        return Event::Proven;
                    }
                }
                Event::Nothing
            }
            Outcome::Failed => {
                self.failed += 1;
                // The question is asked once for every `ASK_AFTER` failures: after an answer the count starts again.
                if self.failed >= ASK_AFTER {
                    self.failed = 0;
                    return Event::Ask;
                }
                Event::Nothing
            }
        }
    }
}

pub fn read(appdir: &Path) -> State {
    std::fs::read_to_string(appdir.join(FILE)).map(|t| State::parse(&t)).unwrap_or_default()
}

pub fn write(appdir: &Path, s: &State) -> Result<()> {
    crate::durable::write_file(&appdir.join(FILE), s.text().as_bytes(), 0o644)
}

/// The floor that was in force when a previous version below it was taken out, if one was.
pub fn withdrawn_floor(appdir: &Path) -> Option<u64> {
    std::fs::read_to_string(appdir.join(WITHDRAWN)).ok()?.lines().find_map(|l| l.strip_prefix("floor=")?.trim().parse().ok())
}

/// Is the previous version always kept? (`panpak keep-previous`.)
pub fn keeps_previous(appdir: &Path) -> bool {
    appdir.join(KEEP_PREVIOUS).exists()
}

pub fn set_keep_previous(appdir: &Path, on: bool) -> Result<()> {
    let p = appdir.join(KEEP_PREVIOUS);
    if on {
        crate::durable::write_file(&p, b"", 0o644)
    } else {
        match std::fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// The record that `last_good` names, if the link is there.
pub fn last_good(appdir: &Path) -> Option<PathBuf> {
    crate::durable::resolve_link(&appdir.join(LAST_GOOD)).ok()
}

/// A start of the installed version `root` ended: count it, and act on the event. `current` is the record that `current` names now. Returns the event, so that the
/// caller can ask the person.
pub fn record_end(appdir: &Path, root: &str, outcome: Outcome) -> Result<Event> {
    let mut s = read(appdir);
    let before = s.clone();
    let ev = s.count(root, outcome);
    if s != before {
        write(appdir, &s)?;
    }
    if ev == Event::Proven {
        if let Ok(cur) = crate::durable::resolve_link(&appdir.join("current")) {
            crate::durable::replace_symlink(&cur, &appdir.join(LAST_GOOD))?;
        }
    }
    Ok(ev)
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    const R2: &str = "0000000000000000000000000000000000000000000000000000000000000002";

    #[test]
    fn a_start_is_good_failed_or_neither() {
        let c = |code, sig, fwd, secs, start| classify(code, sig, fwd, secs, start);
        assert_eq!(c(Some(1), None, false, 2.0, true), Outcome::Failed);
        assert_eq!(c(None, Some(11), false, 2.0, true), Outcome::Failed, "SEGV");
        assert_eq!(c(None, Some(6), false, 0.5, true), Outcome::Failed, "ABRT");
        assert_eq!(c(Some(0), None, false, 1.0, true), Outcome::Neither, "a quick end with status 0");
        assert_eq!(c(None, Some(9), false, 1.0, true), Outcome::Neither, "SIGKILL");
        assert_eq!(c(None, Some(15), true, 1.0, true), Outcome::Neither, "a stop that was passed on");
        assert_eq!(c(Some(1), None, false, 2.0, false), Outcome::Neither, "--version, --help");
        assert_eq!(c(Some(0), None, false, 300.0, true), Outcome::Good);
        assert_eq!(c(Some(1), None, false, 300.0, true), Outcome::Good, "it ran: a later failure is not a failure to start");
        assert_eq!(c(None, Some(15), true, 300.0, true), Outcome::Neither, "the end of the session");
    }

    #[test]
    fn three_good_starts_prove_a_version_and_two_failures_in_a_row_ask() {
        let mut s = State::default();
        assert_eq!(s.count(R, Outcome::Good), Event::Nothing);
        assert_eq!(s.count(R, Outcome::Neither), Event::Nothing);
        assert_eq!(s.count(R, Outcome::Good), Event::Nothing);
        assert_eq!(s.count(R, Outcome::Good), Event::Proven);
        assert_eq!(s.count(R, Outcome::Good), Event::Nothing, "proven once");
        // A failure and then a good start: not in a row.
        assert_eq!(s.count(R, Outcome::Failed), Event::Nothing);
        assert_eq!(s.count(R, Outcome::Good), Event::Nothing);
        assert_eq!(s.count(R, Outcome::Failed), Event::Nothing);
        assert_eq!(s.count(R, Outcome::Neither), Event::Nothing, "a quick end with 0 counts neither way");
        assert_eq!(s.count(R, Outcome::Failed), Event::Ask);
        assert_eq!(s.failed, 0, "after the question the count starts again");
        // A new version starts again.
        assert_eq!(s.count(R2, Outcome::Failed), Event::Nothing);
        assert_eq!((s.root.as_str(), s.good, s.failed), (R2, 0, 1));
    }

    #[test]
    fn the_file_is_read_with_care() {
        let s = State { root: R.into(), good: 2, failed: 1 };
        assert_eq!(State::parse(&s.text()), s);
        let bad = State::parse("root=zz\ngood=x\nfailed=-1\nnoise\n");
        assert_eq!(bad, State::default());
    }

    #[test]
    fn a_proven_version_is_the_last_good_and_the_setting_is_a_file() {
        let d = std::env::temp_dir().join(format!("panpak-starts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let rec = d.join("x.rec");
        std::fs::write(&rec, b"r").unwrap();
        crate::durable::replace_symlink(&rec, &d.join("current")).unwrap();
        for _ in 0..2 {
            assert_eq!(record_end(&d, R, Outcome::Good).unwrap(), Event::Nothing);
        }
        assert!(last_good(&d).is_none());
        assert_eq!(record_end(&d, R, Outcome::Good).unwrap(), Event::Proven);
        assert_eq!(last_good(&d).as_deref(), Some(rec.as_path()));
        assert!(!keeps_previous(&d));
        set_keep_previous(&d, true).unwrap();
        assert!(keeps_previous(&d));
        set_keep_previous(&d, false).unwrap();
        assert!(!keeps_previous(&d));
        let _ = std::fs::remove_dir_all(&d);
    }
}
