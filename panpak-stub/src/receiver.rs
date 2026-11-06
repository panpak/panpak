// SPDX-License-Identifier: MIT OR Apache-2.0
//! The receiver: the half of the resident stub that reads what the app sends. It is a process of its own (the
//! stub started again with `--panpak-receiver`), with no rights: no file, no network, no signal outside, no
//! new privileges, a short list of system calls, small limits. It owns the listening socket of the channel, reads a
//! request from the app, checks the shape and the address, and passes a valid one to the actor (the outer
//! stub) as a frame (`panpak_core::frames`). It never opens anything on the app's behalf.
//!
//! If the receiver is taken over by a request made to do so, what the attacker gets is a process that can
//! read and write two sockets it already had. The outer stub, which can run programs as the user, only reads
//! frames, checks them again and acts on the ones that pass.
use anyhow::{bail, ensure, Context, Result};
use panpak_core::frames::{self, Status};
use panpak_core::{handle_open_request, OpenTarget, RateLimit, MAX_REQUEST};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::fd::{AsRawFd, FromRawFd, RawFd},
    os::unix::net::{UnixListener, UnixStream},
    time::{Duration, Instant},
};

/// The argument that makes the stub the receiver.
pub const FLAG: &str = "--panpak-receiver";
/// The descriptors the receiver is started with: the listening socket and the socket to the actor.
pub const LISTEN_FD: RawFd = 3;
pub const ACTOR_FD: RawFd = 4;

const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// The actor starts a program and answers at once; this is only the limit for a stuck one.
const ACTOR_TIMEOUT: Duration = Duration::from_secs(5);

/// Limits of the process. A request is a line of 2 KiB and a frame of the same size; the memory limit is far
/// over what that needs and far under what a runaway loop would take.
const MAX_FILES: u64 = 32;
const MAX_ADDRESS_SPACE: u64 = 128 * 1024 * 1024;
const MAX_CPU_SECONDS: u64 = 600;

/// System calls the receiver uses after it is confined (accept, read, write, close, the clock, the
/// allocator, the end). Anything else ends the process. The list is the result of `strace -f` on a run that
/// serves requests of every kind, and the test `the_receiver_is_confined` runs the same flow again.
#[cfg(target_arch = "x86_64")]
const ALLOWED: &[i64] = &[
    libc::SYS_accept4,
    libc::SYS_poll,
    libc::SYS_read,
    libc::SYS_write,
    libc::SYS_recvfrom,
    libc::SYS_sendto,
    libc::SYS_close,
    libc::SYS_setsockopt,
    libc::SYS_clock_gettime,
    libc::SYS_mmap,
    libc::SYS_munmap,
    libc::SYS_mremap,
    libc::SYS_brk,
    libc::SYS_madvise,
    libc::SYS_rt_sigprocmask,
    libc::SYS_exit_group,
    libc::SYS_exit,
];
#[cfg(not(target_arch = "x86_64"))]
const ALLOWED: &[i64] = &[];

/// More calls for the builds of the tests: the standard library checks a descriptor with `fcntl` when it closes
/// it if debug assertions are on, and the glibc tests use more of the C library. The release build, which is the
/// only one that ships, has none of these.
#[cfg(all(target_arch = "x86_64", any(debug_assertions, not(target_env = "musl"))))]
const TEST_EXTRA: &[i64] = &[
    libc::SYS_fcntl,
    libc::SYS_ppoll,
    libc::SYS_recvmsg,
    libc::SYS_sendmsg,
    libc::SYS_futex,
    libc::SYS_getrandom,
    libc::SYS_rt_sigaction,
    libc::SYS_sigaltstack,
    libc::SYS_gettid,
    libc::SYS_getpid,
    libc::SYS_tgkill,
    libc::SYS_ioctl,
    libc::SYS_openat,
    libc::SYS_newfstatat,
    libc::SYS_lseek,
    libc::SYS_getsockopt,
];
#[cfg(not(all(target_arch = "x86_64", any(debug_assertions, not(target_env = "musl")))))]
const TEST_EXTRA: &[i64] = &[];

fn set_limit(resource: u32, value: u64) -> Result<()> {
    let l = libc::rlimit { rlim_cur: value as libc::rlim_t, rlim_max: value as libc::rlim_t };
    // SAFETY: setrlimit with a valid pointer.
    ensure!(unsafe { libc::setrlimit(resource as _, &l) } == 0, "setrlimit: {}", std::io::Error::last_os_error());
    Ok(())
}

/// Take every right away, then check that it is so, then limit the calls. Called once, before the first request.
/// Landlock is required: a receiver that cannot confine itself does not run (the channel is off).
#[allow(clippy::unnecessary_cast)] // the RLIMIT_* constants are c_uint with glibc and c_int with musl
pub fn confine() -> Result<()> {
    // No core file, no ptrace from the same user, and nothing reads this process through /proc.
    // SAFETY: prctl with integer arguments.
    ensure!(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } == 0, "dumpable: {}", std::io::Error::last_os_error());
    // SAFETY: as above; the receiver ends with its parent.
    ensure!(unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong, 0, 0, 0) } == 0, "pdeathsig: {}", std::io::Error::last_os_error());
    set_limit(libc::RLIMIT_NOFILE as u32, MAX_FILES)?;
    set_limit(libc::RLIMIT_AS as u32, MAX_ADDRESS_SPACE)?;
    set_limit(libc::RLIMIT_CPU as u32, MAX_CPU_SECONDS)?;
    set_limit(libc::RLIMIT_CORE as u32, 0)?;
    // The process count is per user; a limit of 0 only means that this process makes no more.
    set_limit(libc::RLIMIT_NPROC as u32, 0)?;
    use landlock::{Access, AccessFs, AccessNet, Ruleset, RulesetAttr, Scope};
    let abi = panpak_core::detect_abi().context("Landlock unsupported by kernel")?;
    let mut b = Ruleset::default().handle_access(AccessFs::from_all(abi))?;
    if abi >= landlock::ABI::V4 {
        b = b.handle_access(AccessNet::from_all(abi))?;
    }
    if abi >= landlock::ABI::V6 {
        b = b.scope(Scope::from_all(abi))?;
    }
    // No rule is added: no path is reachable, no port.
    let st = b.create()?.restrict_self()?;
    ensure!(st.ruleset == landlock::RulesetStatus::FullyEnforced, "partial Landlock ({:?}): the receiver does not run", st.ruleset);
    // Fail closed: what must not work does not work.
    ensure!(std::fs::File::open("/etc/passwd").is_err(), "receiver self-test: a file could be opened");
    ensure!(std::fs::read_dir("/").is_err(), "receiver self-test: a folder could be listed");
    if abi >= landlock::ABI::V4 {
        ensure!(std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], 9)), Duration::from_millis(200)).is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied), "receiver self-test: a TCP connection was not refused");
    }
    let allowed: Vec<i64> = ALLOWED.iter().chain(TEST_EXTRA).copied().collect();
    panpak_core::seccomp::install_allowlist(&allowed)?;
    Ok(())
}

/// Passes an address to the actor and waits for the answer.
struct Forward {
    actor: UnixStream,
    pending: Vec<u8>,
}

impl OpenTarget for Forward {
    fn open(&mut self, url: &str) -> Result<(), String> {
        let frame = frames::encode_request(frames::KIND_OPEN_URI, url.as_bytes()).map_err(|e| e.to_string())?;
        self.actor.write_all(&frame).map_err(|e| format!("the actor does not answer: {e}"))?;
        let mut chunk = [0u8; 512];
        loop {
            match frames::parse_response(&self.pending) {
                Ok(Some((r, used))) => {
                    self.pending.drain(..used);
                    return if r.status == Status::Done { Ok(()) } else { Err(r.text) };
                }
                Ok(None) => {}
                Err(e) => return Err(format!("bad answer from the actor: {e}")),
            }
            let n = self.actor.read(&mut chunk).map_err(|e| format!("the actor does not answer: {e}"))?;
            if n == 0 {
                return Err("the actor ended".into());
            }
            self.pending.extend_from_slice(&chunk[..n]);
        }
    }
}

/// One connection of the app: a line, an answer.
fn serve(conn: UnixStream, limit: &mut RateLimit, target: &mut dyn OpenTarget) {
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

/// Serve the listener until the actor goes away or the listener fails. One connection at a time: a client that is
/// slow or silent holds the others up for at most `IO_TIMEOUT`.
pub fn serve_loop(listener: UnixListener, actor: UnixStream) -> Result<()> {
    actor.set_read_timeout(Some(ACTOR_TIMEOUT)).context("actor timeout")?;
    actor.set_write_timeout(Some(ACTOR_TIMEOUT)).context("actor timeout")?;
    let mut limit = RateLimit::new(Instant::now());
    let mut target = Forward { actor, pending: Vec::new() };
    loop {
        // Wait for a client or for the actor to end. The actor says nothing unless asked, so anything from its
        // side while idle (a hang-up, or data that nobody asked for) ends the receiver.
        let mut fds = [
            libc::pollfd { fd: listener.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: target.actor.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: two valid pollfd entries.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            bail!("poll: {}", std::io::Error::last_os_error());
        }
        if fds[1].revents != 0 {
            return Ok(());
        }
        if fds[0].revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            bail!("the listening socket failed");
        }
        if fds[0].revents & libc::POLLIN != 0 {
            match listener.accept() {
                Ok((c, _)) => serve(c, &mut limit, &mut target),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::WouldBlock) => {}
                Err(e) => bail!("accept: {e}"),
            }
        }
    }
}

/// The entry point of the process: take the two descriptors, confine, serve. The exit status is 0 only when the
/// actor closed its side, which is the normal end.
pub fn main_receiver() -> Result<()> {
    // The descriptors are exactly these two: the parent set them up before the exec.
    let (is_listen, is_sock) = (socket_flag(LISTEN_FD, libc::SO_ACCEPTCONN), socket_flag(ACTOR_FD, libc::SO_TYPE));
    ensure!(is_listen && is_sock, "the receiver needs a listening socket on descriptor {LISTEN_FD} and a socket on {ACTOR_FD}");
    // SAFETY: the parent passed these descriptors to this process, which owns them from here.
    let (listener, actor) = unsafe { (UnixListener::from_raw_fd(LISTEN_FD), UnixStream::from_raw_fd(ACTOR_FD)) };
    confine()?;
    serve_loop(listener, actor)
}

fn socket_flag(fd: RawFd, opt: libc::c_int) -> bool {
    let mut v: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: getsockopt into a c_int of the stated size.
    let r = unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, opt, (&mut v as *mut libc::c_int).cast(), &mut len) };
    r == 0 && v != 0
}
