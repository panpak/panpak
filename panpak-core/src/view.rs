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
