// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `/tmp` and `/dev/shm` of an app in the isolated view: one folder of each per app and per
//! session, in `$XDG_RUNTIME_DIR/panpak/<id>/`, shared by the copies of the same app that run at the
//! same time (their lock files, local sockets and shared memory names meet) and by no other app.
//! A lock file tells whether a copy is alive: the first copy to start, when none is alive, empties
//! both folders (files left by a copy that was killed). Rules: PANPAK-FORMAT.md §9.4.
use anyhow::{Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::{fs::{DirBuilderExt, OpenOptionsExt}, io::AsRawFd},
    path::{Path, PathBuf},
};

/// The two folders, and the lock that says this copy is alive. The lock goes with the process.
pub struct Shared {
    pub tmp: PathBuf,
    pub shm: PathBuf,
    /// No other copy of the app was alive when this one started (it holds the exclusive lock first) and the
    /// last run left its mark "in use": it did not end normally, so its lock files may be stale.
    pub stale: bool,
    marker: PathBuf,
    lock: File,
}

fn flock(f: &File, op: libc::c_int) -> bool {
    // SAFETY: plain flock(2) on a descriptor this process owns.
    unsafe { libc::flock(f.as_raw_fd(), op) == 0 }
}

fn private_dir(p: &Path) -> Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(p).with_context(|| format!("create {}", p.display()))
}

/// Empty a folder without following links, and make sure it exists (0700).
fn reset(p: &Path) -> Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => {
            for e in fs::read_dir(p)?.flatten() {
                let path = e.path();
                let r = if e.file_type().is_ok_and(|t| t.is_dir()) { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
                if let Err(err) = r {
                    eprintln!("panpak: cannot remove {}: {err}", path.display());
                }
            }
        }
        Ok(_) => {
            fs::remove_file(p).with_context(|| format!("remove {}", p.display()))?;
        }
        Err(_) => {}
    }
    private_dir(p)
}

/// Prepare `<base>/tmp` and `<base>/shm`, take the lock and, when no other copy holds it, empty them.
pub fn prepare(base: &Path, marker: &Path) -> Result<Shared> {
    private_dir(base)?;
    let lock = OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(base.join("lock")).context("open the lock file")?;
    let (tmp, shm) = (base.join("tmp"), base.join("shm"));
    // Alone: the exclusive lock is ours, nobody else is in the folders, so empty them. The others
    // wait on the shared lock below until this is done.
    let alone = flock(&lock, libc::LOCK_EX | libc::LOCK_NB);
    let stale = alone && fs::symlink_metadata(marker).is_ok();
    if alone {
        reset(&tmp)?;
        reset(&shm)?;
    } else {
        private_dir(&tmp)?;
        private_dir(&shm)?;
    }
    anyhow::ensure!(flock(&lock, libc::LOCK_SH), "cannot take the shared lock");
    // The mark is made after the shared lock, so that a copy that is leaving cannot remove it afterwards.
    OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(marker).with_context(|| format!("create {}", marker.display()))?;
    Ok(Shared { tmp, shm, stale, marker: marker.to_path_buf(), lock })
}

impl Shared {
    /// The app ended normally: if no other copy is alive, remove the mark "in use". Whether this is the last
    /// copy is told by trying to take the exclusive lock, which fails while another copy holds the shared one.
    pub fn ended_normally(&self) {
        if flock(&self.lock, libc::LOCK_EX | libc::LOCK_NB) {
            let _ = fs::remove_file(&self.marker);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lock belongs to the open file description, and a process forked by another test keeps a copy of
    /// it until it execs (the descriptor is close-on-exec, but only at the exec). The check "no copy is
    /// alive" must not see that, so the scenario runs in a process of its own, where no other test forks.
    #[test]
    fn the_first_copy_empties_the_folders_and_later_ones_leave_them() {
        if std::env::var_os("PANPAK_SHAREDTMP_ALONE").is_some() {
            return scenario();
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "sharedtmp::tests::the_first_copy_empties_the_folders_and_later_ones_leave_them", "--test-threads=1"])
            .env("PANPAK_SHAREDTMP_ALONE", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    }

    fn scenario() {
        let base = std::env::temp_dir().join(format!("panpak-shared-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        private_dir(&base.join("tmp/sub")).unwrap();
        fs::write(base.join("tmp/qipc_stale"), b"x").unwrap();
        fs::write(base.join("tmp/sub/f"), b"x").unwrap();
        private_dir(&base.join("shm")).unwrap();
        fs::write(base.join("shm/old"), b"x").unwrap();
        std::os::unix::fs::symlink("/etc", base.join("tmp/link")).unwrap();
        let mark = base.with_extension("in-use");
        let a = prepare(&base, &mark).unwrap();
        assert!(!a.stale, "no mark: the last run ended normally");
        assert!(!a.tmp.join("qipc_stale").exists() && !a.tmp.join("sub").exists() && !a.shm.join("old").exists());
        assert!(Path::new("/etc/passwd").exists(), "a link in the folder is removed, not followed");
        fs::write(a.tmp.join("qipc_live"), b"x").unwrap();
        // A second copy (another open file description, so a real second holder) keeps the files.
        let b = prepare(&base, &mark).unwrap();
        assert!(b.tmp.join("qipc_live").exists());
        assert!(!b.stale);
        assert_eq!(a.tmp, b.tmp);
        drop(a);
        // One copy is still alive: nothing is emptied.
        let c = prepare(&base, &mark).unwrap();
        assert!(c.tmp.join("qipc_live").exists());
        drop((b, c));
        // Nobody is alive any more: the next start empties the folders.
        let d = prepare(&base, &mark).unwrap();
        assert!(!d.tmp.join("qipc_live").exists());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&d.tmp).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&base).unwrap().permissions().mode() & 0o777, 0o700);
        drop(d);
        fs::remove_dir_all(&base).unwrap();
        // The mark: made at every start; a normal end removes it only for the last copy; a start with none alive
        // that finds it reports a stale run, and one that does not find it does not.
        let _ = fs::remove_file(&mark);
        let x = prepare(&base, &mark).unwrap();
        assert!(mark.exists() && !x.stale);
        let y = prepare(&base, &mark).unwrap();
        x.ended_normally();
        assert!(mark.exists(), "another copy is alive: the mark stays");
        y.ended_normally();
        assert!(!mark.exists(), "the last copy ended normally: no mark");
        drop((x, y));
        let z = prepare(&base, &mark).unwrap();
        assert!(!z.stale);
        drop(z); // killed: nothing removed the mark
        let w = prepare(&base, &mark).unwrap();
        assert!(w.stale, "the mark was left: the run did not end normally");
        drop(w);
        let v = prepare(&base, &mark).unwrap();
        assert!(v.stale);
        drop(v);
        let _ = fs::remove_file(&mark);
        fs::remove_dir_all(&base).unwrap();
    }
}
