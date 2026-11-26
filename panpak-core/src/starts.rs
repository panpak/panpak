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

/// Why an app did not start, as Panpak says it (never taken from what the app printed): it ended with an error code, it crashed, or the system stopped it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Ended by itself with this status.
    Exit(i32),
    /// Ended by a fatal signal of its own (SEGV, ABRT, ...), or with the status that the view gives to one (128 + the signal).
    Crash,
    /// Stopped by the system (SIGKILL), alone or as status 137.
    Stopped,
}

/// The reason of a failed start. The status 128 + n is how a shell, and bubblewrap, report a program that ended by the signal n.
pub fn reason(code: Option<i32>, signal: Option<i32>) -> Reason {
    let sig = signal.or_else(|| code.filter(|c| (129..=192).contains(c)).map(|c| c - 128));
    match (sig, code) {
        (Some(9), _) => Reason::Stopped,
        (Some(_), _) => Reason::Crash,
        (None, Some(c)) => Reason::Exit(c),
        (None, None) => Reason::Exit(1),
    }
}

/// The sentence for the reason, in the language of the person (the catalog `cat`, `None` for English). Panpak's own words: never taken from what the app printed.
pub fn reason_text_in(cat: Option<&crate::i18n::Catalog>, r: Reason) -> String {
    use crate::{i18n, messages as m};
    match r {
        Reason::Exit(c) => i18n::fill(&i18n::tr_in(cat, m::NOT_STARTED_EXIT), &[("code", c.to_string())]),
        Reason::Crash => i18n::tr_in(cat, m::NOT_STARTED_CRASH).into_owned(),
        Reason::Stopped => i18n::tr_in(cat, m::NOT_STARTED_STOPPED).into_owned(),
    }
}

/// `reason_text_in` in the language that is in use.
pub fn reason_text(r: Reason) -> String {
    reason_text_in(crate::i18n::current(), r)
}

impl Reason {
    /// The form that travels as an argument of a detached process: `exit:3`, `crash`, `stopped`.
    pub fn arg(&self) -> String {
        match self {
            Reason::Exit(c) => format!("exit:{c}"),
            Reason::Crash => "crash".into(),
            Reason::Stopped => "stopped".into(),
        }
    }

    /// The inverse of `arg`; anything else is `None`.
    pub fn parse(s: &str) -> Option<Reason> {
        match s {
            "crash" => Some(Reason::Crash),
            "stopped" => Some(Reason::Stopped),
            _ => s.strip_prefix("exit:").and_then(|n| n.parse::<i32>().ok()).filter(|c| (1..=255).contains(c)).map(Reason::Exit),
        }
    }
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
    // The outcome of a start of a version that is not the installed one any more does not count: the end of a start is recorded a little after the app ended, and by then the
    // person may have gone back to another version (or installed one). Counting it would overwrite the counters of the version that is installed now.
    if let Ok(cur) = crate::durable::resolve_link(&appdir.join("current")) {
        if record_root(&cur).is_some_and(|r| r != root) {
            return Ok(Event::Nothing);
        }
    }
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

/// The root (hex) in the name of a record, `<id>@<version>@<root>.rec`; `None` for any other name.
fn record_root(rec: &Path) -> Option<&str> {
    let name = rec.file_name()?.to_str()?.strip_suffix(".rec")?;
    let root = name.rsplit('@').next()?;
    (root.len() == 64 && root.bytes().all(|b| b.is_ascii_hexdigit())).then_some(root)
}

/// The failed starts in a row are forgotten (the good ones are kept): after the version was changed (an install, an update that is applied, a rollback) and after the person chose
/// "Try again". The question is then asked again only after `ASK_AFTER` new failures. The same package installed again has the same root, so the counters would stay otherwise.
pub fn reset_failed(appdir: &Path) {
    let mut s = read(appdir);
    if s.failed != 0 {
        s.failed = 0;
        let _ = write(appdir, &s);
    }
}

/// The file that says a question of kept versions has been asked and not answered.
pub const PENDING: &str = "pending-question";

/// Make the question pending and take the right to wait for its answer: a lock on the pending file, held until the returned file is dropped. `None` when another
/// process holds it (a notification of the same question is already waiting) or the file cannot be made.
pub fn hold_pending(appdir: &Path, root: &str) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let path = appdir.join(PENDING);
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).ok()?;
    // SAFETY: flock on a descriptor that `f` owns.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return None;
    }
    let _ = std::fs::write(&path, format!("kind=kept-versions\nroot={root}\n"));
    Some(f)
}

/// The question was answered, or it is not the case any more.
pub fn clear_pending(appdir: &Path) {
    let _ = std::fs::remove_file(appdir.join(PENDING));
}

/// A question that was asked and not answered, about the version `root` (the one that `current` names): a question about another version is not the case any
/// more and is dropped.
pub fn is_pending(appdir: &Path, root: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(appdir.join(PENDING)) else { return false };
    if text.lines().any(|l| l.strip_prefix("root=") == Some(root)) {
        return true;
    }
    // A file that is being written has no root yet: leave it. Any other one is old.
    if !text.is_empty() {
        clear_pending(appdir);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    const R2: &str = "0000000000000000000000000000000000000000000000000000000000000002";

    #[test]
    fn the_reason_of_a_failed_start_is_an_error_code_a_crash_or_a_stop() {
        assert_eq!(reason(Some(3), None), Reason::Exit(3));
        assert_eq!(reason(Some(1), None), Reason::Exit(1));
        assert_eq!(reason(None, Some(11)), Reason::Crash);
        assert_eq!(reason(None, Some(6)), Reason::Crash);
        // the status that a shell and bubblewrap give to a program that ended by a signal
        assert_eq!(reason(Some(139), None), Reason::Crash);
        assert_eq!(reason(Some(134), None), Reason::Crash);
        assert_eq!(reason(Some(137), None), Reason::Stopped);
        assert_eq!(reason(None, Some(9)), Reason::Stopped);
        assert_eq!(reason(Some(128), None), Reason::Exit(128));
        assert_eq!(reason(Some(193), None), Reason::Exit(193));
        assert_eq!(reason(None, None), Reason::Exit(1));
        // the argument of the detached process: round trip, and nothing else is read
        for r in [Reason::Exit(3), Reason::Exit(255), Reason::Crash, Reason::Stopped] {
            assert_eq!(Reason::parse(&r.arg()), Some(r));
        }
        for bad in ["", "exit:", "exit:0", "exit:256", "exit:-1", "exit:3 ", "Crash", "crash:1", "exit:3\nx"] {
            assert_eq!(Reason::parse(bad), None, "{bad:?}");
        }
    }

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
    fn the_end_of_a_start_of_a_version_that_is_not_installed_does_not_count() {
        let d = std::env::temp_dir().join(format!("panpak-starts-notcurrent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let rec = d.join(format!("org.x@1.0@{R2}.rec"));
        std::fs::write(&rec, b"r").unwrap();
        crate::durable::replace_symlink(&rec, &d.join("current")).unwrap();
        // The installed version is R2: a failure of R is not counted and writes nothing.
        assert_eq!(record_end(&d, R, Outcome::Failed).unwrap(), Event::Nothing);
        assert_eq!(record_end(&d, R, Outcome::Failed).unwrap(), Event::Nothing);
        assert!(!d.join(FILE).exists());
        // The installed version's own failures count, two in a row ask.
        assert_eq!(record_end(&d, R2, Outcome::Failed).unwrap(), Event::Nothing);
        assert_eq!(record_end(&d, R2, Outcome::Failed).unwrap(), Event::Ask);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_failed_starts_are_forgotten_and_the_good_ones_kept() {
        let d = std::env::temp_dir().join(format!("panpak-starts-reset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, &State { root: R.into(), good: 2, failed: 1 }).unwrap();
        reset_failed(&d);
        assert_eq!(read(&d), State { root: R.into(), good: 2, failed: 0 });
        // Nothing to forget: the file is not rewritten (and not made).
        let e = d.join("empty");
        std::fs::create_dir_all(&e).unwrap();
        reset_failed(&e);
        assert!(!e.join(FILE).exists());
        let _ = std::fs::remove_dir_all(&d);
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
