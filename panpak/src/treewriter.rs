// SPDX-License-Identifier: MIT OR Apache-2.0
//! Writing into the tree that `panpak convert` unpacks from an AppImage.
//!
//! The AppImage is untrusted input. Everything `convert` writes below the tree goes through folder descriptors
//! (`panpak_core::safefs`): a folder is opened or made one name at a time and never through a link, a file is made with
//! `O_EXCL | O_NOFOLLOW`, a link is made with `symlinkat`. A link that an AppImage puts in the way (`usr -> /somewhere`,
//! `bin -> ..`, `panpak.toml -> ~/.bashrc`) therefore makes the write fail; it can never make it land outside the tree.

use anyhow::{bail, ensure, Context, Result};
use panpak_core::safefs::{Dir, Kind};
use std::{
    collections::HashMap,
    ffi::OsStr,
    io::Read,
    os::unix::{ffi::OsStringExt, fs::PermissionsExt},
    path::{Component, Path, PathBuf},
};

/// A private temporary folder (`mkdtemp`, mode 0700) that is removed, whatever is in it, when this value is dropped.
pub struct TempTree {
    path: PathBuf,
}

/// The folder where `convert` unpacks: `panpak/convert` in the cache folder of the user (`$XDG_CACHE_HOME`, or `~/.cache`), made private (0700). Never the temporary
/// folder, which is often in memory, and a cache folder that is in memory (tmpfs, ramfs) is refused: an AppImage that is large, or made to be, would fill the memory of the machine.
pub fn work_dir(cache: Option<&Path>) -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = cache.context("no cache folder (set XDG_CACHE_HOME or HOME): `convert` does not unpack in the temporary folder, which may be in memory")?.join("panpak").join("convert");
    // Nothing is made in memory: the file system is looked at on the nearest folder that exists.
    let existing = dir.ancestors().find(|a| a.is_dir()).context("no folder of the cache path exists")?;
    ensure!(!in_memory(existing)?, "{} is in memory (tmpfs): `convert` unpacks on disk, and a large AppImage would fill the memory; set XDG_CACHE_HOME to a folder on disk", existing.display());
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).with_context(|| format!("make the work folder {}", dir.display()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// Is the folder on a file system that lives in memory?
fn in_memory(dir: &Path) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    const TMPFS_MAGIC: u64 = 0x0102_1994;
    const RAMFS_MAGIC: u64 = 0x8584_58f6;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).context("path")?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: statfs(2) with a NUL-terminated path and a buffer that this function owns.
    let r = unsafe { libc::statfs(c.as_ptr(), &mut st) };
    ensure!(r == 0, "cannot look at {}: {}", dir.display(), std::io::Error::last_os_error());
    let t = st.f_type as u64 & 0xffff_ffff;
    Ok(t == TMPFS_MAGIC || t == RAMFS_MAGIC)
}

impl TempTree {
    /// In the temporary folder: for the tests of this module only.
    #[cfg(test)]
    pub fn new(prefix: &str) -> Result<TempTree> {
        Self::new_in(&std::env::temp_dir(), prefix)
    }

    /// A new folder `<prefix>-XXXXXX` in `base`.
    pub fn new_in(base: &Path, prefix: &str) -> Result<TempTree> {
        let mut template = base.join(format!("{prefix}-XXXXXX")).into_os_string().into_vec();
        template.push(0);
        // SAFETY: mkdtemp(3) rewrites the XXXXXX of a NUL-terminated buffer that this function owns.
        let r = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
        ensure!(!r.is_null(), "cannot make a temporary folder: {}", std::io::Error::last_os_error());
        template.pop();
        Ok(TempTree { path: PathBuf::from(std::ffi::OsString::from_vec(template)) })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        // Does not follow links; a folder without the write bit is opened up first, as `remove` does.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// What an AppImage may unpack to. The AppImage is untrusted input and its files are compressed: a few kilobytes can stand for terabytes.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// One file.
    pub file_bytes: u64,
    /// All the files together: what `pack` takes (8 GiB), so a larger tree would be unpacked only to be refused.
    pub tree_bytes: u64,
    /// Folders, files and links together.
    pub entries: u64,
}

impl Limits {
    /// What the index of the squashfs says, checked before a single file is made: the number of entries, the largest file, all the files together.
    pub fn check_index(&self, entries: u64, total_bytes: u64, largest_file: u64) -> Result<()> {
        ensure!(entries <= self.entries, "the AppImage has {entries} entries, more than the {} that `convert` takes: nothing was unpacked", self.entries);
        ensure!(largest_file <= self.file_bytes, "a file of the AppImage is {largest_file} bytes, more than the {} that `convert` takes: nothing was unpacked", self.file_bytes);
        ensure!(total_bytes <= self.tree_bytes, "the AppImage unpacks to {total_bytes} bytes, more than the {} that `convert` takes: nothing was unpacked", self.tree_bytes);
        Ok(())
    }
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { file_bytes: 1 << 33, tree_bytes: 1 << 33, entries: 1_000_000 }
    }
}

/// The tree being filled.
pub struct TreeWriter {
    root: Dir,
    root_path: PathBuf,
    dirs: HashMap<PathBuf, Dir>,
    limits: Limits,
    bytes: u64,
    entries: u64,
}

/// Does the path `rel` of the tree `root` lead to a place inside the tree when the links on the way are followed, one after the other, as the file system does?
/// `link_escapes` looks at one link by itself and counts a `..` after a link as a step back of the link's own folder; the file system steps back from where the link
/// leads, so two links that are each inside can leave together. A link that is not there, or a name that does not exist, is not a way out. Absolute targets, and
/// a chain of more than 40 links (a loop), are.
pub fn resolves_inside(root: &Path, rel: &Path) -> bool {
    use std::collections::VecDeque;
    let mut todo: VecDeque<std::ffi::OsString> = rel.components().map(|c| c.as_os_str().to_os_string()).collect();
    let mut at: Vec<std::ffi::OsString> = Vec::new();
    let mut hops = 0;
    while let Some(n) = todo.pop_front() {
        if n == ".." {
            if at.pop().is_none() {
                return false;
            }
            continue;
        }
        if n == "." || n.is_empty() {
            continue;
        }
        at.push(n);
        let here = at.iter().fold(root.to_path_buf(), |p, c| p.join(c));
        if let Ok(m) = std::fs::symlink_metadata(&here) {
            if m.file_type().is_symlink() {
                hops += 1;
                let Ok(target) = std::fs::read_link(&here) else { return false };
                if hops > 40 || target.is_absolute() {
                    return false;
                }
                at.pop();
                for c in target.components().rev() {
                    todo.push_front(c.as_os_str().to_os_string());
                }
            }
        }
    }
    true
}

/// The components of a relative path of the AppImage, refusing everything but plain names.
fn plain(rel: &Path) -> Result<Vec<&OsStr>> {
    let mut out = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(n) => out.push(n),
            _ => bail!("unsafe path in the AppImage: {}", rel.display()),
        }
    }
    Ok(out)
}

impl TreeWriter {
    pub fn new(root: &Path) -> Result<TreeWriter> {
        Self::with_limits(root, Limits::default())
    }

    pub fn with_limits(root: &Path, limits: Limits) -> Result<TreeWriter> {
        Ok(TreeWriter { root: Dir::open_path(root).with_context(|| format!("open {}", root.display()))?, root_path: root.to_path_buf(), dirs: HashMap::new(), limits, bytes: 0, entries: 0 })
    }

    fn count_entry(&mut self, rel: &Path) -> Result<()> {
        self.entries += 1;
        ensure!(self.entries <= self.limits.entries, "the AppImage has more than {} entries (at {}): refused", self.limits.entries, rel.display());
        Ok(())
    }

    /// Take out of the tree the links of `links` (paths of the tree, made by `symlink`) that lead out of it once the links on the way are followed
    /// (`resolves_inside`). Returns them with their targets. What reads the tree by path afterwards cannot be led out by them.
    pub fn drop_escaping_links(&mut self, links: &[PathBuf]) -> Result<Vec<(PathBuf, String)>> {
        let mut dropped = Vec::new();
        for rel in links {
            if resolves_inside(&self.root_path, rel) {
                continue;
            }
            let target = std::fs::read_link(self.root_path.join(rel)).map(|t| t.to_string_lossy().into_owned()).unwrap_or_default();
            let (parent, name) = Self::split(rel)?;
            self.dir(&parent)?.unlink(name).with_context(|| format!("take out the link {}", rel.display()))?;
            dropped.push((rel.clone(), target));
        }
        Ok(dropped)
    }

    /// The folder `rel` below the root, made one name at a time (mode 0755) if it is not there. A link, a file or
    /// anything else that is not a folder in the way is an error.
    pub fn dir(&mut self, rel: &Path) -> Result<&Dir> {
        if rel.as_os_str().is_empty() {
            return Ok(&self.root);
        }
        if !self.dirs.contains_key(rel) {
            let names = plain(rel)?;
            let (last, above) = names.split_last().context("empty path")?;
            let parent = above.iter().fold(PathBuf::new(), |p, n| p.join(n));
            self.count_entry(rel)?;
            let made = {
                let p = self.dir(&parent)?;
                let made = p.create_dir(last).with_context(|| format!("make the folder {} (a link or a file may be in the way)", rel.display()))?;
                made.set_mode(0o755)?;
                made
            };
            self.dirs.insert(rel.to_path_buf(), made);
        }
        Ok(&self.dirs[rel])
    }

    fn split(rel: &Path) -> Result<(PathBuf, &OsStr)> {
        let names = plain(rel)?;
        let (last, above) = names.split_last().context("empty path")?;
        Ok((above.iter().fold(PathBuf::new(), |p, n| p.join(n)), last))
    }

    /// Make the folder `rel` (and the ones above it).
    pub fn make_dir(&mut self, rel: &Path) -> Result<()> {
        self.dir(rel).map(|_| ())
    }

    /// Make the file `rel` with the bytes of `from` and the permission bits `mode`. Anything already there is an error.
    pub fn write_from(&mut self, rel: &Path, from: &mut dyn Read, mode: u32) -> Result<()> {
        let (parent, name) = Self::split(rel)?;
        self.count_entry(rel)?;
        let room = self.limits.file_bytes.min(self.limits.tree_bytes.saturating_sub(self.bytes));
        let mut f = self.dir(&parent)?.create_new_file(name, 0o600).with_context(|| format!("make the file {}", rel.display()))?;
        // One byte more than there is room for: if it is read, the file is too large.
        let n = std::io::copy(&mut from.take(room + 1), &mut f)?;
        if n > room {
            if room == self.limits.file_bytes {
                bail!("{} is larger than {} bytes: refused", rel.display(), self.limits.file_bytes);
            }
            bail!("the AppImage unpacks to more than {} bytes (at {}): refused", self.limits.tree_bytes, rel.display());
        }
        self.bytes += n;
        f.set_permissions(std::fs::Permissions::from_mode(mode & 0o7777))?;
        Ok(())
    }

    /// As `write_from`, but what is there is removed first (the file or the link itself; a folder is an error).
    pub fn replace_file(&mut self, rel: &Path, data: &[u8], mode: u32) -> Result<()> {
        let (parent, name) = Self::split(rel)?;
        let d = self.dir(&parent)?;
        if let Some(st) = d.lstat(name)? {
            ensure!(st.kind != Kind::Dir, "{} is a folder in the AppImage", rel.display());
            d.unlink(name)?;
        }
        self.write_from(rel, &mut &data[..], mode)
    }

    /// Make the link `rel` with the text `target`. Anything already there is an error.
    pub fn symlink(&mut self, rel: &Path, target: &Path) -> Result<()> {
        let (parent, name) = Self::split(rel)?;
        self.count_entry(rel)?;
        self.dir(&parent)?.symlink(name, target.as_os_str()).with_context(|| format!("make the link {}", rel.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn names(p: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(p).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn writes_files_links_and_folders_inside_the_tree() {
        let t = TempTree::new("panpak-treewriter").unwrap();
        assert_eq!(fs::metadata(t.path()).unwrap().permissions().mode() & 0o777, 0o700);
        let mut w = TreeWriter::new(t.path()).unwrap();
        w.write_from(Path::new("a/b/c.txt"), &mut &b"hello"[..], 0o755).unwrap();
        w.symlink(Path::new("a/l"), Path::new("b/c.txt")).unwrap();
        w.make_dir(Path::new("empty")).unwrap();
        assert_eq!(fs::read(t.path().join("a/b/c.txt")).unwrap(), b"hello");
        assert_eq!(fs::metadata(t.path().join("a/b/c.txt")).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(fs::read_link(t.path().join("a/l")).unwrap(), Path::new("b/c.txt"));
        assert_eq!(fs::metadata(t.path().join("a/b")).unwrap().permissions().mode() & 0o777, 0o755);
        // Replacing a file, and a second plain write that must fail.
        w.replace_file(Path::new("a/b/c.txt"), b"new", 0o644).unwrap();
        assert_eq!(fs::read(t.path().join("a/b/c.txt")).unwrap(), b"new");
        assert!(w.write_from(Path::new("a/b/c.txt"), &mut &b"x"[..], 0o644).is_err());
        let p = t.path().to_path_buf();
        drop(t);
        assert!(!p.exists(), "the folder goes with the value, links and all");
    }

    #[test]
    fn a_link_in_the_way_never_lets_a_write_leave_the_tree() {
        let t = TempTree::new("panpak-treewriter").unwrap();
        let outside = TempTree::new("panpak-treewriter-outside").unwrap();
        fs::write(outside.path().join("victim"), "keep").unwrap();
        let mut w = TreeWriter::new(t.path()).unwrap();
        // A link to an absolute folder, one to a relative folder above the tree, and one to a file.
        w.symlink(Path::new("usr"), outside.path()).unwrap();
        w.symlink(Path::new("up"), Path::new("..")).unwrap();
        w.symlink(Path::new("panpak.toml"), &outside.path().join("victim")).unwrap();
        for rel in ["usr/share/x", "usr/f", "up/f", "bin/../f"] {
            assert!(w.write_from(Path::new(rel), &mut &b"x"[..], 0o644).is_err(), "{rel}");
        }
        assert!(w.make_dir(Path::new("usr/sub")).is_err());
        assert!(w.symlink(Path::new("usr/l"), Path::new("x")).is_err());
        // `panpak.toml` is replaced as a link, not written through.
        w.replace_file(Path::new("panpak.toml"), b"[app]\n", 0o644).unwrap();
        assert_eq!(fs::read_to_string(outside.path().join("victim")).unwrap(), "keep");
        assert!(fs::symlink_metadata(t.path().join("panpak.toml")).unwrap().file_type().is_file());
        assert_eq!(names(outside.path()), ["victim"], "nothing was written outside the tree");
        assert_eq!(names(t.path().parent().unwrap()).iter().filter(|n| *n == "f").count(), 0);
    }

    #[test]
    fn unsafe_paths_are_refused() {
        let t = TempTree::new("panpak-treewriter").unwrap();
        let mut w = TreeWriter::new(t.path()).unwrap();
        for rel in ["../x", "/abs", "a/../../x", "./x", ""] {
            assert!(w.write_from(Path::new(rel), &mut &b"x"[..], 0o644).is_err(), "{rel:?}");
        }
        assert!(w.replace_file(Path::new("d"), b"x", 0o644).is_ok());
        w.make_dir(Path::new("dir")).unwrap();
        assert!(w.replace_file(Path::new("dir"), b"x", 0o644).is_err(), "a folder is not replaced by a file");
    }

    #[test]
    fn a_chain_of_links_is_followed_the_way_the_file_system_does() {
        let t = TempTree::new("panpak-treewriter").unwrap();
        let mut w = TreeWriter::new(t.path()).unwrap();
        w.make_dir(Path::new("d/e")).unwrap();
        w.symlink(Path::new("d/e/l"), Path::new("..")).unwrap();
        w.write_from(Path::new("d/real"), &mut &b"x"[..], 0o644).unwrap();
        // Each is inside when read one link at a time; the first two leave the tree together.
        w.symlink(Path::new("out"), Path::new("d/e/l/../../secret")).unwrap();
        w.symlink(Path::new("out2"), Path::new("d/e/l/../../../x")).unwrap();
        w.symlink(Path::new("in"), Path::new("d/e/l/real")).unwrap();
        w.symlink(Path::new("in2"), Path::new("in")).unwrap();
        w.symlink(Path::new("abs"), Path::new("/etc/passwd")).unwrap();
        w.symlink(Path::new("loop1"), Path::new("loop2")).unwrap();
        w.symlink(Path::new("loop2"), Path::new("loop1")).unwrap();
        w.symlink(Path::new("dangling"), Path::new("not/there")).unwrap();
        let inside = |r: &str| resolves_inside(t.path(), Path::new(r));
        assert!(!inside("out") && !inside("out2") && !inside("abs") && !inside("loop1"));
        assert!(inside("in") && inside("in2") && inside("dangling") && inside("d/e/l") && inside("d/real"));
    }

    #[test]
    fn links_that_leave_the_tree_are_taken_out_and_the_others_stay() {
        let t = TempTree::new("panpak-treewriter").unwrap();
        let mut w = TreeWriter::new(t.path()).unwrap();
        w.make_dir(Path::new("d/e")).unwrap();
        w.symlink(Path::new("d/e/l"), Path::new("..")).unwrap();
        w.symlink(Path::new("app.desktop"), Path::new("d/e/l/../../secret")).unwrap();
        w.symlink(Path::new("fine"), Path::new("d")).unwrap();
        let links: Vec<PathBuf> = ["d/e/l", "app.desktop", "fine"].iter().map(PathBuf::from).collect();
        let dropped = w.drop_escaping_links(&links).unwrap();
        assert_eq!(dropped, [(PathBuf::from("app.desktop"), "d/e/l/../../secret".to_string())]);
        assert!(fs::symlink_metadata(t.path().join("app.desktop")).is_err(), "the link is gone");
        assert!(fs::symlink_metadata(t.path().join("fine")).is_ok() && fs::symlink_metadata(t.path().join("d/e/l")).is_ok());
    }

    #[test]
    fn a_file_larger_than_the_limit_stops_the_unpacking() {
        let t = TempTree::new("panpak-treewriter").unwrap();
        let mut w = TreeWriter::with_limits(t.path(), Limits { file_bytes: 100, tree_bytes: 150, entries: 1000 }).unwrap();
        // An endless reader: the limit is what stops it, not the end of the data.
        let e = w.write_from(Path::new("big"), &mut std::io::repeat(7), 0o644).unwrap_err().to_string();
        assert!(e.contains("big") && e.contains("100"), "{e}");
        assert!(fs::metadata(t.path().join("big")).unwrap().len() <= 101, "no more than the limit and a byte was written");
    }

    #[test]
    fn the_whole_tree_and_the_number_of_entries_have_limits() {
        let t = TempTree::new("panpak-treewriter").unwrap();
        let mut w = TreeWriter::with_limits(t.path(), Limits { file_bytes: 100, tree_bytes: 150, entries: 1000 }).unwrap();
        w.write_from(Path::new("a"), &mut &[1u8; 100][..], 0o644).unwrap();
        assert!(w.write_from(Path::new("b"), &mut &[1u8; 100][..], 0o644).unwrap_err().to_string().contains("150"), "the second file passes the total");
        let t2 = TempTree::new("panpak-treewriter").unwrap();
        let mut w = TreeWriter::with_limits(t2.path(), Limits { file_bytes: 100, tree_bytes: 1 << 20, entries: 5 }).unwrap();
        let mut err = None;
        for i in 0..10 {
            if let Err(e) = w.symlink(Path::new(&format!("l{i}")), Path::new("x")) {
                err = Some(e.to_string());
                break;
            }
        }
        assert!(err.is_some_and(|e| e.contains('5')), "the sixth entry is refused");
    }

    #[test]
    fn the_limits_of_a_normal_unpacking_are_those_of_pack() {
        let l = Limits::default();
        assert_eq!(l.tree_bytes, 1 << 33, "`pack` refuses a tree of more than 8 GiB");
        assert!(l.file_bytes <= l.tree_bytes && l.entries >= 100_000);
    }

    #[test]
    fn the_index_is_checked_before_anything_is_written() {
        let l = Limits { file_bytes: 100, tree_bytes: 250, entries: 10 };
        l.check_index(10, 250, 100).unwrap();
        let e = |entries, total, largest| l.check_index(entries, total, largest).unwrap_err().to_string();
        assert!(e(11, 0, 0).contains("11 entries") && e(11, 0, 0).contains("10"), "{}", e(11, 0, 0));
        assert!(e(1, 101, 101).contains("101") && e(1, 101, 101).contains("100"), "{}", e(1, 101, 101));
        assert!(e(1, 251, 100).contains("251") && e(1, 251, 100).contains("250"), "{}", e(1, 251, 100));
        // Sums that would wrap are refused too.
        assert!(l.check_index(1, u64::MAX, 1).is_err());
    }

    #[test]
    fn the_work_folder_is_a_private_folder_of_the_cache_on_disk() {
        // On disk (the build folder), not in the temporary folder, which is in memory on many machines. Made once for each test run and left: it is small.
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/treewriter-work").join(std::process::id().to_string());
        let cache = base.join("cache");
        let w = work_dir(Some(&cache)).unwrap();
        assert_eq!(w, cache.join("panpak/convert"));
        assert_eq!(fs::metadata(&w).unwrap().permissions().mode() & 0o777, 0o700);
        // A cache in memory is refused before anything is made there.
        let shm = Path::new("/dev/shm");
        if shm.is_dir() && in_memory(shm).unwrap_or(false) {
            let mem = shm.join(format!("panpak-treewriter-{}", std::process::id()));
            let e = work_dir(Some(&mem)).unwrap_err().to_string();
            assert!(e.contains("memory"), "{e}");
            assert!(!mem.exists(), "nothing was made in memory");
        }
        assert!(work_dir(None).is_err(), "no cache folder: no work folder, never the temporary folder");
    }
}
