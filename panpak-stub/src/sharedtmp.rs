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
        // Not there (or not readable): `private_dir` creates it and reports any failure.
        Err(_) => {}
    }
    private_dir(p)
}

/// The shared lock of an installed app, taken before anything of the app is read from the store. `panpak remove`
/// holds the exclusive lock while it takes the app away: a start that comes in that moment waits here, and then
/// finds (the caller checks) that the app is gone. The lock goes with the returned file and with the copies of its
/// descriptor; `prepare` takes it over.
pub fn early_lock(runtime_dir: &Path, app_id: &str) -> Result<File> {
    let base = runtime_dir.join("panpak").join(app_id);
    private_dir(&base)?;
    let lock = OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(base.join("lock")).context("open the lock file")?;
    loop {
        if flock(&lock, libc::LOCK_SH) {
            return Ok(lock);
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e).context("take the shared lock of the app");
        }
    }
}

/// Prepare `<base>/tmp` and `<base>/shm`, take the lock and, when no other copy holds it, empty them. `early` is the
/// lock from `early_lock` if there is one.
pub fn prepare(base: &Path, marker: &Path, early: Option<File>) -> Result<Shared> {
    private_dir(base)?;
    let lock = match early {
        Some(f) => f,
        None => OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(base.join("lock")).context("open the lock file")?,
    };
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
    /// The app ended and this might be the last copy: it is, if the exclusive lock can be taken, which fails while another
    /// copy holds the shared one. Then nobody is alive, so the stale lock files that this copy may have left are searched in
    /// `roots` (the private folders) and removed *before* the mark "in use" goes: an app that stops on a signal, or with status
    /// 0, does not always remove its own lock file (a Qt `QLockFile`), and the next start would take it for a live copy. The
    /// mark is removed only after a normal end (`clean`); after any other end it stays and the next start searches too.
    /// Returns the files removed, and whether this was the last copy.
    pub fn ended(&self, clean: bool, roots: &[&Path]) -> (Vec<PathBuf>, bool) {
        if !flock(&self.lock, libc::LOCK_EX | libc::LOCK_NB) {
            return (Vec::new(), false);
        }
        let gone = panpak_core::stalelocks::remove(roots);
        if clean {
            let _ = fs::remove_file(&self.marker);
        }
        (gone, true)
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
        let a = prepare(&base, &mark, None).unwrap();
        assert!(!a.stale, "no mark: the last run ended normally");
        assert!(!a.tmp.join("qipc_stale").exists() && !a.tmp.join("sub").exists() && !a.shm.join("old").exists());
        assert!(Path::new("/etc/passwd").exists(), "a link in the folder is removed, not followed");
        fs::write(a.tmp.join("qipc_live"), b"x").unwrap();
        // A second copy (another open file description, so a real second holder) keeps the files.
        let b = prepare(&base, &mark, None).unwrap();
        assert!(b.tmp.join("qipc_live").exists());
        assert!(!b.stale);
        assert_eq!(a.tmp, b.tmp);
        drop(a);
        // One copy is still alive: nothing is emptied.
        let c = prepare(&base, &mark, None).unwrap();
        assert!(c.tmp.join("qipc_live").exists());
        drop((b, c));
        // Nobody is alive any more: the next start empties the folders.
        let d = prepare(&base, &mark, None).unwrap();
        assert!(!d.tmp.join("qipc_live").exists());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&d.tmp).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&base).unwrap().permissions().mode() & 0o777, 0o700);
        drop(d);
        fs::remove_dir_all(&base).unwrap();
        // The mark: made at every start; a normal end removes it only for the last copy; a start with none alive
        // that finds it reports a stale run, and one that does not find it does not.
        let _ = fs::remove_file(&mark);
        let x = prepare(&base, &mark, None).unwrap();
        assert!(mark.exists() && !x.stale);
        let y = prepare(&base, &mark, None).unwrap();
        let locks = base.with_extension("locks");
        let _ = fs::remove_dir_all(&locks);
        fs::create_dir_all(&locks).unwrap();
        let lockfile = locks.join("lockfile");
        // A Qt QLockFile that the app left behind: a process id (which a new view reuses), the name and the host.
        fs::write(&lockfile, b"2\nqbittorrent\nhost\n\n0123-4567\n").unwrap();
        assert_eq!(x.ended(true, &[locks.as_path()]), (vec![], false), "another copy is alive: nothing is touched");
        assert!(mark.exists() && lockfile.exists(), "another copy is alive: the mark and its lock files stay");
        let (gone, last) = y.ended(true, &[locks.as_path()]);
        assert!(last && gone == [lockfile.clone()], "the last copy removes the lock file it left: {gone:?}");
        assert!(!lockfile.exists() && !mark.exists(), "the last copy ended normally: no lock file, no mark");
        drop((x, y));
        // An end that is not normal (the app crashed, the runtime saw no clean status) still clears the lock files, and keeps the mark.
        let (p, q) = (prepare(&base, &mark, None).unwrap(), prepare(&base, &mark, None).unwrap());
        fs::write(&lockfile, b"2\nqbittorrent\nhost\n\n0123-4567\n").unwrap();
        fs::write(locks.join("notes.txt"), b"2\nmy notes\nabout locks\n").unwrap();
        drop(p);
        let (gone, last) = q.ended(false, &[locks.as_path()]);
        assert!(last && gone == [lockfile.clone()], "{gone:?}");
        assert!(mark.exists(), "not a normal end: the mark stays");
        assert!(locks.join("notes.txt").exists(), "a file that does not look like a lock is not touched");
        drop(q);
        let _ = fs::remove_file(&mark);
        let _ = fs::remove_dir_all(&locks);
        let z = prepare(&base, &mark, None).unwrap();
        assert!(!z.stale);
        drop(z); // killed: nothing removed the mark
        let w = prepare(&base, &mark, None).unwrap();
        assert!(w.stale, "the mark was left: the run did not end normally");
        drop(w);
        let v = prepare(&base, &mark, None).unwrap();
        assert!(v.stale);
        drop(v);
        let _ = fs::remove_file(&mark);
        fs::remove_dir_all(&base).unwrap();
    }
}
