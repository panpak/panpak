// SPDX-License-Identifier: MIT OR Apache-2.0
//! The router of the private bus of an app (step 13p): the process, and the way the stubs of the copies start it and attach to it.
//!
//! The logic is `panpak_core::dbus::Router`, a state machine; this file is the I/O around it. One router serves all the copies of an app:
//!
//! ```text
//! app --(dir/bus of the copy)--> router --(a connection per app connection, made by the stub of the copy)--> xdg-dbus-proxy of the copy --> host
//! ```
//!
//! The router is the stub started again with `--panpak-busrouter`. It has no rights: Landlock with no rule, a short list of system calls,
//! small limits (as the receiver of step 7). So it **never opens a path and never connects to one**: the stub of each copy makes the listening
//! socket of the app (`dir/bus`) and the connections to the copy's proxy, and hands them over as descriptors on a control connection. The control
//! connection is also how the router knows that a copy is gone (it ends) and the only way anyone can ask the router for anything.
//!
//! The first copy starts the router (under a lock) and the last one to go ends it (when no control connection is left, after a short wait).

use anyhow::{bail, ensure, Context, Result};
use panpak_core::dbus::{self, AuthStep, ClientId, Limits, Msg, Out, Router, RouterConfig, ServerAuth};
use panpak_core::frames::{self, Status};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The argument that makes the stub the router.
pub const FLAG: &str = "--panpak-busrouter";
/// The control listener, as the router is started with it.
const CONTROL_FD: RawFd = 3;
/// The lock file of the app's folder, opened again for the router (its own open file description): the router takes the lock to end, see `Process::may_end`.
const LOCK_FD: RawFd = 4;

const MAX_FILES: u64 = 512;
const MAX_ADDRESS_SPACE: u64 = 1 << 30;
/// Twelve hours of processor time: a runaway loop ends, a busy day of an app does not.
const MAX_CPU_SECONDS: u64 = 12 * 3600;
/// How long the router waits for the first copy, and for another after the last one is gone.
const FIRST_WAIT: Duration = Duration::from_secs(10);
const LAST_WAIT: Duration = Duration::from_millis(1500);
/// Bytes waiting to be written to one connection before it is closed (a peer that does not read).
const MAX_QUEUED: usize = 64 << 20;
const MAX_CONNS: usize = 128;

#[cfg(target_arch = "x86_64")]
const ALLOWED: &[i64] = &[
    libc::SYS_accept4,
    libc::SYS_flock,
    libc::SYS_poll,
    libc::SYS_read,
    libc::SYS_write,
    libc::SYS_recvfrom,
    libc::SYS_sendto,
    libc::SYS_recvmsg,
    libc::SYS_sendmsg,
    libc::SYS_close,
    libc::SYS_getsockopt,
    libc::SYS_setsockopt,
    libc::SYS_shutdown,
    libc::SYS_clock_gettime,
    libc::SYS_getrandom,
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

#[cfg(all(target_arch = "x86_64", any(debug_assertions, not(target_env = "musl"))))]
const TEST_EXTRA: &[i64] = &[
    libc::SYS_fcntl,
    libc::SYS_ppoll,
    libc::SYS_futex,
    libc::SYS_rt_sigaction,
    libc::SYS_sigaltstack,
    libc::SYS_gettid,
    libc::SYS_getpid,
    libc::SYS_tgkill,
    libc::SYS_ioctl,
    libc::SYS_openat,
    libc::SYS_newfstatat,
    libc::SYS_lseek,
    libc::SYS_write,
];
#[cfg(not(all(target_arch = "x86_64", any(debug_assertions, not(target_env = "musl")))))]
const TEST_EXTRA: &[i64] = &[];

// ---- descriptors over a socket -------------------------------------------------------------------------------------

/// Send `bytes` with `fds` attached to the first of them. Returns how many bytes went.
fn send_with_fds(sock: RawFd, bytes: &[u8], fds: &[RawFd]) -> std::io::Result<usize> {
    let iov = libc::iovec { iov_base: bytes.as_ptr() as *mut _, iov_len: bytes.len() };
    let fd_bytes = std::mem::size_of_val(fds) as u32;
    // SAFETY: CMSG_SPACE only computes a size.
    let space = unsafe { libc::CMSG_SPACE(fd_bytes) } as usize;
    let mut ctl = vec![0u8; if fds.is_empty() { 0 } else { space }];
    // SAFETY: msghdr is plain data; the pointers stay valid for the call.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &iov as *const _ as *mut _;
    msg.msg_iovlen = 1;
    if !fds.is_empty() {
        msg.msg_control = ctl.as_mut_ptr().cast();
        msg.msg_controllen = ctl.len() as _;
        // SAFETY: the control buffer is CMSG_SPACE bytes, enough for one message of this size.
        unsafe {
            let cm = libc::CMSG_FIRSTHDR(&msg);
            (*cm).cmsg_level = libc::SOL_SOCKET;
            (*cm).cmsg_type = libc::SCM_RIGHTS;
            (*cm).cmsg_len = libc::CMSG_LEN(fd_bytes) as _;
            std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(cm).cast::<RawFd>(), fds.len());
        }
    }
    loop {
        // SAFETY: a valid descriptor and message header.
        let n = unsafe { libc::sendmsg(sock, &msg, libc::MSG_NOSIGNAL) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// Read what is there into `buf`, taking every descriptor that comes with it. `Ok(0)` is the end of the connection.
fn recv_with_fds(sock: RawFd, buf: &mut [u8], fds: &mut VecDeque<OwnedFd>, max_fds: usize) -> std::io::Result<usize> {
    let iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    // SAFETY: CMSG_SPACE only computes a size.
    let space = unsafe { libc::CMSG_SPACE((16 * std::mem::size_of::<RawFd>()) as u32) } as usize;
    let mut ctl = vec![0u8; space];
    // SAFETY: as in send_with_fds.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &iov as *const _ as *mut _;
    msg.msg_iovlen = 1;
    msg.msg_control = ctl.as_mut_ptr().cast();
    msg.msg_controllen = ctl.len() as _;
    let n = loop {
        // SAFETY: a valid descriptor and message header; MSG_CMSG_CLOEXEC marks the descriptors that arrive.
        let n = unsafe { libc::recvmsg(sock, &mut msg, libc::MSG_CMSG_CLOEXEC) };
        if n >= 0 {
            break n as usize;
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    };
    // Take every descriptor now, so that none leaks whatever happens next.
    // SAFETY: the kernel filled the control buffer; the walk follows the CMSG macros.
    unsafe {
        let mut cm = libc::CMSG_FIRSTHDR(&msg);
        while !cm.is_null() {
            if (*cm).cmsg_level == libc::SOL_SOCKET && (*cm).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(cm).cast::<RawFd>();
                let count = ((*cm).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<RawFd>();
                for i in 0..count {
                    fds.push_back(OwnedFd::from_raw_fd(std::ptr::read_unaligned(data.add(i))));
                }
            }
            cm = libc::CMSG_NXTHDR(&msg, cm);
        }
    }
    if msg.msg_flags & libc::MSG_CTRUNC != 0 || fds.len() > max_fds {
        return Err(std::io::Error::other("too many descriptors in flight"));
    }
    Ok(n)
}

fn peer_uid(fd: RawFd) -> Option<u32> {
    let mut c: libc::ucred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt into a ucred of the stated size.
    let r = unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut c as *mut libc::ucred).cast(), &mut len) };
    (r == 0).then_some(c.uid)
}

// ---- the router process ------------------------------------------------------------------------------------------------

struct OutItem {
    bytes: Vec<u8>,
    fds: Vec<OwnedFd>,
    sent: usize,
}

#[derive(Default)]
struct Side {
    inbuf: Vec<u8>,
    infds: VecDeque<OwnedFd>,
    out: VecDeque<OutItem>,
    queued: usize,
}

impl Side {
    fn push(&mut self, m: Msg<OwnedFd>) {
        self.queued += m.raw.len();
        self.out.push_back(OutItem { bytes: m.raw, fds: m.fds, sent: 0 });
    }

    /// Write as much as the socket takes. Returns false when the connection failed.
    fn flush(&mut self, fd: RawFd) -> bool {
        while let Some(item) = self.out.front_mut() {
            let rest = &item.bytes[item.sent..];
            let raw: Vec<RawFd> = if item.sent == 0 { item.fds.iter().map(AsRawFd::as_raw_fd).collect() } else { Vec::new() };
            match send_with_fds(fd, rest, &raw) {
                Ok(n) => {
                    item.sent += n;
                    self.queued = self.queued.saturating_sub(n);
                    item.fds.clear(); // they travelled with the first byte
                    if item.sent >= item.bytes.len() {
                        self.out.pop_front();
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return true,
                Err(_) => return false,
            }
        }
        true
    }
}

struct Conn {
    app: OwnedFd,
    up: Option<OwnedFd>,
    control: u64,
    auth: Option<ServerAuth>,
    a: Side,
    u: Side,
    /// The connection is to be closed when what is queued has been written (a refusal), or now.
    dead: bool,
}

struct Control {
    sock: OwnedFd,
    buf: Vec<u8>,
    fds: VecDeque<OwnedFd>,
    attached: bool,
    listener: Option<OwnedFd>,
    out: Vec<u8>,
    /// The opener of this copy: a socket to its actor (the outer stub, which checks again and runs `xdg-open`), the frames in and out, and who waits.
    actor: Option<OwnedFd>,
    actor_in: Vec<u8>,
    actor_out: Vec<u8>,
    pending: VecDeque<u64>,
}

struct Process {
    router: Router<OwnedFd>,
    controls: HashMap<u64, Control>,
    conns: HashMap<ClientId, Conn>,
    lim: Limits,
    next_control: u64,
    debug: bool,
    /// The user id, read before the system calls were limited.
    uid: u32,
    t0: Instant,
    end_pause_ms: i32,
}

impl Process {
    fn log(&self, s: &str) {
        if self.debug {
            eprintln!("panpak-busrouter: +{:.1} ms {s}", self.t0.elapsed().as_secs_f64() * 1000.0);
        }
    }

    fn apply(&mut self, outs: Vec<Out<OwnedFd>>) {
        for o in outs {
            match o {
                Out::App(id, m) => {
                    if let Some(c) = self.conns.get_mut(&id) {
                        c.a.push(m);
                    }
                }
                Out::Up(id, m) => {
                    if let Some(c) = self.conns.get_mut(&id) {
                        c.u.push(m);
                    }
                }
                Out::Open { client, token, uri } => {
                    let key = self.conns.get(&client).map(|c| c.control);
                    let sent = key.and_then(|k| self.controls.get_mut(&k)).filter(|c| c.actor.is_some()).and_then(|c| {
                        let frame = frames::encode_request(frames::KIND_OPEN_URI, uri.as_bytes()).ok()?;
                        c.actor_out.extend_from_slice(&frame);
                        c.pending.push_back(token);
                        Some(())
                    });
                    if sent.is_none() {
                        self.log(&format!("OpenURI {uri}: no opener"));
                        let more = self.router.open_done(token, Err("the runtime has no way to open addresses here".into()));
                        self.apply(more);
                    }
                }
                Out::Close(id, why) => {
                    self.log(&format!("closing connection {id}: {why}"));
                    self.drop_conn(id);
                }
                Out::Log(l) => self.log(&l),
            }
        }
    }

    fn drop_conn(&mut self, id: ClientId) {
        if self.conns.remove(&id).is_some() {
            let more = self.router.client_gone(id);
            self.apply(more);
        }
    }

    /// Complete messages in a buffer: (raw, fds).
    fn take_messages(side: &mut Side, lim: &Limits) -> std::result::Result<Vec<Msg<OwnedFd>>, String> {
        let mut out = Vec::new();
        loop {
            if side.inbuf.len() < 16 {
                return Ok(out);
            }
            let head: [u8; 16] = side.inbuf[..16].try_into().expect("16 bytes");
            let total = dbus::message_len(&head, lim).map_err(|e| format!("{e:#}"))?;
            if side.inbuf.len() < total {
                return Ok(out);
            }
            let raw: Vec<u8> = side.inbuf.drain(..total).collect();
            let h = dbus::parse_head(&raw, side.infds.len(), lim).map_err(|e| format!("{e:#}"))?;
            let n = h.msg.unix_fds as usize;
            let fds: Vec<OwnedFd> = side.infds.drain(..n).collect();
            out.push(Msg { raw, fds });
        }
    }

    fn read_app(&mut self, id: ClientId) {
        let mut buf = vec![0u8; 65536];
        let Some(c) = self.conns.get_mut(&id) else { return };
        match recv_with_fds(c.app.as_raw_fd(), &mut buf, &mut c.a.infds, 32) {
            Ok(0) => return self.drop_conn(id),
            Ok(n) => c.a.inbuf.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(_) => return self.drop_conn(id),
        }
        // The handshake first.
        if let Some(auth) = c.auth.as_mut() {
            let data = std::mem::take(&mut c.a.inbuf);
            match auth.feed(&data) {
                AuthStep::More(reply) => {
                    c.a.push(Msg { raw: reply, fds: Vec::new() });
                    return;
                }
                AuthStep::Done { reply, rest } => {
                    c.a.push(Msg { raw: reply, fds: Vec::new() });
                    c.a.inbuf = rest;
                    c.auth = None;
                }
                AuthStep::Refuse(why) => {
                    self.log(&format!("handshake refused: {why}"));
                    return self.drop_conn(id);
                }
            }
        }
        if c.up.is_none() {
            return; // the proxy connection is on its way; the bytes wait in the buffer
        }
        self.process_app(id);
    }

    fn process_app(&mut self, id: ClientId) {
        let lim = self.lim;
        let Some(c) = self.conns.get_mut(&id) else { return };
        if c.auth.is_some() || c.up.is_none() {
            return;
        }
        match Self::take_messages(&mut c.a, &lim) {
            Ok(msgs) => {
                for m in msgs {
                    let outs = self.router.from_app(id, m);
                    self.apply(outs);
                    if !self.conns.contains_key(&id) {
                        return;
                    }
                }
            }
            Err(e) => {
                self.log(&format!("bad data from the app: {e}"));
                self.drop_conn(id)
            }
        }
    }

    fn read_up(&mut self, id: ClientId) {
        let mut buf = vec![0u8; 65536];
        let lim = self.lim;
        let Some(c) = self.conns.get_mut(&id) else { return };
        let Some(up) = c.up.as_ref() else { return };
        match recv_with_fds(up.as_raw_fd(), &mut buf, &mut c.u.infds, 32) {
            Ok(0) => return self.drop_conn(id),
            Ok(n) => c.u.inbuf.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(_) => return self.drop_conn(id),
        }
        match Self::take_messages(&mut c.u, &lim) {
            Ok(msgs) => {
                for m in msgs {
                    let outs = self.router.from_up(id, m);
                    self.apply(outs);
                    if !self.conns.contains_key(&id) {
                        return;
                    }
                }
            }
            Err(e) => {
                self.log(&format!("bad data from the host side: {e}"));
                self.drop_conn(id)
            }
        }
    }

    fn accept_app(&mut self, control: u64) {
        let Some(ctl) = self.controls.get(&control) else { return };
        let Some(l) = ctl.listener.as_ref() else { return };
        // SAFETY: accept4 on a listening socket this process holds; the new descriptor is owned right away.
        let fd = unsafe { libc::accept4(l.as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC) };
        if fd < 0 {
            return;
        }
        // SAFETY: a new descriptor from accept4.
        let app = unsafe { OwnedFd::from_raw_fd(fd) };
        if peer_uid(fd) != Some(self.uid) || self.conns.len() >= MAX_CONNS {
            return;
        }
        let id = self.router.new_client();
        self.log(&format!("app connection {id} on copy {control}"));
        self.conns.insert(id, Conn { app, up: None, control, auth: Some(ServerAuth::new()), a: Side::default(), u: Side::default(), dead: false });
        // Ask the stub of the copy for the connection to its proxy.
        if let Some(ctl) = self.controls.get_mut(&control) {
            ctl.out.extend_from_slice(format!("UP {id}\n").as_bytes());
        }
    }

    fn read_control(&mut self, key: u64) {
        let mut buf = [0u8; 512];
        let Some(ctl) = self.controls.get_mut(&key) else { return };
        match recv_with_fds(ctl.sock.as_raw_fd(), &mut buf, &mut ctl.fds, 4) {
            Ok(0) => return self.drop_control(key),
            Ok(n) => ctl.buf.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(_) => return self.drop_control(key),
        }
        if ctl.buf.len() > 4096 {
            return self.drop_control(key);
        }
        loop {
            let Some(ctl) = self.controls.get_mut(&key) else { return };
            let Some(p) = ctl.buf.iter().position(|b| *b == b'\n') else { return };
            let line: Vec<u8> = ctl.buf.drain(..=p).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]).to_string();
            let mut w = line.split(' ');
            match (w.next(), w.next().and_then(|x| x.parse::<ClientId>().ok())) {
                (Some("ATTACH"), None) if !ctl.attached => match ctl.fds.pop_front() {
                    Some(l) => {
                        ctl.listener = Some(l);
                        ctl.actor = ctl.fds.pop_front();
                        ctl.attached = true;
                        if self.debug {
                            eprintln!("panpak-busrouter: +{:.1} ms copy {key} attached", self.t0.elapsed().as_secs_f64() * 1000.0);
                        }
                        ctl.out.extend_from_slice(b"OK\n");
                    }
                    None => return self.drop_control(key),
                },
                (Some("FD"), Some(id)) => {
                    let fd = ctl.fds.pop_front();
                    let ok = self.conns.get(&id).is_some_and(|c| c.control == key && c.up.is_none());
                    match (fd, ok) {
                        (Some(fd), true) => {
                            let c = self.conns.get_mut(&id).expect("checked");
                            c.up = Some(fd);
                            self.process_app(id);
                        }
                        // The app went away before its connection to the proxy arrived (the inner stub only tries the socket): the descriptor is closed.
                        (Some(_), false) => {}
                        (None, _) => return self.drop_control(key),
                    }
                }
                (Some("NOUP"), Some(id)) => {
                    if self.conns.get(&id).is_some_and(|c| c.control == key) {
                        self.drop_conn(id);
                    }
                }
                _ => return self.drop_control(key),
            }
        }
    }

    /// Answers from the actor of a copy (frames, in order), and the requests waiting to be written to it.
    fn actor_events(&mut self, key: u64, revents: i16) {
        let mut done: Vec<(u64, Result<(), String>)> = Vec::new();
        let mut broken = false;
        if let Some(c) = self.controls.get_mut(&key) {
            let fd = c.actor.as_ref().map(AsRawFd::as_raw_fd).unwrap_or(-1);
            if revents & libc::POLLOUT != 0 && !c.actor_out.is_empty() {
                match send_with_fds(fd, &c.actor_out.clone(), &[]) {
                    Ok(n) => {
                        c.actor_out.drain(..n);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => broken = true,
                }
            }
            if revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                let mut buf = [0u8; 1024];
                let mut none = VecDeque::new();
                match recv_with_fds(fd, &mut buf, &mut none, 0) {
                    Ok(0) => broken = true,
                    Ok(n) => c.actor_in.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => broken = true,
                }
                loop {
                    match frames::parse_response(&c.actor_in) {
                        Ok(Some((r, used))) => {
                            c.actor_in.drain(..used);
                            if let Some(t) = c.pending.pop_front() {
                                done.push((t, if r.status == Status::Done { Ok(()) } else { Err(r.text) }));
                            }
                        }
                        Ok(None) => break,
                        Err(_) => {
                            broken = true;
                            break;
                        }
                    }
                }
            }
            if broken {
                // The opener is gone or confused: everything waiting is answered, and there is no opener any more for this copy.
                while let Some(t) = c.pending.pop_front() {
                    done.push((t, Err("the opener of the runtime ended".into())));
                }
                c.actor = None;
            }
        }
        for (t, r) in done {
            let more = self.router.open_done(t, r);
            self.apply(more);
        }
    }

    /// A copy is gone (or misbehaved): its listener and every connection it brought end.
    fn drop_control(&mut self, key: u64) {
        if self.controls.remove(&key).is_none() {
            return;
        }
        let ids: Vec<ClientId> = self.conns.iter().filter(|(_, c)| c.control == key).map(|(i, _)| *i).collect();
        for id in ids {
            self.drop_conn(id);
        }
        self.log(&format!("copy {key} detached; {} left", self.controls.len()));
    }

    /// Decide to end without losing a copy that arrives at that moment. A copy takes the lock of the app's folder, connects to the control socket and only
    /// then lets go of the lock. So: take the lock without waiting (if a copy holds it, it is connecting now: do not end); with the lock held, look once
    /// more at the control socket (a connection that is waiting is a copy that has just left the lock: do not end). Otherwise end while holding the lock:
    /// the listener closes with the process, and a copy that waits for the lock finds nobody listening and starts a new router.
    fn may_end(&self, control_listener: &UnixListener) -> bool {
        // SAFETY: flock on the descriptor the parent passed; nothing else uses it.
        if unsafe { libc::flock(LOCK_FD, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            self.log("a copy is connecting: the router stays");
            return false;
        }
        let mut pfd = libc::pollfd { fd: control_listener.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: poll on one valid pollfd.
        let waiting = unsafe { libc::poll(&mut pfd, 1, 0) } > 0 && pfd.revents & libc::POLLIN != 0;
        if waiting {
            self.log("a copy has just connected: the router stays");
            // SAFETY: as above.
            unsafe { libc::flock(LOCK_FD, libc::LOCK_UN) };
            return false;
        }
        if self.end_pause_ms > 0 {
            // For tests: a long window between the decision and the end. Poll on nothing is a sleep the filter allows.
            // SAFETY: poll with no descriptors.
            unsafe { libc::poll(std::ptr::null_mut(), 0, self.end_pause_ms) };
        }
        true
    }

    fn run(&mut self, control_listener: UnixListener) -> Result<()> {
        let mut last_copy: Option<Instant> = Some(Instant::now());
        let mut ever = false;
        loop {
            // The descriptors to watch, and what each is.
            #[derive(Clone, Copy)]
            enum Kind {
                Listen,
                Control(u64),
                Accept(u64),
                Actor(u64),
                App(ClientId),
                Up(ClientId),
            }
            let mut fds: Vec<libc::pollfd> = vec![libc::pollfd { fd: control_listener.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
            let mut kinds: Vec<Kind> = vec![Kind::Listen];
            for (k, c) in &self.controls {
                let ev = libc::POLLIN | if c.out.is_empty() { 0 } else { libc::POLLOUT };
                fds.push(libc::pollfd { fd: c.sock.as_raw_fd(), events: ev, revents: 0 });
                kinds.push(Kind::Control(*k));
                if let Some(l) = &c.listener {
                    fds.push(libc::pollfd { fd: l.as_raw_fd(), events: libc::POLLIN, revents: 0 });
                    kinds.push(Kind::Accept(*k));
                }
                if let Some(a) = &c.actor {
                    let ev = libc::POLLIN | if c.actor_out.is_empty() { 0 } else { libc::POLLOUT };
                    fds.push(libc::pollfd { fd: a.as_raw_fd(), events: ev, revents: 0 });
                    kinds.push(Kind::Actor(*k));
                }
            }
            for (id, c) in &self.conns {
                let ev = libc::POLLIN | if c.a.out.is_empty() { 0 } else { libc::POLLOUT };
                fds.push(libc::pollfd { fd: c.app.as_raw_fd(), events: ev, revents: 0 });
                kinds.push(Kind::App(*id));
                if let Some(u) = &c.up {
                    let ev = libc::POLLIN | if c.u.out.is_empty() { 0 } else { libc::POLLOUT };
                    fds.push(libc::pollfd { fd: u.as_raw_fd(), events: ev, revents: 0 });
                    kinds.push(Kind::Up(*id));
                }
            }
            // SAFETY: a valid array of pollfd of the stated length.
            let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 250) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                bail!("poll: {}", std::io::Error::last_os_error());
            }
            for (i, k) in kinds.iter().enumerate() {
                let r = fds[i].revents;
                if r == 0 {
                    continue;
                }
                match *k {
                    Kind::Listen => {
                        // SAFETY: accept4 on the control listener; the new descriptor is owned right away.
                        let fd = unsafe { libc::accept4(control_listener.as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC) };
                        if fd >= 0 {
                            // SAFETY: a new descriptor from accept4.
                            let sock = unsafe { OwnedFd::from_raw_fd(fd) };
                            if peer_uid(fd) == Some(self.uid) {
                                let key = self.next_control;
                                self.next_control += 1;
                                self.controls.insert(key, Control { sock, buf: Vec::new(), fds: VecDeque::new(), attached: false, listener: None, out: Vec::new(), actor: None, actor_in: Vec::new(), actor_out: Vec::new(), pending: VecDeque::new() });
                                ever = true;
                                last_copy = None;
                            }
                        }
                    }
                    Kind::Control(key) => {
                        if r & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                            self.read_control(key);
                        }
                        if r & libc::POLLOUT != 0 {
                            if let Some(c) = self.controls.get_mut(&key) {
                                if let Ok(n) = send_with_fds(c.sock.as_raw_fd(), &c.out.clone(), &[]) {
                                    c.out.drain(..n);
                                }
                            }
                        }
                    }
                    Kind::Accept(key) => self.accept_app(key),
                    Kind::Actor(key) => self.actor_events(key, r),
                    Kind::App(id) => {
                        if r & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                            self.read_app(id);
                        }
                        if r & libc::POLLOUT != 0 {
                            if let Some(c) = self.conns.get_mut(&id) {
                                let fd = c.app.as_raw_fd();
                                if !c.a.flush(fd) || c.a.queued > MAX_QUEUED {
                                    self.drop_conn(id);
                                }
                            }
                        }
                    }
                    Kind::Up(id) => {
                        if r & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                            self.read_up(id);
                        }
                        if r & libc::POLLOUT != 0 {
                            if let Some(c) = self.conns.get_mut(&id) {
                                if let Some(fd) = c.up.as_ref().map(AsRawFd::as_raw_fd) {
                                    if !c.u.flush(fd) || c.u.queued > MAX_QUEUED {
                                        self.drop_conn(id);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // Write what the events queued, without waiting for the next round.
            let ids: Vec<ClientId> = self.conns.keys().copied().collect();
            for id in ids {
                let Some(c) = self.conns.get_mut(&id) else { continue };
                let (afd, ufd) = (c.app.as_raw_fd(), c.up.as_ref().map(AsRawFd::as_raw_fd));
                let bad = !c.a.flush(afd) || ufd.is_some_and(|u| !c.u.flush(u)) || c.a.queued > MAX_QUEUED || c.u.queued > MAX_QUEUED;
                if bad || c.dead {
                    self.drop_conn(id);
                }
            }
            for c in self.controls.values_mut() {
                if !c.out.is_empty() {
                    if let Ok(n) = send_with_fds(c.sock.as_raw_fd(), &c.out.clone(), &[]) {
                        c.out.drain(..n);
                    }
                }
            }
            // The end: nobody came, or the last copy is gone and nobody replaced it.
            if self.controls.is_empty() {
                let since = *last_copy.get_or_insert_with(Instant::now);
                if since.elapsed() > if ever { LAST_WAIT } else { FIRST_WAIT } && self.may_end(&control_listener) {
                    self.log("no copy is left: the router ends");
                    return Ok(());
                }
            }
        }
    }
}

/// Entry of the router process. Arguments after the flag: `<portal id or -> <alias or -> <open_uri 0|1> <debug 0|1> <end pause ms>`.
pub fn main_router(args: &[String]) -> Result<()> {
    ensure!(args.len() == 5, "usage: {FLAG} <portal-id|-> <mpris-alias|-> <open-uri 0|1> <debug 0|1> <end-pause-ms>");
    let opt = |s: &String| (s != "-").then(|| s.clone());
    let cfg = RouterConfig { portal_id: opt(&args[0]), mpris_alias: opt(&args[1]), open_uri: args[2] == "1" };
    let debug = args[3] == "1";
    let end_pause_ms: i32 = args[4].parse::<i32>().unwrap_or(0).clamp(0, 5000);
    // SAFETY: the parent started this process with the control listener on this descriptor and nothing else above 2.
    let ctl = unsafe { UnixListener::from_raw_fd(CONTROL_FD) };
    // Before the system calls are limited: the accept loop must not block on the control listener.
    ctl.set_nonblocking(true)?;
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    confine()?;
    let mut p = Process {
        router: Router::new(cfg),
        controls: HashMap::new(),
        conns: HashMap::new(),
        lim: Limits { max_message: dbus::ROUTER_MAX_MESSAGE, max_header_fields: 4096, ..Limits::default() },
        next_control: 1,
        debug,
        uid,
        t0: Instant::now(),
        end_pause_ms,
    };
    p.run(ctl)
}

fn set_limit(resource: u32, value: u64) -> Result<()> {
    let l = libc::rlimit { rlim_cur: value as libc::rlim_t, rlim_max: value as libc::rlim_t };
    // SAFETY: setrlimit with a valid pointer.
    ensure!(unsafe { libc::setrlimit(resource as _, &l) } == 0, "setrlimit: {}", std::io::Error::last_os_error());
    Ok(())
}

/// Take every right away, then check that it is so, then limit the calls (as the receiver does). Landlock is required: a router that cannot
/// confine itself does not run, and the app then has no bus.
#[allow(clippy::unnecessary_cast)] // the RLIMIT_* constants are c_uint with glibc and c_int with musl
fn confine() -> Result<()> {
    // SAFETY: prctl with integer arguments.
    ensure!(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } == 0, "dumpable: {}", std::io::Error::last_os_error());
    set_limit(libc::RLIMIT_NOFILE as u32, MAX_FILES)?;
    set_limit(libc::RLIMIT_AS as u32, MAX_ADDRESS_SPACE)?;
    set_limit(libc::RLIMIT_CPU as u32, MAX_CPU_SECONDS)?;
    set_limit(libc::RLIMIT_CORE as u32, 0)?;
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
    let st = b.create()?.restrict_self()?;
    ensure!(st.ruleset == landlock::RulesetStatus::FullyEnforced, "partial Landlock ({:?}): the router does not run", st.ruleset);
    ensure!(std::fs::File::open("/etc/passwd").is_err(), "router self-test: a file could be opened");
    let allowed: Vec<i64> = ALLOWED.iter().chain(TEST_EXTRA).copied().collect();
    panpak_core::seccomp::install_allowlist(&allowed)?;
    Ok(())
}

// ---- the side of the stub of a copy --------------------------------------------------------------------------------

/// What the router has to know about the app; the first copy decides.
#[derive(Clone)]
pub struct Spec {
    pub app_dir: PathBuf,
    pub portal_id: Option<String>,
    pub mpris_alias: Option<String>,
    pub open_uri: bool,
    pub debug: bool,
    /// For tests: how long the router waits, holding the lock, before it ends (from PANPAK_ROUTER_END_PAUSE_MS).
    pub end_pause_ms: u64,
}

/// The attachment of one copy to the router of the app. Dropping it detaches the copy (the router closes its listener and its connections).
pub struct Attachment {
    ctl: UnixStream,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let _ = self.ctl.shutdown(std::net::Shutdown::Both);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn flock(fd: RawFd, op: libc::c_int) -> bool {
    // SAFETY: flock on a descriptor this process owns.
    unsafe { libc::flock(fd, op) == 0 }
}

/// Connect to the router of the app, starting it if there is none. Under a lock, so that two copies that start together make one router.
fn connect_or_start(spec: &Spec) -> Result<UnixStream> {
    use std::os::unix::fs::OpenOptionsExt;
    let lockf = std::fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(spec.app_dir.join("router.lock")).context("router lock")?;
    ensure!(flock(lockf.as_raw_fd(), libc::LOCK_EX), "cannot take the router lock");
    let sock = spec.app_dir.join("router.sock");
    if let Ok(s) = UnixStream::connect(&sock) {
        return Ok(s);
    }
    // None answers: a stale file may be there; make the listener and start the router with it.
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).with_context(|| format!("cannot make {}", sock.display()))?;
    let fd = listener.as_raw_fd();
    // The router's own open file description of the lock file (a lock is per description: sharing ours would let the router unlock the copies' lock).
    let router_lock = std::fs::OpenOptions::new().write(true).open(spec.app_dir.join("router.lock")).context("router lock for the router")?;
    let lfd = router_lock.as_raw_fd();
    let exe = std::env::current_exe().context("own path")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(FLAG)
        .arg(spec.portal_id.as_deref().unwrap_or("-"))
        .arg(spec.mpris_alias.as_deref().unwrap_or("-"))
        .arg(if spec.open_uri { "1" } else { "0" })
        .arg(if spec.debug { "1" } else { "0" })
        .arg(spec.end_pause_ms.to_string())
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    // The router outlives the copy by a moment: it must not hold the caller's error output open (a script that reads it would wait for EOF).
    if !spec.debug {
        cmd.stderr(std::process::Stdio::null());
    }
    // SAFETY: only dup2, close_range and setsid run between fork and exec; they are async-signal-safe.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(move || {
            // Either descriptor may already be 3 or 4: move both out of the way first.
            let a = libc::fcntl(fd, libc::F_DUPFD, 10);
            let b = libc::fcntl(lfd, libc::F_DUPFD, 10);
            if a < 0 || b < 0 || libc::dup2(a, CONTROL_FD) < 0 || libc::dup2(b, LOCK_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::setsid();
            libc::syscall(libc::SYS_close_range, 5u32, u32::MAX, 0u32);
            Ok(())
        });
    }
    let mut child = cmd.spawn().with_context(|| format!("cannot run {}", exe.display()))?;
    // The router is a process of its own for good: nobody waits for it, a thread reaps it when it ends.
    let deadline = Instant::now() + Duration::from_secs(3);
    let stream = loop {
        match UnixStream::connect(&sock) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => {
                if let Ok(Some(st)) = child.try_wait() {
                    bail!("the router ended at once ({st}); run it with PANPAK_DEBUG=1 to see why");
                }
                std::thread::sleep(Duration::from_micros(300));
            }
            Err(e) => bail!("the router does not answer: {e}"),
        }
    };
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    drop(listener);
    drop(router_lock);
    Ok(stream)
}

/// Connect to the proxy of the copy and finish the handshake, so that the router gets a connection that is ready for messages.
fn connect_upstream(path: &Path) -> Result<UnixStream> {
    let mut s = UnixStream::connect(path).with_context(|| format!("cannot reach {}", path.display()))?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    // SAFETY: getuid cannot fail.
    let hex: String = unsafe { libc::getuid() }.to_string().bytes().map(|b| format!("{b:02x}")).collect();
    s.write_all(format!("\0AUTH EXTERNAL {hex}\r\nNEGOTIATE_UNIX_FD\r\nBEGIN\r\n").as_bytes())?;
    let mut got = Vec::new();
    let mut b = [0u8; 128];
    while !got.windows(15).any(|w| w == b"AGREE_UNIX_FD\r\n") {
        ensure!(got.len() < 512, "the proxy does not agree to pass descriptors");
        let n = s.read(&mut b)?;
        ensure!(n > 0, "the proxy closed the connection during the handshake");
        got.extend_from_slice(&b[..n]);
        ensure!(!got.starts_with(b"REJECTED"), "the proxy refused the authentication");
    }
    // Nothing may be left over: the proxy says nothing before the first message.
    ensure!(got.ends_with(b"AGREE_UNIX_FD\r\n"), "unexpected bytes after the handshake");
    s.set_read_timeout(None)?;
    s.set_write_timeout(None)?;
    Ok(s)
}

/// Attach this copy: hand the router the listening socket the app will connect to, and serve its requests for connections to the copy's
/// proxy (`upstream`) from a thread. Returns when the router has accepted the copy.
pub fn attach(spec: &Spec, listener: UnixListener, upstream: PathBuf, opener: Option<UnixStream>) -> Result<Attachment> {
    let ctl = connect_or_start(spec)?;
    // The descriptors the router receives share this flag: the router may not call fcntl.
    listener.set_nonblocking(true)?;
    let lfd = listener.as_raw_fd();
    let mut ctl_w = ctl.try_clone()?;
    ctl_w.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut fds = vec![lfd];
    if let Some(o) = &opener {
        o.set_nonblocking(true)?;
        fds.push(o.as_raw_fd());
    }
    ensure!(send_with_fds(ctl_w.as_raw_fd(), b"ATTACH\n", &fds).context("ATTACH")? == 7, "ATTACH was cut");
    drop(opener);
    drop(listener); // the router holds the only descriptor that matters now
    let mut rd = ctl.try_clone()?;
    rd.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut ok = [0u8; 3];
    rd.read_exact(&mut ok).context("the router does not answer ATTACH")?;
    ensure!(&ok == b"OK\n", "the router refused the copy");
    rd.set_read_timeout(None)?;
    let thread = std::thread::Builder::new().name("busrouter-ctl".into()).spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 128];
        loop {
            match rd.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            while let Some(p) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=p).collect();
                let line = String::from_utf8_lossy(&line[..line.len() - 1]).to_string();
                let Some(id) = line.strip_prefix("UP ").and_then(|x| x.parse::<ClientId>().ok()) else { return };
                match connect_upstream(&upstream) {
                    Ok(s) => {
                        let _ = s.set_nonblocking(true);
                        let msg = format!("FD {id}\n");
                        if send_with_fds(ctl_w.as_raw_fd(), msg.as_bytes(), &[s.as_raw_fd()]).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        if panpak_core::debug_enabled() {
                            eprintln!("panpak: the router asked for a connection to the proxy: {e:#}");
                        }
                        if ctl_w.write_all(format!("NOUP {id}\n").as_bytes()).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    })?;
    Ok(Attachment { ctl, thread: Some(thread) })
}
