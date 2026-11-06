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

impl TempTree {
    pub fn new(prefix: &str) -> Result<TempTree> {
        let mut template = std::env::temp_dir().join(format!("{prefix}-XXXXXX")).into_os_string().into_vec();
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

/// The tree being filled.
pub struct TreeWriter {
    root: Dir,
    dirs: HashMap<PathBuf, Dir>,
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
        Ok(TreeWriter { root: Dir::open_path(root).with_context(|| format!("open {}", root.display()))?, dirs: HashMap::new() })
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
        let mut f = self.dir(&parent)?.create_new_file(name, 0o600).with_context(|| format!("make the file {}", rel.display()))?;
        std::io::copy(from, &mut f)?;
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
}
