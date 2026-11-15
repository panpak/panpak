// SPDX-License-Identifier: MIT OR Apache-2.0
//! One lock per app for the commands that change what is installed. PANPAK-FORMAT.md section 6.4.
//!
//! `install`, `update`, `rollback`, `repair` and `remove` of one app take `<store>/locks/<id>.lock` with `flock(LOCK_EX)` while they change the store, so that
//! two of them never work on the links, records and trees of the same app at once. The lock goes with the process (a command that is killed
//! never leaves it held). It is not inside `apps/<id>/`, which `remove` deletes: a lock file that is deleted while another process waits for it
//! locks nothing. A start of the app does not take it (the copies of an app hold their own shared lock, which `remove` and `repair` take
//! exclusively); `panpak-stub prepare` does not either (the tree has its own lock).
use crate::{coded, ErrorCode};
use anyhow::{Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::io::AsRawFd,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// How long a command waits for another one on the same app before it gives up.
pub const WAIT: Duration = Duration::from_secs(60);

/// The path of the lock of an app.
pub fn lock_path(store: &Path, app_id: &str) -> PathBuf {
    store.join("locks").join(format!("{app_id}.lock"))
}

/// Take the lock of `app_id`; wait for it up to `WAIT`. The returned file holds it until it is dropped.
pub fn lock(store: &Path, app_id: &str) -> Result<File> {
    lock_for(store, app_id, WAIT)
}

/// The same with a time of the caller (the tests).
pub fn lock_for(store: &Path, app_id: &str, wait: Duration) -> Result<File> {
    crate::validate_app_id(app_id)?;
    let path = lock_path(store, app_id);
    fs::create_dir_all(path.parent().context("lock without a folder")?)?;
    let f = OpenOptions::new().create(true).truncate(false).write(true).open(&path).with_context(|| format!("open the lock {}", path.display()))?;
    let start = Instant::now();
    loop {
        // SAFETY: flock(2) on a descriptor this function owns.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(f);
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(err).with_context(|| format!("lock {}", path.display()));
        }
        if start.elapsed() >= wait {
            return Err(coded(
                ErrorCode::E2008,
                format!("another panpak command is working on {app_id} (install, update, rollback, repair or remove): wait for it to end, then run the command again"),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The lock of the whole store, in two modes (PANPAK-FORMAT.md 6.4). Every command that makes or changes what an app is made of (install, update, stage, apply,
/// repair, rollback, remove, and the making of a tree) holds it SHARED for as long as it works; `panpak gc` takes it EXCLUSIVE without waiting, so a collection never
/// sees work that is half done and does not depend on how long that work takes. Taken before the lock of an app.
pub fn store_lock_path(store: &Path) -> PathBuf {
    store.join("locks").join(".store")
}

fn open_store_lock(store: &Path) -> Result<File> {
    let path = store_lock_path(store);
    fs::create_dir_all(path.parent().context("lock without a folder")?)?;
    OpenOptions::new().create(true).truncate(false).write(true).open(&path).with_context(|| format!("open the lock {}", path.display()))
}

/// Shared, waiting up to `wait` (only a collection holds it exclusively, and it is short).
pub fn store_shared(store: &Path, wait: Duration) -> Result<File> {
    let f = open_store_lock(store)?;
    let start = Instant::now();
    loop {
        // SAFETY: flock(2) on a descriptor this function owns.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
            return Ok(f);
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(err).context("lock the store");
        }
        if start.elapsed() >= wait {
            return Err(coded(ErrorCode::E2008, "the store is being cleaned (panpak gc): wait a moment and run the command again"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Exclusive, without waiting: `None` when any command holds it shared.
pub fn store_exclusive_try(store: &Path) -> Result<Option<File>> {
    let f = open_store_lock(store)?;
    // SAFETY: flock(2) on a descriptor this function owns.
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    Ok((r == 0).then_some(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_taker_waits_then_gives_up_and_the_lock_goes_with_the_holder() {
        let d = std::env::temp_dir().join(format!("panpak-applock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let first = lock(&d, "org.example.app").unwrap();
        // Another app is another lock.
        drop(lock(&d, "org.example.other").unwrap());
        let t = Instant::now();
        let err = lock_for(&d, "org.example.app", Duration::from_millis(200)).unwrap_err();
        assert!(t.elapsed() >= Duration::from_millis(200));
        assert_eq!(crate::code_of(&err, ErrorCode::E2000), ErrorCode::E2008, "{err:#}");
        drop(first);
        drop(lock_for(&d, "org.example.app", Duration::from_millis(200)).unwrap());
        // A waiter gets it as soon as the holder lets go.
        let held = lock(&d, "org.example.app").unwrap();
        let d2 = d.clone();
        let waiter = std::thread::spawn(move || lock_for(&d2, "org.example.app", Duration::from_secs(5)).map(|_| ()));
        std::thread::sleep(Duration::from_millis(150));
        drop(held);
        waiter.join().unwrap().unwrap();
        assert!(lock(&d, "../x").is_err(), "a name that is not an app id is refused");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_store_lock_is_shared_for_work_and_exclusive_for_a_collection() {
        let d = std::env::temp_dir().join(format!("panpak-storelock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let a = store_shared(&d, Duration::from_millis(100)).unwrap();
        let b = store_shared(&d, Duration::from_millis(100)).unwrap();
        assert!(store_exclusive_try(&d).unwrap().is_none(), "a command at work: no collection");
        drop(a);
        assert!(store_exclusive_try(&d).unwrap().is_none(), "one is still at work");
        drop(b);
        let gc = store_exclusive_try(&d).unwrap().expect("nobody at work");
        assert!(store_shared(&d, Duration::from_millis(100)).is_err(), "a collection is running: work waits, then says so");
        drop(gc);
        assert!(store_shared(&d, Duration::from_millis(100)).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }
}

