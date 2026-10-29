// SPDX-License-Identifier: MIT OR Apache-2.0
//! Opening addresses from inside the isolated view. The app (or a library it uses) runs `xdg-open`,
//! which in the view is this stub; it sends the address over a Unix socket to the outer stub, which
//! checks it and passes it on to a `Target`. Today the target is the real `xdg-open` of the system;
//! it can be replaced by the OpenURI portal without touching the channel or the rules.
//! Rules: PANPAK-FORMAT.md §9.3, THREAT-MODEL.md §3.
use anyhow::{bail, ensure, Context, Result};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::DirBuilderExt, net::{UnixListener, UnixStream}, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// Names the view answers to. The stub looks at `argv[0]`.
pub const NAMES: &[&str] = &["xdg-open", "gnome-open", "kde-open", "kde-open5", "kde-open6"];

/// In the view, below the runtime folder: the folder of the links (first in `PATH`) and the socket.
pub const BIN_DIR: &str = "panpak/bin";
pub const SOCKET: &str = "panpak/open.sock";

/// Requests per second and the burst; the rest is refused.
const RATE_PER_SEC: f64 = 5.0;
const BURST: f64 = 3.0;
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

/// Where addresses go after the checks.
pub trait Target: Send + 'static {
    fn open(&mut self, url: &str) -> Result<()>;
}

/// The `xdg-open` of the system, found in the `PATH` of the outer stub at start and run with the
/// environment of the outer stub: nothing that comes from the app.
pub struct SystemXdgOpen {
    program: PathBuf,
    live: Vec<Child>,
}

impl SystemXdgOpen {
    pub fn new(program: PathBuf) -> Self {
        Self { program, live: Vec::new() }
    }
}

impl Target for SystemXdgOpen {
    fn open(&mut self, url: &str) -> Result<()> {
        self.live.retain_mut(|c| c.try_wait().map(|s| s.is_none()).unwrap_or(false));
        ensure!(self.live.len() < MAX_LIVE, "too many openers running");
        // The address starts with http:, https: or mailto:, so it cannot be taken for an option.
        let child = Command::new(&self.program)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .with_context(|| format!("run {}", self.program.display()))?;
        self.live.push(child);
        Ok(())
    }
}

/// At most `RATE_PER_SEC` per second, with a burst of `BURST`.
pub struct RateLimit {
    tokens: f64,
    last: Instant,
}

impl RateLimit {
    pub fn new(now: Instant) -> Self {
        Self { tokens: BURST, last: now }
    }
    pub fn allow(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * RATE_PER_SEC).min(BURST);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Answer one request line (`OPEN <address>`). The reply is `OK` or `ERR <reason>`.
pub fn handle(line: &str, limit: &mut RateLimit, now: Instant, target: &mut dyn Target) -> String {
    let Some(url) = line.strip_prefix("OPEN ") else { return "ERR bad request".into() };
    if !limit.allow(now) {
        return "ERR too many requests".into();
    }
    if let Err(e) = panpak_core::check_url(url) {
        return format!("ERR {}", e.to_string().replace(['\n', '\r'], " "));
    }
    match target.open(url) {
        Ok(()) => "OK".into(),
        Err(_) => "ERR cannot open".into(),
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
    // A line is read up to the limit of an address plus the verb; more is not an address.
    let max = panpak_core::MAX_URL + 8;
    let mut line = Vec::new();
    let mut r = BufReader::new((&conn).take(max as u64 + 1));
    let reply = match r.read_until(b'\n', &mut line) {
        Ok(n) if n > 0 && line.ends_with(b"\n") && line.len() <= max + 1 => {
            line.pop();
            match std::str::from_utf8(&line) {
                Ok(l) => handle(l, limit, Instant::now(), target),
                Err(_) => "ERR bad request".into(),
            }
        }
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

    struct Fake(Vec<String>);
    impl Target for Fake {
        fn open(&mut self, url: &str) -> Result<()> {
            self.0.push(url.to_string());
            Ok(())
        }
    }

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
    fn only_good_addresses_reach_the_target() {
        let t0 = Instant::now();
        let mut f = Fake(Vec::new());
        let mut l = RateLimit::new(t0);
        assert_eq!(handle("OPEN https://example.org/", &mut l, t0, &mut f), "OK");
        assert!(handle("OPEN file:///etc/passwd", &mut l, t0, &mut f).starts_with("ERR"));
        assert!(handle("OPEN mailto:a@b.org?attach=/etc/passwd", &mut l, t0, &mut f).starts_with("ERR"));
        assert!(handle("OPEN -x", &mut l, t0, &mut f).starts_with("ERR"));
        assert!(handle("hello", &mut l, t0, &mut f).starts_with("ERR"));
        assert!(handle("OPEN  https://example.org/", &mut l, t0, &mut f).starts_with("ERR"));
        assert_eq!(f.0, ["https://example.org/"]);
    }

    #[test]
    fn rate_is_five_per_second_with_a_burst_of_three() {
        let t0 = Instant::now();
        let mut l = RateLimit::new(t0);
        let ok: Vec<bool> = (0..5).map(|_| l.allow(t0)).collect();
        assert_eq!(ok, [true, true, true, false, false]);
        // 200 ms later one more request fits, not two.
        let t1 = t0 + Duration::from_millis(200);
        assert!(l.allow(t1));
        assert!(!l.allow(t1));
        // A long pause gives back the burst only, not more.
        let t2 = t1 + Duration::from_secs(60);
        let n = (0..10).filter(|_| l.allow(t2)).count();
        assert_eq!(n, 3);
        // Over one second of flooding, no more than burst + rate requests pass.
        let mut l = RateLimit::new(t0);
        let passed = (0..1000).filter(|i| l.allow(t0 + Duration::from_millis(*i))).count();
        assert!(passed <= 8, "{passed}");
    }

    #[test]
    fn the_channel_end_to_end_and_a_flood() {
        let dir = std::env::temp_dir().join(format!("panpak-opener-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        struct Rec(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
        impl Target for Rec {
            fn open(&mut self, url: &str) -> Result<()> {
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
