// SPDX-License-Identifier: MIT OR Apache-2.0
//! The isolated view: whether bubblewrap can build it on this machine, and which
//! sockets belong in it. The view is one of the layers of the sandbox; the stub builds
//! it (`panpak-stub`, module `view`) and `doctor` explains when it is missing.
//! Rules: PANPAK-FORMAT.md §9 and THREAT-MODEL.md §3.
use crate::Permissions;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// The only place `bwrap` is taken from. Never `PATH`: the stub runs it outside the
/// sandbox, so a program in a folder the user's `PATH` lists must not be picked.
pub const BWRAP: &str = "/usr/bin/bwrap";

/// How long the quick test may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Why the view cannot be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewProblem {
    /// `/usr/bin/bwrap` is not there or cannot be run.
    Missing,
    /// The kernel has no user namespaces for ordinary users.
    UserNamespacesOff,
    /// AppArmor stops ordinary programs from creating user namespaces.
    AppArmor,
    /// bubblewrap ran and failed for another reason (its first error line).
    Failed(String),
}

impl ViewProblem {
    /// One line: what is wrong.
    pub fn reason(&self) -> String {
        match self {
            ViewProblem::Missing => format!("{BWRAP} is missing or cannot be run"),
            ViewProblem::UserNamespacesOff => "the kernel does not allow user namespaces for ordinary users".to_string(),
            ViewProblem::AppArmor => "AppArmor does not let bubblewrap create a user namespace (kernel.apparmor_restrict_unprivileged_userns=1)".to_string(),
            ViewProblem::Failed(e) => format!("bubblewrap failed its quick test: {e}"),
        }
    }

    /// One line: how to fix it.
    pub fn remedy(&self) -> &'static str {
        match self {
            ViewProblem::Missing => "install the bubblewrap package (for example `apt install bubblewrap`, `dnf install bubblewrap` or `pacman -S bubblewrap`)",
            ViewProblem::UserNamespacesOff => "enable user namespaces: `sysctl -w user.max_user_namespaces=15000`, and on Debian kernels `sysctl -w kernel.unprivileged_userns_clone=1`",
            ViewProblem::AppArmor => "install the AppArmor profile for /usr/bin/bwrap that your distribution ships (on Ubuntu, the apparmor-profiles package or a profile that allows `userns` for bwrap); do not turn the restriction off for the whole system",
            ViewProblem::Failed(_) => "run `/usr/bin/bwrap --unshare-user --ro-bind / / /usr/bin/true` and read its error",
        }
    }
}

/// Result of the quick test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewStatus {
    Usable,
    Unusable(ViewProblem),
}

impl ViewStatus {
    pub fn usable(&self) -> bool {
        matches!(self, ViewStatus::Usable)
    }
}

/// Classify a failed quick test. The arguments are what the host says about user
/// namespaces; they are parameters so that the test needs no particular host.
fn classify(stderr: &str, max_userns: Option<u64>, unpriv_clone: Option<bool>, apparmor_restrict: bool) -> ViewProblem {
    let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("no error message").trim().to_string();
    let namespace_error = ["uid map", "gid map", "namespace", "Operation not permitted", "Permission denied"]
        .iter()
        .any(|m| stderr.contains(m));
    if max_userns == Some(0) || unpriv_clone == Some(false) {
        ViewProblem::UserNamespacesOff
    } else if namespace_error && apparmor_restrict {
        ViewProblem::AppArmor
    } else {
        ViewProblem::Failed(first)
    }
}

fn read_number(path: &str) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Run `bwrap` once with the options the real start needs and see whether it works.
fn probe(bwrap: &Path) -> ViewStatus {
    let Some(truebin) = ["/usr/bin/true", "/bin/true"].into_iter().find(|p| Path::new(p).is_file()) else {
        return ViewStatus::Unusable(ViewProblem::Failed("no /usr/bin/true to run".to_string()));
    };
    let child = Command::new(bwrap)
        .args(["--unshare-user", "--unshare-pid", "--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"])
        .args(["--tmpfs", "/tmp", "--tmpfs", "/run", "--", truebin])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else { return ViewStatus::Unusable(ViewProblem::Missing) };
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() < PROBE_TIMEOUT => std::thread::sleep(Duration::from_millis(2)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return ViewStatus::Unusable(ViewProblem::Failed("the quick test did not finish".to_string()));
            }
        }
    };
    if status.success() {
        return ViewStatus::Usable;
    }
    let mut text = String::new();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read as _;
        let mut buf = vec![0u8; 4096];
        if let Ok(n) = e.read(&mut buf) {
            text = String::from_utf8_lossy(&buf[..n]).into_owned();
        }
    }
    let restrict = read_number("/proc/sys/kernel/apparmor_restrict_unprivileged_userns") == Some(1);
    let clone = read_number("/proc/sys/kernel/unprivileged_userns_clone").map(|v| v != 0);
    ViewStatus::Unusable(classify(&text, read_number("/proc/sys/user/max_user_namespaces"), clone, restrict))
}

/// What identifies the test result: the program (inode, change time, size) and the kernel.
fn cache_line(bwrap: &Path) -> Option<String> {
    let m = fs::metadata(bwrap).ok()?;
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
    Some(format!("1 {} {} {} {}", m.ino(), m.mtime(), m.len(), kernel.trim()))
}

/// The quick test for `bwrap`, with its result kept in `cache_dir` (a private folder such as
/// `$XDG_RUNTIME_DIR/panpak`). Only a success is kept: a failure is tested again at every start,
/// so a fix (installing a package, loading a profile) takes effect at once. A wrong cache entry
/// cannot weaken anything: bubblewrap would fail to start the app, which is an error, not a
/// fallback.
pub fn view_status_at(bwrap: &Path, cache_dir: Option<&Path>) -> ViewStatus {
    let meta = match fs::metadata(bwrap) {
        Ok(m) if m.is_file() && m.mode() & 0o111 != 0 => m,
        _ => return ViewStatus::Unusable(ViewProblem::Missing),
    };
    // bubblewrap must not be writable by others: it runs outside the sandbox.
    if meta.mode() & 0o022 != 0 {
        return ViewStatus::Unusable(ViewProblem::Failed(format!("{} is writable by others", bwrap.display())));
    }
    let line = cache_line(bwrap);
    let file = cache_dir.map(|d| d.join("view-probe"));
    if let (Some(f), Some(l)) = (&file, &line) {
        if fs::read_to_string(f).is_ok_and(|c| c.trim() == l) {
            return ViewStatus::Usable;
        }
    }
    let status = probe(bwrap);
    if let (ViewStatus::Usable, Some(f), Some(l), Some(d)) = (&status, &file, &line, cache_dir) {
        // The cache only saves the next probe: if it cannot be written, the probe runs again.
        let _ = fs::DirBuilder::new().recursive(true).mode(0o700).create(d);
        let tmp = d.join(format!(".view-probe.{}", std::process::id()));
        if fs::write(&tmp, format!("{l}\n")).is_ok() && fs::rename(&tmp, f).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }
    status
}

/// Where the cache lives: `$XDG_RUNTIME_DIR/panpak` (a private folder of the user).
fn runtime_cache_dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?);
    d.is_absolute().then(|| d.join("panpak"))
}

/// Whether the view can be built here: the quick test of `/usr/bin/bwrap`.
pub fn view_status() -> ViewStatus {
    view_status_at(Path::new(BWRAP), runtime_cache_dir().as_deref())
}

/// `bwrap --version` (for `doctor`), if it runs.
pub fn bwrap_version() -> Option<String> {
    let out = Command::new(BWRAP).arg("--version").stdin(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether the manifest asks for any network: `network = true`, or a TCP port to connect to or to bind.
/// Without it the isolated view has no network but the loopback interface.
pub fn network_requested(perms: Option<&Permissions>) -> bool {
    perms.is_some_and(|p| {
        p.network == Some(true) || p.network_allow_tcp.as_ref().is_some_and(|v| !v.is_empty()) || p.network_bind_tcp.as_ref().is_some_and(|v| !v.is_empty())
    })
}

/// Path sockets every app may reach without listing them: the Wayland compositor, the
/// audio servers when `audio = true`, the journal (logs) and, when the manifest asks for network, the
/// resolver of systemd.
/// Only the ones that exist matter; the caller filters. The same list feeds the view
/// (what is mounted into it) and Landlock V9 (`ResolveUnix` rules).
pub fn implied_sockets(perms: Option<&Permissions>, runtime_dir: Option<&Path>, wayland_display: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(rt) = runtime_dir {
        if let Some(w) = wayland_display.filter(|w| !w.is_empty()) {
            let p = Path::new(w);
            out.push(if p.is_absolute() { p.to_path_buf() } else { rt.join(p) });
        }
        if perms.is_some_and(|p| p.audio == Some(true)) {
            out.push(rt.join("pipewire-0"));
            out.push(rt.join("pulse/native"));
        }
    }
    out.push(PathBuf::from("/run/systemd/journal/socket"));
    out.push(PathBuf::from("/run/systemd/journal/dev-log"));
    // The resolver does the lookups for the app: an app with no network must not have it, or it could
    // send names to a server of its choice through the resolver.
    if network_requested(perms) {
        out.push(PathBuf::from("/run/systemd/resolve/io.systemd.Resolve"));
    }
    out
}

/// The folder of the sockets that `gvfsd` makes for each mount, in the runtime folder of the user. An app with the `gvfs` permission may use what is in it.
pub const GVFSD_DIR: &str = "gvfsd";

/// The folder of the sockets of `gvfsd`, ready for an app with the permission `gvfs`: made (mode 0700, ours) when it is not there, left alone when it is a folder of ours. The daemon starts on
/// request and makes the folder itself, but an app that is the first to use GVfs in the session would find none to open, and the mounts with sockets of their own (`sftp://`, `smb://`) would fail,
/// so the folder is made before the app starts, as Flatpak does for `xdg-run/gvfsd`. `gvfsd` accepts a folder that is there (it calls `g_mkdir (socket_dir, 0700)` and does not look at the result).
/// `None` when there is no runtime folder, or the place is not a folder of ours (a link, a file, a folder of another user): nothing is opened then.
pub fn prepare_gvfsd_dir(runtime_dir: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let dir = runtime_dir.join(GVFSD_DIR);
    // SAFETY: geteuid cannot fail.
    let me = unsafe { libc::geteuid() };
    let mine = |m: &fs::Metadata| m.file_type().is_dir() && m.uid() == me;
    match fs::symlink_metadata(&dir) {
        Ok(m) => mine(&m).then_some(dir),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new().mode(0o700).create(&dir).ok()?;
            fs::symlink_metadata(&dir).ok().filter(mine).map(|_| dir)
        }
        Err(_) => None,
    }
}

/// The FUSE tree of GVfs (`gvfsd-fuse`): the mounts of the session as paths of files (`$XDG_RUNTIME_DIR/gvfs/sftp:host=...`). It is made by `gvfsd-fuse`, never by us.
pub const GVFS_FUSE_DIR: &str = "gvfs";

/// Undo the octal escapes of `/proc/self/mountinfo` (`\040` is a space, `\134` a backslash).
fn unescape_mountinfo(field: &str) -> String {
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 4 <= b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = (b[i + 1] - b'0') as u32 * 64 + (b[i + 2] - b'0') as u32 * 8 + (b[i + 3] - b'0') as u32;
            out.push(v as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Is `dir` the mount point of a FUSE file system that the user `uid` mounted, according to the text of `/proc/self/mountinfo`? The last line for the path is the
/// one that counts (a later mount covers an earlier one). It never touches the folder: a stat of the root of a stuck FUSE mount waits for its daemon, and the wait cannot be
/// ended (measured, docs/COMPATIBILITY.md, "Network locations with the permission gvfs"). A link is not a mount point, so it never matches.
pub fn is_fuse_mount_of(mountinfo: &str, dir: &Path, uid: u32) -> bool {
    let mut found = false;
    for line in mountinfo.lines() {
        let Some((head, tail)) = line.split_once(" - ") else { continue };
        let mut h = head.split(' ');
        let Some(point) = h.nth(4) else { continue };
        if Path::new(&unescape_mountinfo(point)) != dir {
            continue;
        }
        let mut t = tail.split(' ');
        let fstype = t.next().unwrap_or("");
        let super_opts = t.nth(1).unwrap_or("");
        found = fstype.starts_with("fuse") && super_opts.split(',').any(|o| o.strip_prefix("user_id=").and_then(|v| v.parse::<u32>().ok()) == Some(uid));
    }
    found
}

/// What the FUSE tree of GVfs is, for an app with the permission `gvfs`.
#[derive(Debug, PartialEq, Eq)]
pub enum GvfsFuse {
    /// A mount of FUSE of the user, and it answers: ready to be put in the view (the canonical path).
    Ready(PathBuf),
    /// Not a mount point of FUSE of the user (absent, a plain folder, a link, a file, a mount of another user): nothing is opened, and nothing is made.
    NotMounted,
    /// A mount that does not answer: its daemon is stuck, or is dead and the mount was left behind (the error is "Transport endpoint is not connected"). Bubblewrap would wait for it, or refuse
    /// the bind and the app would not start at all; so the tree is left out and the app starts without it.
    NotAnswering,
}

/// Does `stat` of `dir` answer within `wait`? The `stat` is made by a child process (`fork`, then only `stat` and `_exit`): the stat of the root of a stuck FUSE mount waits for its
/// daemon and cannot be ended, so nothing of ours may wait for it. A child that does not answer is left behind (it ends when the daemon answers or goes away).
fn answers(dir: &Path, wait: std::time::Duration) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else { return false };
    // The child that is left behind must hold nothing of ours: not the pipes of whoever waits for our output (it would never see the end of the file), not a lock of the app
    // (a lock held by a stuck child would never be let go). So its descriptors are the null device and nothing else.
    // SAFETY: opens a file, the descriptor is closed below in the parent.
    let null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    // SAFETY: after `fork` the child calls only `dup2`, `close_range`, `stat` and `_exit`, which are safe in a child of a process with threads; the C string was made before.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        // SAFETY: `null` is ours (or -1, which `close` refuses).
        unsafe { libc::close(null) };
        return false;
    }
    if pid == 0 {
        // SAFETY: as above; `stat` writes in a buffer that is on the stack of the child.
        unsafe {
            if null >= 0 {
                libc::dup2(null, 0);
                libc::dup2(null, 1);
                libc::dup2(null, 2);
            }
            // If the descriptors cannot be closed the child ends at once: the parent then reads "does not answer" (fail closed), and nothing of ours is held by a child that stays behind.
            if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) != 0 {
                libc::_exit(2);
            }
            let mut st: libc::stat = std::mem::zeroed();
            libc::_exit(if libc::stat(c.as_ptr(), &mut st) == 0 { 0 } else { 1 });
        }
    }
    // SAFETY: `null` is ours (or -1).
    unsafe { libc::close(null) };
    let end = std::time::Instant::now() + wait;
    loop {
        let mut status = 0;
        // SAFETY: `pid` is our child; `status` is a valid place.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r == pid {
            return libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
        }
        if r < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if r < 0 || std::time::Instant::now() >= end {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// The FUSE tree of GVfs for an app with the permission `gvfs`: opened only when it is there, is a mount point of FUSE, is the user's (see `is_fuse_mount_of`) and answers.
/// It is never made (`gvfsd-fuse` makes it); a link, a file, a folder of another user or a folder where nothing is mounted is `NotMounted`; a mount that does not answer is
/// `NotAnswering`. An app that starts before the mount does not get it until its next start. The path is the canonical one of the runtime folder, so that nothing has to resolve it again.
pub fn gvfs_fuse_state(runtime_dir: &Path) -> GvfsFuse {
    let Some(dir) = fs::canonicalize(runtime_dir).ok().map(|d| d.join(GVFS_FUSE_DIR)) else { return GvfsFuse::NotMounted };
    // SAFETY: geteuid cannot fail.
    let me = unsafe { libc::geteuid() };
    let Ok(info) = fs::read_to_string("/proc/self/mountinfo") else { return GvfsFuse::NotMounted };
    if !is_fuse_mount_of(&info, &dir, me) {
        return GvfsFuse::NotMounted;
    }
    if answers(&dir, std::time::Duration::from_millis(500)) {
        GvfsFuse::Ready(dir)
    } else {
        GvfsFuse::NotAnswering
    }
}

/// Is GVfs on this system? The daemon is started by the session bus when something asks for `org.gtk.vfs.Daemon`, so the service file or the program is what tells.
pub fn gvfs_installed() -> bool {
    gvfs_installed_at(&["/usr/share/dbus-1/services/org.gtk.vfs.Daemon.service", "/usr/libexec/gvfsd", "/usr/lib/gvfs/gvfsd", "/usr/lib/gvfsd", "/usr/lib/x86_64-linux-gnu/gvfs/gvfsd"])
}

pub fn gvfs_installed_at(paths: &[&str]) -> bool {
    paths.iter().any(|p| Path::new(p).exists())
}

/// The folder in `/run` that holds the target of `/etc/resolv.conf`, if the file is a link
/// into `/run` (systemd-resolved, NetworkManager). The view mounts it read-only so that
/// name resolution works.
pub fn resolv_conf_run_dir() -> Option<PathBuf> {
    let target = fs::canonicalize("/etc/resolv.conf").ok()?;
    let dir = target.parent()?;
    (dir.starts_with("/run") && dir != Path::new("/run")).then(|| dir.to_path_buf())
}

/// Host folders the sandbox opens read-only, one Landlock rule each.
pub const SYSTEM_RULE_PATHS: [&str; 7] = ["/usr", "/lib", "/lib64", "/etc", "/proc", "/sys", "/run"];

/// Open `path` with `O_PATH` for a Landlock rule. `landlock::PathFd` cannot be used on the musl build:
/// the standard library drops `O_PATH` from the custom flags there (musl's `O_ACCMODE` contains it), so the
/// file is opened for reading, which fails on a socket (ENXIO) and may block on a fifo.
pub fn open_o_path(path: &Path) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
    // SAFETY: valid C string; the descriptor, if any, is owned by the OwnedFd.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd is a new descriptor that nothing else owns.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// The paths among `paths` that exist but cannot be opened for a rule, with the reason. A path that is not
/// there is not listed: the sandbox has nothing to allow, and access stays denied.
pub fn unopenable_rule_paths<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Vec<(PathBuf, std::io::Error)> {
    paths
        .into_iter()
        .filter_map(|p| match open_o_path(p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Some((p.to_path_buf(), e)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-view-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake_bwrap(dir: &Path, script: &str) -> PathBuf {
        let p = dir.join("bwrap");
        fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn missing_or_not_executable_bwrap_is_unusable() {
        let d = temp("missing");
        assert_eq!(view_status_at(&d.join("nothing"), None), ViewStatus::Unusable(ViewProblem::Missing));
        let plain = d.join("plain");
        fs::write(&plain, "x").unwrap();
        fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(view_status_at(&plain, None), ViewStatus::Unusable(ViewProblem::Missing));
        assert_eq!(view_status_at(&d, None), ViewStatus::Unusable(ViewProblem::Missing));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn bwrap_writable_by_others_is_refused() {
        let d = temp("writable");
        let b = fake_bwrap(&d, "exit 0");
        fs::set_permissions(&b, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(view_status_at(&b, None), ViewStatus::Unusable(ViewProblem::Failed(_))));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn success_is_cached_and_failure_is_not() {
        let d = temp("cache");
        let cache = d.join("cache");
        let ok = fake_bwrap(&d, "exit 0");
        assert_eq!(view_status_at(&ok, Some(&cache)), ViewStatus::Usable);
        let entry = fs::read_to_string(cache.join("view-probe")).unwrap();
        assert!(entry.starts_with("1 "), "{entry}");
        assert_eq!(fs::metadata(&cache).unwrap().permissions().mode() & 0o777, 0o700);
        // The entry answers without running the program: swap in one that fails and keep the
        // identity the entry is bound to by writing the same line for it.
        let bad_dir = d.join("bad");
        fs::create_dir_all(&bad_dir).unwrap();
        let bad = fake_bwrap(&bad_dir, "echo 'bwrap: broken' >&2; exit 1");
        let bad_cache = d.join("bad-cache");
        assert!(matches!(view_status_at(&bad, Some(&bad_cache)), ViewStatus::Unusable(ViewProblem::Failed(_))));
        assert!(!bad_cache.join("view-probe").exists(), "a failure must not be cached");
        // A different program (other inode) does not reuse the entry of the first one.
        fs::create_dir_all(&bad_cache).unwrap();
        fs::copy(cache.join("view-probe"), bad_cache.join("view-probe")).unwrap();
        assert!(matches!(view_status_at(&bad, Some(&bad_cache)), ViewStatus::Unusable(_)));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn failures_are_classified_from_the_host_state() {
        let uid_map = "bwrap: setting up uid map: Permission denied";
        assert_eq!(classify(uid_map, Some(0), None, false), ViewProblem::UserNamespacesOff);
        assert_eq!(classify(uid_map, Some(1000), Some(false), false), ViewProblem::UserNamespacesOff);
        assert_eq!(classify(uid_map, Some(1000), Some(true), true), ViewProblem::AppArmor);
        assert_eq!(classify(uid_map, Some(1000), None, false), ViewProblem::Failed(uid_map.to_string()));
        assert_eq!(classify("\n  \n", Some(1000), None, true), ViewProblem::Failed("no error message".to_string()));
        for p in [ViewProblem::Missing, ViewProblem::UserNamespacesOff, ViewProblem::AppArmor, ViewProblem::Failed("x".into())] {
            assert!(!p.reason().is_empty() && !p.remedy().is_empty());
        }
    }

    #[test]
    fn network_is_requested_by_the_flag_or_by_a_port() {
        assert!(!network_requested(None));
        let none: Permissions = toml::from_str("x11 = true").unwrap();
        assert!(!network_requested(Some(&none)));
        let off: Permissions = toml::from_str("network = false\nnetwork_allow_tcp = []").unwrap();
        assert!(!network_requested(Some(&off)));
        let on: Permissions = toml::from_str("network = true").unwrap();
        assert!(network_requested(Some(&on)));
        let conn: Permissions = toml::from_str("network_allow_tcp = [443]").unwrap();
        assert!(network_requested(Some(&conn)));
        let bind: Permissions = toml::from_str("network_bind_tcp = [8080]").unwrap();
        assert!(network_requested(Some(&bind)));
    }

    #[test]
    fn the_fuse_tree_of_gvfs_is_a_mount_of_the_user_and_nothing_else() {
        let info = "36 35 0:30 / /run/user/1000 rw,nosuid shared:1 - tmpfs tmpfs rw,size=1k\n\
                    8172 39 0:167 / /run/user/1000/gvfs rw,nosuid,nodev,relatime shared:2489 - fuse.gvfsd-fuse gvfsd-fuse rw,user_id=1000,group_id=1000\n";
        let dir = Path::new("/run/user/1000/gvfs");
        assert!(is_fuse_mount_of(info, dir, 1000));
        // Another user's mount, a mount of another kind, another path, a parent folder: no.
        assert!(!is_fuse_mount_of(info, dir, 1001));
        assert!(!is_fuse_mount_of(&info.replace("fuse.gvfsd-fuse", "tmpfs"), dir, 1000));
        assert!(!is_fuse_mount_of(info, Path::new("/run/user/1000/gvfsd"), 1000));
        assert!(!is_fuse_mount_of(info, Path::new("/run/user/1000"), 1000));
        assert!(!is_fuse_mount_of("", dir, 1000));
        assert!(!is_fuse_mount_of("garbage\nmore garbage - x\n", dir, 1000));
        // The last line for the path counts: a later mount that is not FUSE covers it.
        let covered = format!("{info}9000 39 0:200 / /run/user/1000/gvfs rw - tmpfs tmpfs rw\n");
        assert!(!is_fuse_mount_of(&covered, dir, 1000));
        // Escapes in the path: a space is \040.
        let spaced = "1 2 0:3 / /run/user/1000/g\\040vfs rw - fuse.x x rw,user_id=1000\n";
        assert!(is_fuse_mount_of(spaced, Path::new("/run/user/1000/g vfs"), 1000));
        // `user_id=10000` is not `user_id=1000`.
        assert!(!is_fuse_mount_of(&info.replace("user_id=1000,", "user_id=10000,"), dir, 1000));
    }

    #[test]
    fn a_folder_answers_and_a_missing_path_does_not() {
        assert!(answers(&std::env::temp_dir(), std::time::Duration::from_millis(500)));
        assert!(!answers(Path::new("/nonexistent/panpak-test"), std::time::Duration::from_millis(500)));
    }

    #[test]
    fn the_fuse_tree_is_never_made_and_a_plain_folder_or_a_link_is_not_it() {
        let rt = std::env::temp_dir().join(format!("panpak-gvfsfuse-{}", std::process::id()));
        let _ = fs::create_dir_all(&rt);
        let gv = rt.join("gvfs");
        // Absent: nothing is made.
        assert_eq!(gvfs_fuse_state(&rt), GvfsFuse::NotMounted);
        assert!(!gv.exists(), "the tree must not be made by us");
        // A plain folder is not a mount point.
        fs::create_dir(&gv).unwrap();
        assert_eq!(gvfs_fuse_state(&rt), GvfsFuse::NotMounted);
        // A link, and a file, are not either.
        let rt2 = std::env::temp_dir().join(format!("panpak-gvfsfuse2-{}", std::process::id()));
        let _ = fs::create_dir_all(&rt2);
        std::os::unix::fs::symlink("/tmp", rt2.join("gvfs")).unwrap();
        assert_eq!(gvfs_fuse_state(&rt2), GvfsFuse::NotMounted);
        let rt3 = std::env::temp_dir().join(format!("panpak-gvfsfuse3-{}", std::process::id()));
        let _ = fs::create_dir_all(&rt3);
        fs::write(rt3.join("gvfs"), b"x").unwrap();
        assert_eq!(gvfs_fuse_state(&rt3), GvfsFuse::NotMounted);
        assert_eq!(gvfs_fuse_state(&rt.join("missing/deeper")), GvfsFuse::NotMounted);
        for d in [&rt, &rt2, &rt3] {
            let _ = fs::remove_dir_all(d);
        }
    }

    #[test]
    fn the_folder_of_the_sockets_of_gvfsd_is_made_when_it_is_not_there() {
        use std::os::unix::fs::PermissionsExt;
        let rt = std::env::temp_dir().join(format!("panpak-gvfsd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&rt);
        fs::create_dir_all(&rt).unwrap();
        // Not there at the start (the app is the first to use GVfs in the session): made, 0700, ours.
        let made = prepare_gvfsd_dir(&rt).expect("made");
        assert_eq!(made, rt.join("gvfsd"));
        assert_eq!(fs::metadata(&made).unwrap().permissions().mode() & 0o777, 0o700);
        // There already (the daemon made it, or an earlier start): left as it is, with what is in it.
        fs::write(made.join("socket-abc"), b"").unwrap();
        assert_eq!(prepare_gvfsd_dir(&rt), Some(made.clone()));
        assert!(made.join("socket-abc").exists());
        // A link in its place, or a file: nothing is opened.
        let rt2 = rt.join("other");
        fs::create_dir_all(&rt2).unwrap();
        std::os::unix::fs::symlink("/tmp", rt2.join("gvfsd")).unwrap();
        assert_eq!(prepare_gvfsd_dir(&rt2), None);
        let rt3 = rt.join("third");
        fs::create_dir_all(&rt3).unwrap();
        fs::write(rt3.join("gvfsd"), b"x").unwrap();
        assert_eq!(prepare_gvfsd_dir(&rt3), None);
        // No runtime folder: nothing to make.
        assert_eq!(prepare_gvfsd_dir(&rt.join("missing/deeper")), None);
        let _ = fs::remove_dir_all(&rt);
    }

    #[test]
    fn gvfs_is_there_when_one_of_its_files_is() {
        assert!(!gvfs_installed_at(&["/nonexistent/panpak-gvfs-a", "/nonexistent/panpak-gvfs-b"]));
        assert!(gvfs_installed_at(&["/nonexistent/panpak-gvfs-a", "/"]));
        assert!(!gvfs_installed_at(&[]));
    }

    #[test]
    fn implied_sockets_follow_the_permissions() {
        let rt = Path::new("/run/user/1000");
        let none = implied_sockets(None, Some(rt), Some("wayland-0"));
        assert_eq!(none[0], PathBuf::from("/run/user/1000/wayland-0"));
        assert!(!none.iter().any(|p| p.ends_with("pipewire-0") || p.ends_with("native")));
        assert!(none.contains(&PathBuf::from("/run/systemd/journal/socket")));
        let audio: Permissions = toml::from_str("audio = true").unwrap();
        let with = implied_sockets(Some(&audio), Some(rt), None);
        assert!(with.contains(&PathBuf::from("/run/user/1000/pipewire-0")));
        assert!(with.contains(&PathBuf::from("/run/user/1000/pulse/native")));
        assert!(!with.iter().any(|p| p.ends_with("wayland-0")));
        let abs = implied_sockets(None, Some(rt), Some("/tmp/wl"));
        assert_eq!(abs[0], PathBuf::from("/tmp/wl"));
        // Without a runtime folder only the system sockets remain.
        assert!(implied_sockets(Some(&audio), None, Some("wayland-0")).iter().all(|p| p.starts_with("/run/systemd")));
        // The resolver only for an app that asks for network.
        let resolver = PathBuf::from("/run/systemd/resolve/io.systemd.Resolve");
        assert!(!none.contains(&resolver));
        let net: Permissions = toml::from_str("network = true").unwrap();
        assert!(implied_sockets(Some(&net), Some(rt), None).contains(&resolver));
    }

    #[test]
    fn rule_paths_open_sockets_and_report_what_cannot_be_opened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("panpak-rulepath-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("closed")).unwrap();
        let sock = dir.join("sock");
        let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        // A socket opens (`O_PATH`), also where the std library would try to read it.
        open_o_path(&sock).unwrap();
        let hidden = dir.join("closed/inner");
        fs::write(&hidden, "x").unwrap();
        fs::set_permissions(dir.join("closed"), fs::Permissions::from_mode(0o000)).unwrap();
        let missing = dir.join("missing");
        let bad = unopenable_rule_paths([sock.as_path(), missing.as_path(), hidden.as_path()]);
        fs::set_permissions(dir.join("closed"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        // A missing path is not a problem; a path that exists but cannot be reached is.
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert_eq!(bad[0].0, hidden);
    }
}
