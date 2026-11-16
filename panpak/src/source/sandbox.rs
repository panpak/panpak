// SPDX-License-Identifier: MIT OR Apache-2.0
//! Running a command in a build image (a Debian root file system in a folder) as root of a user namespace of bubblewrap. One user id only:
//! the id of the user maps to 0 inside. `chown` to another id would fail, so a preload library (assets/nochown.c) makes it succeed,
//! which is enough for `apt` and `dpkg`. Nothing here needs root, a setuid helper or a container engine.
use anyhow::{bail, ensure, Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The preload library, built by `build.rs`.
const NOCHOWN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/nochown.so"));

pub struct Mount {
    pub src: PathBuf,
    pub dst: String,
    pub rw: bool,
}

pub struct Run {
    pub root: PathBuf,
    /// Where the preload library is put (a file outside the root, bound into it).
    pub shim: PathBuf,
    pub mounts: Vec<Mount>,
    pub env: Vec<(String, String)>,
    pub net: bool,
    pub cwd: Option<String>,
    pub argv: Vec<String>,
}

/// Write the preload library where bubblewrap can bind it from.
pub fn shim_in(dir: &Path) -> Result<PathBuf> {
    let p = dir.join("nochown.so");
    if std::fs::read(&p).ok().as_deref() != Some(NOCHOWN) {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".nochown.so.{}", std::process::id()));
        std::fs::write(&tmp, NOCHOWN)?;
        std::fs::rename(&tmp, &p)?;
    }
    Ok(p)
}

impl Run {
    pub fn new(root: &Path, shim: &Path, argv: &[&str]) -> Run {
        Run { root: root.to_path_buf(), shim: shim.to_path_buf(), mounts: Vec::new(), env: Vec::new(), net: false, cwd: None, argv: argv.iter().map(|s| s.to_string()).collect() }
    }

    pub fn bind(mut self, src: &Path, dst: &str, rw: bool) -> Run {
        self.mounts.push(Mount { src: src.to_path_buf(), dst: dst.to_string(), rw });
        self
    }

    pub fn env(mut self, k: &str, v: &str) -> Run {
        self.env.push((k.to_string(), v.to_string()));
        self
    }

    /// The `bwrap` command line.
    pub fn args(&self) -> Vec<OsString> {
        let mut a: Vec<OsString> = Vec::new();
        let mut push = |s: &str| a.push(OsString::from(s));
        push("--die-with-parent");
        // Nothing of the environment of the user reaches the commands of the recipe (tokens, agent sockets, keys): only the variables set below.
        push("--clearenv");
        // Its own session: no way to push keystrokes into the terminal of the user (TIOCSTI).
        push("--new-session");
        push("--bind");
        a.push(self.root.clone().into());
        a.push("/".into());
        for s in ["--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp", "--ro-bind"] {
            a.push(s.into());
        }
        a.push(self.shim.clone().into());
        a.push("/opt/nochown.so".into());
        for m in &self.mounts {
            a.push(if m.rw { "--bind" } else { "--ro-bind" }.into());
            a.push(m.src.clone().into());
            a.push(m.dst.clone().into());
        }
        for s in ["--unshare-user", "--uid", "0", "--gid", "0", "--unshare-pid", "--unshare-ipc", "--unshare-uts"] {
            a.push(s.into());
        }
        if !self.net {
            a.push("--unshare-net".into());
        }
        if let Some(c) = &self.cwd {
            a.push("--chdir".into());
            a.push(c.into());
        }
        let mut env = vec![
            ("HOME", "/root"),
            ("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"),
            ("LANG", "C.UTF-8"),
            ("LD_PRELOAD", "/opt/nochown.so"),
            ("DEBIAN_FRONTEND", "noninteractive"),
        ];
        let own: Vec<(&str, &str)> = self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        env.retain(|(k, _)| !own.iter().any(|(o, _)| o == k));
        for (k, v) in env.into_iter().chain(own) {
            a.extend(["--setenv".into(), k.into(), v.into()]);
        }
        a.push("--".into());
        a.extend(self.argv.iter().map(OsString::from));
        a
    }

    fn prepare(&self) -> Result<()> {
        if self.net {
            // The resolver of the host (a stub on 127.0.0.53 on most systems: the network is shared, so it works). The image is a folder of files that it brings itself:
            // the file is made through folder descriptors, so a link of the image (`etc -> /some/folder`) can neither take the removal nor the write out of it.
            if let Ok(t) = std::fs::read(Path::new("/etc/resolv.conf")) {
                use std::ffi::OsStr;
                let root = panpak_core::safefs::Dir::open_path(&self.root).context("open the image")?;
                let etc = root.open_dir(OsStr::new("etc")).context("etc of the image is not a folder (or is a link): refused")?;
                let name = OsStr::new("resolv.conf");
                if let Some(st) = etc.lstat(name)? {
                    ensure!(st.kind != panpak_core::safefs::Kind::Dir, "etc/resolv.conf of the image is a folder");
                    etc.unlink(name)?;
                }
                use std::io::Write;
                etc.create_new_file(name, 0o644).context("write resolv.conf in the image")?.write_all(&t).context("write resolv.conf in the image")?;
            }
        }
        Ok(())
    }

    /// Run and capture the output.
    pub fn output(&self) -> Result<Output> {
        self.prepare()?;
        Command::new("bwrap").args(self.args()).stdin(Stdio::null()).output().context("run bwrap (bubblewrap must be installed and allowed to make a user namespace)")
    }

    /// Run with the output to a log file; the error carries the end of the log.
    pub fn run_logged(&self, log: &Path, what: &str) -> Result<()> {
        self.prepare()?;
        let out = std::fs::File::create(log).with_context(|| format!("create {}", log.display()))?;
        let st = Command::new("bwrap").args(self.args()).stdin(Stdio::null()).stdout(out.try_clone()?).stderr(out).status().context("run bwrap (bubblewrap must be installed and allowed to make a user namespace)")?;
        if !st.success() {
            bail!("{what} failed ({st}); the end of {}:\n{}", log.display(), tail(log, 25));
        }
        Ok(())
    }
}

/// The last `n` lines of a file.
pub fn tail(p: &Path, n: usize) -> String {
    let t = std::fs::read_to_string(p).unwrap_or_default();
    let lines: Vec<&str> = t.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_has_one_uid_no_network_and_the_shim() {
        let r = Run::new(Path::new("/img"), Path::new("/c/nochown.so"), &["/usr/bin/true"]).bind(Path::new("/src"), "/src", false).bind(Path::new("/out"), "/out", true).env("A", "b");
        let a: Vec<String> = r.args().iter().map(|s| s.to_string_lossy().into_owned()).collect();
        let has = |w: &[&str]| a.windows(w.len()).any(|x| x == w);
        assert!(has(&["--bind", "/img", "/"]) && has(&["--ro-bind", "/c/nochown.so", "/opt/nochown.so"]));
        assert!(has(&["--ro-bind", "/src", "/src"]) && has(&["--bind", "/out", "/out"]));
        assert!(has(&["--unshare-user", "--uid", "0", "--gid", "0"]) && a.contains(&"--unshare-net".to_string()));
        assert!(has(&["--setenv", "A", "b"]) && has(&["--setenv", "LD_PRELOAD", "/opt/nochown.so"]));
        assert_eq!(a.last().unwrap(), "/usr/bin/true");
        let mut r = r;
        r.net = true;
        assert!(!r.args().iter().any(|s| s == "--unshare-net"));
    }

    #[test]
    fn the_build_gets_no_environment_of_the_user_and_its_own_session() {
        let r = Run::new(Path::new("/img"), Path::new("/c/nochown.so"), &["/usr/bin/true"]).env("A", "b");
        let a: Vec<String> = r.args().iter().map(|s| s.to_string_lossy().into_owned()).collect();
        let at = |w: &str| a.iter().position(|x| x == w);
        let (clear, first_set) = (at("--clearenv"), at("--setenv"));
        assert!(clear.is_some() && clear < first_set, "--clearenv must come before the first --setenv: {a:?}");
        assert!(a.contains(&"--new-session".to_string()), "a command of the recipe must not reach the terminal of the user: {a:?}");
    }

    /// A new, empty folder on disk for one test (never removed: it is small, and nothing here deletes).
    fn fresh(n: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/sandbox-tests").join(format!("{n}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn resolv_conf_is_put_in_the_image_and_a_link_of_the_image_cannot_take_it_elsewhere() {
        // A plain image: a link at etc/resolv.conf (Debian-like systems have one) is replaced by a file with the text of the host.
        let (root, shim) = (fresh("img"), fresh("shim").join("nochown.so"));
        std::fs::create_dir_all(root.join("etc")).unwrap();
        std::os::unix::fs::symlink("../run/resolv", root.join("etc/resolv.conf")).unwrap();
        let mut r = Run::new(&root, &shim, &["/usr/bin/true"]);
        r.net = true;
        r.prepare().unwrap();
        assert!(std::fs::symlink_metadata(root.join("etc/resolv.conf")).unwrap().file_type().is_file());
        // An image whose `etc` is a link to a folder of the host: the file there is not removed and not written.
        let (root2, victim) = (fresh("img2"), fresh("victim"));
        std::fs::write(victim.join("resolv.conf"), "keep").unwrap();
        std::os::unix::fs::symlink(&victim, root2.join("etc")).unwrap();
        let mut r2 = Run::new(&root2, &shim, &["/usr/bin/true"]);
        r2.net = true;
        assert!(r2.prepare().is_err(), "a link in the way of the file is an error");
        assert_eq!(std::fs::read_to_string(victim.join("resolv.conf")).unwrap(), "keep", "a file of the host was removed or written through the link of the image");
        // The same for a link at `etc/resolv.conf` that goes out through a folder that is a link.
        let (root3, victim3) = (fresh("img3"), fresh("victim3"));
        std::fs::write(victim3.join("target"), "keep").unwrap();
        std::fs::create_dir_all(root3.join("etc")).unwrap();
        std::os::unix::fs::symlink(victim3.join("target"), root3.join("etc/resolv.conf")).unwrap();
        let mut r3 = Run::new(&root3, &shim, &["/usr/bin/true"]);
        r3.net = true;
        r3.prepare().unwrap();
        assert_eq!(std::fs::read_to_string(victim3.join("target")).unwrap(), "keep", "the link was written through");
    }

    #[test]
    fn the_shim_is_a_shared_object_without_dependencies() {
        assert_eq!(&NOCHOWN[..4], b"\x7fELF");
        let d = std::env::temp_dir().join(format!("panpak-shim-{}", std::process::id()));
        let p = shim_in(&d).unwrap();
        let info = panpak_core::read_elf_info(&p).unwrap();
        assert!(info.needs.is_empty(), "the preload library must not need any library: {:?}", info.needs);
        let _ = std::fs::remove_dir_all(&d);
    }
}
