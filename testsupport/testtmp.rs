// SPDX-License-Identifier: MIT OR Apache-2.0
//! Every test binary includes this file (`#[cfg(test)] #[path = "..."] mod testtmp;`). Before `main` runs, the process gets its own temporary folder
//! (`panpak-t<pid>` under the usual one; short, because a socket path has 108 bytes at most) and `TMPDIR` points to it, so every `std::env::temp_dir()` of the tests, and of the programs they start, is inside it.
//! When the process ends the folder is removed, also what a test made unreadable. `PANPAK_KEEP_TEST_TMP=1` keeps it, to look at a failure.
//! A forked child that ends does not remove it: only the process that made it does. A test that starts its own binary again (the helper that runs one test alone)
//! finds `PANPAK_TEST_TMP_OWNED` and uses the folder of its parent, which removes it.
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static DIR: OnceLock<(u32, PathBuf)> = OnceLock::new();

#[used]
#[link_section = ".init_array"]
static INIT: extern "C" fn() = init;

extern "C" fn init() {
    if std::env::var_os("PANPAK_TEST_TMP_OWNED").is_some() {
        return;
    }
    let me = std::process::id();
    let base = std::env::temp_dir();
    if !base.is_absolute() {
        return; // a relative TMPDIR: nothing is made or removed
    }
    let dir = base.join(format!("panpak-t{me}"));
    // A folder with this name is left by an earlier process that had the same number and was killed: it is ours, by its name.
    if dir.exists() {
        remove(&dir);
    }
    if let Err(e) = std::fs::create_dir(&dir) {
        eprintln!("testtmp: cannot make {}: {e}; the tests use the shared temporary folder", dir.display());
        return;
    }
    std::env::set_var("TMPDIR", &dir);
    std::env::set_var("PANPAK_TEST_TMP_OWNED", "1");
    if std::env::var_os("PANPAK_KEEP_TEST_TMP").is_some() {
        return; // the folder is made and used, and left in place
    }
    let _ = DIR.set((me, dir));
    // SAFETY: `cleanup` is a plain function that takes no argument.
    unsafe {
        libc::atexit(cleanup);
    }
}

extern "C" fn cleanup() {
    if let Some((owner, dir)) = DIR.get() {
        if *owner == std::process::id() {
            remove(dir);
        }
    }
}

fn remove(p: &Path) {
    let Ok(md) = std::fs::symlink_metadata(p) else { return };
    if md.is_dir() {
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                remove(&e.path());
            }
        }
        let _ = std::fs::remove_dir(p);
    } else {
        let _ = std::fs::remove_file(p);
    }
}
