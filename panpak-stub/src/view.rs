// SPDX-License-Identifier: MIT OR Apache-2.0
//! The isolated view. The outer stub (which verified the package) runs
//! `/usr/bin/bwrap`, and bubblewrap runs the stub again inside the view with
//! `--panpak-inner <fd>`. The inner stub reads a job from a sealed memfd, applies
//! Landlock and executes the app. It does not verify the package a second time:
//! see THREAT-MODEL.md §3 for why that is safe.
use anyhow::{bail, ensure, Context, Result};
use std::{
    ffi::OsString,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{ffi::OsStrExt, io::FromRawFd, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
};

/// What the inner stub needs: everything the outer stub decided, nothing it must check again.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Job {
    /// The version of this structure; the inner stub refuses another one.
    pub version: u32,
    pub tree: PathBuf,
    pub manifest: panpak_core::Manifest,
    pub rw: Vec<PathBuf>,
    pub ro: Vec<PathBuf>,
    pub sockets: Vec<PathBuf>,
    pub allow_unfiltered: bool,
    /// The home folder may be listed inside the view (see `Access::home_list`).
    #[serde(default)]
    pub home_list: Option<PathBuf>,
    pub open: Option<JobOpen>,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Environment of the app: variables to set, then variables to remove.
    pub env_set: Vec<(String, String)>,
    pub env_remove: Vec<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct JobOpen {
    pub path: PathBuf,
    pub write: bool,
    pub rename: bool,
}

pub const JOB_VERSION: u32 = 1;

/// A command taken apart so that it can cross the view.
pub struct CommandParts {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env_set: Vec<(String, String)>,
    pub env_remove: Vec<String>,
}

impl Job {
    /// The command of the app, as the outer stub built it.
    pub fn command_of(cmd: &Command) -> Result<CommandParts> {
        let text = |s: &std::ffi::OsStr| s.to_str().map(str::to_string).context("non-UTF-8 text in the command of the app");
        let args = cmd.get_args().map(text).collect::<Result<Vec<_>>>()?;
        let (mut env_set, mut env_remove) = (Vec::new(), Vec::new());
        for (k, v) in cmd.get_envs() {
            match v {
                Some(v) => env_set.push((text(k)?, text(v)?)),
                None => env_remove.push(text(k)?),
            }
        }
        Ok(CommandParts { program: PathBuf::from(cmd.get_program()), args, env_set, env_remove })
    }

    pub fn to_command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args);
        c.envs(self.env_set.iter().map(|(k, v)| (k, v)));
        for k in &self.env_remove {
            c.env_remove(k);
        }
        c
    }
}

/// A memfd with the job, sealed against any change. It is inherited through bubblewrap.
pub fn job_memfd(job: &Job) -> Result<i32> {
    let text = toml::to_string(job).context("encode the job")?;
    // SAFETY: plain system calls on a descriptor created here.
    unsafe {
        let fd = libc::memfd_create(c"panpak-job".as_ptr(), libc::MFD_ALLOW_SEALING);
        ensure!(fd >= 0, "memfd_create: {}", std::io::Error::last_os_error());
        let mut f = std::fs::File::from_raw_fd(fd);
        f.write_all(text.as_bytes())?;
        let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
        ensure!(libc::fcntl(fd, libc::F_ADD_SEALS, seals) == 0, "seal the job: {}", std::io::Error::last_os_error());
        Ok(std::os::unix::io::IntoRawFd::into_raw_fd(f))
    }
}

/// The inner stub: read the job from `fd`. The descriptor must be a sealed memfd, which only a
/// process of this program (the outer stub) makes, and it is closed before the app starts.
pub fn read_job(fd: i32) -> Result<Job> {
    // SAFETY: the descriptor was inherited from the outer stub; this program owns it from here.
    unsafe {
        let seals = libc::fcntl(fd, libc::F_GET_SEALS);
        let need = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE | libc::F_SEAL_SEAL;
        ensure!(seals >= 0 && seals & need == need, "the job descriptor is not a sealed memfd");
        let mut f = std::fs::File::from_raw_fd(fd);
        let mut text = String::new();
        f.seek(SeekFrom::Start(0)).context("rewind the job")?;
        f.read_to_string(&mut text).context("read the job")?;
        let job: Job = toml::from_str(&text).context("decode the job")?;
        ensure!(job.version == JOB_VERSION, "job version {} not supported", job.version);
        Ok(job)
    }
}

/// A path mounted into the view at its own place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bind {
    pub path: PathBuf,
    pub rw: bool,
}

/// Order and thin out the binds: parents before children, no duplicates, and no read-only
/// bind below a bind that already covers it (a read-only child under a writable parent would
/// only get in the way of Landlock, which allows the parent). A writable child under a read-only
/// parent stays: it is mounted on top.
pub fn merge_binds(binds: Vec<Bind>) -> Vec<Bind> {
    let mut sorted = binds;
    sorted.sort_by_key(|b| b.path.components().count());
    let mut out: Vec<Bind> = Vec::new();
    for b in sorted {
        let covered = out.iter().any(|o| b.path.starts_with(&o.path) && (o.rw || !b.rw));
        if !covered {
            out.push(b);
        }
    }
    out
}

/// What the view contains besides `/`, `/dev` and `/proc`.
pub struct ViewPlan {
    /// `$XDG_RUNTIME_DIR` of the user: it becomes a new tmpfs with only the allowed sockets in it.
    pub runtime_dir: Option<PathBuf>,
    /// Sockets that exist and are allowed: the manifest's, the implied ones. Each is mounted at its own path.
    pub sockets: Vec<PathBuf>,
    /// Folders in `/run` mounted read-only (name resolution).
    pub ro_run_dirs: Vec<PathBuf>,
    /// X11 sockets (`/tmp/.X11-unix/X<n>`): present only when the manifest asks for X11.
    pub x11: Vec<PathBuf>,
    /// The home folder (canonical). It becomes an empty tmpfs, made read-only after the binds.
    /// None: the home folder is left as `/` shows it (no home, or the home is `/`).
    pub home: Option<PathBuf>,
    /// What is put back into the empty home, and what is mounted writable elsewhere. See `merge_binds`.
    pub binds: Vec<Bind>,
    /// The manifest asks for X11: the IPC namespace stays shared (MIT-SHM).
    pub x11_access: bool,
    pub stub: PathBuf,
    pub fd: i32,
}

fn os(s: &str) -> OsString {
    OsString::from(s)
}

/// The arguments of `bwrap`. Pure: the same plan gives the same arguments.
pub fn bwrap_args(plan: &ViewPlan) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    // The process group of the app ends with the stub, and the app has no terminal to inject keys into.
    a.extend(["--die-with-parent", "--new-session", "--unshare-user", "--unshare-pid"].map(os));
    // SysV and POSIX IPC objects are separate, except with X11 (MIT-SHM needs the shared ones).
    if !plan.x11_access {
        a.push(os("--unshare-ipc"));
    }
    a.extend(["--ro-bind", "/", "/", "--dev-bind", "/dev", "/dev", "--proc", "/proc"].map(os));
    a.extend(["--tmpfs", "/tmp", "--tmpfs", "/run"].map(os));
    // The home folder is empty; the binds below put back only what the app needs. It comes after
    // /tmp and /run so that a home below them is not hidden again.
    if let Some(h) = &plan.home {
        a.extend([os("--perms"), os("0700"), os("--tmpfs"), h.as_os_str().into()]);
    }
    for d in &plan.ro_run_dirs {
        a.extend([os("--ro-bind"), d.as_os_str().into(), d.as_os_str().into()]);
    }
    if let Some(rt) = &plan.runtime_dir {
        a.extend([os("--perms"), os("0700"), os("--tmpfs"), rt.as_os_str().into()]);
    }
    for s in plan.sockets.iter().chain(&plan.x11) {
        a.extend([os("--bind"), s.as_os_str().into(), s.as_os_str().into()]);
    }
    for b in &plan.binds {
        a.extend([os(if b.rw { "--bind" } else { "--ro-bind" }), b.path.as_os_str().into(), b.path.as_os_str().into()]);
    }
    // Nothing can be created in the home folder any more, outside the binds.
    if let Some(h) = &plan.home {
        a.extend([os("--remount-ro"), h.as_os_str().into()]);
    }
    a.push(os("--"));
    a.push(plan.stub.as_os_str().into());
    a.push(os("--panpak-inner"));
    a.push(plan.fd.to_string().into());
    a
}

/// The local X11 socket of `$DISPLAY` (`:N` or `:N.M`), if there is one.
pub fn x11_socket(display: Option<&str>) -> Option<PathBuf> {
    let d = display?.strip_prefix(':')?;
    let n: String = d.chars().take_while(char::is_ascii_digit).collect();
    if n.is_empty() {
        return None;
    }
    let p = PathBuf::from(format!("/tmp/.X11-unix/X{n}"));
    p.exists().then_some(p)
}

/// Replace this process with `bwrap`, which runs the inner stub. Returns only on error.
pub fn exec_bwrap(plan: &ViewPlan) -> Result<std::convert::Infallible> {
    let args = bwrap_args(plan);
    ensure!(args.iter().all(|a| !a.as_bytes().contains(&0)), "NUL in a bubblewrap argument");
    let err = Command::new(panpak_core::BWRAP).args(&args).exec();
    bail!("cannot run {}: {err}", panpak_core::BWRAP)
}

/// Drop duplicates, keep the order.
pub fn unique(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    paths.into_iter().filter(|p| seen.insert(p.clone())).collect()
}

/// The parent of `p` that exists, for a rename grant.
pub fn existing_parent(p: &Path) -> Option<PathBuf> {
    p.parent().filter(|d| d.is_dir()).map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> ViewPlan {
        ViewPlan {
            runtime_dir: Some("/run/user/1000".into()),
            sockets: vec!["/run/user/1000/wayland-0".into(), "/run/systemd/journal/socket".into()],
            ro_run_dirs: vec!["/run/systemd/resolve".into()],
            x11: vec![],
            home: Some("/home/u".into()),
            binds: merge_binds(vec![
                Bind { path: "/home/u/.local/share/panpak/userdata/org.example.app".into(), rw: true },
                Bind { path: "/home/u/.cache/panpak/org.example.app".into(), rw: true },
                Bind { path: "/home/u/.local/share/panpak/trees/abc".into(), rw: false },
                Bind { path: "/home/u/.config/gtk-3.0".into(), rw: false },
            ]),
            x11_access: false,
            stub: "/home/u/.local/share/panpak/runtimes/1/panpak-stub".into(),
            fd: 7,
        }
    }

    fn strings(plan: &ViewPlan) -> Vec<String> {
        bwrap_args(plan).iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    fn pos(a: &[String], seq: &[&str]) -> Option<usize> {
        a.windows(seq.len()).position(|w| w.iter().zip(seq).all(|(x, y)| x == y))
    }

    #[test]
    fn arguments_for_a_typical_app() {
        let a = strings(&plan());
        assert_eq!(&a[..5], ["--die-with-parent", "--new-session", "--unshare-user", "--unshare-pid", "--unshare-ipc"]);
        assert!(pos(&a, &["--ro-bind", "/", "/"]).unwrap() < pos(&a, &["--tmpfs", "/tmp"]).unwrap());
        assert!(pos(&a, &["--dev-bind", "/dev", "/dev", "--proc", "/proc"]).is_some());
        // /run is replaced first, then the runtime folder and the allowed sockets are put back.
        let run = pos(&a, &["--tmpfs", "/run"]).unwrap();
        assert!(run < pos(&a, &["--ro-bind", "/run/systemd/resolve", "/run/systemd/resolve"]).unwrap());
        let rt = pos(&a, &["--perms", "0700", "--tmpfs", "/run/user/1000"]).unwrap();
        assert!(run < rt);
        assert!(rt < pos(&a, &["--bind", "/run/user/1000/wayland-0", "/run/user/1000/wayland-0"]).unwrap());
        assert!(pos(&a, &["--bind", "/run/systemd/journal/socket", "/run/systemd/journal/socket"]).is_some());
        // The home folder is an empty tmpfs after /tmp and /run, then only what is listed comes back,
        // read-only except the two app folders, and the tmpfs is made read-only at the end.
        let home = pos(&a, &["--perms", "0700", "--tmpfs", "/home/u"]).unwrap();
        assert!(pos(&a, &["--tmpfs", "/run"]).unwrap() < home);
        assert!(home < pos(&a, &["--ro-bind", "/home/u/.config/gtk-3.0", "/home/u/.config/gtk-3.0"]).unwrap());
        assert!(pos(&a, &["--ro-bind", "/home/u/.local/share/panpak/trees/abc", "/home/u/.local/share/panpak/trees/abc"]).is_some());
        let writable: Vec<&String> = a.iter().enumerate().filter(|(i, s)| *s == "--bind" && a[*i + 1].contains("panpak")).map(|(_, s)| s).collect();
        assert_eq!(writable.len(), 2);
        let last_bind = a.iter().rposition(|s| s == "--bind" || s == "--ro-bind").unwrap();
        assert!(last_bind < pos(&a, &["--remount-ro", "/home/u"]).unwrap());
        assert_eq!(&a[a.len() - 4..], ["--", "/home/u/.local/share/panpak/runtimes/1/panpak-stub", "--panpak-inner", "7"]);
        // Nothing of the session is in the plan.
        assert!(!a.iter().any(|s| s.ends_with("/bus") || s.contains("gnupg") || s.contains("systemd/private") || s.contains("pulse") || s.contains("pipewire")));
    }

    #[test]
    fn x11_keeps_the_ipc_namespace_and_adds_the_socket() {
        let mut p = plan();
        p.x11_access = true;
        p.x11 = vec!["/tmp/.X11-unix/X0".into()];
        let a = strings(&p);
        assert!(!a.contains(&"--unshare-ipc".to_string()));
        let tmp = pos(&a, &["--tmpfs", "/tmp"]).unwrap();
        assert!(tmp < pos(&a, &["--bind", "/tmp/.X11-unix/X0", "/tmp/.X11-unix/X0"]).unwrap());
    }

    #[test]
    fn x11_socket_comes_from_display() {
        assert_eq!(x11_socket(None), None);
        assert_eq!(x11_socket(Some("host:0")), None);
        assert_eq!(x11_socket(Some(":")), None);
        assert_eq!(x11_socket(Some(":999999")), None);
    }

    #[test]
    fn the_job_survives_the_trip_and_only_a_sealed_memfd_is_read() {
        let man: panpak_core::Manifest = toml::from_str(
            "[app]\nid=\"org.example.app\"\nname=\"App\"\nversion=\"1\"\nentry=\"bin/app\"\n[permissions]\nx11=true\naudio=true\n[update]\nkey=\"ed25519:00\"\n",
        )
        .unwrap();
        let mut cmd = Command::new("/t/bin/app");
        cmd.args(["--a", "b c"]).env("TMPDIR", "/tmp").env_remove("SSH_AUTH_SOCK");
        let parts = Job::command_of(&cmd).unwrap();
        let job = Job {
            version: JOB_VERSION,
            tree: "/t".into(),
            manifest: man,
            rw: vec!["/a".into()],
            ro: vec!["/b".into()],
            sockets: vec!["/s".into()],
            allow_unfiltered: true,
            home_list: None,
            open: Some(JobOpen { path: "/f".into(), write: true, rename: false }),
            program: parts.program,
            args: parts.args,
            env_set: parts.env_set,
            env_remove: parts.env_remove,
        };
        let fd = job_memfd(&job).unwrap();
        // Inherited by a child, so it must not be close-on-exec.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC, 0);
        let back = read_job(fd).unwrap();
        assert_eq!(back.tree, PathBuf::from("/t"));
        assert_eq!(back.manifest.permissions.as_ref().and_then(|p| p.audio), Some(true));
        assert!(back.open.as_ref().is_some_and(|o| o.write && !o.rename));
        let c = back.to_command();
        assert_eq!(c.get_program(), "/t/bin/app");
        assert_eq!(c.get_args().collect::<Vec<_>>(), ["--a", "b c"]);
        let envs: Vec<_> = c.get_envs().map(|(k, v)| (k.to_str().unwrap().to_string(), v.map(|v| v.to_str().unwrap().to_string()))).collect();
        assert!(envs.contains(&("TMPDIR".to_string(), Some("/tmp".to_string()))));
        assert!(envs.contains(&("SSH_AUTH_SOCK".to_string(), None)));
        assert_eq!(back.env_set, vec![("TMPDIR".to_string(), "/tmp".to_string())]);
        assert_eq!(back.env_remove, vec!["SSH_AUTH_SOCK".to_string()]);
        // An ordinary file is refused: it carries no seals.
        let path = std::env::temp_dir().join(format!("panpak-job-{}", std::process::id()));
        std::fs::write(&path, "x").unwrap();
        let f = std::fs::File::open(&path).unwrap();
        let raw = std::os::unix::io::IntoRawFd::into_raw_fd(f);
        assert!(read_job(raw).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn binds_are_ordered_and_thinned() {
        let b = |p: &str, rw: bool| Bind { path: p.into(), rw };
        let m = merge_binds(vec![
            b("/h/.config/gtk-3.0", false),
            b("/h/app/data", true),
            b("/h", false),
            b("/h", false),
            b("/h/app", true),
            b("/h/app/x", false),
            b("/h/ro/sub", true),
            b("/h/ro", false),
        ]);
        let got: Vec<(&str, bool)> = m.iter().map(|b| (b.path.to_str().unwrap(), b.rw)).collect();
        // /h read-only covers the read-only children; /h/app (rw) covers /h/app/data and /h/app/x;
        // the rw child of a read-only parent stays and comes after it.
        assert_eq!(got, vec![("/h", false), ("/h/app", true), ("/h/ro/sub", true)]);
        assert!(merge_binds(vec![]).is_empty());
    }

    #[test]
    fn duplicates_are_dropped() {
        let u = unique(vec!["/a".into(), "/b".into(), "/a".into()]);
        assert_eq!(u, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }
}
