// SPDX-License-Identifier: MIT OR Apache-2.0
//! The private cache folder of an app is emptied when the files it was made from change.
//!
//! Some caches take a file for stale only when the file is newer than the cache (KDE's ksycoca keeps the latest time that it saw, and GTK and Qt
//! drop an icon cache older than its folder). The times in a tree come from its content (the stub, `tree_times`), so a file that changed may have an older
//! time than the cache. A cache can always be made again by the app, so the folder is emptied: by `update`, `repair` and `rollback`, when no copy of the app
//! runs, and by the first copy that starts on a tree other than the one the cache was made on (the stamp), which covers a copy that was running at that moment.
//! Never emptied: `tmp` (the app's `TMPDIR`, which the stub empties by its own rule) and the stamp. Rules: PANPAK-FORMAT.md section 6.9.
use anyhow::{Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The file in the cache folder that names the tree (its root hash, in hex) that the cache was made on.
pub const STAMP: &str = ".panpak-cache-of";

/// Remove everything in `cache` but `tmp` and the stamp. A link is removed itself, never followed. Returns how many entries went.
pub fn empty(cache: &Path) -> Result<usize> {
    let rd = match fs::read_dir(cache) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("read {}", cache.display())),
    };
    let mut n = 0;
    for e in rd {
        let e = e?;
        let name = e.file_name();
        if name == "tmp" || name == STAMP {
            continue;
        }
        let path = e.path();
        let r = if e.file_type().is_ok_and(|t| t.is_dir()) { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
        r.with_context(|| format!("remove {}", path.display()))?;
        n += 1;
    }
    Ok(n)
}

/// At the start of the first copy: when the stamp is not the tree that is started, empty the cache and write the stamp. Returns how many entries went.
/// The caller holds the proof that no other copy runs.
pub fn at_start(cache: &Path, tree: &str) -> Result<usize> {
    let stamp = cache.join(STAMP);
    if fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == tree) {
        return Ok(0);
    }
    fs::create_dir_all(cache).with_context(|| format!("create {}", cache.display()))?;
    let n = empty(cache)?;
    fs::write(&stamp, format!("{tree}\n")).with_context(|| format!("write {}", stamp.display()))?;
    Ok(n)
}

/// After an update, a repair or a rollback of `app_id`: empty its cache, unless a copy runs (`None`; the first copy that starts afterwards does it by the stamp).
pub fn after_change(app_id: &str) -> Result<Option<usize>> {
    let dirs = crate::app_dirs(app_id)?;
    let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else { return Ok(None) };
    if crate::stalelocks::copy_alive(&crate::stalelocks::copies_lock_path(&rt, app_id))? {
        return Ok(None);
    }
    empty(&dirs.cache).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-cachewipe-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_cache_goes_but_tmp_and_the_stamp_and_never_what_a_link_points_to() {
        let d = tmp("empty");
        let (cache, outside) = (d.join("cache"), d.join("outside"));
        fs::create_dir_all(cache.join("tmp")).unwrap();
        fs::create_dir_all(cache.join("fontconfig/sub")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(cache.join("tmp/big"), b"x").unwrap();
        fs::write(cache.join("ksycoca6_en_abc"), b"x").unwrap();
        fs::write(cache.join("fontconfig/sub/a.cache"), b"x").unwrap();
        fs::write(outside.join("keep"), b"x").unwrap();
        symlink(&outside, cache.join("link")).unwrap();
        fs::write(cache.join(STAMP), b"old\n").unwrap();
        assert_eq!(empty(&cache).unwrap(), 3);
        assert!(cache.join("tmp/big").exists() && cache.join(STAMP).exists());
        assert!(!cache.join("ksycoca6_en_abc").exists() && !cache.join("fontconfig").exists() && fs::symlink_metadata(cache.join("link")).is_err());
        assert!(outside.join("keep").exists(), "a link is removed itself");
        assert_eq!(empty(&d.join("none")).unwrap(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_start_on_another_tree_empties_once() {
        let d = tmp("stamp");
        let cache = d.join("cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("ksycoca6"), b"x").unwrap();
        assert_eq!(at_start(&cache, "aaaa").unwrap(), 1, "no stamp: the first start empties");
        fs::write(cache.join("ksycoca6"), b"y").unwrap();
        assert_eq!(at_start(&cache, "aaaa").unwrap(), 0, "the same tree: nothing");
        assert!(cache.join("ksycoca6").exists());
        assert_eq!(at_start(&cache, "bbbb").unwrap(), 1, "another tree (an update or a rollback): empties");
        assert_eq!(fs::read_to_string(cache.join(STAMP)).unwrap().trim(), "bbbb");
        assert_eq!(at_start(&d.join("new"), "cccc").unwrap(), 0, "a cache folder that is not there is made");
        let _ = fs::remove_dir_all(&d);
    }
}
