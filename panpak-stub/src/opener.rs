// SPDX-License-Identifier: MIT OR Apache-2.0
//! Opening addresses from inside the isolated view. The app (or a library it uses) runs `xdg-open`,
//! which in the view is this stub; it sends the address over a Unix socket to the outer stub, which
//! checks it and passes it on to a `Target`. Today the target is the real `xdg-open` of the system;
//! it can be replaced by the OpenURI portal without touching the channel or the rules.
//! Rules: PANPAK-FORMAT.md §9.3, THREAT-MODEL.md §3.
use anyhow::{bail, ensure, Context, Result};
use crate::receiver;
use panpak_core::{frames, OpenTarget as Target, RateLimit};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::DirBuilderExt, net::{UnixListener, UnixStream}, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Names the view answers to. The stub looks at `argv[0]`.
pub const NAMES: &[&str] = &["xdg-open", "gnome-open", "kde-open", "kde-open5", "kde-open6"];

/// In the view, below the runtime folder: the folder of the links (first in `PATH`) and the socket.
pub const BIN_DIR: &str = "panpak/bin";
/// How long the actor waits to learn whether `xdg-open` worked.
const OPEN_WAIT: Duration = Duration::from_secs(4);
pub const SOCKET: &str = "panpak/open.sock";

/// Opener processes alive at the same time. A browser started by `xdg-open` may be that process.
const MAX_LIVE: usize = 4;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Is this program name one of the opener commands?
pub fn is_opener_name(argv0: &str) -> bool {
    Path::new(argv0).file_name().and_then(|n| n.to_str()).is_some_and(|n| NAMES.contains(&n))
}

/// The first `name` in `path` (a `PATH` value) that is a file someone can execute.
pub fn find_in_path(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(name))
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// The `xdg-open` of the system, found in the `PATH` of the outer stub at start and run with the
/// environment of the outer stub: nothing that comes from the app.
///
/// The process is detached from this one by construction: it has a session of its own (`setsid`, so
/// no signal or hangup of ours or of the app reaches it, and no terminal), every descriptor above 2 is
/// closed before it starts (the job descriptor of the view is not close-on-exec, for one), its input
/// and output are `/dev/null`, and nobody waits for it in the request: a thread reaps it, and
/// the stub exits without it. A browser started this way outlives the app and the stub.
pub struct SystemXdgOpen {
    program: PathBuf,
    live: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl SystemXdgOpen {
    pub fn new(program: PathBuf) -> Self {
        Self { program, live: Default::default() }
    }
}

/// Close every descriptor from 3 up. Runs between `fork` and `exec`, in a process that was copied from
/// one with other threads: only async-signal-safe calls, nothing that allocates or takes a lock (so no
/// listing of `/proc/self/fd`). First `close_range(3, ~0, CLOSE_RANGE_CLOEXEC)` (Linux 5.11): the
/// descriptors close at the `exec`, and the pipe on which the standard library reports a failed `exec`
/// stays usable. Then `close_range(3, ~0, 0)` (Linux 5.9), then a plain loop with no allocation.
fn close_from_3() {
    const CLOSE_RANGE_CLOEXEC: u32 = 4;
    // SAFETY: close_range and close are async-signal-safe; the descriptors are this process's own.
    unsafe {
        if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, CLOSE_RANGE_CLOEXEC) == 0 {
            return;
        }
        if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) != 0 {
            for fd in 3..1024 {
                libc::close(fd);
            }
        }
    }
}

/// The `XDG_DATA_DIRS` to give `xdg-open` when the stub's own is empty or missing, or None to leave the environment as it is. In order: the one of the
/// stub (the launch environment, which is the user's session when started from the menu); the one of the user's systemd manager (a start from a unit or a
/// terminal without it); the defaults of the specification with the folders where Flatpak and Snap export their applications. On Bluefin the only browser
/// is a Flatpak, and without those folders `xdg-open` ended with "Not Found" (defect 55).
fn data_dirs_for_open(own: Option<String>, home: Option<String>, manager: impl FnOnce() -> Option<String>) -> Option<String> {
    if own.as_deref().is_some_and(|v| !v.is_empty()) {
        return None;
    }
    if let Some(v) = manager().filter(|v| !v.is_empty()) {
        return Some(v);
    }
    let mut dirs = Vec::new();
    if let Some(h) = home.filter(|h| !h.is_empty()) {
        dirs.push(format!("{h}/.local/share/flatpak/exports/share"));
        dirs.push(format!("{h}/.local/share"));
    }
    dirs.extend(["/var/lib/flatpak/exports/share", "/var/lib/snapd/desktop", "/usr/local/share", "/usr/share"].map(String::from));
    Some(dirs.join(":"))
}

/// `XDG_DATA_DIRS` from `systemctl --user show-environment`, if the manager is there and sets it.
fn user_manager_data_dirs() -> Option<String> {
    let exe = ["/usr/bin/systemctl", "/bin/systemctl", "/run/current-system/sw/bin/systemctl"].into_iter().find(|p| Path::new(p).exists())?;
    let out = Command::new(exe).args(["--user", "show-environment"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix("XDG_DATA_DIRS=")).map(|v| v.trim_matches('\'').to_string())
}

impl Target for SystemXdgOpen {
    /// Start `xdg-open` and say what happened: its exit status if it ends within `OPEN_WAIT` (it normally hands the address to the browser and
    /// ends), "started" if it is still running then (a browser that was started by it is not waited for). An app and a user are not told "done" for
    /// a program that failed or that found nothing to open the address with.
    fn open(&mut self, url: &str) -> Result<(), String> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.live.load(SeqCst) >= MAX_LIVE {
            return Err("too many openers running".into());
        }
        let mut cmd = Command::new(&self.program);
        // The address starts with http:, https: or mailto:, so it cannot be taken for an option.
        cmd.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        if let Some(v) = data_dirs_for_open(std::env::var("XDG_DATA_DIRS").ok(), std::env::var("HOME").ok(), user_manager_data_dirs) {
            cmd.env("XDG_DATA_DIRS", v);
        }
        // SAFETY: the closure calls only setsid and close_range/close (async-signal-safe).
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                close_from_3();
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(|e| format!("run {}: {e}", self.program.display()))?;
        self.live.fetch_add(1, SeqCst);
        let live = self.live.clone();
        let end = Instant::now() + OPEN_WAIT;
        let outcome = loop {
            match child.try_wait() {
                Ok(Some(st)) => break Some(st),
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(15)),
                Ok(None) => break None,
                Err(e) => {
                    live.fetch_sub(1, SeqCst);
                    return Err(format!("cannot wait for xdg-open: {e}"));
                }
            }
        };
        match outcome {
            Some(st) => {
                live.fetch_sub(1, SeqCst);
                if st.success() {
                    return Ok(());
                }
                let mut text = String::new();
                if let Some(mut e) = child.stderr.take() {
                    use std::io::Read;
                    let _ = e.by_ref().take(400).read_to_string(&mut text);
                }
                let why = match st.code() {
                    Some(3) => "no browser or program is set to open this kind of address (xdg-open found nothing to use)".to_string(),
                    Some(4) => "the program that opens addresses failed".to_string(),
                    Some(c) => format!("xdg-open ended with status {c}"),
                    None => "xdg-open was stopped by a signal".to_string(),
                };
                let text = text.trim();
                Err(if text.is_empty() { why } else { format!("{why}: {}", text.lines().next().unwrap_or("")) })
            }
            None => {
                // Still running: a browser that xdg-open started and waits for. A thread reaps it.
                std::thread::spawn(move || {
                    let _ = child.wait();
                    live.fetch_sub(1, SeqCst);
                });
                Ok(())
            }
        }
    }
}

/// How the receiver is started. `Exec` is the real one; the tests use `Fork`, which runs the same code in a
/// child of the test process (the test binary cannot be started as the stub).
pub enum Launch {
    Exec(PathBuf),
    #[cfg(test)]
    Fork,
}

/// The outer side (the actor): the socket, the receiver process that reads it, and the thread that acts on what the
/// receiver passes on. The socket file goes away and the receiver ends with `drop`.
pub struct Server {
    path: PathBuf,
    receiver: Receiver,
}

/// The receiver process.
struct Receiver {
    pid: i32,
    child: Option<std::process::Child>,
}

impl Receiver {
    fn end(&mut self) {
        // SAFETY: SIGKILL to the process this value started, which has not been waited for yet.
        unsafe { libc::kill(self.pid, libc::SIGKILL) };
        match self.child.as_mut() {
            Some(c) => {
                let _ = c.wait();
            }
            None => {
                let mut st = 0;
                // SAFETY: waitpid on the child this process forked.
                unsafe { libc::waitpid(self.pid, &mut st, 0) };
            }
        }
    }
}

/// Close every descriptor from `first` up (at the `exec`). Async-signal-safe, like `close_from_3`.
fn close_from(first: u32) {
    const CLOSE_RANGE_CLOEXEC: u32 = 4;
    // SAFETY: close_range is async-signal-safe; the descriptors are this process's own.
    unsafe {
        if libc::syscall(libc::SYS_close_range, first, u32::MAX, CLOSE_RANGE_CLOEXEC) != 0 {
            for fd in first as i32..1024 {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
    }
}

/// In the child, before the exec: put the two sockets on the descriptors the receiver expects and close the rest.
/// Async-signal-safe calls only. The sockets are first moved out of the way, since either may already be 3 or 4.
fn place_fds(listener: i32, actor: i32) -> std::io::Result<()> {
    // SAFETY: fcntl and dup2 on this process's own descriptors.
    unsafe {
        let l = libc::fcntl(listener, libc::F_DUPFD, 10);
        let a = libc::fcntl(actor, libc::F_DUPFD, 10);
        if l < 0 || a < 0 || libc::dup2(l, receiver::LISTEN_FD) < 0 || libc::dup2(a, receiver::ACTOR_FD) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    close_from(5);
    Ok(())
}

impl Server {
    /// Create `<dir>/open.<pid>.sock` (one socket per start, so that several copies of an app each have
    /// their own and none depends on another one staying alive), start the receiver on it and act on what the
    /// receiver passes on. Sockets of stubs that no longer exist are removed.
    pub fn start(dir: &Path, target: impl Target) -> Result<Self> {
        Self::start_with(dir, target, Launch::Exec(std::env::current_exe().context("path of the stub")?))
    }

    pub fn start_with(dir: &Path, target: impl Target, launch: Launch) -> Result<Self> {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("create {}", dir.display()))?;
        clean_stale(dir);
        let path = dir.join(format!("open.{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        let (mine, theirs) = UnixStream::pair().context("socket pair")?;
        let receiver = match spawn_receiver(launch, &listener, &theirs) {
            Ok(r) => r,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return Err(e);
            }
        };
        // The receiver has its copies. With ours closed, a receiver that ends leaves nobody listening and a
        // client is refused at once instead of waiting.
        drop(listener);
        drop(theirs);
        let mut server = Server { path, receiver };
        let spawned = std::thread::Builder::new().name("opener".into()).spawn(move || act_loop(mine, Some(target), None));
        if let Err(e) = spawned {
            server.receiver.end();
            return Err(e).context("start the opener thread");
        }
        Ok(server)
    }

    pub fn socket(&self) -> &Path {
        &self.path
    }
}

fn spawn_receiver(launch: Launch, listener: &UnixListener, theirs: &UnixStream) -> Result<Receiver> {
    use std::os::fd::AsRawFd;
    let (l, a) = (listener.as_raw_fd(), theirs.as_raw_fd());
    match launch {
        Launch::Exec(exe) => {
            let mut cmd = Command::new(exe);
            cmd.arg(receiver::FLAG).stdin(Stdio::null()).stdout(Stdio::null());
            // SAFETY: the closure calls only fcntl, dup2 and close_range (async-signal-safe).
            unsafe { cmd.pre_exec(move || place_fds(l, a)) };
            let child = cmd.spawn().context("start the receiver")?;
            Ok(Receiver { pid: child.id() as i32, child: Some(child) })
        }
        #[cfg(test)]
        Launch::Fork => {
            // SAFETY: the test runs alone (`testutil::alone`); the child makes system calls and ends with `_exit`.
            unsafe {
                let pid = libc::fork();
                ensure!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
                if pid == 0 {
                    let code = match place_fds(l, a).map_err(anyhow::Error::from).and_then(|()| {
                        libc::fcntl(receiver::LISTEN_FD, libc::F_SETFD, 0);
                        libc::fcntl(receiver::ACTOR_FD, libc::F_SETFD, 0);
                        receiver::main_receiver()
                    }) {
                        Ok(()) => 0,
                        Err(e) => {
                            eprintln!("receiver: {e:#}");
                            1
                        }
                    };
                    libc::_exit(code);
                }
                Ok(Receiver { pid, child: None })
            }
        }
    }
}

/// Serve the frames of a router (it does what the receiver does, in the same process as the bus router): the actor on this end of a socket pair, with
/// the file dialog too: the opener (when this system has `xdg-open`) and the chooser (when it has the document portal) are both optional.
pub fn serve_actor_with(
    sock: UnixStream,
    target: Option<impl Target>,
    chooser: Option<crate::chooser::Actor>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new().name("opener-router".into()).spawn(move || act_loop(sock, target, chooser))
}

/// The program that opens addresses, found in the `PATH` of this process.
pub fn system_target() -> Option<SystemXdgOpen> {
    let path = std::env::var_os("PATH")?;
    find_in_path("xdg-open", &path).map(SystemXdgOpen::new)
}

/// A target for an actor that has no `xdg-open`: every address fails with a message.
struct NoOpener;

impl Target for NoOpener {
    fn open(&mut self, _url: &str) -> Result<(), String> {
        Err("this system has no xdg-open".into())
    }
}

/// The actor: read frames from the receiver, check each one again, act on it, answer. Ends when the receiver closes
/// its side or sends something that is not a frame (the receiver is then not trusted any more). A file dialog request is
/// handled on a thread of its own, because the dialog takes as long as the user takes; its answer carries the token of the request.
fn act_loop(mut sock: UnixStream, target: Option<impl Target>, chooser: Option<crate::chooser::Actor>) {
    let mut target: Box<dyn Target> = match target {
        Some(t) => Box::new(t),
        None => Box::new(NoOpener),
    };
    let writer = match sock.try_clone() {
        Ok(w) => std::sync::Arc::new(std::sync::Mutex::new(w)),
        Err(_) => return,
    };
    let mut limit = RateLimit::new(Instant::now());
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        loop {
            match frames::parse_request(&pending) {
                Ok(Some((req, used))) => {
                    pending.drain(..used);
                    // A dialog request has the token of the router in front; one without it is not one (the answer is the old "not done").
                    if req.kind == frames::KIND_FILE_CHOOSER && panpak_core::filechooser::parse_request_payload(&req.payload).is_some() {
                        match &chooser {
                            Some(c) => c.request(&req.payload, writer.clone()),
                            None => crate::chooser::answer_ended(&writer, &req.payload, "this system has no file dialog for the app"),
                        }
                        continue;
                    }
                    let resp = frames::act(&req, &mut limit, Instant::now(), &mut *target);
                    let ok = writer.lock().map(|mut w| w.write_all(&frames::encode_response(resp.status, &resp.text)).is_ok()).unwrap_or(false);
                    if !ok {
                        return;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("panpak: the receiver sent a bad frame ({e}); the open channel is closed");
                    return;
                }
            }
        }
        match sock.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => pending.extend_from_slice(&chunk[..n]),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        self.receiver.end();
    }
}

/// Remove `open.<pid>.sock` files whose process is gone.
fn clean_stale(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.strip_prefix("open.")).and_then(|n| n.strip_suffix(".sock")).and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        if !Path::new(&format!("/proc/{pid}")).exists() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The inner side: `xdg-open <address>` in the view. Returns the exit status.
pub fn client(args: &[String], runtime_dir: Option<&Path>) -> i32 {
    let run = || -> Result<()> {
        let [url] = args else { bail!("usage: xdg-open <address> (one address: http, https or mailto)") };
        panpak_core::check_url(url)?;
        let dir = runtime_dir.context("no runtime folder")?;
        let mut s = UnixStream::connect(dir.join(SOCKET)).context("no opener channel (the app has no way to open addresses)")?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        s.set_write_timeout(Some(IO_TIMEOUT))?;
        s.write_all(format!("OPEN {url}\n").as_bytes())?;
        let mut reply = String::new();
        BufReader::new(s.take(256)).read_line(&mut reply)?;
        ensure!(reply.trim_end() == "OK", "{}", reply.trim_end().strip_prefix("ERR ").unwrap_or("no answer"));
        Ok(())
    };
    match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("xdg-open: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_search() {
        assert!(is_opener_name("xdg-open"));
        assert!(is_opener_name("/usr/bin/xdg-open"));
        assert!(is_opener_name("kde-open5"));
        assert!(!is_opener_name("panpak-stub"));
        assert!(!is_opener_name("xdg-open2"));
        let dir = std::env::temp_dir().join(format!("panpak-opener-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("xdg-open"), b"#!/bin/sh\n").unwrap();
        let path = std::env::join_paths([Path::new("relative"), dir.as_path()]).unwrap();
        assert_eq!(find_in_path("xdg-open", &path), None, "not executable");
        std::fs::set_permissions(dir.join("xdg-open"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert_eq!(find_in_path("xdg-open", &path), Some(dir.join("xdg-open")));
        assert_eq!(find_in_path("nothing-here", &path), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_channel_end_to_end_and_a_flood() {
        // The receiver is a process (here a child of the test process): the test runs alone, in a process of its own.
        if !crate::testutil::alone() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("panpak-opener-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        struct Rec(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
        impl Target for Rec {
            fn open(&mut self, url: &str) -> Result<(), String> {
                self.0.lock().unwrap().push(url.to_string());
                Ok(())
            }
        }
        let server = Server::start_with(&dir.join("s"), Rec(sent.clone()), Launch::Fork).unwrap();
        let ask = |text: &[u8]| -> String {
            let mut s = UnixStream::connect(server.socket()).unwrap();
            let _ = s.write_all(text); // the server may answer and close before a huge request is sent
            let mut r = String::new();
            let _ = BufReader::new(s).read_line(&mut r);
            r.trim_end().to_string()
        };
        // Wait out the burst of the first requests in a predictable way: ask for the refusals first,
        // they do not use the target, and the limit counts every request.
        assert_eq!(ask(b"OPEN https://example.org/a\n"), "OK");
        assert!(ask(b"OPEN file:///etc/passwd\n").starts_with("ERR"));
        // No newline and a line much longer than an address are refused without waiting.
        let long = vec![b'a'; 100_000];
        assert!(ask(&long).starts_with("ERR"));
        // A flood: most requests are refused and the target sees few.
        let ok = (0..30).filter(|_| ask(b"OPEN https://example.org/b\n") == "OK").count();
        assert!(ok <= 4, "{ok} requests passed in a flood");
        assert!(sent.lock().unwrap().len() <= 5);
        let path = server.socket().to_path_buf();
        drop(server);
        assert!(!path.exists(), "the socket goes away with the server");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The actor against a receiver that has been taken over: it sends what it likes over the socket.
    #[test]
    fn the_actor_does_not_trust_the_receiver() {
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        struct Rec(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
        impl Target for Rec {
            fn open(&mut self, url: &str) -> Result<(), String> {
                self.0.lock().unwrap().push(url.to_string());
                Ok(())
            }
        }
        let ask = |bytes: &[u8]| -> Vec<frames::Response> {
            let (mine, theirs) = UnixStream::pair().unwrap();
            let t = sent.clone();
            let h = std::thread::spawn(move || act_loop(theirs, Some(Rec(t)), None));
            let mut mine = mine;
            mine.write_all(bytes).unwrap();
            mine.shutdown(std::net::Shutdown::Write).unwrap();
            let mut all = Vec::new();
            mine.read_to_end(&mut all).unwrap();
            h.join().unwrap();
            let mut out = Vec::new();
            let mut rest = all.as_slice();
            while let Ok(Some((r, used))) = frames::parse_response(rest) {
                out.push(r);
                rest = &rest[used..];
            }
            out
        };
        let req = |kind, payload: &[u8]| frames::encode_request(kind, payload).unwrap();
        // A good one passes; one that the receiver should have refused is refused again; other kinds are not done.
        let r = ask(&[req(1, b"https://example.org/x"), req(1, b"file:///etc/passwd"), req(1, b"\xff\xfe"), req(2, b""), req(3, b""), req(77, b"x")].concat());
        let st: Vec<_> = r.iter().map(|r| r.status).collect();
        use frames::Status::*;
        assert_eq!(st, [Done, Refused, Refused, Unsupported, Unsupported, Unsupported]);
        assert_eq!(*sent.lock().unwrap(), vec!["https://example.org/x".to_string()]);
        // Garbage ends the conversation without an answer; so does a frame cut short; a frame after garbage is not read.
        assert!(ask(b"hello hello hello").is_empty());
        assert!(ask(&req(1, b"https://example.org/")[..12]).is_empty());
        assert!(ask(&[b"PKRQ\x09\x01\x00\x00".to_vec(), req(1, b"https://example.org/after")].concat()).is_empty());
        assert!(ask(&[b"PKRQ\x01\x01\xff\xff".to_vec(), req(1, b"https://example.org/after")].concat()).is_empty(), "a length over the limit");
        // A flood is limited by the actor itself.
        let flood: Vec<u8> = (0..40).flat_map(|_| req(1, b"https://example.org/f")).collect();
        let done = ask(&flood).iter().filter(|r| r.status == Done).count();
        assert!(done <= 3, "{done}");
        assert!(!sent.lock().unwrap().iter().any(|u| u.contains("after") || u.contains("passwd")));
    }

    /// The receiver is confined: it runs, serves requests, and shows the marks of its confinement.
    #[test]
    fn the_receiver_is_confined() {
        if !crate::testutil::alone() {
            return;
        }
        struct Null;
        impl Target for Null {
            fn open(&mut self, _: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let dir = std::env::temp_dir().join(format!("panpak-receiver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let server = Server::start_with(&dir.join("s"), Null, Launch::Fork).unwrap();
        let pid = server.receiver.pid;
        // Ask once, so that the receiver is past `confine`.
        let mut s = UnixStream::connect(server.socket()).unwrap();
        s.write_all(b"OPEN https://example.org/\n").unwrap();
        let mut r = String::new();
        BufReader::new(s).read_line(&mut r).unwrap();
        assert_eq!(r.trim_end(), "OK", "the receiver does not serve");
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        let field = |k: &str| status.lines().find_map(|l| l.strip_prefix(k)).map(|v| v.trim().to_string()).unwrap_or_default();
        assert_eq!(field("NoNewPrivs:"), "1");
        assert_eq!(field("Seccomp:"), "2");
        // The limits.
        let limits = std::fs::read_to_string(format!("/proc/{pid}/limits")).unwrap();
        let line = |k: &str| limits.lines().find(|l| l.starts_with(k)).unwrap_or_default().to_string();
        assert!(line("Max open files").contains("32"), "{}", line("Max open files"));
        assert!(line("Max processes").split_whitespace().rev().nth(1) == Some("0"), "{}", line("Max processes"));
        // Nothing but the two sockets and the standard descriptors.
        let fds = std::fs::read_dir(format!("/proc/{pid}/fd")).map(|d| d.count()).unwrap_or(0);
        assert!(fds <= 8, "{fds} descriptors open in the receiver");
        drop(server);
        // The receiver is gone with the server.
        assert!(!Path::new(&format!("/proc/{pid}")).exists() || std::fs::read_to_string(format!("/proc/{pid}/status")).map(|s| s.contains("Z (zombie)")).unwrap_or(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Other threads allocate all the time while the opener forks: the child between `fork` and `exec` must
    /// not need the allocator or any lock, or it would wait for ever for a lock that no thread of the
    /// child can release. 500 launches, a watchdog that ends the process if one hangs.
    #[test]
    fn launches_do_not_hang_while_other_threads_allocate() {
        // 500 forks in this process would also copy the descriptors that other tests have open for writing at
        // that moment (a script being written and then run fails with ETXTBSY), so it runs alone.
        if !crate::testutil::alone() {
            return;
        }
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let done = std::sync::Arc::new(AtomicBool::new(false));
        let load: Vec<_> = (0..4)
            .map(|i| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut keep: Vec<String> = Vec::new();
                    while !stop.load(SeqCst) {
                        keep.push(format!("{i}-{}", keep.len()).repeat(8));
                        if keep.len() > 2000 {
                            keep.clear();
                        }
                    }
                })
            })
            .collect();
        let d = done.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(90));
            if !d.load(SeqCst) {
                eprintln!("a launch hung between fork and exec");
                std::process::exit(99);
            }
        });
        let mut t = SystemXdgOpen::new(PathBuf::from("/bin/true"));
        let mut launched = 0;
        while launched < 500 {
            match t.open("https://example.org/") {
                Ok(()) => launched += 1,
                Err(e) if e.contains("too many") => std::thread::sleep(Duration::from_millis(1)),
                Err(e) => panic!("{e}"),
            }
        }
        done.store(true, SeqCst);
        stop.store(true, SeqCst);
        for h in load {
            h.join().unwrap();
        }
    }

    /// The program the opener starts is a fake browser that stays alive. It records where it runs.
    fn fake_browser(dir: &Path) -> PathBuf {
        let prog = dir.join("xdg-open");
        std::fs::write(
            &prog,
            format!(
                "#!/bin/sh\nexec /bin/sh -c 'echo $$ > {d}/pid; echo \"$0\" > {d}/arg; ls /proc/self/fd > {d}/fds; awk \"{{print \\$6, \\$5}}\" /proc/self/stat > {d}/sid; exec sleep 120' \"$1\"\n",
                d = dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&prog, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        prog
    }

    #[test]
    fn data_dirs_for_xdg_open_come_from_the_launch_then_the_manager_then_the_defaults() {
        let home = Some("/h".to_string());
        // The launch environment has it: nothing is changed.
        assert_eq!(data_dirs_for_open(Some("/a:/b".into()), home.clone(), || panic!("not asked")), None);
        // Empty or missing: the user's manager, when it knows.
        assert_eq!(data_dirs_for_open(None, home.clone(), || Some("/m1:/m2".into())), Some("/m1:/m2".into()));
        assert_eq!(data_dirs_for_open(Some(String::new()), home.clone(), || Some("/m1".into())), Some("/m1".into()));
        // Nothing else: the defaults with Flatpak and Snap.
        let d = data_dirs_for_open(None, home, || None).unwrap();
        assert!(d.starts_with("/h/.local/share/flatpak/exports/share:/h/.local/share:/var/lib/flatpak/exports/share:/var/lib/snapd/desktop:"), "{d}");
        assert!(d.ends_with(":/usr/local/share:/usr/share"));
        assert!(!data_dirs_for_open(None, None, || None).unwrap().starts_with(':'));
    }

    #[test]
    fn the_process_is_detached_from_the_stub_and_stays_alive() {
        // A script written and then run fails with ETXTBSY if another thread forks while it is open for writing.
        if !crate::testutil::alone() {
            return;
        }
        use std::os::unix::io::AsRawFd;
        let dir = std::env::temp_dir().join(format!("panpak-opener-detach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A descriptor that is not close-on-exec, like the job descriptor of the view.
        let leaked = std::fs::File::open("/dev/null").unwrap();
        // SAFETY: fcntl on a descriptor owned by this test.
        unsafe { libc::fcntl(leaked.as_raw_fd(), libc::F_SETFD, 0) };
        let mut t = SystemXdgOpen::new(fake_browser(&dir));
        let started = Instant::now();
        t.open("https://example.org/stay").unwrap();
        assert!(started.elapsed() < OPEN_WAIT + Duration::from_secs(1), "open() waited for the process for too long");
        let pid_file = dir.join("pid");
        for _ in 0..100 {
            if dir.join("sid").exists() && std::fs::metadata(dir.join("sid")).map(|m| m.len() > 0).unwrap_or(false) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("arg")).unwrap().trim(), "https://example.org/stay");
        // Own session and process group, neither ours.
        let sid_pgrp = std::fs::read_to_string(dir.join("sid")).unwrap();
        let mut it = sid_pgrp.split_whitespace();
        let (sid, pgrp): (i32, i32) = (it.next().unwrap().parse().unwrap(), it.next().unwrap().parse().unwrap());
        // SAFETY: plain queries about this process.
        let (my_sid, my_pgrp) = unsafe { (libc::getsid(0), libc::getpgrp()) };
        assert_ne!(sid, my_sid, "the process is in the session of the stub");
        assert_ne!(pgrp, my_pgrp);
        assert_eq!(sid, pid, "the process leads a session of its own");
        // Only the standard descriptors (plus the one `ls` opens to list them).
        let fds: Vec<i32> = std::fs::read_to_string(dir.join("fds")).unwrap().split_whitespace().filter_map(|n| n.parse().ok()).collect();
        assert!(fds.iter().all(|&fd| fd <= 3), "inherited descriptors: {fds:?}");
        assert!(!fds.contains(&leaked.as_raw_fd()) || leaked.as_raw_fd() <= 3);
        // The stub's side is gone and the process is still there: it does not depend on us.
        drop(t);
        std::thread::sleep(Duration::from_millis(100));
        assert!(Path::new(&format!("/proc/{pid}")).exists(), "the fake browser died with the opener");
        // SAFETY: end the process this test started.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn at_most_four_processes_stay_alive_and_none_is_waited_for() {
        // A script written and then run fails with ETXTBSY if another thread forks while it is open for writing.
        if !crate::testutil::alone() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("panpak-opener-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let prog = dir.join("xdg-open");
        std::fs::write(&prog, "#!/bin/sh\necho $$ >> PIDS\nexec sleep 120\n".replace("PIDS", &dir.join("pids").display().to_string())).unwrap();
        std::fs::set_permissions(&prog, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let mut t = SystemXdgOpen::new(prog);
        let results: Vec<bool> = (0..6).map(|_| t.open("https://example.org/").is_ok()).collect();
        assert_eq!(results, [true, true, true, true, false, false]);
        std::thread::sleep(Duration::from_millis(300));
        for pid in std::fs::read_to_string(dir.join("pids")).unwrap().split_whitespace() {
            // SAFETY: end the processes this test started.
            unsafe { libc::kill(pid.parse().unwrap(), libc::SIGKILL) };
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_sockets_are_removed() {
        let dir = std::env::temp_dir().join(format!("panpak-opener-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("open.4000000000.sock"), b"").unwrap();
        std::fs::write(dir.join(format!("open.{}.sock", std::process::id())), b"").unwrap();
        std::fs::write(dir.join("other"), b"").unwrap();
        clean_stale(&dir);
        assert!(!dir.join("open.4000000000.sock").exists());
        assert!(dir.join(format!("open.{}.sock", std::process::id())).exists());
        assert!(dir.join("other").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_client_refuses_before_connecting() {
        let none = Some(Path::new("/nonexistent"));
        assert_eq!(client(&[], none), 1);
        assert_eq!(client(&["a".into(), "b".into()], none), 1);
        assert_eq!(client(&["file:///etc/passwd".into()], none), 1);
        assert_eq!(client(&["https://example.org/".into()], none), 1, "no channel");
    }
}
