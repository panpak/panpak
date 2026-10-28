// SPDX-License-Identifier: MIT OR Apache-2.0
//! Socket probe: a way to MEASURE what an app does with sockets. It is not a security feature.
//!
//! Turned on only when the user sets `PANPAK_SOCKET_PROBE=<log file>` for the stub; never on by default
//! and never from a manifest. The stub stays alive as the supervisor and the app runs as its child.
//! A seccomp filter sends `connect`, `sendto` (with an address), `sendmsg`, `sendmmsg`, `clone3` and
//! the `clone` and `unshare` calls that ask for a new namespace to the supervisor. The supervisor reads
//! what the call asks for, writes one JSON line and answers CONTINUE every time, so the kernel runs the
//! call exactly as the app asked. CONTINUE is not safe against an app that changes its memory, its file
//! descriptors or its namespaces after the read: this mode checks and blocks nothing.
//!
//! Log lines: `{"t":<ms>,"tid":N,"syscall":"connect","family":"AF_UNIX","path":"/run/x"}`; at the end a
//! `{"summary":{...}}` line with the counts (also every two seconds while the counts change: the last
//! one counts). `sendmsg` and `sendmmsg` with neither an address nor control
//! data are only counted, not logged. `sendto` without an address is filtered out in the kernel and is not
//! counted at all. See docs/SOCKET-PROBE.md.
use anyhow::{bail, Context, Result};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read as _, Write as _},
    os::unix::{
        fs::{FileExt as _, OpenOptionsExt as _},
        io::FromRawFd as _,
        process::CommandExt as _,
    },
    path::Path,
    process::Command,
    sync::atomic::{AtomicI32, Ordering},
};

const SECCOMP_SET_MODE_FILTER: libc::c_long = 1;
const SECCOMP_FILTER_FLAG_NEW_LISTENER: libc::c_ulong = 1 << 3;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;
const IOCTL_NOTIF_RECV: libc::Ioctl = 0xc050_2100u32 as libc::Ioctl;
const IOCTL_NOTIF_SEND: libc::Ioctl = 0xc018_2101u32 as libc::Ioctl;
const IOCTL_NOTIF_ID_VALID: libc::Ioctl = 0x4008_2102u32 as libc::Ioctl;
const NOTIF_FLAG_CONTINUE: u32 = 1;
const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;

const SYS_CONNECT: u32 = 42;
const SYS_SENDTO: u32 = 44;
const SYS_SENDMSG: u32 = 46;
const SYS_CLONE: u32 = 56;
const SYS_UNSHARE: u32 = 272;
const SYS_SENDMMSG: u32 = 307;
const SYS_CLONE3: u32 = 435;

/// Every `CLONE_NEW*` flag: NS 0x20000, CGROUP 0x02000000, UTS, IPC, USER, PID, NET.
const CLONE_NEW_MASK: u32 = 0x7e02_0000;
const CLONE_NEWUSER: u64 = 0x1000_0000;

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
const BPF_JSET_K: u16 = 0x45;
const BPF_RET_K: u16 = 0x06;
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
const fn off_arg_lo(i: u32) -> u32 {
    16 + 8 * i
}

/// Largest address read from the app: `sockaddr_un` is 110 bytes.
const MAX_ADDR: usize = 128;
/// Control data read from the app, at most.
const MAX_CONTROL: usize = 1024;
/// `sendmmsg` messages looked at per call.
const MAX_MMSG: usize = 8;

/// The part of the filter language we need: a label is an index in the program.
enum Ins {
    Ld(u32),
    /// jump to `.1` if equal to the constant, else to `.2` (`None` = the next instruction)
    Jeq(u32, Option<&'static str>, Option<&'static str>),
    Jset(u32, Option<&'static str>, Option<&'static str>),
    Ret(u32),
    Label(&'static str),
}

/// Assemble the instructions into BPF. Jumps go forward only.
fn assemble(prog: &[Ins]) -> Vec<libc::sock_filter> {
    let mut labels = BTreeMap::new();
    let mut n = 0usize;
    for i in prog {
        match i {
            Ins::Label(l) => {
                labels.insert(*l, n);
            }
            _ => n += 1,
        }
    }
    let mut out = Vec::with_capacity(n);
    for i in prog {
        let at = out.len();
        let rel = |l: &Option<&'static str>| -> u8 {
            match l {
                None => 0,
                Some(name) => u8::try_from(labels[name] - (at + 1)).expect("BPF jump too far"),
            }
        };
        let ins = match i {
            Ins::Label(_) => continue,
            Ins::Ld(off) => libc::sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: *off },
            Ins::Jeq(k, t, f) => libc::sock_filter { code: BPF_JEQ_K, jt: rel(t), jf: rel(f), k: *k },
            Ins::Jset(k, t, f) => libc::sock_filter { code: BPF_JSET_K, jt: rel(t), jf: rel(f), k: *k },
            Ins::Ret(k) => libc::sock_filter { code: BPF_RET_K, jt: 0, jf: 0, k: *k },
        };
        out.push(ins);
    }
    out
}

/// The filter: which calls go to the supervisor. Other architectures than x86-64 are let through
/// (a limit of this measurement).
fn filter_program() -> Vec<libc::sock_filter> {
    use Ins::*;
    assemble(&[
        Ld(OFF_ARCH),
        Jeq(AUDIT_ARCH_X86_64, None, Some("allow")),
        Ld(OFF_NR),
        Jeq(SYS_CONNECT, Some("notify"), None),
        Jeq(SYS_SENDMSG, Some("notify"), None),
        Jeq(SYS_SENDMMSG, Some("notify"), None),
        Jeq(SYS_CLONE3, Some("notify"), None),
        Jeq(SYS_SENDTO, Some("sendto"), None),
        Jeq(SYS_CLONE, Some("clone"), None),
        Jeq(SYS_UNSHARE, Some("clone"), Some("allow")),
        // sendto: only with an address (argument 4, a 64-bit pointer, not null).
        Label("sendto"),
        Ld(off_arg_lo(4)),
        Jeq(0, None, Some("notify")),
        Ld(off_arg_lo(4) + 4),
        Jeq(0, Some("allow"), Some("notify")),
        // clone and unshare: only when a CLONE_NEW* flag is set (argument 0).
        Label("clone"),
        Ld(off_arg_lo(0)),
        Jset(CLONE_NEW_MASK, Some("notify"), Some("allow")),
        Label("notify"),
        Ret(SECCOMP_RET_USER_NOTIF),
        Label("allow"),
        Ret(SECCOMP_RET_ALLOW),
    ])
}

/// Address families by name, for the log.
fn family_name(f: u16) -> String {
    match f {
        0 => "AF_UNSPEC".into(),
        1 => "AF_UNIX".into(),
        2 => "AF_INET".into(),
        10 => "AF_INET6".into(),
        16 => "AF_NETLINK".into(),
        17 => "AF_PACKET".into(),
        38 => "AF_ALG".into(),
        40 => "AF_VSOCK".into(),
        n => format!("AF_{n}"),
    }
}

/// What a socket address says: family, and a text for the path or the IP address, and a port.
#[derive(Debug, PartialEq, Eq)]
struct Addr {
    family: u16,
    /// `path` for AF_UNIX (`@name` when abstract), `addr` for AF_INET and AF_INET6.
    text: Option<(&'static str, String)>,
    port: Option<u16>,
}

/// Read a `sockaddr` the way the kernel does for its length: the family from the first two bytes.
fn parse_sockaddr(b: &[u8]) -> Option<Addr> {
    if b.len() < 2 {
        return None;
    }
    let family = u16::from_le_bytes([b[0], b[1]]);
    let mut a = Addr { family, text: None, port: None };
    match family {
        1 => {
            let p = &b[2..];
            if p.is_empty() {
                a.text = Some(("path", String::new()));
            } else if p[0] == 0 {
                // Abstract: the name is every byte given, a NUL included.
                a.text = Some(("path", format!("@{}", String::from_utf8_lossy(&p[1..]))));
            } else {
                let end = p.iter().position(|c| *c == 0).unwrap_or(p.len());
                a.text = Some(("path", String::from_utf8_lossy(&p[..end]).into_owned()));
            }
        }
        2 if b.len() >= 8 => {
            a.port = Some(u16::from_be_bytes([b[2], b[3]]));
            a.text = Some(("addr", std::net::Ipv4Addr::new(b[4], b[5], b[6], b[7]).to_string()));
        }
        10 if b.len() >= 24 => {
            a.port = Some(u16::from_be_bytes([b[2], b[3]]));
            let mut o = [0u8; 16];
            o.copy_from_slice(&b[8..24]);
            a.text = Some(("addr", std::net::Ipv6Addr::from(o).to_string()));
        }
        _ => {}
    }
    Some(a)
}

/// What the control data of a message holds.
#[derive(Debug, Default, PartialEq, Eq)]
struct Control {
    present: bool,
    scm_rights: bool,
    /// file descriptors sent in SCM_RIGHTS messages
    fds: u32,
    other: bool,
}

/// Walk `cmsghdr`s (`len: u64, level: i32, type: i32`, data aligned to 8). `declared` is the length the
/// app gave; `buf` is what was read of it.
fn scan_control(buf: &[u8], declared: u64) -> Control {
    let mut c = Control { present: declared > 0, ..Control::default() };
    let mut off = 0usize;
    while off + 16 <= buf.len() {
        let len = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap()) as usize;
        let level = i32::from_le_bytes(buf[off + 8..off + 12].try_into().unwrap());
        let typ = i32::from_le_bytes(buf[off + 12..off + 16].try_into().unwrap());
        if len < 16 {
            break;
        }
        if level == 1 && typ == 1 {
            c.scm_rights = true;
            c.fds += ((len - 16) / 4) as u32;
        } else {
            c.other = true;
        }
        off += (len + 7) & !7;
    }
    c
}

fn json_str(b: &str) -> String {
    let mut s = String::with_capacity(b.len() + 2);
    s.push('"');
    for ch in b.chars() {
        match ch {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => s.push_str(&format!("\\u{:04x}", c as u32)),
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

/// Open the log file: created if missing, appended to, never through a symbolic link.
pub fn open_log(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("PANPAK_SOCKET_PROBE: cannot open {}", path.display()))
}

struct Notif {
    id: u64,
    tid: u32,
    nr: u32,
    args: [u64; 6],
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn parse_notif(b: &[u8; 80]) -> Notif {
    let mut args = [0u64; 6];
    for (i, a) in args.iter_mut().enumerate() {
        *a = u64_at(b, 32 + 8 * i);
    }
    Notif { id: u64_at(b, 0), tid: u32::from_le_bytes(b[8..12].try_into().unwrap()), nr: i32::from_le_bytes(b[16..20].try_into().unwrap()) as u32, args }
}

/// Read `len` bytes of the memory of thread `tid` at `addr`.
fn read_mem(tid: u32, addr: u64, len: usize) -> std::io::Result<Vec<u8>> {
    let f = File::open(format!("/proc/{tid}/mem"))?;
    let mut buf = vec![0u8; len];
    let mut got = 0;
    while got < len {
        match f.read_at(&mut buf[got..], addr + got as u64) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if got > 0 && e.raw_os_error() == Some(libc::EIO) => break,
            Err(e) => return Err(e),
        }
    }
    buf.truncate(got);
    Ok(buf)
}

struct Supervisor {
    log: File,
    counts: BTreeMap<String, u64>,
    t0: std::time::Instant,
    wall0: u128,
}

impl Supervisor {
    fn line(&mut self, tid: u32, syscall: &str, fields: &str) {
        let t = self.wall0 + self.t0.elapsed().as_millis();
        let l = format!("{{\"t\":{t},\"tid\":{tid},\"syscall\":\"{syscall}\"{fields}}}\n");
        let _ = self.log.write_all(l.as_bytes());
    }

    fn count(&mut self, key: String) {
        *self.counts.entry(key).or_insert(0) += 1;
    }

    fn addr_fields(a: &Addr) -> String {
        let mut s = format!(",\"family\":{}", json_str(&family_name(a.family)));
        if let Some((k, v)) = &a.text {
            s.push_str(&format!(",\"{k}\":{}", json_str(v)));
        }
        if let Some(p) = a.port {
            s.push_str(&format!(",\"port\":{p}"));
        }
        s
    }

    /// Read an address at `ptr` of `len` bytes; log it. `kind` names it in the counts.
    fn address(&mut self, n: &Notif, syscall: &str, ptr: u64, len: u64, extra: &str) {
        let take = (len as usize).min(MAX_ADDR);
        match read_mem(n.tid, ptr, take) {
            Ok(b) => match parse_sockaddr(&b) {
                Some(a) => {
                    self.count(format!("{syscall}:{}:addr", family_name(a.family)));
                    let f = format!("{}{extra}", Self::addr_fields(&a));
                    self.line(n.tid, syscall, &f);
                }
                None => {
                    self.count(format!("{syscall}:short:addr"));
                    self.line(n.tid, syscall, &format!(",\"error\":\"address shorter than 2 bytes\"{extra}"));
                }
            },
            Err(e) => {
                self.count(format!("{syscall}:unreadable:addr"));
                self.line(n.tid, syscall, &format!(",\"error\":{}{extra}", json_str(&format!("read: {e}"))));
            }
        }
    }

    /// One `msghdr` at `ptr` (from `sendmsg` or an element of `sendmmsg`).
    fn msghdr(&mut self, n: &Notif, syscall: &str, ptr: u64) {
        let h = match read_mem(n.tid, ptr, 56) {
            Ok(h) if h.len() == 56 => h,
            Ok(_) | Err(_) => {
                self.count(format!("{syscall}:unreadable"));
                self.line(n.tid, syscall, ",\"error\":\"msghdr not readable\"");
                return;
            }
        };
        let (name, namelen) = (u64_at(&h, 0), u32::from_le_bytes(h[8..12].try_into().unwrap()) as u64);
        let (ctl, ctllen) = (u64_at(&h, 32), u64_at(&h, 40));
        let control = if ctl != 0 && ctllen > 0 {
            read_mem(n.tid, ctl, (ctllen as usize).min(MAX_CONTROL)).map(|b| scan_control(&b, ctllen)).unwrap_or(Control { present: true, ..Control::default() })
        } else {
            Control::default()
        };
        if control.scm_rights {
            self.count(format!("{syscall}:scm_rights"));
        }
        if control.present {
            self.count(format!("{syscall}:control"));
        }
        let extra = format!(
            ",\"control\":{},\"scm_rights\":{},\"fds\":{}",
            control.present, control.scm_rights, control.fds
        );
        if name != 0 && namelen > 0 {
            self.address(n, syscall, name, namelen, &extra);
        } else if control.present {
            self.count(format!("{syscall}:noaddr:control"));
            self.line(n.tid, syscall, &format!(",\"family\":\"connected\"{extra}"));
        } else {
            self.count(format!("{syscall}:noaddr"));
        }
    }

    fn handle(&mut self, n: &Notif) {
        match n.nr {
            SYS_CONNECT => self.address(n, "connect", n.args[1], n.args[2], ""),
            SYS_SENDTO => self.address(n, "sendto", n.args[4], n.args[5], ""),
            SYS_SENDMSG => self.msghdr(n, "sendmsg", n.args[1]),
            SYS_SENDMMSG => {
                for i in 0..(n.args[2] as usize).min(MAX_MMSG) {
                    self.msghdr(n, "sendmmsg", n.args[1] + 64 * i as u64);
                }
            }
            SYS_CLONE | SYS_UNSHARE => {
                let (name, flags) = (if n.nr == SYS_CLONE { "clone" } else { "unshare" }, n.args[0] & 0xffff_ffff);
                self.namespace_call(n, name, flags);
            }
            SYS_CLONE3 => {
                // struct clone_args starts with the flags (u64).
                match read_mem(n.tid, n.args[0], 8) {
                    Ok(b) if b.len() == 8 => self.namespace_call(n, "clone3", u64_at(&b, 0)),
                    _ => self.count("clone3:unreadable".to_string()),
                }
            }
            _ => {}
        }
    }

    /// Log a `clone`-like call only when it asks for a new namespace; count the others.
    fn namespace_call(&mut self, n: &Notif, name: &str, flags: u64) {
        if flags & u64::from(CLONE_NEW_MASK) == 0 {
            self.count(format!("{name}:no_namespace"));
            return;
        }
        let user = flags & CLONE_NEWUSER != 0;
        self.count(format!("{name}:{}", if user { "newuser" } else { "newns_other" }));
        self.line(n.tid, name, &format!(",\"flags\":\"0x{flags:x}\",\"newuser\":{user}"));
    }

    fn summary(&mut self) {
        let items: Vec<String> = self.counts.iter().map(|(k, v)| format!("{}:{v}", json_str(k))).collect();
        let _ = self.log.write_all(format!("{{\"summary\":{{{}}}}}\n", items.join(",")).as_bytes());
    }
}

/// The child that gets the signals the supervisor receives.
static CHILD: AtomicI32 = AtomicI32::new(0);

extern "C" fn forward_signal(sig: libc::c_int) {
    let pid = CHILD.load(Ordering::Relaxed);
    if pid > 0 {
        // SAFETY: kill is async-signal-safe.
        unsafe { libc::kill(pid, sig) };
    }
}

fn last_err() -> std::io::Error {
    std::io::Error::last_os_error()
}

/// In the forked child: set the death signal, install the filter, hand the listener to the parent,
/// wait for its acknowledgement. Returns the error text on failure.
unsafe fn child_setup(prog: &libc::sock_fprog, to_parent: libc::c_int, from_parent: libc::c_int) -> std::result::Result<(), std::io::Error> {
    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong, 0, 0, 0) != 0 {
        return Err(last_err());
    }
    if libc::getppid() == 1 {
        return Err(std::io::Error::other("supervisor already gone"));
    }
    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1 as libc::c_ulong, 0, 0, 0) != 0 {
        return Err(last_err());
    }
    let fd = libc::syscall(libc::SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_NEW_LISTENER, prog as *const _);
    let msg: i32 = if fd < 0 { -(last_err().raw_os_error().unwrap_or(libc::EINVAL)) } else { fd as i32 };
    if libc::write(to_parent, msg.to_ne_bytes().as_ptr().cast(), 4) != 4 {
        return Err(last_err());
    }
    if fd < 0 {
        return Err(std::io::Error::from_raw_os_error(-msg));
    }
    let mut ack = [0u8; 1];
    if libc::read(from_parent, ack.as_mut_ptr().cast(), 1) != 1 {
        return Err(std::io::Error::other("no acknowledgement from the supervisor"));
    }
    Ok(())
}

/// Run `cmd` as a child under the probe and return its exit code (128 + n if a signal ended it).
/// The supervisor waits for every process that descends from the app.
pub fn run(mut cmd: Command, log: File) -> Result<i32> {
    eprintln!("panpak: SOCKET PROBE is on: measurement only, it checks and blocks nothing (PANPAK_SOCKET_PROBE)");
    let prog_insns = filter_program();
    let prog = libc::sock_fprog { len: prog_insns.len() as u16, filter: prog_insns.as_ptr() as *mut libc::sock_filter };
    let mut c2p = [0; 2];
    let mut p2c = [0; 2];
    // SAFETY: plain system calls on arrays of the right size.
    unsafe {
        if libc::pipe2(c2p.as_mut_ptr(), libc::O_CLOEXEC) != 0 || libc::pipe2(p2c.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            bail!("pipe: {}", last_err());
        }
        if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1 as libc::c_ulong, 0, 0, 0) != 0 {
            bail!("PR_SET_CHILD_SUBREAPER: {}", last_err());
        }
    }
    // SAFETY: the stub has one thread, so the child may use the allocator before exec.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        bail!("fork: {}", last_err());
    }
    if pid == 0 {
        // SAFETY: in the child, after fork; the pipes are valid.
        let res = unsafe { child_setup(&prog, c2p[1], p2c[0]) };
        if let Err(e) = res {
            eprintln!("panpak: socket probe: cannot set up the filter: {e}");
            unsafe { libc::_exit(126) };
        }
        let err = cmd.exec();
        eprintln!("panpak: exec failed: {err}");
        unsafe { libc::_exit(127) };
    }
    CHILD.store(pid, Ordering::Relaxed);
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT, libc::SIGUSR1, libc::SIGUSR2] {
        // SAFETY: the handler only calls kill.
        unsafe { libc::signal(sig, forward_signal as extern "C" fn(libc::c_int) as libc::sighandler_t) };
    }
    // SAFETY: fds from pipe2 above; the parent closes the child's ends.
    let (mut from_child, mut to_child) = unsafe {
        libc::close(c2p[1]);
        libc::close(p2c[0]);
        (File::from_raw_fd(c2p[0]), File::from_raw_fd(p2c[1]))
    };
    let mut word = [0u8; 4];
    from_child.read_exact(&mut word).context("the child did not report the listener")?;
    let child_fd = i32::from_ne_bytes(word);
    if child_fd < 0 {
        let mut st = 0;
        unsafe { libc::waitpid(pid, &mut st, 0) };
        bail!("seccomp user notification is not available here: {}", std::io::Error::from_raw_os_error(-child_fd));
    }
    // SAFETY: pidfd_open and pidfd_getfd (Linux 5.6) take plain integers.
    let lfd = unsafe {
        let pidfd = libc::syscall(libc::SYS_pidfd_open, pid, 0);
        if pidfd < 0 {
            bail!("pidfd_open: {}", last_err());
        }
        let l = libc::syscall(libc::SYS_pidfd_getfd, pidfd, child_fd, 0);
        libc::close(pidfd as i32);
        if l < 0 {
            bail!("pidfd_getfd: {}", last_err());
        }
        l as i32
    };
    to_child.write_all(&[1]).context("acknowledge to the child")?;
    drop(to_child);
    drop(from_child);

    let wall0 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let mut sup = Supervisor { log, counts: BTreeMap::new(), t0: std::time::Instant::now(), wall0 };
    let mut main_status: Option<i32> = None;
    let mut buf = [0u8; 80];
    // The summary is written again every two seconds when the counts changed, so a log of an app that
    // was killed still ends with a summary line (the last one counts).
    let (mut last_summary, mut last_total) = (std::time::Instant::now(), 0u64);
    loop {
        if last_summary.elapsed() >= std::time::Duration::from_secs(2) {
            let total: u64 = sup.counts.values().sum();
            if total != last_total {
                sup.summary();
                last_total = total;
            }
            last_summary = std::time::Instant::now();
        }
        let mut p = libc::pollfd { fd: lfd, events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&mut p, 1, 100) };
        if r > 0 && p.revents & libc::POLLIN != 0 {
            buf.fill(0);
            // SAFETY: the buffer has the size of struct seccomp_notif (80 bytes).
            if unsafe { libc::ioctl(lfd, IOCTL_NOTIF_RECV, buf.as_mut_ptr()) } == 0 {
                let n = parse_notif(&buf);
                sup.handle(&n);
                // The call may have ended while it was read (a signal, a kill): count such lines.
                // SAFETY: the id is a u64 the kernel gave us.
                if unsafe { libc::ioctl(lfd, IOCTL_NOTIF_ID_VALID, &n.id as *const u64) } != 0 {
                    sup.count("stale_reads".to_string());
                }
                let mut resp = [0u8; 24];
                resp[0..8].copy_from_slice(&n.id.to_ne_bytes());
                resp[20..24].copy_from_slice(&NOTIF_FLAG_CONTINUE.to_ne_bytes());
                // SAFETY: struct seccomp_notif_resp is 24 bytes.
                unsafe { libc::ioctl(lfd, IOCTL_NOTIF_SEND, resp.as_ptr()) };
            }
        }
        // Reap every child, the app's own descendants included (this process is their subreaper).
        loop {
            let mut st = 0;
            // SAFETY: waitpid with a valid status pointer.
            let w = unsafe { libc::waitpid(-1, &mut st, libc::WNOHANG) };
            if w == pid {
                main_status = Some(if libc::WIFEXITED(st) {
                    libc::WEXITSTATUS(st)
                } else if libc::WIFSIGNALED(st) {
                    128 + libc::WTERMSIG(st)
                } else {
                    1
                });
            }
            if w <= 0 {
                if w < 0 && last_err().raw_os_error() == Some(libc::ECHILD) {
                    sup.summary();
                    return Ok(main_status.unwrap_or(1));
                }
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sockaddr_in(port: u16, ip: [u8; 4]) -> Vec<u8> {
        let mut b = vec![2, 0];
        b.extend_from_slice(&port.to_be_bytes());
        b.extend_from_slice(&ip);
        b.extend_from_slice(&[0; 8]);
        b
    }

    #[test]
    fn sockaddr_families() {
        let a = parse_sockaddr(&sockaddr_in(53, [127, 0, 0, 53])).unwrap();
        assert_eq!((a.family, a.port), (2, Some(53)));
        assert_eq!(a.text, Some(("addr", "127.0.0.53".to_string())));
        let mut v6 = vec![10, 0, 0x01, 0xbb, 0, 0, 0, 0];
        v6.extend_from_slice(&std::net::Ipv6Addr::LOCALHOST.octets());
        v6.extend_from_slice(&[0; 4]);
        let a = parse_sockaddr(&v6).unwrap();
        assert_eq!((a.family, a.port, a.text), (10, Some(443), Some(("addr", "::1".to_string()))));
        let mut un = vec![1, 0];
        un.extend_from_slice(b"/run/user/1000/bus\0junk");
        assert_eq!(parse_sockaddr(&un).unwrap().text, Some(("path", "/run/user/1000/bus".to_string())));
        // A path with no NUL ends at the length given.
        let mut un = vec![1, 0];
        un.extend_from_slice(b"/tmp/x");
        assert_eq!(parse_sockaddr(&un).unwrap().text, Some(("path", "/tmp/x".to_string())));
        // Abstract and unnamed.
        let mut ab = vec![1, 0, 0];
        ab.extend_from_slice(b"dbus-x");
        assert_eq!(parse_sockaddr(&ab).unwrap().text, Some(("path", "@dbus-x".to_string())));
        assert_eq!(parse_sockaddr(&[1, 0]).unwrap().text, Some(("path", String::new())));
        // Short, and other families.
        assert_eq!(parse_sockaddr(&[1]), None);
        assert_eq!(parse_sockaddr(&[16, 0, 0, 0]).unwrap().family, 16);
        assert_eq!(parse_sockaddr(&[2, 0, 0, 80]).unwrap().port, None, "AF_INET shorter than 8 bytes has no port");
        assert_eq!(family_name(16), "AF_NETLINK");
        assert_eq!(family_name(99), "AF_99");
    }

    fn cmsg(level: i32, typ: i32, data: &[u8]) -> Vec<u8> {
        let len = 16 + data.len();
        let mut b = (len as u64).to_le_bytes().to_vec();
        b.extend_from_slice(&level.to_le_bytes());
        b.extend_from_slice(&typ.to_le_bytes());
        b.extend_from_slice(data);
        while !b.len().is_multiple_of(8) {
            b.push(0);
        }
        b
    }

    #[test]
    fn control_data_scan() {
        let rights = cmsg(1, 1, &[3, 0, 0, 0, 4, 0, 0, 0]);
        let c = scan_control(&rights, rights.len() as u64);
        assert_eq!(c, Control { present: true, scm_rights: true, fds: 2, other: false });
        let mut both = cmsg(1, 2, &[0; 12]);
        both.extend(cmsg(1, 1, &[5, 0, 0, 0]));
        let c = scan_control(&both, both.len() as u64);
        assert!(c.scm_rights && c.other && c.fds == 1, "{c:?}");
        // Garbage or truncated headers end the walk without a panic.
        assert!(!scan_control(&[0xff; 15], 15).scm_rights);
        assert!(!scan_control(&[0; 32], 32).scm_rights);
        assert_eq!(scan_control(&[], 0), Control::default());
    }

    #[test]
    fn json_is_escaped() {
        assert_eq!(json_str("a\"b\\c\n\u{1}"), "\"a\\\"b\\\\c\\u000a\\u0001\"");
        assert_eq!(json_str("/run/ü"), "\"/run/ü\"");
    }

    /// Run the filter on sample calls: a tiny BPF interpreter for the instructions we emit.
    fn run_filter(nr: u32, args: [u64; 6], arch: u32) -> u32 {
        let prog = filter_program();
        let (mut a, mut pc) = (0u32, 0usize);
        loop {
            let i = &prog[pc];
            pc += 1;
            match i.code {
                BPF_LD_W_ABS => {
                    a = match i.k {
                        OFF_NR => nr,
                        OFF_ARCH => arch,
                        k => {
                            let n = ((k - 16) / 8) as usize;
                            let w = args[n];
                            if (k - 16).is_multiple_of(8) { w as u32 } else { (w >> 32) as u32 }
                        }
                    }
                }
                BPF_JEQ_K => pc += if a == i.k { i.jt } else { i.jf } as usize,
                BPF_JSET_K => pc += if a & i.k != 0 { i.jt } else { i.jf } as usize,
                BPF_RET_K => return i.k,
                c => panic!("code {c:#x}"),
            }
        }
    }

    #[test]
    fn filter_sends_the_right_calls_to_the_supervisor() {
        let z = [0u64; 6];
        let n = SECCOMP_RET_USER_NOTIF;
        let ok = SECCOMP_RET_ALLOW;
        for nr in [SYS_CONNECT, SYS_SENDMSG, SYS_SENDMMSG, SYS_CLONE3] {
            assert_eq!(run_filter(nr, z, AUDIT_ARCH_X86_64), n, "nr {nr}");
        }
        // sendto: only with an address.
        assert_eq!(run_filter(SYS_SENDTO, z, AUDIT_ARCH_X86_64), ok);
        assert_eq!(run_filter(SYS_SENDTO, [0, 0, 0, 0, 0x7fff_0000_1000, 16], AUDIT_ARCH_X86_64), n);
        assert_eq!(run_filter(SYS_SENDTO, [0, 0, 0, 0, 0x1000, 16], AUDIT_ARCH_X86_64), n);
        assert_eq!(run_filter(SYS_SENDTO, [0, 0, 0, 0, 1 << 40, 16], AUDIT_ARCH_X86_64), n, "high word set");
        // clone and unshare: only with a new namespace.
        for nr in [SYS_CLONE, SYS_UNSHARE] {
            assert_eq!(run_filter(nr, [0x11, 0, 0, 0, 0, 0], AUDIT_ARCH_X86_64), ok);
            assert_eq!(run_filter(nr, [CLONE_NEWUSER | 0x11, 0, 0, 0, 0, 0], AUDIT_ARCH_X86_64), n);
            assert_eq!(run_filter(nr, [0x2000_0000, 0, 0, 0, 0, 0], AUDIT_ARCH_X86_64), n, "CLONE_NEWPID");
            assert_eq!(run_filter(nr, [0x0002_0000, 0, 0, 0, 0, 0], AUDIT_ARCH_X86_64), n, "CLONE_NEWNS");
        }
        // Everything else, and other architectures, pass.
        assert_eq!(run_filter(0, z, AUDIT_ARCH_X86_64), ok);
        assert_eq!(run_filter(SYS_CONNECT, z, 0x4000_0028), ok);
    }

    #[test]
    fn filter_fits_the_kernel_limit() {
        let p = filter_program();
        assert!(p.len() < 4096 && p.len() > 10);
    }
}
