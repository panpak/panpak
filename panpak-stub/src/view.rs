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

/// A file mounted read-only into the view at another path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub src: PathBuf,
    pub dest: PathBuf,
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

/// Folders of the host that are mounted read-only at their own place. Landlock lets the app read
/// only `/usr`, `/etc`, `/sys`, `/proc`, `/run` and `/dev` outside its own folders, so nothing else
/// of the host is needed; and a folder that is mounted can hold a socket the app could connect to,
/// which Landlock below ABI V9 does not stop. `/nix/store` and `/gnu/store` are the library
/// stores of those systems (only the store, not the daemon socket beside it).
const SYSTEM_DIRS: &[&str] = &["usr", "etc", "sys", "nix/store", "gnu/store"];

/// Names at the top of `/` that are never put into the view, even as symbolic links.
const HIDDEN_TOP: &[&str] = &["var", "opt", "srv", "mnt", "media", "root", "boot", "snap", "tmp", "run", "dev", "proc", "nix", "gnu"];

/// The part of the host that is put into the view: what the app needs of the system, and nothing
/// that is not needed. Everything else of `/` is a new empty tmpfs.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RootLayout {
    /// (source, destination), mounted read-only.
    pub binds: Vec<(PathBuf, PathBuf)>,
    /// (target, destination): the symbolic links at the top of `/` (`/bin`, `/lib64`, and `/home`
    /// on systems that keep the homes in `/var/home`).
    pub links: Vec<(PathBuf, PathBuf)>,
}

/// Decide the layout from the host root (`/`; another folder in tests).
pub fn root_layout(host: &Path) -> RootLayout {
    let mut out = RootLayout::default();
    for d in SYSTEM_DIRS {
        let src = host.join(d);
        if src.is_dir() {
            out.binds.push((src, Path::new("/").join(d)));
        }
    }
    let mut links = Vec::new();
    if let Ok(rd) = std::fs::read_dir(host) {
        for e in rd.flatten() {
            let name = e.file_name();
            let hidden = name.to_str().is_none_or(|n| HIDDEN_TOP.contains(&n) || SYSTEM_DIRS.contains(&n));
            if hidden || !e.file_type().is_ok_and(|t| t.is_symlink()) {
                continue;
            }
            if let Ok(target) = std::fs::read_link(e.path()) {
                links.push((target, Path::new("/").join(&name)));
            }
        }
    }
    links.sort_by(|a, b| a.1.cmp(&b.1));
    out.links = links;
    out
}

/// Whether a host path is visible in the view as the host has it (a read-only mount of a system
/// folder, or `/dev`, `/proc`, `/tmp`): a path elsewhere must be mounted to be seen.
pub fn in_system_view(p: &Path) -> bool {
    ["/usr", "/etc", "/sys", "/dev", "/proc", "/tmp", "/run", "/nix/store", "/gnu/store"].iter().any(|d| p.starts_with(d))
}

/// What the view contains besides `/dev` and `/proc`.
pub struct ViewPlan {
    /// The system folders and top-level links of the host. See `root_layout`.
    pub root: RootLayout,
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
    /// Files mounted read-only at a place of their own: the opener channel and the stub as `xdg-open`.
    pub mounts: Vec<Mount>,
    /// Folders of the host mounted as `/tmp` and `/dev/shm`, shared by the copies of one app.
    /// None: a private tmpfs for `/tmp`, and the host's `/dev/shm`.
    pub shared_tmp: Option<PathBuf>,
    pub shared_shm: Option<PathBuf>,
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
    let mut a = view_args(plan);
    a.push(os("--"));
    a.push(plan.stub.as_os_str().into());
    a.push(os("--panpak-inner"));
    a.push(plan.fd.to_string().into());
    a
}

/// The arguments of `bwrap` that build the view, without the command that runs in it.
fn view_args(plan: &ViewPlan) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    // The process group of the app ends with the stub, and the app has no terminal to inject keys into.
    a.extend(["--die-with-parent", "--new-session", "--unshare-user", "--unshare-pid"].map(os));
    // SysV and POSIX IPC objects are separate, except with X11 (MIT-SHM needs the shared ones).
    if !plan.x11_access {
        a.push(os("--unshare-ipc"));
    }
    // The root is a new tmpfs: only the system folders come in. `/var` is an empty tmpfs with the
    // two links that name the runtime folder, so that old paths like /var/run/... still lead there.
    for (src, dest) in &plan.root.binds {
        a.extend([os("--ro-bind"), src.as_os_str().into(), dest.as_os_str().into()]);
    }
    for (target, dest) in &plan.root.links {
        a.extend([os("--symlink"), target.as_os_str().into(), dest.as_os_str().into()]);
    }
    a.extend(["--tmpfs", "/var", "--symlink", "../run", "/var/run", "--symlink", "../run/lock", "/var/lock"].map(os));
    a.extend(["--dev-bind", "/dev", "/dev", "--proc", "/proc"].map(os));
    match &plan.shared_tmp {
        Some(t) => a.extend([os("--bind"), t.as_os_str().into(), os("/tmp")]),
        None => a.extend(["--tmpfs", "/tmp"].map(os)),
    }
    a.extend(["--tmpfs", "/run"].map(os));
    if let Some(shm) = &plan.shared_shm {
        a.extend([os("--bind"), shm.as_os_str().into(), os("/dev/shm")]);
    }
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
    for m in &plan.mounts {
        a.extend([os("--ro-bind"), m.src.as_os_str().into(), m.dest.as_os_str().into()]);
    }
    // Nothing can be created in the home folder any more, outside the binds.
    if let Some(h) = &plan.home {
        a.extend([os("--remount-ro"), h.as_os_str().into()]);
    }
    a.extend(["--remount-ro", "/var", "--remount-ro", "/"].map(os));
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

static CHILD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn forward_signal(sig: libc::c_int) {
    let pid = CHILD.load(std::sync::atomic::Ordering::Relaxed);
    if pid > 0 {
        // SAFETY: kill is async-signal-safe.
        unsafe { libc::kill(pid, sig) };
    }
}

/// Run `bwrap` as a child and stay until it ends, so that `keep` (the opener server) lives as long
/// as the app. Termination signals go on to `bwrap`; its exit status becomes ours. Returns only on error.
pub fn run_bwrap(plan: &ViewPlan, keep: impl Sized) -> Result<std::convert::Infallible> {
    use std::os::unix::process::ExitStatusExt;
    let args = bwrap_args(plan);
    ensure!(args.iter().all(|a| !a.as_bytes().contains(&0)), "NUL in a bubblewrap argument");
    let mut child = Command::new(panpak_core::BWRAP).args(&args).spawn().with_context(|| format!("cannot run {}", panpak_core::BWRAP))?;
    CHILD.store(child.id() as i32, std::sync::atomic::Ordering::Relaxed);
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT] {
        // SAFETY: the handler only calls kill.
        unsafe { libc::signal(sig, forward_signal as extern "C" fn(libc::c_int) as libc::sighandler_t) };
    }
    let status = child.wait()?;
    drop(keep);
    std::process::exit(status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
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
            root: root_layout(Path::new("/")),
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
            mounts: vec![],
            shared_tmp: None,
            shared_shm: None,
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
        assert!(pos(&a, &["--ro-bind", "/usr", "/usr"]).unwrap() < pos(&a, &["--tmpfs", "/tmp"]).unwrap());
        assert!(pos(&a, &["--dev-bind", "/dev", "/dev", "--proc", "/proc"]).is_some());
        // The host root is never mounted whole, and /var is a new tmpfs with the two links.
        assert!(pos(&a, &["--ro-bind", "/", "/"]).is_none() && pos(&a, &["--bind", "/", "/"]).is_none());
        let var = pos(&a, &["--tmpfs", "/var", "--symlink", "../run", "/var/run", "--symlink", "../run/lock", "/var/lock"]).unwrap();
        assert!(var < pos(&a, &["--tmpfs", "/run"]).unwrap());
        assert_eq!(&a[a.len() - 8..a.len() - 4], ["--remount-ro", "/var", "--remount-ro", "/"]);
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

    /// A fake host root with the folders of a typical system, a Silverblue-like `/home` link and
    /// folders that the view must leave out.
    fn fake_host(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-root-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for sub in ["usr/bin", "etc", "sys", "var/lib/lxd", "opt/x", "srv", "mnt", "media", "root", "boot", "snap", "nix/store", "nix/var/nix/daemon-socket", "home"] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
        }
        std::fs::remove_dir(d.join("home")).unwrap();
        for (target, name) in [("usr/bin", "bin"), ("usr/lib", "lib"), ("usr/lib64", "lib64"), ("var/home", "home"), ("var/opt", "opt2"), ("run/media", "media2")] {
            std::os::unix::fs::symlink(target, d.join(name)).unwrap();
        }
        d
    }

    #[test]
    fn the_layout_has_the_system_folders_and_the_top_links_only() {
        let host = fake_host("layout");
        let l = root_layout(&host);
        let dests: Vec<&str> = l.binds.iter().map(|(_, d)| d.to_str().unwrap()).collect();
        assert_eq!(dests, ["/usr", "/etc", "/sys", "/nix/store"]);
        assert!(l.binds.iter().all(|(s, d)| *s == host.join(d.strip_prefix("/").unwrap())));
        let links: Vec<(&str, &str)> = l.links.iter().map(|(t, d)| (t.to_str().unwrap(), d.to_str().unwrap())).collect();
        assert_eq!(links, [("usr/bin", "/bin"), ("var/home", "/home"), ("usr/lib", "/lib"), ("usr/lib64", "/lib64"), ("run/media", "/media2"), ("var/opt", "/opt2")]);
        // The hidden names are not mounted whatever they are on the host.
        let mut p = plan();
        p.root = l;
        let a = strings(&p);
        for hidden in ["/opt", "/srv", "/mnt", "/media", "/root", "/boot", "/snap", "/nix", "/gnu"] {
            assert!(!a.iter().any(|s| s == hidden), "{hidden} is in the view");
        }
        assert!(!a.iter().any(|s| s.contains("daemon-socket") || s.contains("lxd")));
        let _ = std::fs::remove_dir_all(&host);
    }

    #[test]
    fn a_path_is_in_the_system_view_only_below_the_system_folders() {
        for p in ["/usr/lib/x", "/etc/passwd", "/sys/class", "/dev/null", "/tmp/a", "/nix/store/abc"] {
            assert!(in_system_view(Path::new(p)), "{p}");
        }
        for p in ["/var/lib/x", "/opt/app", "/srv/data", "/mnt/disk", "/media/u/usb", "/nix/var/nix/daemon-socket", "/home/u", "/usrx"] {
            assert!(!in_system_view(Path::new(p)), "{p}");
        }
    }

    /// Every socket file below the folders given (default `/`) of a view, one per line. Python 3 from the host's /usr.
    const SCAN: &str = "import os, stat, sys\n\
        def scan(d):\n\
        \ttry: it = os.scandir(d)\n\
        \texcept OSError: return\n\
        \twith it:\n\
        \t\tfor e in it:\n\
        \t\t\ttry:\n\
        \t\t\t\tif e.is_symlink(): continue\n\
        \t\t\t\tif e.is_dir(follow_symlinks=False):\n\
        \t\t\t\t\tif e.path != '/proc': scan(e.path)\n\
        \t\t\t\telif stat.S_ISSOCK(e.stat(follow_symlinks=False).st_mode): print(e.path)\n\
        \t\t\texcept OSError: pass\n\
        for r in sys.argv[1:] or ['/']: scan(r)\n";

    fn sockets_in(bwrap_args: Vec<OsString>, roots: &[&str]) -> Vec<String> {
        let out = Command::new(panpak_core::BWRAP).args(bwrap_args).args(["--", "/usr/bin/python3", "-I", "-c", SCAN]).args(roots).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let mut v: Vec<String> = String::from_utf8(out.stdout).unwrap().lines().map(str::to_string).collect();
        v.sort();
        v
    }

    /// The scan of the whole visible tree finds the sockets that the old view (the host root,
    /// read-only) showed, and finds none of them in the new view. The host is a fake root with
    /// sockets in the places of real services (LXD, a daemon in /opt, a service in /srv, Nix).
    #[test]
    fn no_socket_of_the_host_is_reachable_in_the_view() {
        if !panpak_core::view_status().usable() || !Path::new("/usr/bin/python3").exists() {
            eprintln!("skipped: bubblewrap or python3 is not usable here");
            return;
        }
        let host = fake_host("scan");
        // The system folders of the fake host are the real ones.
        for d in ["usr", "etc", "sys"] {
            let _ = std::fs::remove_dir_all(host.join(d));
            std::os::unix::fs::symlink(Path::new("/").join(d), host.join(d)).unwrap();
        }
        let planted: Vec<PathBuf> = ["var/lib/lxd/unix.socket", "opt/x/daemon.sock", "srv/service.sock", "mnt/disk.sock", "media/usb.sock", "nix/var/nix/daemon-socket/socket", "root/agent.sock", "boot/b.sock", "snap/s.sock"]
            .iter()
            .map(|r| host.join(r))
            .collect();
        let _listeners: Vec<std::os::unix::net::UnixListener> = planted
            .iter()
            .map(|p| {
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::os::unix::net::UnixListener::bind(p).unwrap()
            })
            .collect();
        // Control: the old shape (the real root, read-only) with those folders of the fake host on top.
        let mut old: Vec<OsString> = ["--unshare-user", "--ro-bind", "/", "/", "--dev-bind", "/dev", "/dev", "--proc", "/proc"].map(os).into();
        let on_top: Vec<&str> = ["var", "opt", "srv", "mnt", "media", "root", "boot", "snap"].into_iter().filter(|d| Path::new("/").join(d).is_dir()).collect();
        for d in &on_top {
            old.extend([os("--ro-bind"), host.join(d).into(), Path::new("/").join(d).into()]);
        }
        let before = sockets_in(old, &on_top.iter().map(|d| format!("/{d}")).collect::<Vec<_>>().iter().map(String::as_str).collect::<Vec<_>>());
        for p in planted.iter().filter(|p| on_top.iter().any(|d| p.starts_with(host.join(d)))) {
            let want = p.strip_prefix(&host).unwrap();
            assert!(before.iter().any(|s| Path::new(s) == Path::new("/").join(want)), "the scan does not find {} in the old view: {before:?}", want.display());
        }
        // The new view, built from the fake host. Landlock is not applied here: the scan lists folders.
        let mut p = plan();
        p.root = root_layout(&host);
        p.home = None;
        p.runtime_dir = None;
        p.sockets = vec![];
        p.ro_run_dirs = vec![];
        p.binds = vec![];
        let after = sockets_in(view_args(&p), &[]);
        assert!(after.is_empty(), "sockets reachable in the new view: {after:?}");
        let _ = std::fs::remove_dir_all(&host);
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
    fn shared_folders_replace_the_private_tmp_and_the_host_shm() {
        let mut p = plan();
        p.shared_tmp = Some("/run/user/1000/panpak/org.example.app/tmp".into());
        p.shared_shm = Some("/run/user/1000/panpak/org.example.app/shm".into());
        let a = strings(&p);
        assert!(pos(&a, &["--tmpfs", "/tmp"]).is_none());
        let dev = pos(&a, &["--dev-bind", "/dev", "/dev"]).unwrap();
        let tmp = pos(&a, &["--bind", "/run/user/1000/panpak/org.example.app/tmp", "/tmp"]).unwrap();
        let shm = pos(&a, &["--bind", "/run/user/1000/panpak/org.example.app/shm", "/dev/shm"]).unwrap();
        assert!(dev < tmp && dev < shm, "the binds come after the device tree they cover");
    }

    #[test]
    fn mounts_come_after_the_runtime_folder_and_before_the_home_is_sealed() {
        let mut p = plan();
        p.mounts = vec![
            Mount { src: "/h/open.5.sock".into(), dest: "/run/user/1000/panpak/open.sock".into() },
            Mount { src: "/h/panpak-stub".into(), dest: "/usr/bin/xdg-open".into() },
        ];
        let a = strings(&p);
        let rt = pos(&a, &["--tmpfs", "/run/user/1000"]).unwrap();
        let sock = pos(&a, &["--ro-bind", "/h/open.5.sock", "/run/user/1000/panpak/open.sock"]).unwrap();
        let exe = pos(&a, &["--ro-bind", "/h/panpak-stub", "/usr/bin/xdg-open"]).unwrap();
        assert!(rt < sock && sock < exe);
        assert!(exe < pos(&a, &["--remount-ro", "/home/u"]).unwrap());
        assert!(pos(&a, &["--ro-bind", "/usr", "/usr"]).unwrap() < exe);
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
