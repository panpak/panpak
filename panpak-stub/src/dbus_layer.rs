// SPDX-License-Identifier: MIT OR Apache-2.0
//! The private session bus of an app in the view (PANPAK-FORMAT.md §9.5, docs/PRIVATE-BUS.md).
//!
//! Outside the sandbox, before bubblewrap: put the stub in its own systemd scope (so that the
//! portal knows the app as `panpak.<id>`), start one `xdg-dbus-proxy` for this start in the same
//! scope (the filter towards the host bus, on a socket the app cannot see), attach to the router of the
//! app (`busrouter.rs`, started by the first copy), which owns the app's own names and sits between
//! the app and the proxy, and start a second proxy for the accessibility bus. The view gets the
//! router's socket as the session bus, the accessibility socket, and the app's folder of the
//! document portal. When any step cannot be done the app gets no bus, as before, and the reason is
//! printed once.

use crate::view::Mount;
use anyhow::{bail, ensure, Context, Result};
use panpak_core::dbus::{self, BusNeeds, Connection};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Everything the view needs from the layer. The layer is finished on a thread of its own: the app is started
/// without waiting for systemd. `wait` tells the inner stub to wait for the proxy's socket before it starts the app.
pub struct Layer {
    /// The proxy folder at `$XDG_RUNTIME_DIR/panpak/dbus` (the socket appears in it when the proxy listens) and the
    /// document folder at `$XDG_RUNTIME_DIR/doc`.
    pub mounts: Vec<Mount>,
    /// `$XDG_RUNTIME_DIR/bus` in the view is a link to the socket in the proxy folder: (target, link).
    pub symlinks: Vec<(PathBuf, PathBuf)>,
    /// Variables for the app.
    pub env: Vec<(&'static str, String)>,
    /// Paths in the view that Landlock (ABI 9) must let the app connect to.
    pub landlock_sockets: Vec<PathBuf>,
    /// What the inner stub waits for, as paths in the view.
    pub wait: crate::view::DbusWait,
    /// The worker says here whether the stub is in a scope that gives the app its identity (see `wait_scope`).
    scope: Option<std::sync::mpsc::Receiver<bool>>,
    /// Dropping the sender lets the worker end the proxy; `Drop` waits for it.
    stop: Option<std::sync::mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Layer {
    /// Wait until the stub is in the scope of the app. MUST be called before bubblewrap is started, so that bubblewrap, the
    /// app and everything it starts are in the scope: systemd-oomd and the user then see one unit for the whole app.
    /// Returns false if the scope could not be made (the layer then gives the app no bus).
    pub fn wait_scope(&mut self) -> bool {
        self.scope.take().is_none_or(|r| r.recv().unwrap_or(false))
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Ends the proxy and removes its folder when the stub is done with the app.
struct Guard {
    child: Child,
    dir: PathBuf,
    /// Read end of the pipe the proxy writes to: closing it (end of this process) stops the proxy.
    _sync: OwnedFd,
    /// The attachment to the router of the app and the proxy of the accessibility bus, ended before the main proxy.
    attachment: Option<crate::busrouter::Attachment>,
    a11y: Option<Box<Guard>>,
    /// More folders to remove (the one the app sees).
    also: Vec<PathBuf>,
    /// Remove `dir` when this guard goes (not for the accessibility proxy, whose socket is in the folder the app sees).
    remove_dir: bool,
}

impl Drop for Guard {
    fn drop(&mut self) {
        // The router first (it lets go of the copy's listener and connections), then both proxies are stopped together (the waits come after the
        // kills: one after the other cost a millisecond or two at every exit), then the folders.
        self.attachment.take();
        if let Some(a) = self.a11y.as_mut() {
            let _ = a.child.kill();
        }
        let _ = self.child.kill();
        self.a11y.take();
        let _ = self.child.wait();
        if self.remove_dir {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
        for d in &self.also {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

/// A reason for having no layer that needs no warning at every start: the system has no proxy, or
/// the manifest already gives the app the bus. `doctor` and `install` report these.
#[derive(Debug)]
pub struct Quiet(pub String);

impl std::fmt::Display for Quiet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Quiet {}

/// What the layer needs to know about the app.
pub struct Request<'a> {
    pub man: &'a panpak_core::Manifest,
    pub runtime_dir: &'a Path,
}

fn token() -> Result<String> {
    use std::io::Read;
    let mut b = [0u8; 4];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// The user unit of this process, the way `sd_pid_get_user_unit` finds it (the one the portal asks for): from the
/// cgroup in `/proc/self/cgroup`, the first component after `user@<uid>.service` that is not a slice.
fn current_unit() -> Option<String> {
    let s = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let line = s.lines().find(|l| l.starts_with("0::"))?;
    panpak_core::unitname::user_unit_of_cgroup(line.trim_start_matches("0::")).map(str::to_string)
}

pub use panpak_core::unitname::unit_gives_id;

/// Move this process into a new scope named for the app. The scope is asked of the user's systemd,
/// which is on `$XDG_RUNTIME_DIR/bus` (the session bus the proxy forwards to is another thing in a
/// session that is not the user manager's).
fn enter_scope(runtime_dir: &Path, app_id: &str, name: &str) -> Result<String> {
    let unit = dbus::scope_name(app_id, name)?;
    let bus = runtime_dir.join("bus");
    // No bus socket, or a bus without systemd on it: this system has no user manager. Not an error to shout about.
    if !bus.exists() {
        return Err(Quiet("no user bus: this system has no systemd user manager".into()).into());
    }
    let mut c = Connection::open(&bus, Duration::from_secs(10)).context("cannot reach the user's systemd bus")?;
    if let Err(e) = dbus::start_scope(&mut c, &unit, &format!("Panpak: {app_id}"), std::process::id(), Some("app.slice")) {
        let no_systemd = e.downcast_ref::<dbus::DbusError>().is_some_and(|d| matches!(d.name.as_str(), "org.freedesktop.DBus.Error.ServiceUnknown" | "org.freedesktop.DBus.Error.NameHasNoOwner"));
        return Err(if no_systemd { Quiet("the bus has no systemd: this system has no systemd user manager".into()).into() } else { e.context("the user's systemd refused the scope") });
    }
    ensure!(current_unit().as_deref() == Some(unit.as_str()), "the process is not in {unit} after the scope was made");
    Ok(unit)
}

fn pipe() -> Result<(OwnedFd, OwnedFd)> {
    let mut p = [0; 2];
    // SAFETY: pipe2 fills two descriptors.
    ensure!(unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC) } == 0, "pipe: {}", std::io::Error::last_os_error());
    // SAFETY: both are new descriptors that nothing else owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(p[0]), OwnedFd::from_raw_fd(p[1])) })
}

/// Wait up to `ms` for one byte on `fd`.
fn wait_ready(fd: RawFd, ms: i32) -> bool {
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd.
    let n = unsafe { libc::poll(&mut p, 1, ms) };
    if n <= 0 {
        return false;
    }
    let mut b = [0u8; 1];
    // SAFETY: reads one byte into a buffer of one.
    unsafe { libc::read(fd, b.as_mut_ptr().cast(), 1) == 1 }
}

/// Start the proxy; it is not waited for (see `await_proxy`).
fn spawn_proxy(proxy: &Path, address: &str, dir: &Path, socket: &str, args: &[String]) -> Result<Guard> {
    let (r, w) = pipe()?;
    let wfd = w.as_raw_fd();
    let mut cmd = Command::new(proxy);
    cmd.arg(format!("--fd={wfd}")).arg(address).arg(dir.join(socket)).args(args);
    cmd.env_clear().stdin(Stdio::null());
    // For diagnosis: `PANPAK_DBUS_LOG=/absolute/file` makes the proxy log what it forwards and what it filters.
    if let Some(log) = std::env::var_os("PANPAK_DBUS_LOG").map(PathBuf::from).filter(|p| p.is_absolute()) {
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&log) {
            if let Ok(g) = f.try_clone() {
                cmd.arg("--log").stdout(Stdio::from(g)).stderr(Stdio::from(f));
            }
        }
    }
    // SAFETY: only fcntl runs between fork and exec; it clears close-on-exec on the write end.
    unsafe {
        cmd.pre_exec(move || {
            let fl = libc::fcntl(wfd, libc::F_GETFD);
            if fl < 0 || libc::fcntl(wfd, libc::F_SETFD, fl & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().with_context(|| format!("cannot run {}", proxy.display()))?;
    drop(w);
    Ok(Guard { child, dir: dir.to_path_buf(), _sync: r, attachment: None, a11y: None, also: Vec::new(), remove_dir: true })
}

/// Wait until the proxy says it listens.
fn await_proxy(g: &mut Guard, socket: &str) -> Result<()> {
    if !wait_ready(g._sync.as_raw_fd(), 5000) {
        let st = g.child.try_wait().ok().flatten();
        bail!("the proxy did not become ready ({})", st.map_or("still running".to_string(), |s| format!("exited with {s}")));
    }
    ensure!(g.dir.join(socket).exists(), "the proxy made no socket");
    Ok(())
}

/// The address of the accessibility bus, from the session bus. `None` when there is no such bus (no assistive technology is installed).
fn a11y_address(bus: &Path) -> Option<String> {
    let mut c = Connection::open(bus, Duration::from_secs(2)).ok()?;
    let r = c.call("org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus", "GetAddress", Vec::new(), Vec::new()).ok()?;
    match r.body.first() {
        Some(dbus::Value::Str(a)) if a.starts_with("unix:") && a.len() < 512 && !a.contains(['\n', '\0']) => Some(a.clone()),
        _ => None,
    }
}

/// Build the layer. The checks and the folder are done here; the systemd scope and the proxy are made on a thread,
/// in that order: the proxy MUST start inside the scope, because the portal knows the app by the cgroup of the
/// connection it sees, which is the proxy's. The app is not kept waiting for any of it; the inner stub waits for the
/// socket (see `view::DbusWait`) before it starts the app, so the app never connects too early and the portal always sees
/// `panpak.<id>`. If the thread fails it leaves a file `failed` in the folder, the inner stub stops waiting and starts the
/// app without a bus, as when there is no layer.
pub fn start(req: &Request<'_>) -> Result<Layer> {
    let man = req.man;
    if panpak_core::allows_session_bus(man.permissions.as_ref()) {
        return Err(Quiet("the manifest lists the session bus socket: the app has the whole bus".into()).into());
    }
    // The cheap questions first: with no session bus there is nothing to ask the proxy.
    let Some(bus) = dbus::session_bus_path() else { return Err(Quiet("no session bus socket".into()).into()) };
    let status = dbus::proxy_status_cached(req.runtime_dir);
    let dbus::ProxyStatus::Usable { path: proxy, .. } = status else {
        return Err(if matches!(status, dbus::ProxyStatus::NotFound | dbus::ProxyStatus::TooOld { .. }) { Quiet(describe_status(&status)).into() } else { anyhow::anyhow!("{}", describe_status(&status)) });
    };
    // No systemd user manager: no scope, so no layer.
    if !req.runtime_dir.join("bus").exists() {
        return Err(Quiet("no user bus: this system has no systemd user manager".into()).into());
    }
    let n = token()?;
    let app_dir = req.runtime_dir.join("panpak").join(&man.app.id);
    // `dir` is what the app sees (its sockets only); `proxy_dir` is the proxy's socket, which the app must not see.
    let dir = app_dir.join(format!("dbus-{n}"));
    let proxy_dir = app_dir.join(format!("proxy-{n}"));
    for d in [&dir, &proxy_dir] {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(d).with_context(|| format!("cannot create {}", d.display()))?;
    }
    let args = dbus::proxy_args(&BusNeeds { app_id: &man.app.id, app_name: &man.app.name, entry: &man.app.entry, perms: man.permissions.as_ref() });
    let rc = dbus::router_config(&man.app.id, man.permissions.as_ref());
    let spec = crate::busrouter::Spec { app_dir: app_dir.clone(), portal_id: rc.portal_id, mpris_alias: rc.mpris_alias, open_uri: rc.open_uri, debug: panpak_core::debug_enabled(), end_pause_ms: std::env::var("PANPAK_ROUTER_END_PAUSE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0) };
    crate::stage("dbus: checks done");
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let (scope_tx, scope_rx) = std::sync::mpsc::channel::<bool>();
    let (runtime_dir, app_id, work_dir, pdir) = (req.runtime_dir.to_path_buf(), man.app.id.clone(), dir.clone(), proxy_dir.clone());
    // When the desktop started the app in a scope that already gives it its identity, the stub stays in that scope: no
    // scope is made, the proxy and the app are in it.
    let own_scope = current_unit().is_some_and(|u| unit_gives_id(&u, &app_id));
    let worker = std::thread::Builder::new().name("dbus".into()).spawn(move || {
        let scope = if own_scope { Ok(()) } else { enter_scope(&runtime_dir, &app_id, &n).map(|_| ()) };
        let _ = scope_tx.send(scope.is_ok());
        crate::stage(if own_scope { "dbus: already in the scope of the app" } else { "dbus: scope made" });
        let made = scope.and_then(|()| {
            // Both proxies start at once (neither is waited for yet), then the router: its start overlaps with theirs.
            let mut guard = spawn_proxy(&proxy, &format!("unix:path={}", bus.display()), &pdir, "real", &args)?;
            guard.also.push(work_dir.clone());
            let mut a11y_ok = false;
            if let Some(addr) = a11y_address(&bus) {
                match spawn_proxy(&proxy, &addr, &work_dir, "a11y", &dbus::a11y_proxy_args()) {
                    Ok(mut g2) => {
                        g2.remove_dir = false;
                        guard.a11y = Some(Box::new(g2));
                        a11y_ok = true;
                    }
                    Err(e) => eprintln!("panpak: WARNING: no accessibility bus for the app: {}", panpak_core::err_text(&e)),
                }
            }
            if !a11y_ok {
                let _ = std::fs::write(work_dir.join(A11Y_FAILED), "");
            }
            // The socket the app will connect to: made under another name, and put in place when everything behind it is ready.
            let pending = work_dir.join(".bus");
            let _ = std::fs::remove_file(&pending);
            let listener = UnixListener::bind(&pending).with_context(|| format!("cannot make {}", pending.display()))?;
            std::fs::set_permissions(&pending, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
            // The opener of this copy for OpenURI: the actor (this process) is on one end of a socket pair, the router on the other.
            let opener = crate::opener::system_target().and_then(|t| {
                let (mine, theirs) = std::os::unix::net::UnixStream::pair().ok()?;
                crate::opener::serve_actor(mine, t).ok()?;
                Some(theirs)
            });
            guard.attachment = Some(crate::busrouter::attach(&spec, listener, pdir.join("real"), opener)?);
            await_proxy(&mut guard, "real")?;
            if let Some(g2) = guard.a11y.as_mut() {
                if let Err(e) = await_proxy(g2, "a11y") {
                    eprintln!("panpak: WARNING: no accessibility bus for the app: {}", panpak_core::err_text(&e));
                    guard.a11y = None;
                    let _ = std::fs::write(work_dir.join(A11Y_FAILED), "");
                }
            }
            std::fs::rename(&pending, work_dir.join("bus")).context("cannot put the bus socket in place")?;
            crate::stage("dbus: router attached, proxy ready");
            Ok(guard)
        });
        match made {
            // Held until the app is over (the sender is dropped), then the router lets go, the proxies end and the folders go.
            Ok(guard) => {
                let _ = stopped.recv();
                drop(guard);
            }
            Err(e) => {
                if e.downcast_ref::<Quiet>().is_none() {
                    eprintln!("panpak: WARNING: no D-Bus for the app (portals, notifications): {}", panpak_core::err_text(&e));
                }
                let _ = std::fs::write(work_dir.join(FAILED), panpak_core::err_text(&e));
                let _ = stopped.recv();
                let _ = std::fs::remove_dir_all(&work_dir);
                let _ = std::fs::remove_dir_all(&pdir);
            }
        }
    })?;
    let in_view = req.runtime_dir.join("panpak/dbus");
    let view_bus = req.runtime_dir.join("bus");
    let mut mounts = vec![Mount::ro(dir.clone(), in_view.clone())];
    let landlock_sockets = vec![in_view.join("bus"), in_view.join("a11y")];
    // The app's folder of the document portal (empty until the stub grants a document).
    let by_app = req.runtime_dir.join("doc/by-app");
    if by_app.is_dir() {
        mounts.push(Mount { src: by_app.join(format!("panpak.{}", man.app.id)), dest: req.runtime_dir.join("doc"), rw: true });
    }
    let launch_env = |k: &str| man.launch.as_ref().and_then(|l| l.env.as_ref()).is_some_and(|e| e.contains_key(k));
    // AT_SPI_BUS_ADDRESS points the toolkits at the filtered accessibility bus (as Flatpak does), so that they do not ask the session bus; the inner stub takes it away
    // again if that bus could not be made.
    let mut env = vec![
        ("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}", view_bus.display())),
        ("GTK_USE_PORTAL", "1".to_string()),
        ("AT_SPI_BUS_ADDRESS", format!("unix:path={}", in_view.join("a11y").display())),
    ];
    let set_back = vec![("DBUS_SESSION_BUS_ADDRESS".to_string(), "disabled:".to_string())];
    let mut remove_back = vec!["GTK_USE_PORTAL".to_string()];
    if !launch_env("QT_QPA_PLATFORMTHEME") {
        env.push(("QT_QPA_PLATFORMTHEME", "xdgdesktopportal".to_string()));
        remove_back.push("QT_QPA_PLATFORMTHEME".to_string());
    }
    Ok(Layer {
        mounts,
        symlinks: vec![(PathBuf::from("panpak/dbus/bus"), view_bus.clone())],
        env,
        landlock_sockets,
        wait: crate::view::DbusWait {
            socket: in_view.join("bus"),
            failed: in_view.join(FAILED),
            link: view_bus.clone(),
            on_failure_set: set_back,
            on_failure_remove: remove_back,
            a11y: Some(crate::view::A11yWait { socket: in_view.join("a11y"), failed: in_view.join(A11Y_FAILED), env: "AT_SPI_BUS_ADDRESS".into() }),
        },
        scope: Some(scope_rx),
        stop: Some(stop),
        worker: Some(worker),
    })
}

/// The file the worker leaves in the proxy folder when it could not make the scope or start the proxy.
const FAILED: &str = "failed";
/// The file the worker leaves when there is no accessibility bus or its proxy could not be made.
const A11Y_FAILED: &str = "a11y-failed";

pub fn describe_status(s: &dbus::ProxyStatus) -> String {
    use dbus::ProxyStatus::*;
    match s {
        Usable { path, version } => format!("xdg-dbus-proxy {}.{}.{} at {}", version.0, version.1, version.2, path.display()),
        NotFound => format!("xdg-dbus-proxy not found (looked in {})", dbus::PROXY_PATHS.join(", ")),
        TooOld { path, version } => format!("{} is version {}.{}.{}, {}.{}.{} or newer is needed", path.display(), version.0, version.1, version.2, dbus::MIN_PROXY.0, dbus::MIN_PROXY.1, dbus::MIN_PROXY.2),
        Broken { path, why } => format!("{} does not work: {why}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn man(extra: &str) -> panpak_core::Manifest {
        toml::from_str(&format!("[app]\nid = \"org.example.app\"\nname = \"App\"\nversion = \"1\"\nentry = \"bin/app\"\n{extra}")).unwrap()
    }

    #[test]
    fn a_manifest_that_lists_the_session_bus_gets_no_layer_and_no_warning() {
        let m = man("[permissions]\nsockets_allow = [\"$XDG_RUNTIME_DIR/bus\"]\n");
        let e = start(&Request { man: &m, runtime_dir: Path::new("/nonexistent") }).err().expect("no layer");
        assert!(e.downcast_ref::<Quiet>().is_some(), "{e:#}");
    }

    #[test]
    fn the_unit_is_the_last_part_of_the_cgroup() {
        // This test process is in some cgroup; the function returns its last component or nothing, never panics.
        if let Some(u) = current_unit() {
            assert!(!u.contains('/') && !u.is_empty());
        }
    }

    #[test]
    fn tokens_are_eight_hex_digits_and_differ() {
        let (a, b) = (token().unwrap(), token().unwrap());
        assert!(a.len() == 8 && a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn status_texts_name_what_is_missing() {
        use dbus::ProxyStatus::*;
        assert!(describe_status(&NotFound).contains("/usr/bin/xdg-dbus-proxy"));
        assert!(describe_status(&TooOld { path: "/x".into(), version: (0, 1, 4) }).contains("0.1.4"));
    }
}
