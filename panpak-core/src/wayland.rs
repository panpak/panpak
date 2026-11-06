// SPDX-License-Identifier: MIT OR Apache-2.0
//! A small Wayland client for one job: register a security context (`wp_security_context_v1`) with the
//! compositor, so that the app gets a Wayland socket of its own that the compositor knows as "sandboxed".
//!
//! A compositor that supports the protocol can then keep the app away from the powers that a trusted client
//! has (screen capture, virtual keyboard, global shortcuts, input injection, and so on, as it chooses). One that
//! does not (GNOME's Mutter today) does not advertise the global; the caller then gives the app the normal
//! socket and `panpak doctor` says so.
//!
//! What is here: the wire format (pure functions, fuzzed: `fuzz_wayland`), and `register`, which talks to the
//! compositor over a socket with a time and size limit on everything it reads. Nothing here is linked to
//! libwayland; the protocol has six requests that matter, listed in `register`.
//!
//! Wire format: a message is a header of two 32-bit words in host byte order (the object id; the size in the
//! upper 16 bits and the opcode in the lower 16) followed by the arguments, each padded to 4 bytes. A string is
//! its length with the final NUL, the bytes, the NUL and the padding.
use crate::{coded, ErrorCode};
use anyhow::{bail, ensure, Context, Result};
use std::{
    io::Read,
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// The interface that carries the protocol.
pub const MANAGER: &str = "wp_security_context_manager_v1";
/// The name that panpak gives as the sandbox engine.
pub const ENGINE: &str = "panpak";

/// Largest message the client accepts from the compositor (the Wayland limit for a message is 4096).
pub const MAX_MESSAGE: usize = 4096;
/// Everything the client reads in one conversation. A compositor with many globals sends a few kilobytes.
const MAX_TOTAL: usize = 256 * 1024;
/// The whole conversation, in the worst case.
const TIMEOUT: Duration = Duration::from_secs(3);
/// Longest string the client sends (a name, an app id, an instance id).
const MAX_STRING: usize = 255;

// Object ids the client allocates. `wl_display` is 1 by definition.
const DISPLAY: u32 = 1;
const REGISTRY: u32 = 2;
const SYNC1: u32 = 3;
const MANAGER_ID: u32 = 4;
const CONTEXT: u32 = 5;
const SYNC2: u32 = 6;

/// An event of the compositor that the client understands. Other events are `Other` and skipped.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Event {
    Global { name: u32, interface: String, version: u32 },
    GlobalRemove(u32),
    /// A `wl_callback` of ours is done.
    Done(u32),
    /// A fatal error on an object (the compositor closes the connection after it).
    Error { object: u32, code: u32, message: String },
    DeleteId(u32),
    Other { object: u32, opcode: u16 },
}

/// Why bytes from the compositor cannot be taken as a message.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The size field is under the header size, not a multiple of 4, or over `MAX_MESSAGE`.
    BadSize(usize),
    /// An argument does not fit the message or is not well formed.
    BadArgument(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::BadSize(n) => write!(f, "bad Wayland message size {n}"),
            ParseError::BadArgument(w) => write!(f, "bad Wayland message: {w}"),
        }
    }
}

impl std::error::Error for ParseError {}

fn word(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]]))
}

/// A Wayland string at `at` in `body`: returns the text and the offset after its padding. A length of 0 is a
/// null string, which none of the events that matter here uses.
fn read_string(body: &[u8], at: usize) -> Result<(String, usize), ParseError> {
    let len = word(body, at).ok_or(ParseError::BadArgument("string length missing"))? as usize;
    if len == 0 || len > MAX_MESSAGE {
        return Err(ParseError::BadArgument("string length"));
    }
    let start = at + 4;
    let bytes = body.get(start..start + len).ok_or(ParseError::BadArgument("string longer than the message"))?;
    let (text, nul) = bytes.split_at(len - 1);
    if nul != [0] || text.contains(&0) {
        return Err(ParseError::BadArgument("string is not NUL-terminated"));
    }
    let text = std::str::from_utf8(text).map_err(|_| ParseError::BadArgument("string is not UTF-8"))?;
    Ok((text.to_string(), start + len.div_ceil(4) * 4))
}

/// Take the first message off `buf`. `Ok(None)`: not complete yet. `Ok(Some((event, used)))`: the event and the
/// number of bytes it took. An error means the stream cannot be followed any more.
pub fn parse_event(buf: &[u8]) -> Result<Option<(Event, usize)>, ParseError> {
    let (Some(object), Some(so)) = (word(buf, 0), word(buf, 4)) else { return Ok(None) };
    let size = (so >> 16) as usize;
    let opcode = (so & 0xffff) as u16;
    if size < 8 || !size.is_multiple_of(4) || size > MAX_MESSAGE {
        return Err(ParseError::BadSize(size));
    }
    if buf.len() < size {
        return Ok(None);
    }
    let body = &buf[8..size];
    let exact = |used: usize| if used == body.len() { Ok(()) } else { Err(ParseError::BadArgument("trailing bytes")) };
    let ev = match (object, opcode) {
        (DISPLAY, 0) => {
            let object = word(body, 0).ok_or(ParseError::BadArgument("error: object"))?;
            let code = word(body, 4).ok_or(ParseError::BadArgument("error: code"))?;
            let (message, end) = read_string(body, 8)?;
            exact(end)?;
            Event::Error { object, code, message }
        }
        (DISPLAY, 1) => {
            let id = word(body, 0).ok_or(ParseError::BadArgument("delete_id"))?;
            exact(4)?;
            Event::DeleteId(id)
        }
        (REGISTRY, 0) => {
            let name = word(body, 0).ok_or(ParseError::BadArgument("global: name"))?;
            let (interface, end) = read_string(body, 4)?;
            let version = word(body, end).ok_or(ParseError::BadArgument("global: version"))?;
            exact(end + 4)?;
            Event::Global { name, interface, version }
        }
        (REGISTRY, 1) => {
            let name = word(body, 0).ok_or(ParseError::BadArgument("global_remove"))?;
            exact(4)?;
            Event::GlobalRemove(name)
        }
        (SYNC1 | SYNC2, 0) => {
            word(body, 0).ok_or(ParseError::BadArgument("done"))?;
            exact(4)?;
            Event::Done(object)
        }
        _ => Event::Other { object, opcode },
    };
    Ok(Some((ev, size)))
}

/// Parse a whole stream. For tests and the fuzz target: the events, and an error if the stream breaks (an
/// incomplete message at the end is not an error).
pub fn parse_stream(mut buf: &[u8]) -> Result<Vec<Event>, ParseError> {
    let mut out = Vec::new();
    while let Some((ev, used)) = parse_event(buf)? {
        out.push(ev);
        buf = &buf[used..];
    }
    Ok(out)
}

/// A request being built.
struct Msg {
    buf: Vec<u8>,
}

impl Msg {
    fn new(object: u32, opcode: u16) -> Msg {
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&object.to_ne_bytes());
        buf.extend_from_slice(&u32::from(opcode).to_ne_bytes());
        Msg { buf }
    }
    fn uint(mut self, v: u32) -> Msg {
        self.buf.extend_from_slice(&v.to_ne_bytes());
        self
    }
    fn string(mut self, s: &str) -> Msg {
        let len = s.len() + 1;
        self.buf.extend_from_slice(&(len as u32).to_ne_bytes());
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
        while !self.buf.len().is_multiple_of(4) {
            self.buf.push(0);
        }
        self
    }
    fn done(mut self) -> Vec<u8> {
        let size = self.buf.len() as u32;
        let opcode = u32::from_ne_bytes([self.buf[4], self.buf[5], self.buf[6], self.buf[7]]) & 0xffff;
        self.buf[4..8].copy_from_slice(&((size << 16) | opcode).to_ne_bytes());
        self.buf
    }
}

/// What the app is told to the compositor.
pub struct Meta<'a> {
    pub app_id: &'a str,
    pub instance_id: &'a str,
}

/// What `register` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The compositor accepted the context: it listens on the socket now.
    Registered,
    /// The compositor does not offer the protocol (or hides it, because this process is itself in a context).
    Unsupported,
}

fn check_string(what: &str, s: &str) -> Result<()> {
    ensure!(!s.is_empty() && s.len() <= MAX_STRING && !s.contains('\0'), "{what} is empty, too long or has a NUL");
    Ok(())
}

/// Send `data` with `fds` as ancillary data (SCM_RIGHTS).
fn send_with_fds(sock: RawFd, data: &[u8], fds: &[RawFd]) -> std::io::Result<()> {
    let mut iov = libc::iovec { iov_base: data.as_ptr() as *mut libc::c_void, iov_len: data.len() };
    let space = unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) } as usize;
    let mut control = vec![0u8; space];
    // SAFETY: `msg` points at `iov` and `control`, which live to the end of the call; the control buffer is
    // sized by CMSG_SPACE and filled by the CMSG macros.
    unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        if !fds.is_empty() {
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = space as _;
            let c = libc::CMSG_FIRSTHDR(&msg);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as _;
            std::ptr::copy_nonoverlapping(fds.as_ptr().cast::<u8>(), libc::CMSG_DATA(c), std::mem::size_of_val(fds));
        }
        let sent = libc::sendmsg(sock, &msg, libc::MSG_NOSIGNAL);
        if sent < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if sent as usize != data.len() {
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "short write to the compositor"));
        }
    }
    Ok(())
}

/// The connection with its limits.
struct Conn {
    sock: UnixStream,
    pending: Vec<u8>,
    total: usize,
    deadline: Instant,
}

impl Conn {
    fn open(path: &Path) -> Result<Conn> {
        let sock = UnixStream::connect(path).with_context(|| format!("connect to {}", path.display()))?;
        Ok(Conn { sock, pending: Vec::new(), total: 0, deadline: Instant::now() + TIMEOUT })
    }

    fn send(&mut self, data: &[u8], fds: &[RawFd]) -> Result<()> {
        send_with_fds(self.sock.as_raw_fd(), data, fds).context("write to the compositor")
    }

    /// The next event, waiting for it within the deadline and the size limit.
    fn next_event(&mut self) -> Result<Event> {
        loop {
            if let Some((ev, used)) = parse_event(&self.pending)? {
                self.pending.drain(..used);
                return Ok(ev);
            }
            let left = self.deadline.saturating_duration_since(Instant::now());
            ensure!(!left.is_zero(), "the compositor did not answer in time");
            ensure!(self.total < MAX_TOTAL, "the compositor sent more than {MAX_TOTAL} bytes");
            self.sock.set_read_timeout(Some(left)).context("set the read time limit")?;
            let mut chunk = [0u8; 4096];
            let n = match self.sock.read(&mut chunk) {
                Ok(0) => bail!("the compositor closed the connection"),
                Ok(n) => n,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => bail!("the compositor did not answer in time"),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context("read from the compositor"),
            };
            self.total += n;
            self.pending.extend_from_slice(&chunk[..n]);
        }
    }

    /// Read events until the callback `done` arrives; a protocol error of the compositor is an error. Returns
    /// the globals seen on the way.
    fn until_done(&mut self, callback: u32) -> Result<Vec<(u32, String, u32)>> {
        let mut globals = Vec::new();
        loop {
            match self.next_event()? {
                Event::Done(id) if id == callback => return Ok(globals),
                Event::Global { name, interface, version } => globals.push((name, interface, version)),
                Event::Error { object, code, message } => {
                    return Err(coded(ErrorCode::E3000, format!("the compositor refused the security context (object {object}, code {code}): {}", printable(&message))));
                }
                _ => {}
            }
        }
    }
}

/// Text from the compositor, safe to print: control characters out, cut short.
fn printable(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(120).collect()
}

/// Ask the compositor at `display` whether it offers the protocol. Reads the list of globals and nothing more.
pub fn supported(display: &Path) -> Result<bool> {
    let mut c = Conn::open(display)?;
    let mut req = Msg::new(DISPLAY, 1).uint(REGISTRY).done();
    req.extend(Msg::new(DISPLAY, 0).uint(SYNC1).done());
    c.send(&req, &[])?;
    let globals = c.until_done(SYNC1)?;
    Ok(globals.iter().any(|(_, i, v)| i == MANAGER && *v >= 1))
}

/// Register a security context with the compositor at `display`. `listen` is a Unix socket that is already
/// bound and listening (the compositor accepts the clients of the sandbox on it); `close_read` is the read end
/// of a pipe whose write end the caller keeps: when it is closed, the compositor stops listening.
///
/// Requests, in order: `wl_display.get_registry`, `wl_display.sync`; then (if the global is there)
/// `wl_registry.bind`, `create_listener`, `set_sandbox_engine`, `set_app_id`, `set_instance_id`, `commit`,
/// `destroy` (the context; the listener lives on), `destroy` (the manager) and a last `wl_display.sync` to
/// learn of an error before the app starts.
pub fn register(display: &Path, listen: BorrowedFd, close_read: BorrowedFd, meta: &Meta) -> Result<Outcome> {
    check_string("the app id", meta.app_id)?;
    check_string("the instance id", meta.instance_id)?;
    let mut c = Conn::open(display)?;
    let mut req = Msg::new(DISPLAY, 1).uint(REGISTRY).done();
    req.extend(Msg::new(DISPLAY, 0).uint(SYNC1).done());
    c.send(&req, &[])?;
    let globals = c.until_done(SYNC1)?;
    let Some((name, _, version)) = globals.into_iter().find(|(_, i, v)| i == MANAGER && *v >= 1) else {
        return Ok(Outcome::Unsupported);
    };
    let _ = version;
    // wl_registry.bind(name, interface, version, new id): an untyped new id carries the interface and version.
    let mut req = Msg::new(REGISTRY, 0).uint(name).string(MANAGER).uint(1).uint(MANAGER_ID).done();
    c.send(&req, &[])?;
    // create_listener carries the two descriptors out of band.
    req = Msg::new(MANAGER_ID, 1).uint(CONTEXT).done();
    c.send(&req, &[listen.as_raw_fd(), close_read.as_raw_fd()])?;
    let mut rest = Msg::new(CONTEXT, 1).string(ENGINE).done();
    rest.extend(Msg::new(CONTEXT, 2).string(meta.app_id).done());
    rest.extend(Msg::new(CONTEXT, 3).string(meta.instance_id).done());
    rest.extend(Msg::new(CONTEXT, 4).done());
    rest.extend(Msg::new(CONTEXT, 0).done());
    rest.extend(Msg::new(MANAGER_ID, 0).done());
    rest.extend(Msg::new(DISPLAY, 0).uint(SYNC2).done());
    c.send(&rest, &[])?;
    c.until_done(SYNC2)?;
    Ok(Outcome::Registered)
}

/// The sockets of a context: the listening socket in a folder of the user, and the pipe whose write end keeps
/// the compositor listening.
pub struct Listener {
    pub path: PathBuf,
    pub socket: OwnedFd,
    pub close_read: OwnedFd,
    /// Held until the sandbox ends; dropping it tells the compositor to stop.
    pub close_write: OwnedFd,
}

impl Listener {
    /// Bind and listen at `path` (the folder must exist and be private to the user). The path must be short
    /// enough for a Unix socket address.
    pub fn create(path: &Path) -> Result<Listener> {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        // SAFETY: plain socket calls; every descriptor is wrapped at once.
        unsafe {
            let mut addr: libc::sockaddr_un = std::mem::zeroed();
            ensure!(bytes.len() < addr.sun_path.len() && !bytes.contains(&0), "the path of the Wayland socket is too long or has a NUL");
            addr.sun_family = libc::AF_UNIX as _;
            for (d, s) in addr.sun_path.iter_mut().zip(bytes) {
                *d = *s as _;
            }
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
            ensure!(fd >= 0, "socket: {}", std::io::Error::last_os_error());
            let socket = OwnedFd::from_raw_fd(fd);
            let r = libc::bind(fd, (&addr as *const libc::sockaddr_un).cast(), std::mem::size_of::<libc::sockaddr_un>() as _);
            ensure!(r == 0, "bind {}: {}", path.display(), std::io::Error::last_os_error());
            // The folder is private to the user; the socket is too.
            std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).with_context(|| format!("chmod {}", path.display()))?;
            ensure!(libc::listen(fd, 128) == 0, "listen: {}", std::io::Error::last_os_error());
            let mut p = [0 as RawFd; 2];
            ensure!(libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC) == 0, "pipe: {}", std::io::Error::last_os_error());
            Ok(Listener { path: path.to_path_buf(), socket, close_read: OwnedFd::from_raw_fd(p[0]), close_write: OwnedFd::from_raw_fd(p[1]) })
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixListener;

    fn event_bytes(object: u32, opcode: u16, body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&object.to_ne_bytes());
        v.extend_from_slice(&((((8 + body.len()) as u32) << 16) | opcode as u32).to_ne_bytes());
        v.extend_from_slice(body);
        v
    }

    fn string_arg(s: &str) -> Vec<u8> {
        let mut v = ((s.len() + 1) as u32).to_ne_bytes().to_vec();
        v.extend_from_slice(s.as_bytes());
        v.push(0);
        while !v.len().is_multiple_of(4) {
            v.push(0);
        }
        v
    }

    fn global(name: u32, iface: &str, version: u32) -> Vec<u8> {
        let mut b = name.to_ne_bytes().to_vec();
        b.extend(string_arg(iface));
        b.extend(version.to_ne_bytes());
        event_bytes(REGISTRY, 0, &b)
    }

    #[test]
    fn parse_global_and_done() {
        let mut s = global(7, "wl_compositor", 6);
        s.extend(global(9, MANAGER, 1));
        s.extend(event_bytes(SYNC1, 0, &1u32.to_ne_bytes()));
        let ev = parse_stream(&s).unwrap();
        assert_eq!(ev[0], Event::Global { name: 7, interface: "wl_compositor".into(), version: 6 });
        assert_eq!(ev[1], Event::Global { name: 9, interface: MANAGER.into(), version: 1 });
        assert_eq!(ev[2], Event::Done(SYNC1));
    }

    #[test]
    fn parse_error_event() {
        let mut b = 5u32.to_ne_bytes().to_vec();
        b.extend(3u32.to_ne_bytes());
        b.extend(string_arg("bad metadata"));
        let ev = parse_stream(&event_bytes(DISPLAY, 0, &b)).unwrap();
        assert_eq!(ev[0], Event::Error { object: 5, code: 3, message: "bad metadata".into() });
    }

    #[test]
    fn incomplete_is_not_an_error() {
        let s = global(1, "wl_shm", 1);
        for cut in 0..s.len() {
            assert_eq!(parse_stream(&s[..cut]).unwrap(), vec![], "cut at {cut}");
        }
    }

    #[test]
    fn bad_streams_are_errors() {
        // size under 8, not a multiple of 4, over the limit
        for size in [0u32, 4, 9, 4100, 65532] {
            let mut s = 2u32.to_ne_bytes().to_vec();
            s.extend((size << 16).to_ne_bytes());
            s.extend([0; 16]);
            assert!(matches!(parse_stream(&s), Err(ParseError::BadSize(_))), "{size}");
        }
        // a string whose length runs past the message, without NUL, with an inner NUL, not UTF-8, null string
        let mut long = 1u32.to_ne_bytes().to_vec();
        long.extend(100u32.to_ne_bytes());
        long.extend(*b"abcd");
        assert!(parse_stream(&event_bytes(REGISTRY, 0, &long)).is_err());
        for bad in [b"abc\x01".to_vec(), b"a\0c\0".to_vec(), b"\xff\xfe\xfd\0".to_vec()] {
            let mut b = 1u32.to_ne_bytes().to_vec();
            b.extend(4u32.to_ne_bytes());
            b.extend(&bad);
            b.extend(1u32.to_ne_bytes());
            assert!(parse_stream(&event_bytes(REGISTRY, 0, &b)).is_err(), "{bad:?}");
        }
        let mut nul = 1u32.to_ne_bytes().to_vec();
        nul.extend(0u32.to_ne_bytes());
        nul.extend(1u32.to_ne_bytes());
        assert!(parse_stream(&event_bytes(REGISTRY, 0, &nul)).is_err());
        // trailing bytes after a known event
        let mut b = global(1, "x", 1);
        let n = b.len();
        b[4..8].copy_from_slice(&(((n + 4) as u32) << 16).to_ne_bytes());
        b.extend([0; 4]);
        assert!(parse_stream(&b).is_err());
    }

    #[test]
    fn unknown_events_are_skipped() {
        let ev = parse_stream(&event_bytes(77, 3, &[1, 2, 3, 4])).unwrap();
        assert_eq!(ev, vec![Event::Other { object: 77, opcode: 3 }]);
    }

    /// What the fake compositor saw.
    #[derive(Default, Debug)]
    struct Seen {
        engine: Option<String>,
        app_id: Option<String>,
        instance_id: Option<String>,
        committed: bool,
        fds: usize,
        order: Vec<(u32, u16)>,
    }

    /// Read one request; returns (object, opcode, body, fds received).
    fn read_request(s: &UnixStream, buf: &mut Vec<u8>) -> Option<(u32, u16, Vec<u8>, usize)> {
        let mut fds_seen = 0usize;
        loop {
            if buf.len() >= 8 {
                let size = (word(buf, 4).unwrap() >> 16) as usize;
                if size >= 8 && buf.len() >= size {
                    let obj = word(buf, 0).unwrap();
                    let op = (word(buf, 4).unwrap() & 0xffff) as u16;
                    let body = buf[8..size].to_vec();
                    buf.drain(..size);
                    return Some((obj, op, body, fds_seen));
                }
            }
            let mut chunk = [0u8; 4096];
            let mut control = [0u8; 256];
            let mut iov = libc::iovec { iov_base: chunk.as_mut_ptr().cast(), iov_len: chunk.len() };
            // SAFETY: the buffers outlive the call.
            let n = unsafe {
                let mut msg: libc::msghdr = std::mem::zeroed();
                msg.msg_iov = &mut iov;
                msg.msg_iovlen = 1;
                msg.msg_control = control.as_mut_ptr().cast();
                msg.msg_controllen = control.len() as _;
                let n = libc::recvmsg(s.as_raw_fd(), &mut msg, 0);
                if n > 0 {
                    let mut c = libc::CMSG_FIRSTHDR(&msg);
                    while !c.is_null() {
                        if (*c).cmsg_type == libc::SCM_RIGHTS {
                            let len = (*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                            fds_seen += len / std::mem::size_of::<RawFd>();
                        }
                        c = libc::CMSG_NXTHDR(&msg, c);
                    }
                }
                n
            };
            if n <= 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n as usize]);
        }
    }

    /// A compositor that speaks just enough: the registry, sync, and the security context requests.
    /// `offer`: whether it advertises the manager. `fail`: answers the commit with a protocol error.
    fn fake_compositor(path: PathBuf, offer: bool, fail: bool) -> std::thread::JoinHandle<Seen> {
        let listener = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut seen = Seen::default();
            while let Some((obj, op, body, fds)) = read_request(&s, &mut buf) {
                seen.order.push((obj, op));
                seen.fds += fds;
                match (obj, op) {
                    (DISPLAY, 1) => {
                        s.write_all(&global(1, "wl_compositor", 6)).unwrap();
                        if offer {
                            s.write_all(&global(2, MANAGER, 1)).unwrap();
                        }
                    }
                    (DISPLAY, 0) => {
                        let cb = word(&body, 0).unwrap();
                        if fail && seen.committed {
                            let mut b = CONTEXT.to_ne_bytes().to_vec();
                            b.extend(3u32.to_ne_bytes());
                            b.extend(string_arg("invalid metadata"));
                            s.write_all(&event_bytes(DISPLAY, 0, &b)).unwrap();
                        } else {
                            s.write_all(&event_bytes(cb, 0, &0u32.to_ne_bytes())).unwrap();
                        }
                    }
                    (CONTEXT, 1) => seen.engine = Some(read_string(&body, 0).unwrap().0),
                    (CONTEXT, 2) => seen.app_id = Some(read_string(&body, 0).unwrap().0),
                    (CONTEXT, 3) => seen.instance_id = Some(read_string(&body, 0).unwrap().0),
                    (CONTEXT, 4) => seen.committed = true,
                    _ => {}
                }
                if obj == DISPLAY && op == 0 && word(&body, 0) == Some(SYNC2) {
                    break;
                }
            }
            seen
        })
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-wl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn registers_with_a_compositor_that_supports_it() {
        let d = dir("ok");
        let h = fake_compositor(d.join("wayland-0"), true, false);
        let l = Listener::create(&d.join("app")).unwrap();
        let meta = Meta { app_id: "panpak.org.example.app", instance_id: "42-1" };
        let out = register(&d.join("wayland-0"), l.socket.as_fd(), l.close_read.as_fd(), &meta).unwrap();
        assert_eq!(out, Outcome::Registered);
        let seen = h.join().unwrap();
        assert_eq!(seen.engine.as_deref(), Some("panpak"));
        assert_eq!(seen.app_id.as_deref(), Some("panpak.org.example.app"));
        assert_eq!(seen.instance_id.as_deref(), Some("42-1"));
        assert!(seen.committed);
        assert_eq!(seen.fds, 2, "listen and close descriptors");
        // The order of the requests of the security context: engine, app id, instance id, commit.
        let ctx: Vec<u16> = seen.order.iter().filter(|(o, _)| *o == CONTEXT).map(|(_, op)| *op).collect();
        assert_eq!(ctx, vec![1, 2, 3, 4, 0]);
        // The listener is a real listening socket at its path.
        assert!(UnixStream::connect(d.join("app")).is_ok());
        drop(l);
        assert!(!d.join("app").exists(), "the socket file is removed with the listener");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unsupported_compositor() {
        let d = dir("no");
        let h = fake_compositor(d.join("wayland-0"), false, false);
        let l = Listener::create(&d.join("app")).unwrap();
        let meta = Meta { app_id: "panpak.x", instance_id: "1" };
        assert_eq!(register(&d.join("wayland-0"), l.socket.as_fd(), l.close_read.as_fd(), &meta).unwrap(), Outcome::Unsupported);
        drop(l);
        drop(h); // the thread ends when the client closes the connection
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn supported_query() {
        let d = dir("q");
        let h = fake_compositor(d.join("wayland-0"), true, false);
        assert!(supported(&d.join("wayland-0")).unwrap());
        drop(h);
        let h = fake_compositor(d.join("wayland-1"), false, false);
        assert!(!supported(&d.join("wayland-1")).unwrap());
        drop(h);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn compositor_error_is_reported() {
        let d = dir("err");
        let h = fake_compositor(d.join("wayland-0"), true, true);
        let l = Listener::create(&d.join("app")).unwrap();
        let meta = Meta { app_id: "panpak.x", instance_id: "1" };
        let e = register(&d.join("wayland-0"), l.socket.as_fd(), l.close_read.as_fd(), &meta).unwrap_err();
        assert!(crate::err_text(&e).contains("invalid metadata"), "{e:#}");
        drop(h);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn silent_compositor_times_out_and_garbage_is_refused() {
        let d = dir("slow");
        let path = d.join("wayland-0");
        let listener = UnixListener::bind(&path).unwrap();
        let t = std::thread::spawn(move || {
            // First connection: garbage. Second: silence.
            let (mut s, _) = listener.accept().unwrap();
            let mut b = [0u8; 64];
            let _ = s.read(&mut b);
            s.write_all(&[0xff; 32]).unwrap();
            let (s2, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(4));
            drop(s2);
        });
        assert!(supported(&path).is_err(), "garbage must be an error");
        let t0 = Instant::now();
        let e = supported(&path).unwrap_err();
        assert!(t0.elapsed() < Duration::from_secs(4), "{:?}", t0.elapsed());
        assert!(crate::err_text(&e).contains("in time"), "{e:#}");
        t.join().unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn strings_that_cannot_be_sent_are_refused() {
        assert!(check_string("x", "").is_err());
        assert!(check_string("x", &"a".repeat(256)).is_err());
        assert!(check_string("x", "a\0b").is_err());
        assert!(check_string("x", "panpak.org.example.app").is_ok());
    }

    #[test]
    fn listener_path_too_long() {
        let long = PathBuf::from(format!("/tmp/{}", "a".repeat(200)));
        assert!(Listener::create(&long).is_err());
    }

    #[test]
    fn requests_have_the_wire_format() {
        // wl_display.get_registry(2): object 1, opcode 1, size 12.
        let m = Msg::new(DISPLAY, 1).uint(REGISTRY).done();
        assert_eq!(m, [1u32.to_ne_bytes(), ((12u32 << 16) | 1).to_ne_bytes(), 2u32.to_ne_bytes()].concat());
        // A string is padded to 4 bytes: "panpak" + NUL = 7 -> 8.
        let m = Msg::new(CONTEXT, 1).string("panpak").done();
        assert_eq!(m.len(), 8 + 4 + 8);
        assert_eq!(word(&m, 8), Some(7));
        assert_eq!(&m[12..19], b"panpak\0");
    }
}
