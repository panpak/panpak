// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stale lock files in the private folders of an app.
//!
//! In the isolated view the process ids start again at 1 in every start, so a lock file that holds a
//! process id (Chromium and Electron `SingletonLock`, Qt `QLockFile`, Firefox `lock`) left by a
//! copy that was killed looks alive to the next copy and the app refuses to start. The per-app lock
//! of the runtime folder tells with certainty that no copy is alive; at that moment every such lock
//! file in the private folders is stale and is removed. The files are recognised by their content,
//! not by a list in the manifest. Only the private folders are searched (never `xdg_dirs`,
//! `home_paths` or documents), to a limited depth, without following links: a link is removed itself
//! and never its target, and every step is relative to the descriptor of the folder.
//! Rules: PANPAK-FORMAT.md §9.4.
use anyhow::{bail, Context, Result};
use std::{
    ffi::{CStr, CString, OsStr, OsString},
    fs::File,
    os::unix::{ffi::{OsStrExt, OsStringExt}, io::{AsRawFd, FromRawFd, RawFd}},
    path::{Path, PathBuf},
};

/// How deep below a private folder the search goes (the folder itself is depth 0).
pub const MAX_DEPTH: usize = 3;

/// Folders of caches that are large and hold no lock: not entered.
const SKIP_DIRS: &[&str] = &["Cache", "Code Cache", "GPUCache", "GrShaderCache", "ShaderCache", "DawnCache", "CacheStorage", "node_modules", "blob_storage", "IndexedDB", "Service Worker"];

/// A lock file the size of which is above this is not one (Qt writes about 50 bytes).
const MAX_LOCK_SIZE: i64 = 512;

fn cstr(s: &OsStr) -> Option<CString> {
    CString::new(s.as_bytes()).ok()
}

fn open_dir_at(dirfd: RawFd, name: &CStr) -> Option<File> {
    // SAFETY: plain openat(2); the result is owned by the File.
    let fd = unsafe { libc::openat(dirfd, name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    // SAFETY: fd is a new descriptor that nothing else owns.
    (fd >= 0).then(|| unsafe { File::from_raw_fd(fd) })
}

/// The names in a folder, from its descriptor (not following anything).
fn names(dir: &File) -> Vec<OsString> {
    // SAFETY: the duplicate is owned by the DIR stream, which closedir closes.
    unsafe {
        let fd = libc::dup(dir.as_raw_fd());
        if fd < 0 {
            return Vec::new();
        }
        let d = libc::fdopendir(fd);
        if d.is_null() {
            libc::close(fd);
            return Vec::new();
        }
        libc::rewinddir(d);
        let mut out = Vec::new();
        loop {
            let e = libc::readdir(d);
            if e.is_null() {
                break;
            }
            let n = CStr::from_ptr((*e).d_name.as_ptr()).to_bytes();
            if n != b"." && n != b".." {
                out.push(OsString::from_vec(n.to_vec()));
            }
        }
        libc::closedir(d);
        out
    }
}

fn stat_at(dirfd: RawFd, name: &CStr) -> Option<libc::stat> {
    // SAFETY: the buffer is plain data written by fstatat.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        (libc::fstatat(dirfd, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) == 0).then_some(st)
    }
}

fn link_target(dirfd: RawFd, name: &CStr) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 512];
    // SAFETY: readlinkat writes at most buf.len() bytes.
    let n = unsafe { libc::readlinkat(dirfd, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    let n = usize::try_from(n).ok().filter(|&n| n < buf.len())?;
    buf.truncate(n);
    Some(buf)
}

fn read_small(dirfd: RawFd, name: &CStr) -> Option<Vec<u8>> {
    use std::io::Read;
    // SAFETY: plain openat(2); O_NONBLOCK so that a fifo cannot hold the caller.
    let fd = unsafe { libc::openat(dirfd, name.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: fd is a new descriptor that nothing else owns.
    let f = unsafe { File::from_raw_fd(fd) };
    let mut v = Vec::new();
    f.take(MAX_LOCK_SIZE as u64 + 1).read_to_end(&mut v).ok()?;
    (v.len() <= MAX_LOCK_SIZE as usize).then_some(v)
}

fn digits(b: &[u8]) -> bool {
    !b.is_empty() && b.len() <= 10 && b.iter().all(u8::is_ascii_digit)
}

/// Chromium: `SingletonLock` -> `<host>-<pid>`.
pub fn is_chromium_lock_target(t: &[u8]) -> bool {
    t.iter().rposition(|&c| c == b'-').is_some_and(|i| i > 0 && digits(&t[i + 1..]))
}

/// Firefox: `lock` -> `<ip>:+<pid>`.
pub fn is_firefox_lock_target(t: &[u8]) -> bool {
    t.windows(2).position(|w| w == b":+").is_some_and(|i| i > 0 && t[..i].iter().all(|c| c.is_ascii_hexdigit() || *c == b'.' || *c == b':') && digits(&t[i + 2..]))
}

/// Qt `QLockFile`: the process id, the name of the application and the host, one per line.
pub fn is_qt_lock_content(c: &[u8]) -> bool {
    if c.contains(&0) {
        return false;
    }
    let Ok(text) = std::str::from_utf8(c) else { return false };
    let lines: Vec<&str> = text.split_terminator('\n').collect();
    lines.len() >= 3 && digits(lines[0].as_bytes()) && !lines[2].is_empty()
}

struct Walk<'a> {
    remove: bool,
    found: &'a mut Vec<PathBuf>,
}

impl Walk<'_> {
    fn hit(&mut self, dirfd: RawFd, shown: &Path, name: &OsStr) {
        let Some(c) = cstr(name) else { return };
        if self.remove {
            // SAFETY: unlinkat(2) of a name relative to the descriptor of its folder; never a folder.
            if unsafe { libc::unlinkat(dirfd, c.as_ptr(), 0) } != 0 {
                return;
            }
        }
        self.found.push(shown.join(name));
    }

    fn dir(&mut self, dir: &File, shown: &Path, depth: usize) {
        let fd = dir.as_raw_fd();
        let list = names(dir);
        let mut chromium = false;
        for name in &list {
            let Some(c) = cstr(name) else { continue };
            let Some(st) = stat_at(fd, &c) else { continue };
            let lower = name.to_string_lossy().to_lowercase();
            match st.st_mode & libc::S_IFMT {
                libc::S_IFLNK => {
                    let target = link_target(fd, &c).unwrap_or_default();
                    if (name.as_bytes() == b"SingletonLock" && is_chromium_lock_target(&target)) || (name.as_bytes() == b"lock" && is_firefox_lock_target(&target)) {
                        chromium |= name.as_bytes() == b"SingletonLock";
                        self.hit(fd, shown, name);
                    }
                }
                libc::S_IFREG if lower.contains("lock") && st.st_size <= MAX_LOCK_SIZE => {
                    if read_small(fd, &c).is_some_and(|b| is_qt_lock_content(&b)) {
                        self.hit(fd, shown, name);
                    }
                }
                libc::S_IFDIR if depth < MAX_DEPTH && !SKIP_DIRS.contains(&name.to_string_lossy().as_ref()) => {
                    if let Some(sub) = open_dir_at(fd, &c) {
                        self.dir(&sub, &shown.join(name), depth + 1);
                    }
                }
                _ => {}
            }
        }
        // The companions of a Chromium lock, in the same folder and only as links.
        if chromium {
            for n in ["SingletonSocket", "SingletonCookie"] {
                let is_link = CString::new(n).ok().and_then(|c| stat_at(fd, &c)).is_some_and(|s| s.st_mode & libc::S_IFMT == libc::S_IFLNK);
                if is_link {
                    self.hit(fd, shown, OsStr::new(n));
                }
            }
        }
    }
}

fn walk(roots: &[&Path], remove: bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots {
        let Some(c) = cstr(root.as_os_str()) else { continue };
        if let Some(dir) = open_dir_at(libc::AT_FDCWD, &c) {
            Walk { remove, found: &mut found }.dir(&dir, root, 0);
        }
    }
    found
}

/// The stale locks below `roots` (the private folders), left in place.
pub fn find(roots: &[&Path]) -> Vec<PathBuf> {
    walk(roots, false)
}

/// Remove the stale locks below `roots` and return them. The caller holds the proof that no copy of
/// the app is alive.
pub fn remove(roots: &[&Path]) -> Vec<PathBuf> {
    walk(roots, true)
}

/// The folders searched for an app: `config`, `data`, `state` and the cache folder. Not the `tmp` of
/// the app and nothing granted by the manifest.
pub fn roots_of(d: &crate::AppDirs) -> [PathBuf; 4] {
    [d.config.clone(), d.data.clone(), d.state.clone(), d.cache.clone()]
}

/// The lock file of the copies of an app in the runtime folder (see `sharedtmp` in the stub).
pub fn copies_lock_path(runtime_dir: &Path, app_id: &str) -> PathBuf {
    runtime_dir.join("panpak").join(app_id).join("lock")
}

/// Whether a copy of the app is alive: another process holds its lock.
fn copy_alive(lock: &Path) -> Result<bool> {
    let f = match File::open(lock) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("open {}", lock.display())),
    };
    // SAFETY: flock(2) on a descriptor this function owns; the lock goes with `f`.
    let got = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    Ok(!got)
}

/// The stale locks of an app, or none while a copy is alive (then they are not stale).
pub fn find_for_app(app_id: &str) -> Result<Vec<PathBuf>> {
    let dirs = crate::app_dirs(app_id)?;
    let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else { return Ok(Vec::new()) };
    if copy_alive(&copies_lock_path(&rt, app_id))? {
        return Ok(Vec::new());
    }
    let roots = roots_of(&dirs);
    Ok(find(&roots.iter().map(PathBuf::as_path).collect::<Vec<_>>()))
}

/// `panpak data unlock`: remove the stale locks of an app, and refuse while a copy is alive.
pub fn unlock_app(app_id: &str) -> Result<Vec<PathBuf>> {
    let dirs = crate::app_dirs(app_id)?;
    let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else {
        bail!("XDG_RUNTIME_DIR is not set: cannot tell whether a copy of {app_id} is running");
    };
    if copy_alive(&copies_lock_path(&rt, app_id))? {
        bail!("a copy of {app_id} is running: close it first");
    }
    let roots = roots_of(&dirs);
    Ok(remove(&roots.iter().map(PathBuf::as_path).collect::<Vec<_>>()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-locks-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_formats_are_told_by_their_content() {
        assert!(is_chromium_lock_target(b"myhost-4242"));
        assert!(is_chromium_lock_target(b"my-host-name-17"));
        assert!(!is_chromium_lock_target(b"-17") && !is_chromium_lock_target(b"host-") && !is_chromium_lock_target(b"host-12a") && !is_chromium_lock_target(b"/etc/passwd"));
        assert!(is_firefox_lock_target(b"127.0.0.1:+4242") && is_firefox_lock_target(b"::1:+9"));
        assert!(!is_firefox_lock_target(b"127.0.0.1:4242") && !is_firefox_lock_target(b":+5") && !is_firefox_lock_target(b"host:+x"));
        assert!(is_qt_lock_content(b"4242\nqbittorrent\nmyhost\n") && is_qt_lock_content(b"4242\napp\nhost\nbootid\n"));
        assert!(!is_qt_lock_content(b"4242\n") && !is_qt_lock_content(b"x\na\nb\n") && !is_qt_lock_content(b"{\"a\":1}\n\n\n") && !is_qt_lock_content(b"1\n2\n\n") && !is_qt_lock_content(b"1\n\0\nh\n"));
    }

    #[test]
    fn stale_locks_go_and_everything_else_stays() {
        let base = tmp("remove");
        let (cfg, data, outside) = (base.join("config"), base.join("data"), base.join("outside"));
        for d in [&cfg, &data, &outside, &cfg.join("App/deep/er/est")] {
            fs::create_dir_all(d).unwrap();
        }
        // Chromium, with its companions.
        let app = cfg.join("App");
        symlink("host-4242", app.join("SingletonLock")).unwrap();
        symlink("/tmp/org.chromium.x/SingletonSocket", app.join("SingletonSocket")).unwrap();
        symlink("123456", app.join("SingletonCookie")).unwrap();
        // Qt, with and without .lock in the name; Firefox.
        fs::write(data.join("lockfile"), "4242\nqbittorrent\nhost\n").unwrap();
        fs::write(data.join("note.lock"), "7\napp\nhost\n").unwrap();
        symlink("127.0.0.1:+4242", data.join("lock")).unwrap();
        // Not locks: a lock file of a package manager, a real pid file, a link that points elsewhere, a
        // lock too deep, a lock in a cache folder, a lock in a folder outside the roots.
        fs::write(data.join("yarn.lock"), "# yarn lockfile v1\n").unwrap();
        fs::write(data.join("app.pid"), "4242\napp\nhost\n").unwrap();
        symlink("/etc/hostname", data.join("SingletonLock")).unwrap();
        fs::write(cfg.join("App/deep/er/est/deep.lock"), "1\na\nh\n").unwrap();
        fs::create_dir_all(data.join("Cache")).unwrap();
        fs::write(data.join("Cache/x.lock"), "1\na\nh\n").unwrap();
        fs::write(outside.join("o.lock"), "1\na\nh\n").unwrap();
        symlink("host-1", outside.join("SingletonLock")).unwrap();
        let seen = find(&[&cfg, &data]);
        assert_eq!(seen.len(), 6, "{seen:?}");
        assert!(fs::symlink_metadata(app.join("SingletonLock")).is_ok(), "find removes nothing");
        let gone = remove(&[&cfg, &data]);
        assert_eq!(gone.len(), 6, "{gone:?}");
        for f in ["App/SingletonLock", "App/SingletonSocket", "App/SingletonCookie"] {
            assert!(fs::symlink_metadata(cfg.join(f)).is_err(), "{f}");
        }
        for f in ["lockfile", "note.lock", "lock"] {
            assert!(fs::symlink_metadata(data.join(f)).is_err(), "{f}");
        }
        for (d, f) in [(&data, "yarn.lock"), (&data, "app.pid"), (&data, "SingletonLock"), (&cfg, "App/deep/er/est/deep.lock"), (&data, "Cache/x.lock"), (&outside, "o.lock"), (&outside, "SingletonLock")] {
            assert!(fs::symlink_metadata(d.join(f)).is_ok(), "{f} was removed");
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn links_are_never_followed() {
        let base = tmp("links");
        let (root, target) = (base.join("root"), base.join("target"));
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("x.lock"), "1\na\nh\n").unwrap();
        symlink("host-9", target.join("SingletonLock")).unwrap();
        // A link to a folder inside the root, and the root itself as a link.
        symlink(&target, root.join("sub")).unwrap();
        let link_root = base.join("link-root");
        symlink(&target, &link_root).unwrap();
        // A lock file that is a link to a real lock file: the link is not read as a file.
        symlink(target.join("x.lock"), root.join("l.lock")).unwrap();
        assert!(remove(&[&root, &link_root]).is_empty());
        assert!(target.join("x.lock").exists() && fs::symlink_metadata(target.join("SingletonLock")).is_ok());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn unlock_refuses_while_a_copy_holds_the_lock() {
        let base = tmp("alive");
        let lock = copies_lock_path(&base, "org.example.app");
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        assert!(!copy_alive(&lock.with_file_name("none")).unwrap(), "no lock file: nobody is alive");
        let held = fs::OpenOptions::new().create(true).truncate(false).write(true).open(&lock).unwrap();
        // SAFETY: flock(2) on a descriptor owned here.
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_SH) }, 0);
        assert!(copy_alive(&lock).unwrap());
        drop(held);
        assert!(!copy_alive(&lock).unwrap());
        let _ = fs::remove_dir_all(&base);
    }
}
