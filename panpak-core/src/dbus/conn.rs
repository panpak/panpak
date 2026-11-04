// SPDX-License-Identifier: MIT OR Apache-2.0
//! A blocking connection to a D-Bus daemon over a Unix socket: EXTERNAL authentication, file
//! descriptor passing (SCM_RIGHTS), `Hello`, method calls with their replies. No threads, no
//! dependencies beyond `libc`. Used by the outer stub only, never inside the sandbox.

use super::wire::{self, encode_message, message_len, parse_message, Limits, Message, Value};
use anyhow::{bail, ensure, Context, Result};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// An error reply: the D-Bus error name and the text the service gave.
#[derive(Debug, Clone)]
pub struct DbusError {
    pub name: String,
    pub text: String,
}

impl std::fmt::Display for DbusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.name, self.text)
    }
}

impl std::error::Error for DbusError {}

/// The body and descriptors of a method return.
#[derive(Debug)]
pub struct Reply {
    pub body: Vec<Value>,
    pub fds: Vec<OwnedFd>,
}

pub struct Connection {
    sock: UnixStream,
    serial: u32,
    lim: Limits,
    /// Descriptors that arrived and are not yet claimed by a message.
    pending_fds: Vec<OwnedFd>,
    /// Unique name given by the daemon after `Hello`.
    pub unique_name: String,
}

/// The socket path of a `unix:path=...` address (the first such entry of a `;`-separated list).
pub fn address_path(addr: &str) -> Result<PathBuf> {
    for entry in addr.split(';') {
        let Some(rest) = entry.strip_prefix("unix:") else { continue };
        for kv in rest.split(',') {
            if let Some(p) = kv.strip_prefix("path=") {
                let mut out = Vec::new();
                let b = p.as_bytes();
                let mut i = 0;
                while i < b.len() {
                    if b[i] == b'%' {
                        let h = p.get(i + 1..i + 3).context("D-Bus address: bad escape")?;
                        out.push(u8::from_str_radix(h, 16).context("D-Bus address: bad escape")?);
                        i += 3;
                    } else {
                        out.push(b[i]);
                        i += 1;
                    }
                }
                use std::os::unix::ffi::OsStringExt;
                let p = PathBuf::from(std::ffi::OsString::from_vec(out));
                ensure!(p.is_absolute(), "D-Bus address: the path is not absolute");
                return Ok(p);
            }
        }
    }
    bail!("D-Bus address has no unix:path= entry")
}

fn is_timeout(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(|i| matches!(i.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)))
}

impl Connection {
    /// Connect to the socket, authenticate, and say hello.
    pub fn open(path: &Path, timeout: Duration) -> Result<Connection> {
        // std sets close-on-exec on the socket it creates.
        let sock = UnixStream::connect(path).with_context(|| format!("cannot connect to {}", path.display()))?;
        sock.set_read_timeout(Some(timeout))?;
        sock.set_write_timeout(Some(timeout))?;
        let mut c = Connection { sock, serial: 0, lim: Limits::default(), pending_fds: Vec::new(), unique_name: String::new() };
        c.authenticate()?;
        let r = c.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "Hello", vec![], vec![])?;
        match r.body.as_slice() {
            [Value::Str(n)] if n.starts_with(':') && n.len() <= 255 => c.unique_name = n.clone(),
            _ => bail!("Hello: unexpected reply"),
        }
        Ok(c)
    }

    /// The user's session bus: `DBUS_SESSION_BUS_ADDRESS`, else `$XDG_RUNTIME_DIR/bus`.
    pub fn session(timeout: Duration) -> Result<Connection> {
        let path = match std::env::var("DBUS_SESSION_BUS_ADDRESS") {
            Ok(a) if !a.is_empty() => address_path(&a)?,
            _ => {
                let rt = std::env::var_os("XDG_RUNTIME_DIR").context("no session bus: XDG_RUNTIME_DIR is not set")?;
                PathBuf::from(rt).join("bus")
            }
        };
        Connection::open(&path, timeout)
    }

    fn read_line(&mut self) -> Result<String> {
        use std::io::Read;
        let mut line = Vec::new();
        let mut b = [0u8; 1];
        while !line.ends_with(b"\r\n") {
            ensure!(line.len() < 512, "authentication reply too long");
            self.sock.read_exact(&mut b).context("authentication: connection closed")?;
            line.push(b[0]);
        }
        line.truncate(line.len() - 2);
        String::from_utf8(line).context("authentication reply is not text")
    }

    fn authenticate(&mut self) -> Result<()> {
        use std::io::Write;
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() }.to_string();
        let hex: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
        self.sock.write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())?;
        let l = self.read_line()?;
        ensure!(l.starts_with("OK "), "authentication refused: {}", l.chars().take(60).collect::<String>());
        self.sock.write_all(b"NEGOTIATE_UNIX_FD\r\n")?;
        let l = self.read_line()?;
        ensure!(l == "AGREE_UNIX_FD", "the daemon does not pass file descriptors");
        self.sock.write_all(b"BEGIN\r\n")?;
        Ok(())
    }

    fn send(&mut self, bytes: &[u8], fds: &[RawFd]) -> Result<()> {
        let mut off = 0;
        let mut first = true;
        while off < bytes.len() {
            let chunk = &bytes[off..];
            let iov = libc::iovec { iov_base: chunk.as_ptr() as *mut _, iov_len: chunk.len() };
            let fd_bytes = std::mem::size_of_val(fds) as u32;
            // SAFETY: CMSG_SPACE only computes a size.
            let space = unsafe { libc::CMSG_SPACE(fd_bytes) } as usize;
            let mut ctl = vec![0u8; if first && !fds.is_empty() { space } else { 0 }];
            // SAFETY: msghdr is plain data; the pointers stay valid for the call.
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_iov = &iov as *const _ as *mut _;
            msg.msg_iovlen = 1;
            if !ctl.is_empty() {
                msg.msg_control = ctl.as_mut_ptr() as *mut _;
                msg.msg_controllen = ctl.len() as _;
                // SAFETY: the control buffer is CMSG_SPACE bytes, enough for one cmsg of this size.
                unsafe {
                    let cm = libc::CMSG_FIRSTHDR(&msg);
                    (*cm).cmsg_level = libc::SOL_SOCKET;
                    (*cm).cmsg_type = libc::SCM_RIGHTS;
                    (*cm).cmsg_len = libc::CMSG_LEN(fd_bytes) as _;
                    std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(cm) as *mut RawFd, fds.len());
                }
            }
            // SAFETY: a valid descriptor and message header.
            let n = unsafe { libc::sendmsg(self.sock.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e).context("D-Bus send");
            }
            ensure!(n > 0, "D-Bus send wrote nothing");
            off += n as usize;
            first = false;
        }
        Ok(())
    }

    /// Read exactly `buf.len()` bytes, keeping the descriptors that come with them.
    fn recv_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        let mut off = 0;
        let max = self.lim.max_fds;
        // SAFETY: CMSG_SPACE only computes a size.
        let space = unsafe { libc::CMSG_SPACE((max * std::mem::size_of::<RawFd>()) as u32) } as usize;
        while off < buf.len() {
            let iov = libc::iovec { iov_base: buf[off..].as_mut_ptr() as *mut _, iov_len: buf.len() - off };
            let mut ctl = vec![0u8; space];
            // SAFETY: as in send.
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_iov = &iov as *const _ as *mut _;
            msg.msg_iovlen = 1;
            msg.msg_control = ctl.as_mut_ptr() as *mut _;
            msg.msg_controllen = ctl.len() as _;
            // SAFETY: a valid descriptor and message header; MSG_CMSG_CLOEXEC marks received descriptors.
            let n = unsafe { libc::recvmsg(self.sock.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e).context("D-Bus receive");
            }
            // Take every descriptor now, so that none leaks whatever happens next.
            // SAFETY: the kernel filled the control buffer; the walk follows the CMSG macros.
            unsafe {
                let mut cm = libc::CMSG_FIRSTHDR(&msg);
                while !cm.is_null() {
                    if (*cm).cmsg_level == libc::SOL_SOCKET && (*cm).cmsg_type == libc::SCM_RIGHTS {
                        let data = libc::CMSG_DATA(cm) as *const RawFd;
                        let count = ((*cm).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<RawFd>();
                        for i in 0..count {
                            self.pending_fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(data.add(i))));
                        }
                    }
                    cm = libc::CMSG_NXTHDR(&msg, cm);
                }
            }
            ensure!(msg.msg_flags & libc::MSG_CTRUNC == 0, "descriptors were cut off");
            ensure!(self.pending_fds.len() <= max, "more than {max} descriptors in flight");
            ensure!(n > 0, "the daemon closed the connection");
            off += n as usize;
        }
        Ok(())
    }

    /// Read the next message with the descriptors that belong to it.
    pub fn recv(&mut self) -> Result<(Message, Vec<OwnedFd>)> {
        let mut head = [0u8; 16];
        self.recv_exact(&mut head)?;
        let total = message_len(&head, &self.lim)?;
        let mut buf = vec![0u8; total];
        buf[..16].copy_from_slice(&head);
        self.recv_exact(&mut buf[16..])?;
        let m = parse_message(&buf, self.pending_fds.len(), &self.lim)?;
        let fds: Vec<OwnedFd> = self.pending_fds.drain(..m.unix_fds as usize).collect();
        Ok((m, fds))
    }

    /// Send a method call and wait for its reply. Signals and unrelated messages that arrive
    /// meanwhile are dropped (at most 1000).
    pub fn call(&mut self, dest: &str, path: &str, iface: &str, member: &str, args: Vec<Value>, fds: Vec<RawFd>) -> Result<Reply> {
        self.serial = self.serial.checked_add(1).context("serial overflow")?;
        let serial = self.serial;
        let m = Message {
            kind: wire::CALL,
            serial,
            path: Some(path.into()),
            interface: Some(iface.into()),
            member: Some(member.into()),
            destination: Some(dest.into()),
            signature: args.iter().map(Value::signature).collect(),
            unix_fds: fds.len() as u32,
            body: args,
            ..Default::default()
        };
        let bytes = encode_message(&m)?;
        self.send(&bytes, &fds)?;
        let started = Instant::now();
        for _ in 0..1000 {
            let (r, rfds) = self.recv()?;
            if r.reply_serial != Some(serial) {
                ensure!(started.elapsed() < Duration::from_secs(60), "no reply to {member}");
                continue;
            }
            return match r.kind {
                wire::RETURN => Ok(Reply { body: r.body, fds: rfds }),
                wire::ERROR => {
                    let text = match r.body.first() {
                        Some(Value::Str(s)) => s.chars().take(200).collect(),
                        _ => String::new(),
                    };
                    Err(DbusError { name: r.error_name.unwrap_or_default(), text }.into())
                }
                _ => bail!("reply of unexpected type"),
            };
        }
        bail!("no reply to {member} among 1000 messages")
    }

    /// Wait up to `wait` for a signal that `accept` likes. `None` on timeout.
    pub fn wait_signal(&mut self, wait: Duration, mut accept: impl FnMut(&Message) -> bool) -> Result<Option<Message>> {
        let end = Instant::now() + wait;
        let restore = self.sock.read_timeout()?;
        let res = loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Ok(None);
            }
            self.sock.set_read_timeout(Some(left))?;
            match self.recv() {
                Ok((m, _)) if m.kind == wire::SIGNAL && accept(&m) => break Ok(Some(m)),
                Ok(_) => {}
                Err(e) if is_timeout(&e) => break Ok(None),
                Err(e) => break Err(e),
            }
        };
        self.sock.set_read_timeout(restore)?;
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-dbus-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut p = [0; 2];
        // SAFETY: pipe2 fills two descriptors.
        assert_eq!(unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        unsafe { (OwnedFd::from_raw_fd(p[0]), OwnedFd::from_raw_fd(p[1])) }
    }

    /// A serving end that speaks just enough: authentication, then Echo (writes to the descriptor
    /// it gets) and Give (returns a descriptor).
    fn fake_daemon(dir: &Path) -> (PathBuf, std::thread::JoinHandle<()>) {
        let path = dir.join("bus");
        let l = UnixListener::bind(&path).unwrap();
        let h = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut c = Connection { sock: s, serial: 0, lim: Limits::default(), pending_fds: Vec::new(), unique_name: ":1.7".into() };
            // authentication: "\0AUTH EXTERNAL ..", NEGOTIATE_UNIX_FD, BEGIN
            let mut line = Vec::new();
            let mut b = [0u8; 1];
            let mut stage = 0;
            while stage < 3 {
                c.sock.read_exact(&mut b).unwrap();
                line.push(b[0]);
                if line.ends_with(b"\r\n") {
                    match stage {
                        0 => c.sock.write_all(b"OK 0123456789abcdef0123456789abcdef\r\n").unwrap(),
                        1 => c.sock.write_all(b"AGREE_UNIX_FD\r\n").unwrap(),
                        _ => {}
                    }
                    stage += 1;
                    line.clear();
                }
            }
            loop {
                let Ok((m, fds)) = c.recv() else { return };
                let mut reply = Message { kind: wire::RETURN, serial: 1, reply_serial: Some(m.serial), ..Default::default() };
                let mut out_fds: Vec<RawFd> = vec![];
                let keep;
                match m.member.as_deref() {
                    Some("Hello") => {
                        reply.signature = "s".into();
                        reply.body = vec![Value::str(":1.7")];
                    }
                    Some("Echo") => {
                        assert_eq!(m.body, vec![Value::Fd(0), Value::str("hello")]);
                        let mut f = std::fs::File::from(fds.into_iter().next().unwrap());
                        f.write_all(b"echo").unwrap();
                    }
                    Some("Give") => {
                        let (r, w) = pipe();
                        std::fs::File::from(w).write_all(b"given").unwrap();
                        keep = r;
                        out_fds.push(keep.as_raw_fd());
                        reply.signature = "h".into();
                        reply.body = vec![Value::Fd(0)];
                        reply.unix_fds = 1;
                    }
                    Some("Fail") => {
                        reply.kind = wire::ERROR;
                        reply.error_name = Some("org.example.Failed".into());
                        reply.signature = "s".into();
                        reply.body = vec![Value::str("no good")];
                    }
                    Some("Spam") => {
                        // a signal first, then the reply
                        let sig = Message { kind: wire::SIGNAL, serial: 9, path: Some("/x".into()), interface: Some("org.x.I".into()), member: Some("Hi".into()), ..Default::default() };
                        c.send(&encode_message(&sig).unwrap(), &[]).unwrap();
                    }
                    _ => return,
                }
                let bytes = encode_message(&reply).unwrap();
                c.send(&bytes, &out_fds).unwrap();
            }
        });
        (path, h)
    }

    #[test]
    fn descriptors_travel_both_ways_and_errors_come_back() {
        let dir = scratch("fake");
        let (path, h) = fake_daemon(&dir);
        let mut c = Connection::open(&path, Duration::from_secs(10)).unwrap();
        assert_eq!(c.unique_name, ":1.7");
        // to the service
        let (r, w) = pipe();
        c.call("org.x", "/x", "org.x.I", "Echo", vec![Value::Fd(0), Value::str("hello")], vec![w.as_raw_fd()]).unwrap();
        drop(w);
        let mut s = String::new();
        std::fs::File::from(r).read_to_string(&mut s).unwrap();
        assert_eq!(s, "echo");
        // from the service
        let rep = c.call("org.x", "/x", "org.x.I", "Give", vec![], vec![]).unwrap();
        assert_eq!(rep.body, vec![Value::Fd(0)]);
        assert_eq!(rep.fds.len(), 1);
        let mut s = String::new();
        std::fs::File::from(rep.fds.into_iter().next().unwrap()).read_to_string(&mut s).unwrap();
        assert_eq!(s, "given");
        // an error reply
        let e = c.call("org.x", "/x", "org.x.I", "Fail", vec![], vec![]).unwrap_err();
        let d = e.downcast_ref::<DbusError>().expect("a DbusError");
        assert_eq!((d.name.as_str(), d.text.as_str()), ("org.example.Failed", "no good"));
        // a signal in front of the reply is skipped
        c.call("org.x", "/x", "org.x.I", "Spam", vec![], vec![]).unwrap();
        drop(c);
        h.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_daemon_that_cuts_the_connection_is_an_error() {
        let dir = scratch("cut");
        let l = UnixListener::bind(dir.join("bus")).unwrap();
        let h = std::thread::spawn(move || drop(l.accept().unwrap()));
        assert!(Connection::open(&dir.join("bus"), Duration::from_secs(5)).is_err());
        h.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn addresses() {
        assert_eq!(address_path("unix:path=/run/user/1000/bus").unwrap(), Path::new("/run/user/1000/bus"));
        assert_eq!(address_path("unix:guid=ab,path=/a%20b/bus;tcp:host=x").unwrap(), Path::new("/a b/bus"));
        assert!(address_path("unix:abstract=/tmp/x").is_err());
        assert!(address_path("unix:path=rel").is_err());
        assert!(address_path("unix:path=/a%zz").is_err());
        assert!(address_path("tcp:host=x").is_err());
    }

    /// A real daemon, when one is installed: Hello and a few calls.
    #[test]
    fn talks_to_a_real_daemon() {
        let Some(bin) = ["/usr/bin/dbus-daemon", "/bin/dbus-daemon"].into_iter().find(|p| Path::new(p).exists()) else {
            eprintln!("skipped: no dbus-daemon");
            return;
        };
        let conf = ["/usr/share/dbus-1/session.conf", "/etc/dbus-1/session.conf"].into_iter().find(|p| Path::new(p).exists());
        let Some(conf) = conf else {
            eprintln!("skipped: no session.conf");
            return;
        };
        let dir = scratch("real");
        let sock = dir.join("bus");
        let mut child = std::process::Command::new(bin)
            .args(["--nofork", "--config-file", conf, "--address"])
            .arg(format!("unix:path={}", sock.display()))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..100 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let res = (|| -> Result<()> {
            let mut c = Connection::open(&sock, Duration::from_secs(10))?;
            ensure!(c.unique_name.starts_with(':'), "unique name");
            let r = c.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "ListNames", vec![], vec![])?;
            match r.body.as_slice() {
                [Value::Array(_, names)] => ensure!(names.contains(&Value::str("org.freedesktop.DBus")), "names {names:?}"),
                b => bail!("ListNames gave {b:?}"),
            }
            let e = c.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NoSuchMethod", vec![], vec![]).unwrap_err();
            ensure!(e.downcast_ref::<DbusError>().is_some_and(|d| d.name == "org.freedesktop.DBus.Error.UnknownMethod"), "{e}");
            // a signal that we wait for: our own NameOwnerChanged-free request-name
            c.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName", vec![Value::str("org.example.Test"), Value::U32(0)], vec![])?;
            Ok(())
        })();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(dir);
        res.unwrap();
    }
}
