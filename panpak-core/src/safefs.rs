// SPDX-License-Identifier: MIT OR Apache-2.0
//! Deleting, measuring and renaming inside folders that other programs can write, without ever
//! following a link. Rules: THREAT-MODEL.md, "Removal".
//!
//! Every operation below a folder goes through a descriptor of that folder (`openat`, `unlinkat`,
//! `fstatat`, `renameat2`) with `O_NOFOLLOW`: no path is built from strings after the first
//! folder is opened, so a link put in place of a folder (or swapped in while the walk runs) is
//! never entered. A link found while deleting is removed itself, never its target. A walk does not
//! cross into another file system and has a depth limit.
use std::{
    collections::HashSet,
    ffi::{CStr, CString, OsStr, OsString},
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::{OsStrExt, OsStringExt},
    },
    path::Path,
};

/// How deep a walk goes. A tree below this is left in place and reported.
pub const MAX_DEPTH: usize = 512;

/// What an entry is, without following links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Link,
    Other,
}

#[derive(Debug, Clone, Copy)]
pub struct Stat {
    pub kind: Kind,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub size: u64,
    /// Disk blocks of 512 bytes.
    pub blocks: u64,
    pub uid: u32,
    pub mode: u32,
}

#[allow(clippy::unnecessary_cast)] // the field types differ between targets
fn stat_of(st: &libc::stat) -> Stat {
    let kind = match st.st_mode & libc::S_IFMT {
        libc::S_IFDIR => Kind::Dir,
        libc::S_IFREG => Kind::File,
        libc::S_IFLNK => Kind::Link,
        _ => Kind::Other,
    };
    Stat {
        kind,
        dev: st.st_dev as u64,
        ino: st.st_ino as u64,
        nlink: st.st_nlink as u64,
        size: st.st_size.max(0) as u64,
        blocks: st.st_blocks.max(0) as u64,
        uid: st.st_uid,
        mode: st.st_mode & 0o7777,
    }
}

/// An entry name: not empty, no `/`, no NUL, not `.` or `..`.
pub fn valid_name(name: &OsStr) -> bool {
    let b = name.as_bytes();
    !b.is_empty() && b != b"." && b != b".." && !b.contains(&b'/') && !b.contains(&0)
}

fn cname(name: &OsStr) -> io::Result<CString> {
    if !valid_name(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("not a plain entry name: {name:?}")));
    }
    CString::new(name.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn cvt(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

fn cvt_isize(r: isize) -> io::Result<usize> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r as usize)
    }
}

/// An open folder.
#[derive(Debug)]
pub struct Dir {
    fd: OwnedFd,
}

/// What a deletion did.
#[derive(Debug, Default, Clone)]
pub struct Removed {
    pub entries: u64,
    /// The first things that could not be removed (at most 8), with the reason.
    pub failures: Vec<String>,
    pub failed: u64,
}

impl Removed {
    fn fail(&mut self, what: String) {
        self.failed += 1;
        if self.failures.len() < 8 {
            self.failures.push(what);
        }
    }
}

impl Dir {
    /// Open a folder by path. The last component must not be a link; the components above are the
    /// caller's to trust (a base such as the user's data folder).
    pub fn open_path(path: &Path) -> io::Result<Dir> {
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: plain open(2) with a NUL-terminated path.
        let fd = cvt(unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) })?;
        // SAFETY: `fd` was just returned by open and is owned here.
        Ok(Dir { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    /// Open the folder `name` below this one. A link, a file or anything else in its place is an error.
    pub fn open_dir(&self, name: &OsStr) -> io::Result<Dir> {
        let c = cname(name)?;
        // SAFETY: openat(2) relative to a descriptor this value owns.
        let fd = cvt(unsafe { libc::openat(self.fd.as_raw_fd(), c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) })?;
        // SAFETY: `fd` was just returned by openat and is owned here.
        Ok(Dir { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    /// As `open_dir`, but `None` when there is nothing by that name.
    pub fn open_dir_opt(&self, name: &OsStr) -> io::Result<Option<Dir>> {
        match self.open_dir(name) {
            Ok(d) => Ok(Some(d)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Create the folder `name` (mode 0700) and open it. An existing folder is opened.
    pub fn create_dir(&self, name: &OsStr) -> io::Result<Dir> {
        let c = cname(name)?;
        // SAFETY: mkdirat(2) relative to a descriptor this value owns.
        let r = unsafe { libc::mkdirat(self.fd.as_raw_fd(), c.as_ptr(), 0o700) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::EEXIST) {
                return Err(e);
            }
        }
        self.open_dir(name)
    }

    /// Open the regular file `name` for reading; a link or anything that is not a regular file is an error.
    pub fn open_file(&self, name: &OsStr) -> io::Result<File> {
        let c = cname(name)?;
        // SAFETY: openat(2) relative to a descriptor this value owns; O_NONBLOCK so a FIFO cannot block.
        let fd = cvt(unsafe { libc::openat(self.fd.as_raw_fd(), c.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK) })?;
        // SAFETY: `fd` was just returned by openat and is owned here.
        let f = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        if !f.metadata()?.file_type().is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
        }
        Ok(f)
    }

    /// Make or open the file `name` for writing (0600), never through a link.
    pub fn create_file(&self, name: &OsStr) -> io::Result<File> {
        let c = cname(name)?;
        // SAFETY: openat(2) relative to a descriptor this value owns.
        let fd = cvt(unsafe { libc::openat(self.fd.as_raw_fd(), c.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600 as libc::c_uint) })?;
        // SAFETY: `fd` was just returned by openat and is owned here.
        Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    /// Make the new regular file `name` for writing with `mode`. It fails if anything, a link included, is already there.
    pub fn create_new_file(&self, name: &OsStr, mode: u32) -> io::Result<File> {
        let c = cname(name)?;
        // SAFETY: openat(2) relative to a descriptor this value owns; O_EXCL with O_CREAT never opens an existing entry.
        let fd = cvt(unsafe {
            libc::openat(self.fd.as_raw_fd(), c.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, (mode & 0o7777) as libc::c_uint)
        })?;
        // SAFETY: `fd` was just returned by openat and is owned here.
        Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    /// Make the link `name` with the text `target`. It fails if anything is already there.
    pub fn symlink(&self, name: &OsStr, target: &OsStr) -> io::Result<()> {
        let (c, t) = (cname(name)?, CString::new(target.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?);
        // SAFETY: symlinkat(2) relative to a descriptor this value owns.
        cvt(unsafe { libc::symlinkat(t.as_ptr(), self.fd.as_raw_fd(), c.as_ptr()) }).map(|_| ())
    }

    /// Set the permission bits of this folder.
    pub fn set_mode(&self, mode: u32) -> io::Result<()> {
        // SAFETY: fchmod(2) on a descriptor this value owns.
        cvt(unsafe { libc::fchmod(self.fd.as_raw_fd(), mode & 0o7777) }).map(|_| ())
    }

    /// What `name` is, without following a link. `None` when it does not exist.
    pub fn lstat(&self, name: &OsStr) -> io::Result<Option<Stat>> {
        let c = cname(name)?;
        // SAFETY: an all-zero `stat` is a valid value to be filled in.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: fstatat(2) relative to a descriptor this value owns, not following links.
        let r = unsafe { libc::fstatat(self.fd.as_raw_fd(), c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
        if r < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::NotFound { Ok(None) } else { Err(e) };
        }
        Ok(Some(stat_of(&st)))
    }

    /// What this folder is.
    pub fn fstat(&self) -> io::Result<Stat> {
        // SAFETY: an all-zero `stat` is a valid value to be filled in.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: fstat(2) on a descriptor this value owns.
        cvt(unsafe { libc::fstat(self.fd.as_raw_fd(), &mut st) })?;
        Ok(stat_of(&st))
    }

    /// The names in this folder (not `.` and `..`), in no particular order.
    pub fn list(&self) -> io::Result<Vec<OsString>> {
        // SAFETY: dup(2) of a descriptor this value owns; fdopendir takes over the copy.
        let dup = cvt(unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) })?;
        // SAFETY: `dup` is a valid directory descriptor; on success the DIR owns it.
        let dir = unsafe { libc::fdopendir(dup) };
        if dir.is_null() {
            let e = io::Error::last_os_error();
            // SAFETY: fdopendir failed, so `dup` is still ours to close.
            unsafe { libc::close(dup) };
            return Err(e);
        }
        // SAFETY: the copy shares the offset of the original; start from the beginning.
        unsafe { libc::rewinddir(dir) };
        let mut out = Vec::new();
        let result = loop {
            // SAFETY: errno is cleared so that the end of the folder can be told from an error.
            unsafe { *libc::__errno_location() = 0 };
            // SAFETY: `dir` is a valid open DIR until closedir below.
            let ent = unsafe { libc::readdir(dir) };
            if ent.is_null() {
                let e = io::Error::last_os_error();
                break if e.raw_os_error() == Some(0) { Ok(()) } else { Err(e) };
            }
            // SAFETY: readdir returned a valid entry whose name is NUL-terminated.
            let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                out.push(OsString::from_vec(name.to_vec()));
            }
        };
        // SAFETY: `dir` is valid and closed exactly once.
        unsafe { libc::closedir(dir) };
        result.map(|()| out)
    }

    pub fn unlink(&self, name: &OsStr) -> io::Result<()> {
        let c = cname(name)?;
        // SAFETY: unlinkat(2) relative to a descriptor this value owns; it never follows a link.
        cvt(unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), 0) }).map(|_| ())
    }

    pub fn rmdir(&self, name: &OsStr) -> io::Result<()> {
        let c = cname(name)?;
        // SAFETY: unlinkat(2) with AT_REMOVEDIR relative to a descriptor this value owns.
        cvt(unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), libc::AT_REMOVEDIR) }).map(|_| ())
    }

    /// Move `name` to `new_name` in `other` (the same file system), failing when the target exists.
    pub fn rename_to(&self, name: &OsStr, other: &Dir, new_name: &OsStr) -> io::Result<()> {
        let (a, b) = (cname(name)?, cname(new_name)?);
        // SAFETY: renameat2(2) between descriptors this code owns; RENAME_NOREPLACE never overwrites.
        let r = unsafe { libc::syscall(libc::SYS_renameat2, self.fd.as_raw_fd(), a.as_ptr(), other.fd.as_raw_fd(), b.as_ptr(), 1 as libc::c_uint) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// The target of the link `name`, as written (never followed).
    pub fn read_link(&self, name: &OsStr) -> io::Result<OsString> {
        let c = cname(name)?;
        let mut buf = vec![0u8; 4096];
        // SAFETY: readlinkat(2) relative to a descriptor this value owns, into a buffer of the length given.
        let n = cvt_isize(unsafe { libc::readlinkat(self.fd.as_raw_fd(), c.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) })?;
        buf.truncate(n);
        Ok(OsString::from_vec(buf))
    }

    /// Open (and make if missing) the file `name`, never through a link, and take its exclusive lock without
    /// waiting. `None` when another process holds the lock. The lock goes with the returned file.
    pub fn lock_file(&self, name: &OsStr) -> io::Result<Option<File>> {
        let f = self.create_file(name)?;
        // SAFETY: flock(2) on a descriptor owned by `f`.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(f));
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(e)
        }
    }

    /// Take the exclusive lock of this folder without waiting. `false` when another process holds it.
    pub fn try_lock(&self) -> io::Result<bool> {
        // SAFETY: flock(2) on a descriptor this value owns; the lock goes with the descriptor.
        let r = unsafe { libc::flock(self.fd.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if r == 0 {
            return Ok(true);
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(false)
        } else {
            Err(e)
        }
    }

    /// Remove `name` and everything below it. A link (to anything) is removed itself and never entered.
    /// A folder on another file system, or deeper than `MAX_DEPTH`, is left in place and reported.
    /// Returns what was removed and what was not; an error only when `name` could not even be looked at.
    pub fn remove_all(&self, name: &OsStr) -> io::Result<Removed> {
        let mut out = Removed::default();
        let Some(st) = self.lstat(name)? else { return Ok(out) };
        if st.kind != Kind::Dir {
            match self.unlink(name) {
                Ok(()) => out.entries += 1,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => out.fail(format!("{name:?}: {e}")),
            }
            return Ok(out);
        }
        match self.open_dir(name) {
            Ok(child) => {
                // The folder opened is the one that was looked at.
                match child.fstat() {
                    Ok(now) if now.dev == st.dev && now.ino == st.ino => {
                        child.empty(st.dev, 1, &mut out);
                        drop(child);
                        match self.rmdir(name) {
                            Ok(()) => out.entries += 1,
                            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                            Err(e) => out.fail(format!("{name:?}: {e}")),
                        }
                    }
                    _ => out.fail(format!("{name:?}: replaced while it was being removed")),
                }
            }
            Err(e) => out.fail(format!("{name:?}: {e}")),
        }
        Ok(out)
    }

    /// Remove everything in this folder.
    fn empty(&self, root_dev: u64, depth: usize, out: &mut Removed) {
        // Folders of a read-only tree have no write bit: the owner may add it to its own folder.
        if let Ok(me) = self.fstat() {
            if me.mode & 0o700 != 0o700 {
                // SAFETY: fchmod(2) on a descriptor this value owns.
                unsafe { libc::fchmod(self.fd.as_raw_fd(), me.mode | 0o700) };
            }
        }
        let names = match self.list() {
            Ok(n) => n,
            Err(e) => {
                out.fail(format!("cannot list a folder: {e}"));
                return;
            }
        };
        for name in names {
            let st = match self.lstat(&name) {
                Ok(Some(s)) => s,
                Ok(None) => continue,
                Err(e) => {
                    out.fail(format!("{name:?}: {e}"));
                    continue;
                }
            };
            if st.kind != Kind::Dir {
                match self.unlink(&name) {
                    Ok(()) => out.entries += 1,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => out.fail(format!("{name:?}: {e}")),
                }
                continue;
            }
            if st.dev != root_dev {
                out.fail(format!("{name:?}: on another file system, left in place"));
                continue;
            }
            if depth >= MAX_DEPTH {
                out.fail(format!("{name:?}: deeper than {MAX_DEPTH} levels, left in place"));
                continue;
            }
            let child = match self.open_dir(&name) {
                Ok(c) => c,
                Err(e) => {
                    out.fail(format!("{name:?}: {e}"));
                    continue;
                }
            };
            match child.fstat() {
                Ok(now) if now.dev == st.dev && now.ino == st.ino => {}
                _ => {
                    out.fail(format!("{name:?}: replaced while it was being removed"));
                    continue;
                }
            }
            child.empty(root_dev, depth + 1, out);
            drop(child);
            match self.rmdir(&name) {
                Ok(()) => out.entries += 1,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => out.fail(format!("{name:?}: {e}")),
            }
        }
    }

    /// Disk space used by `name` and everything below it, links not followed, a file with several
    /// links counted once. Unreadable parts count for nothing.
    pub fn disk_usage(&self, name: &OsStr) -> io::Result<u64> {
        let Some(st) = self.lstat(name)? else { return Ok(0) };
        let mut seen = HashSet::new();
        let mut total = st.blocks * 512;
        if st.kind == Kind::Dir {
            if let Ok(child) = self.open_dir(name) {
                child.usage_below(st.dev, 1, &mut seen, &mut total);
            }
        }
        Ok(total)
    }

    fn usage_below(&self, root_dev: u64, depth: usize, seen: &mut HashSet<(u64, u64)>, total: &mut u64) {
        let Ok(names) = self.list() else { return };
        for name in names {
            let Ok(Some(st)) = self.lstat(&name) else { continue };
            if st.nlink > 1 && st.kind != Kind::Dir && !seen.insert((st.dev, st.ino)) {
                continue;
            }
            *total += st.blocks * 512;
            if st.kind == Kind::Dir && st.dev == root_dev && depth < MAX_DEPTH {
                if let Ok(child) = self.open_dir(&name) {
                    child.usage_below(root_dev, depth + 1, seen, total);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
        path::PathBuf,
    };

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-safefs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn new_files_and_links_never_go_through_a_link() {
        use std::os::unix::fs::MetadataExt;
        let d = scratch("newfile");
        fs::create_dir_all(d.join("t")).unwrap();
        fs::create_dir_all(d.join("outside")).unwrap();
        let t = Dir::open_path(&d.join("t")).unwrap();
        let f = t.create_new_file(OsStr::new("f"), 0o755).unwrap();
        assert!(f.metadata().unwrap().file_type().is_file());
        t.symlink(OsStr::new("l"), OsStr::new("../outside")).unwrap();
        // A second file or link by the same name, and a file through a link, are errors; nothing is written outside.
        assert!(t.create_new_file(OsStr::new("f"), 0o600).is_err());
        assert!(t.create_new_file(OsStr::new("l"), 0o600).is_err());
        assert!(t.symlink(OsStr::new("f"), OsStr::new("x")).is_err());
        assert!(t.open_dir(OsStr::new("l")).is_err(), "a link is not entered");
        assert!(t.create_dir(OsStr::new("l")).is_err(), "nor made into a folder");
        assert_eq!(fs::read_dir(d.join("outside")).unwrap().count(), 0);
        // The link is a link, and a folder can be given its mode.
        assert!(fs::symlink_metadata(d.join("t/l")).unwrap().file_type().is_symlink());
        let sub = t.create_dir(OsStr::new("sub")).unwrap();
        sub.set_mode(0o755).unwrap();
        assert_eq!(fs::metadata(d.join("t/sub")).unwrap().mode() & 0o7777, 0o755);
        for bad in ["..", "a/b", ""] {
            assert!(t.create_new_file(OsStr::new(bad), 0o600).is_err(), "{bad:?}");
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn names_are_plain() {
        for bad in ["", ".", "..", "a/b", "/", "a\0b"] {
            assert!(!valid_name(OsStr::new(bad)), "{bad:?}");
        }
        assert!(valid_name(OsStr::new("org.example.app-1.panpak")));
    }

    #[test]
    fn a_tree_without_write_bits_is_removed() {
        let d = scratch("ro");
        fs::create_dir_all(d.join("t/a/b")).unwrap();
        fs::write(d.join("t/a/b/f"), "x").unwrap();
        fs::write(d.join("t/a/g"), "x").unwrap();
        for p in ["t/a/b", "t/a", "t"] {
            fs::set_permissions(d.join(p), fs::Permissions::from_mode(0o555)).unwrap();
        }
        let root = Dir::open_path(&d).unwrap();
        let r = root.remove_all(OsStr::new("t")).unwrap();
        assert_eq!(r.failed, 0, "{r:?}");
        assert_eq!(r.entries, 5);
        assert!(!d.join("t").exists());
    }

    #[test]
    fn a_link_inside_is_removed_and_its_target_is_untouched() {
        let d = scratch("link");
        let outside = d.join("outside");
        fs::create_dir_all(outside.join("sub")).unwrap();
        fs::write(outside.join("sub/precious"), "keep").unwrap();
        fs::write(outside.join("file"), "keep").unwrap();
        fs::create_dir_all(d.join("app/data")).unwrap();
        symlink(&outside, d.join("app/data/to-dir")).unwrap();
        symlink(outside.join("file"), d.join("app/data/to-file")).unwrap();
        symlink("/nonexistent", d.join("app/dangling")).unwrap();
        let root = Dir::open_path(&d).unwrap();
        let r = root.remove_all(OsStr::new("app")).unwrap();
        assert_eq!(r.failed, 0, "{r:?}");
        assert!(!d.join("app").exists());
        assert_eq!(fs::read_to_string(outside.join("sub/precious")).unwrap(), "keep");
        assert_eq!(fs::read_to_string(outside.join("file")).unwrap(), "keep");
    }

    #[test]
    fn a_link_in_place_of_the_folder_is_removed_itself() {
        let d = scratch("top");
        let outside = d.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("precious"), "keep").unwrap();
        symlink(&outside, d.join("app")).unwrap();
        let root = Dir::open_path(&d).unwrap();
        // The folder cannot be opened as a folder...
        assert!(root.open_dir(OsStr::new("app")).is_err());
        // ... and a deletion removes the link, not what it points to.
        let r = root.remove_all(OsStr::new("app")).unwrap();
        assert_eq!((r.failed, r.entries), (0, 1));
        assert!(fs::symlink_metadata(d.join("app")).is_err());
        assert_eq!(fs::read_to_string(outside.join("precious")).unwrap(), "keep");
    }

    #[test]
    fn a_base_that_is_a_link_is_not_opened() {
        let d = scratch("base");
        fs::create_dir_all(d.join("real")).unwrap();
        symlink(d.join("real"), d.join("lnk")).unwrap();
        assert!(Dir::open_path(&d.join("lnk")).is_err());
        assert!(Dir::open_path(&d.join("real")).is_ok());
    }

    #[test]
    fn a_missing_entry_is_nothing_to_do() {
        let d = scratch("none");
        let root = Dir::open_path(&d).unwrap();
        let r = root.remove_all(OsStr::new("absent")).unwrap();
        assert_eq!((r.failed, r.entries), (0, 0));
        assert!(root.lstat(OsStr::new("absent")).unwrap().is_none());
    }

    #[test]
    fn a_walk_that_is_too_deep_leaves_the_rest_and_says_so() {
        let d = scratch("deep");
        // Build with descriptors: a path this long cannot be created by name.
        let root = Dir::open_path(&d).unwrap();
        let mut cur = root.create_dir(OsStr::new("t")).unwrap();
        for _ in 0..MAX_DEPTH + 3 {
            cur = cur.create_dir(OsStr::new("d")).unwrap();
        }
        drop(cur);
        let r = root.remove_all(OsStr::new("t")).unwrap();
        assert!(r.failed > 0 && r.failures[0].contains("deeper"), "{r:?}");
        assert!(root.lstat(OsStr::new("t")).unwrap().is_some());
    }

    #[test]
    fn rename_never_replaces_and_lists_are_plain() {
        let d = scratch("rename");
        fs::create_dir_all(d.join("a")).unwrap();
        fs::create_dir_all(d.join("b")).unwrap();
        fs::write(d.join("a/x"), "1").unwrap();
        let root = Dir::open_path(&d).unwrap();
        let a = root.open_dir(OsStr::new("a")).unwrap();
        let b = root.open_dir(OsStr::new("b")).unwrap();
        a.rename_to(OsStr::new("x"), &b, OsStr::new("y")).unwrap();
        assert_eq!(fs::read_to_string(d.join("b/y")).unwrap(), "1");
        fs::write(d.join("a/z"), "2").unwrap();
        assert!(a.rename_to(OsStr::new("z"), &b, OsStr::new("y")).is_err(), "no replace");
        assert_eq!(fs::read_to_string(d.join("b/y")).unwrap(), "1");
        assert_eq!(b.list().unwrap(), vec![OsString::from("y")]);
        assert!(a.rename_to(OsStr::new("../b"), &b, OsStr::new("q")).is_err());
    }

    #[test]
    fn usage_counts_links_once_and_does_not_follow() {
        let d = scratch("du");
        fs::create_dir_all(d.join("t")).unwrap();
        fs::write(d.join("t/big"), vec![1u8; 100_000]).unwrap();
        fs::hard_link(d.join("t/big"), d.join("t/big2")).unwrap();
        let outside = d.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("huge"), vec![1u8; 1_000_000]).unwrap();
        symlink(&outside, d.join("t/lnk")).unwrap();
        let root = Dir::open_path(&d).unwrap();
        let n = root.disk_usage(OsStr::new("t")).unwrap();
        assert!((100_000..200_000).contains(&n), "{n}");
        assert_eq!(root.disk_usage(OsStr::new("absent")).unwrap(), 0);
    }

    #[test]
    fn open_file_refuses_links_and_other_kinds() {
        let d = scratch("file");
        fs::write(d.join("f"), "hello").unwrap();
        symlink(d.join("f"), d.join("l")).unwrap();
        fs::create_dir(d.join("sub")).unwrap();
        let root = Dir::open_path(&d).unwrap();
        assert!(root.open_file(OsStr::new("f")).is_ok());
        assert!(root.open_file(OsStr::new("l")).is_err());
        assert!(root.open_file(OsStr::new("sub")).is_err());
    }

    #[test]
    fn links_are_read_and_a_lock_file_is_exclusive() {
        let d = scratch("rl");
        symlink("/some/target", d.join("l")).unwrap();
        let root = Dir::open_path(&d).unwrap();
        assert_eq!(root.read_link(OsStr::new("l")).unwrap(), OsString::from("/some/target"));
        assert!(root.read_link(OsStr::new("absent")).is_err());
        let first = root.lock_file(OsStr::new("lock")).unwrap();
        assert!(first.is_some());
        assert!(root.lock_file(OsStr::new("lock")).unwrap().is_none());
        drop(first);
        assert!(root.lock_file(OsStr::new("lock")).unwrap().is_some());
        // A link in place of the lock file is not followed.
        symlink(d.join("elsewhere"), d.join("lk")).unwrap();
        assert!(root.lock_file(OsStr::new("lk")).is_err());
        assert!(!d.join("elsewhere").exists());
    }

    #[test]
    fn the_lock_is_exclusive_between_descriptors() {
        let d = scratch("lock");
        let a = Dir::open_path(&d).unwrap();
        let b = Dir::open_path(&d).unwrap();
        assert!(a.try_lock().unwrap());
        assert!(!b.try_lock().unwrap());
        drop(a);
        assert!(b.try_lock().unwrap());
    }
}
