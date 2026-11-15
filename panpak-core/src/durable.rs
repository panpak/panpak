// SPDX-License-Identifier: MIT OR Apache-2.0
//! Writing the store so that a link is never on disk before what it leads to. PANPAK-FORMAT.md section 6.2.
//!
//! The rule: a link (`current`, a tree published by a rename, a record) is made only after the data and the directory entries
//! it leads to are written back. That takes `fsync` on exactly the files and folders of Panpak, in that order, and nothing
//! else: `syncfs` would also write the data of every other program on the file system. Many files are made durable by asking
//! the kernel to start the writeback of each as it is written (`start_writeback`, `sync_file_range`) and calling `fsync` on
//! each at the end: measured on ext4, 3240 files of 123 MB cost 0.7 s that way and about 8 s with an `fsync` after each file.
use anyhow::{Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{fs::OpenOptionsExt, io::AsRawFd},
    path::{Path, PathBuf},
};

/// Ask the kernel to start writing the data of this file now, without waiting. Best effort: a file system that does not
/// support it just does nothing here, and the `fsync` at the end still makes the file durable.
pub fn start_writeback(f: &File) {
    // SAFETY: sync_file_range(2) on a descriptor this function does not own beyond the call; the result is ignored on purpose.
    unsafe {
        libc::sync_file_range(f.as_raw_fd(), 0, 0, libc::SYNC_FILE_RANGE_WRITE);
    }
}

/// `fsync` of a file (opened for reading: it works for a file we cannot write, a blob mode 0444 for example).
pub fn fsync_file(path: &Path) -> Result<()> {
    File::open(path).and_then(|f| f.sync_all()).with_context(|| format!("sync {}", path.display()))
}

/// `fsync` of a folder: makes the entries made in it (files, links, renames) durable.
pub fn fsync_dir(path: &Path) -> Result<()> {
    File::open(path).and_then(|f| f.sync_all()).with_context(|| format!("sync folder {}", path.display()))
}

/// `fsync` of many files, `threads` at a time (the same reason as for folders: the syncs wait for the disk together).
pub fn fsync_files(files: &[PathBuf], threads: usize) -> Result<()> {
    fsync_many(files, threads, fsync_file)
}

/// `fsync` of many folders, `threads` at a time: syncs that wait for the disk together share the wait. Stops at the first error.
pub fn fsync_dirs(dirs: &[PathBuf], threads: usize) -> Result<()> {
    fsync_many(dirs, threads, fsync_dir)
}

fn fsync_many(paths: &[PathBuf], threads: usize, one: fn(&Path) -> Result<()>) -> Result<()> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failed: std::sync::Mutex<Option<anyhow::Error>> = std::sync::Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..threads.max(1).min(paths.len().max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(d) = paths.get(i) else { break };
                if failed.lock().map(|f| f.is_some()).unwrap_or(true) {
                    break;
                }
                if let Err(e) = one(d) {
                    if let Ok(mut f) = failed.lock() {
                        f.get_or_insert(e);
                    }
                    break;
                }
            });
        }
    });
    match failed.into_inner().ok().flatten() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Write `data` to `path` so that it is whole or absent, never partial: a temporary file in the same folder, `fsync`, rename,
/// `fsync` of the folder.
pub fn write_file(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    let dir = path.parent().context("path without a folder")?;
    let tmp = tmp_name(path);
    let _ = fs::remove_file(&tmp);
    let res = (|| -> Result<()> {
        let mut f = OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        crate::failpoint!("durable.file_written");
        fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
        fsync_dir(dir)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// Point the link `link` at `target`, replacing what is there in one rename, and make the change durable. The link is never
/// missing and never partial.
///
/// A link of the store is written relative to its own folder when `target` is an absolute path (`../../packages/x.rec`), so that the store works
/// after it is copied or moved to another path. Read such a link with `resolve_link`.
pub fn replace_symlink(target: &Path, link: &Path) -> Result<()> {
    let dir = link.parent().context("link without a folder")?;
    let tmp = tmp_name(link);
    let _ = fs::remove_file(&tmp);
    let text = if target.is_absolute() && dir.is_absolute() { relative_to(target, dir) } else { target.to_path_buf() };
    std::os::unix::fs::symlink(&text, &tmp).with_context(|| format!("make the link {}", tmp.display()))?;
    crate::failpoint!("durable.link_made");
    if let Err(e) = fs::rename(&tmp, link) {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("rename to {}", link.display()));
    }
    fsync_dir(dir)
}

/// `target` as a path relative to the folder `dir` (both absolute and without `..`; the common prefix is dropped, then one `..` for each name left in `dir`).
pub fn relative_to(target: &Path, dir: &Path) -> PathBuf {
    let t: Vec<_> = target.components().collect();
    let d: Vec<_> = dir.components().collect();
    let common = t.iter().zip(&d).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..d.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Where a link of the store points, as an absolute path: a relative text is taken from the folder of the link, and `..` and `.` are folded
/// lexically (the folders of the store are real folders). An absolute text, as older stores have, is returned as it is.
pub fn resolve_link(link: &Path) -> std::io::Result<PathBuf> {
    let text = fs::read_link(link)?;
    if text.is_absolute() {
        return Ok(text);
    }
    let mut out = link.parent().map(Path::to_path_buf).unwrap_or_default();
    for c in text.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    Ok(out)
}

fn tmp_name(path: &Path) -> PathBuf {
    let mut n = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    n.push(format!(".tmp{}", std::process::id()));
    path.with_file_name(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-durable-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn links_of_the_store_are_relative_and_read_back_whole() {
        assert_eq!(relative_to(Path::new("/s/packages/x.rec"), Path::new("/s/apps/a.b")), Path::new("../../packages/x.rec"));
        assert_eq!(relative_to(Path::new("/s/trees/r/lib/l.so"), Path::new("/s/farms/r")), Path::new("../../trees/r/lib/l.so"));
        assert_eq!(relative_to(Path::new("/s/a"), Path::new("/s")), Path::new("a"));
        assert_eq!(relative_to(Path::new("/s"), Path::new("/s")), Path::new("."));
        let d = dir("rel");
        fs::create_dir_all(d.join("apps/a.b")).unwrap();
        fs::create_dir_all(d.join("packages")).unwrap();
        fs::write(d.join("packages/x.rec"), b"r").unwrap();
        let l = d.join("apps/a.b/current");
        replace_symlink(&d.join("packages/x.rec"), &l).unwrap();
        assert_eq!(fs::read_link(&l).unwrap(), Path::new("../../packages/x.rec"));
        assert_eq!(resolve_link(&l).unwrap(), d.join("packages/x.rec"));
        assert_eq!(fs::read(&l).unwrap(), b"r");
        // The store moved: the same link, now in another place, is still right.
        let moved = std::env::temp_dir().join(format!("panpak-durable-rel-moved-{}", std::process::id()));
        let _ = fs::remove_dir_all(&moved);
        fs::rename(&d, &moved).unwrap();
        assert_eq!(resolve_link(&moved.join("apps/a.b/current")).unwrap(), moved.join("packages/x.rec"));
        assert_eq!(fs::read(moved.join("apps/a.b/current")).unwrap(), b"r");
        // An absolute link (an older store) is returned as it is.
        std::os::unix::fs::symlink("/some/where", moved.join("abs")).unwrap();
        assert_eq!(resolve_link(&moved.join("abs")).unwrap(), Path::new("/some/where"));
        let _ = fs::remove_dir_all(&moved);
    }

    #[test]
    fn a_file_is_whole_or_absent_and_replaces_the_old_one() {
        let d = dir("file");
        let p = d.join("rec");
        write_file(&p, b"one", 0o644).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"one");
        write_file(&p, b"two", 0o644).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        // No temporary file is left, and the mode is the one asked.
        let names: Vec<_> = fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["rec"]);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o644);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_link_is_replaced_in_one_step_and_a_missing_folder_is_an_error() {
        let d = dir("link");
        let l = d.join("current");
        replace_symlink(Path::new("a"), &l).unwrap();
        replace_symlink(Path::new("b"), &l).unwrap();
        assert_eq!(fs::read_link(&l).unwrap(), Path::new("b"));
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
        assert!(replace_symlink(Path::new("a"), &d.join("no/such/current")).is_err());
        assert!(write_file(&d.join("no/such/file"), b"x", 0o644).is_err());
        // Syncing a file we cannot write, and a folder, works.
        fs::write(d.join("ro"), b"x").unwrap();
        fs::set_permissions(d.join("ro"), fs::Permissions::from_mode_for_test(0o444)).unwrap();
        fsync_file(&d.join("ro")).unwrap();
        fsync_dir(&d).unwrap();
        let f = File::open(d.join("ro")).unwrap();
        start_writeback(&f);
        let _ = fs::remove_dir_all(&d);
    }

    trait FromModeForTest {
        fn from_mode_for_test(mode: u32) -> fs::Permissions;
    }
    impl FromModeForTest for fs::Permissions {
        fn from_mode_for_test(mode: u32) -> fs::Permissions {
            use std::os::unix::fs::PermissionsExt;
            fs::Permissions::from_mode(mode)
        }
    }
}
