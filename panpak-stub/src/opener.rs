// SPDX-License-Identifier: MIT OR Apache-2.0
//! Opening addresses from inside the isolated view. The app (or a library it uses) runs `xdg-open`,
//! which in the view is this stub; it sends the address over a Unix socket to the outer stub, which
//! checks it and passes it on to a `Target`. Today the target is the real `xdg-open` of the system;
//! it can be replaced by the OpenURI portal without touching the channel or the rules.
//! Rules: PANPAK-FORMAT.md §9.3, THREAT-MODEL.md §3.
use anyhow::{bail, ensure, Context, Result};
use panpak_core::{handle_open_request, OpenTarget as Target, RateLimit, MAX_REQUEST};
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

impl Target for SystemXdgOpen {
    fn open(&mut self, url: &str) -> Result<(), String> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.live.load(SeqCst) >= MAX_LIVE {
            return Err("too many openers running".into());
        }
        let mut cmd = Command::new(&self.program);
        // The address starts with http:, https: or mailto:, so it cannot be taken for an option.
        cmd.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
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
        std::thread::spawn(move || {
            let _ = child.wait();
            live.fetch_sub(1, SeqCst);
        });
        Ok(())
    }
}

/// The outer side: the socket and the thread that serves it. The socket file goes away with `drop`.
pub struct Server {
    path: PathBuf,
}

impl Server {
    /// Create `<dir>/open.<pid>.sock` (one socket per start, so that several copies of an app each have
    /// their own and none depends on another one staying alive) and serve it. Sockets of stubs that
    /// no longer exist are removed.
    pub fn start(dir: &Path, target: impl Target) -> Result<Self> {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("create {}", dir.display()))?;
        clean_stale(dir);
        let path = dir.join(format!("open.{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        let mut target = target;
        std::thread::Builder::new().name("opener".into()).spawn(move || {
            let mut limit = RateLimit::new(Instant::now());
            // One connection at a time: a client that is slow or silent holds the others up for at
            // most `IO_TIMEOUT`, and nothing else.
            for conn in listener.incoming().flatten() {
                serve(conn, &mut limit, &mut target);
            }
        })?;
        Ok(Self { path })
    }

    pub fn socket(&self) -> &Path {
        &self.path
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn serve(conn: UnixStream, limit: &mut RateLimit, target: &mut dyn Target) {
    let _ = conn.set_read_timeout(Some(IO_TIMEOUT));
    let _ = conn.set_write_timeout(Some(IO_TIMEOUT));
    // One line, read up to the longest request plus one byte: a longer one is not a request.
    let mut line = Vec::new();
    let mut r = BufReader::new((&conn).take(MAX_REQUEST as u64 + 1));
    let reply = match r.read_until(b'\n', &mut line) {
        Ok(n) if n > 0 => handle_open_request(&line, limit, Instant::now(), target),
        _ => "ERR bad request".into(),
    };
    let _ = (&conn).write_all(format!("{reply}\n").as_bytes());
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
        let server = Server::start(&dir.join("s"), Rec(sent.clone())).unwrap();
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
        assert!(started.elapsed() < Duration::from_millis(400), "open() waited for the process");
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
