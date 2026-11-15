// SPDX-License-Identifier: MIT OR Apache-2.0
//! An update that is ready and waits to be applied. PANPAK-FORMAT.md section 6.6.
//!
//! `panpak update --stage` (and the update that `panpak update` makes when a copy of the app is running) downloads and builds a new version without
//! changing what the app runs: its record is in `packages/`, its tree is made, and `apps/<id>/pending` names the record. The version is applied
//! (`current` moves to it) by `panpak apply`, which the stub runs at the start of the app when no copy runs, or by `panpak update` when the user is
//! there. `apps/<id>/pending-state` says whether the version asks for permissions that the installed one did not have: then it is not applied
//! until the user accepts them.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The link that names the staged record, in `apps/<id>/`.
pub const LINK: &str = "pending";
/// The file that says what the staged version waits for.
pub const STATE: &str = "pending-state";

/// What the staged version waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    /// It asks for permissions that the installed version did not have: only the user can apply it.
    pub needs_accept: bool,
}

pub fn link_path(appdir: &Path) -> PathBuf {
    appdir.join(LINK)
}

pub fn state_path(appdir: &Path) -> PathBuf {
    appdir.join(STATE)
}

/// The record that is staged, if any.
pub fn staged_record(appdir: &Path) -> Option<PathBuf> {
    std::fs::read_link(link_path(appdir)).ok()
}

pub fn read_state(appdir: &Path) -> Option<State> {
    let text = std::fs::read_to_string(state_path(appdir)).ok()?;
    Some(State { needs_accept: text.lines().any(|l| l.trim() == "needs_accept=1") })
}

pub fn write_state(appdir: &Path, state: State) -> Result<()> {
    crate::durable::write_file(&state_path(appdir), format!("needs_accept={}\n", u8::from(state.needs_accept)).as_bytes(), 0o644)
}

/// Take the staged version away (the link and its state). Nothing else: the record and the tree stay for whoever names them.
pub fn clear(appdir: &Path) -> Result<()> {
    for p in [link_path(appdir), state_path(appdir)] {
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("remove {}", p.display())),
        }
    }
    crate::durable::fsync_dir(appdir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_goes_round_and_clear_takes_both_away() {
        let d = std::env::temp_dir().join(format!("panpak-pending-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(read_state(&d), None);
        write_state(&d, State { needs_accept: true }).unwrap();
        assert_eq!(read_state(&d), Some(State { needs_accept: true }));
        write_state(&d, State { needs_accept: false }).unwrap();
        assert_eq!(read_state(&d), Some(State { needs_accept: false }));
        std::os::unix::fs::symlink("../packages/x.rec", link_path(&d)).unwrap();
        assert_eq!(staged_record(&d).unwrap().to_str(), Some("../packages/x.rec"));
        clear(&d).unwrap();
        clear(&d).unwrap();
        assert!(staged_record(&d).is_none() && read_state(&d).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
