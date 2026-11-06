// SPDX-License-Identifier: MIT OR Apache-2.0
//! Finding the system `xdg-dbus-proxy`, as bubblewrap is found: at fixed paths, never through
//! `PATH` or the home folder.

use anyhow::{bail, ensure, Context, Result};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Where the proxy is looked for, in order.
pub const PROXY_PATHS: [&str; 2] = ["/usr/bin/xdg-dbus-proxy", "/run/current-system/sw/bin/xdg-dbus-proxy"];

/// The oldest version whose rule syntax this module relies on (`--call`, `--broadcast`, `.*` names).
pub const MIN_PROXY: (u32, u32, u32) = (0, 1, 5);

/// What `doctor` and `install` say about the layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyStatus {
    Usable { path: PathBuf, version: (u32, u32, u32) },
    NotFound,
    TooOld { path: PathBuf, version: (u32, u32, u32) },
    Broken { path: PathBuf, why: String },
}

/// The first executable regular file among `PROXY_PATHS`.
pub fn find_proxy() -> Option<PathBuf> {
    PROXY_PATHS.iter().map(PathBuf::from).find(|p| p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// `xdg-dbus-proxy 0.1.7` -> (0, 1, 7).
pub fn parse_version(out: &str) -> Result<(u32, u32, u32)> {
    let v = out.trim().strip_prefix("xdg-dbus-proxy").context("not the output of xdg-dbus-proxy --version")?.trim();
    ensure!(v.len() <= 32, "version too long");
    let mut it = v.split('.').map(|p| p.parse::<u32>());
    match (it.next(), it.next(), it.next(), it.next()) {
        (Some(Ok(a)), Some(Ok(b)), Some(Ok(c)), None) => Ok((a, b, c)),
        _ => bail!("version {v:?} is not x.y.z"),
    }
}

/// Run `<path> --version` with a clean environment and a deadline.
pub fn proxy_version(path: &Path) -> Result<(u32, u32, u32)> {
    let mut child = Command::new(path).arg("--version").env_clear().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().with_context(|| format!("cannot run {}", path.display()))?;
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(st) = child.try_wait()? {
            ensure!(st.success(), "{} --version failed", path.display());
            break;
        }
        if Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{} --version did not finish in 3 s", path.display());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut out = String::new();
    child.stdout.take().context("no output")?.take(256).read_to_string(&mut out)?;
    parse_version(&out)
}

pub fn proxy_status() -> ProxyStatus {
    let Some(path) = find_proxy() else { return ProxyStatus::NotFound };
    match proxy_version(&path) {
        Ok(v) if v >= MIN_PROXY => ProxyStatus::Usable { path, version: v },
        Ok(v) => ProxyStatus::TooOld { path, version: v },
        Err(e) => ProxyStatus::Broken { path, why: crate::err_text(&e) },
    }
}

/// What identifies a version of the proxy binary: where it is, how big, when it changed, which inode.
fn proxy_identity(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let m = path.metadata().ok()?;
    Some(format!("{} {} {}.{} {}", path.display(), m.len(), m.mtime(), m.mtime_nsec(), m.ino()))
}

/// `proxy_status`, remembered for the next start: running `--version` costs 5 to 10 ms, and the answer only
/// changes when the binary does. The answer is kept in a file of the private runtime folder
/// (`<runtime>/panpak/proxy-version`), with the identity of the binary; a file that does not match is ignored and
/// rewritten. Only a working, new enough proxy is remembered; every other answer is asked again.
pub fn proxy_status_cached(runtime: &Path) -> ProxyStatus {
    let Some(path) = find_proxy() else { return ProxyStatus::NotFound };
    let cache = runtime.join("panpak").join("proxy-version");
    let identity = proxy_identity(&path);
    if let (Some(id), Ok(text)) = (&identity, std::fs::read_to_string(&cache)) {
        if let Some((cached_id, version)) = text.trim_end().rsplit_once(' ') {
            let parsed: Vec<_> = version.split('.').map(|p| p.parse::<u32>()).collect();
            if cached_id == id {
                if let [Ok(a), Ok(b), Ok(c)] = parsed[..] {
                    if (a, b, c) >= MIN_PROXY {
                        return ProxyStatus::Usable { path, version: (a, b, c) };
                    }
                }
            }
        }
    }
    let status = proxy_status();
    if let (ProxyStatus::Usable { version: (a, b, c), .. }, Some(id)) = (&status, &identity) {
        // Best effort: a failure to write only means the next start asks again.
        {
            use std::os::unix::fs::DirBuilderExt;
            let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(runtime.join("panpak"));
        }
        let tmp = cache.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, format!("{id} {a}.{b}.{c}\n")).is_ok() && std::fs::rename(&tmp, &cache).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    status
}

/// The user's session bus socket: `DBUS_SESSION_BUS_ADDRESS` if it is a `unix:path=`, else
/// `$XDG_RUNTIME_DIR/bus`. Only a socket that exists.
pub fn session_bus_path() -> Option<PathBuf> {
    use std::os::unix::fs::FileTypeExt;
    let p = match std::env::var("DBUS_SESSION_BUS_ADDRESS") {
        Ok(a) if !a.is_empty() => super::address_path(&a).ok()?,
        _ => PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("bus"),
    };
    p.metadata().ok().filter(|m| m.file_type().is_socket()).map(|_| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_parsed_and_compared() {
        assert_eq!(parse_version("xdg-dbus-proxy 0.1.7\n").unwrap(), (0, 1, 7));
        assert_eq!(parse_version("xdg-dbus-proxy 0.1.5").unwrap(), MIN_PROXY);
        assert!((0, 1, 4) < MIN_PROXY && (0, 1, 9) > MIN_PROXY && (0, 2, 0) > MIN_PROXY);
        for bad in ["", "bwrap 0.1.7", "xdg-dbus-proxy", "xdg-dbus-proxy 0.1", "xdg-dbus-proxy 0.1.7.1", "xdg-dbus-proxy a.b.c", "xdg-dbus-proxy 0.1.-1"] {
            assert!(parse_version(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn identity_changes_with_the_file() {
        let dir = std::env::temp_dir().join(format!("panpak-proxyid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("p");
        std::fs::write(&f, "a").unwrap();
        let one = proxy_identity(&f).unwrap();
        assert_eq!(proxy_identity(&f).unwrap(), one);
        std::fs::write(&f, "ab").unwrap();
        assert_ne!(proxy_identity(&f).unwrap(), one, "a different size is a different identity");
        assert!(proxy_identity(&dir.join("absent")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_program_that_hangs_or_fails_is_reported() {
        let dir = std::env::temp_dir().join(format!("panpak-proxy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mk = |name: &str, body: &str| {
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        assert_eq!(proxy_version(&mk("ok", "echo xdg-dbus-proxy 0.1.9")).unwrap(), (0, 1, 9));
        assert!(proxy_version(&mk("fail", "exit 3")).is_err());
        assert!(proxy_version(&mk("junk", "echo hello")).is_err());
        let t = Instant::now();
        assert!(proxy_version(&mk("hang", "sleep 30")).is_err());
        assert!(t.elapsed() < Duration::from_secs(6));
        let _ = std::fs::remove_dir_all(dir);
    }
}
