// SPDX-License-Identifier: MIT OR Apache-2.0
//! Named points where a test build stops, to prove that a stop there leaves the store whole. Not in a release.
//!
//! `failpoint!("install.record_written")` is nothing at all in a release build: the macro expands to a test of `cfg!(feature = "failpoints")` in the
//! crate that uses it, which is false, so the call and its name are not in the program (CI checks the release binaries for the word that the
//! environment variable is read by). With the feature on, a point stops the process with `abort` (as a kill does: no destructor runs) when the
//! environment names it: `PANPAK_FAILPOINT=<name>` for its first hit, `PANPAK_FAILPOINT=<name>:<n>` for the n-th.
//! `tests/failpoints.py` runs every command with every point that the sources name.

/// A point of the program. See the module.
#[macro_export]
macro_rules! failpoint {
    ($name:literal) => {
        if cfg!(feature = "failpoints") {
            $crate::failpoint::hit($name);
        }
    };
}

/// Called by the macro with the feature on. Stops the process when the environment asks.
#[cfg(feature = "failpoints")]
pub fn hit(name: &'static str) {
    use std::{collections::HashMap, sync::Mutex};
    static COUNTS: Mutex<Option<HashMap<&'static str, usize>>> = Mutex::new(None);
    let Ok(want) = std::env::var(["PANPAK_", "FAILPOINT"].concat()) else { return };
    let (point, nth) = match want.split_once(':') {
        Some((p, n)) => (p, n.parse::<usize>().unwrap_or(1)),
        None => (want.as_str(), 1),
    };
    if point != name {
        return;
    }
    let mut guard = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let n = guard.get_or_insert_with(HashMap::new).entry(name).or_insert(0);
    *n += 1;
    if *n == nth {
        eprintln!("panpak: failpoint {name}");
        std::process::abort();
    }
}

/// Without the feature the function is not called; this keeps the path valid.
#[cfg(not(feature = "failpoints"))]
pub fn hit(_name: &'static str) {}
