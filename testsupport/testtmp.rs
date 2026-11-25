// SPDX-License-Identifier: MIT OR Apache-2.0
//! Every test binary includes this file (`#[cfg(test)] #[path = "..."] mod testtmp;`). Before `main` runs, the process gets its own temporary folder
//! (`panpak-test-<pid>` under the usual one) and `TMPDIR` points to it, so every `std::env::temp_dir()` of the tests, and of the programs they start, is inside it.
//! When the process ends the folder is removed, also what a test made unreadable. `PANPAK_KEEP_TEST_TMP=1` keeps it, to look at a failure.
//! A forked child that ends does not remove it: only the process that made it does.
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static DIR: OnceLock<(u32, PathBuf)> = OnceLock::new();

#[used]
#[link_section = ".init_array"]
static INIT: extern "C" fn() = init;

extern "C" fn init() {
    if std::env::var_os("PANPAK_KEEP_TEST_TMP").is_some() {
        return;
    }
    let me = std::process::id();
    let dir = std::env::temp_dir().join(format!("panpak-test-{me}"));
    if std::fs::create_dir(&dir).is_err() {
        return;
    }
    std::env::set_var("TMPDIR", &dir);
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
