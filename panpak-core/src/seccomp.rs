// SPDX-License-Identifier: MIT OR Apache-2.0
//! The seccomp filter of the inner stub: a deny list, installed as the last step before the app is executed.
//!
//! Landlock limits what the app can reach in the file system, the network and the other processes. It does not
//! limit system calls that are legal for the user but have a large kernel attack surface (`io_uring`, `bpf`,
//! `userfaultfd`, user namespaces ...). This filter refuses them and lets everything else through. A refused
//! call returns an error number; the process is never killed, so a program that probes for a feature goes on.
//!
//! The program is made here, without a library: a short list of comparisons in classic BPF. The numbers come from
//! `libc::SYS_*`, so one table serves x86_64 and aarch64. The table is data (`RULES`); the generator and the test
//! interpreter below are small enough to read in one sitting.
//!
//! Policy (the reason of each line is in `RULES`):
//! - a call that is not native (the 32-bit ABI, or x32 on x86_64) is refused as a whole with `EPERM`;
//! - `ENOSYS` for calls that have a fallback in every runtime library, so that the library uses it:
//!   `clone3` (glibc and musl go back to `clone` only on `ENOSYS`) and the families that a program may
//!   not have at all (`io_uring`, the new mount API);
//! - `EPERM` for the rest.
use anyhow::Result;
use crate::{coded, ErrorCode};

// The classic BPF instruction set, the part that is used here.
const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
const BPF_JSET_K: u16 = 0x45;
const BPF_RET_K: u16 = 0x06;

const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;

/// `struct seccomp_data`: `nr` at 0, `arch` at 4, the arguments (64 bits each) from 16.
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
const fn off_arg(i: u32) -> u32 {
    16 + 8 * i
}

#[cfg(target_arch = "x86_64")]
const NATIVE_ARCH: u32 = 0xC000_003E; // AUDIT_ARCH_X86_64
#[cfg(target_arch = "aarch64")]
const NATIVE_ARCH: u32 = 0xC000_00B7; // AUDIT_ARCH_AARCH64
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const NATIVE_ARCH: u32 = 0;

/// `__X32_SYSCALL_BIT`: a call with this bit set uses the x32 table, and arrives with the x86_64 architecture.
#[cfg(target_arch = "x86_64")]
const X32_BIT: u32 = 0x4000_0000;

/// `CLONE_NEW*` for `clone` and `unshare`: mount, cgroup, uts, ipc, user, pid, net (and time for `unshare`).
const NEW_NAMESPACES: u32 = 0x0002_0000 | 0x0200_0000 | 0x0400_0000 | 0x0800_0000 | 0x1000_0000 | 0x2000_0000 | 0x4000_0000;
const CLONE_NEWTIME: u32 = 0x0000_0080;

const TIOCSTI: u32 = 0x5412;
const TIOCLINUX: u32 = 0x541C;

/// The values of `personality(2)` that remain allowed: Linux, Linux 32-bit, and both with UNAME26. Any other
/// value changes how the kernel lays out memory (no randomisation, executable stack by default, page zero).
const PERSONALITY_OK: &[u32] = &[0x0, 0x8, 0x0002_0000, 0x0002_0008, 0xffff_ffff];

const EPERM: u32 = libc::EPERM as u32;
const ENOSYS: u32 = libc::ENOSYS as u32;

/// How a call is judged.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Check {
    /// Always refused with this error number.
    Deny(u32),
    /// `ioctl`: refused when the request is one of the terminal injection ones.
    Ioctl,
    /// `personality`: refused unless the value is in `PERSONALITY_OK`.
    Personality,
    /// `clone` and `unshare`: refused when the flags ask for a new namespace (`time` too for `unshare`).
    Namespaces { time: bool },
}

struct Rule {
    nr: i64,
    name: &'static str,
    check: Check,
    why: &'static str,
}

macro_rules! rule {
    ($sys:ident, $check:expr, $why:expr) => {
        Rule { nr: libc::$sys as i64, name: stringify!($sys), check: $check, why: $why }
    };
}

const DENY: Check = Check::Deny(EPERM);
const NOSYS: Check = Check::Deny(ENOSYS);

/// The deny list. The reason of each entry is part of the data so that a reader
/// can say why a call is on it.
const RULES: &[Rule] = &[
    // Large attack surface, reachable by any user: the usual starting point of kernel exploits.
    rule!(SYS_io_uring_setup, NOSYS, "io_uring: a large, fast-changing kernel surface that bypasses the other filters of file and network calls"),
    rule!(SYS_io_uring_enter, NOSYS, "io_uring"),
    rule!(SYS_io_uring_register, NOSYS, "io_uring"),
    rule!(SYS_bpf, DENY, "loads programs into the kernel"),
    rule!(SYS_userfaultfd, DENY, "lets an attacker stop the kernel in the middle of a copy; a tool of race exploits"),
    rule!(SYS_perf_event_open, DENY, "a large surface and a source of side-channel data"),
    rule!(SYS_keyctl, DENY, "the kernel key ring is shared by processes of the user, outside the file system"),
    rule!(SYS_add_key, DENY, "kernel key ring"),
    rule!(SYS_request_key, DENY, "kernel key ring"),
    // Other processes.
    rule!(SYS_ptrace, DENY, "reads and writes the memory of other processes of the user"),
    rule!(SYS_process_vm_readv, DENY, "reads the memory of another process"),
    rule!(SYS_process_vm_writev, DENY, "writes the memory of another process"),
    rule!(SYS_pidfd_getfd, DENY, "takes a file descriptor out of another process"),
    rule!(SYS_kcmp, DENY, "compares the descriptors of other processes (an information leak between processes)"),
    // Kernel and machine.
    rule!(SYS_kexec_load, DENY, "replaces the kernel"),
    rule!(SYS_kexec_file_load, DENY, "replaces the kernel"),
    rule!(SYS_init_module, DENY, "loads kernel code"),
    rule!(SYS_finit_module, DENY, "loads kernel code"),
    rule!(SYS_delete_module, DENY, "unloads kernel code"),
    rule!(SYS_syslog, DENY, "reads the kernel log (addresses and hardware data)"),
    rule!(SYS_acct, DENY, "process accounting writes a file as root"),
    rule!(SYS_swapon, DENY, "machine-wide"),
    rule!(SYS_swapoff, DENY, "machine-wide"),
    rule!(SYS_reboot, DENY, "machine-wide"),
    // Mounts: old and new API. The view of the app is fixed before it starts.
    rule!(SYS_mount, DENY, "the view of the app is fixed before it starts"),
    rule!(SYS_umount2, DENY, "mounts"),
    rule!(SYS_pivot_root, DENY, "mounts"),
    rule!(SYS_chroot, DENY, "a way to leave the paths that the view sets"),
    rule!(SYS_fsopen, NOSYS, "new mount API"),
    rule!(SYS_fsconfig, NOSYS, "new mount API"),
    rule!(SYS_fsmount, NOSYS, "new mount API"),
    rule!(SYS_fspick, NOSYS, "new mount API"),
    rule!(SYS_move_mount, NOSYS, "new mount API"),
    rule!(SYS_open_tree, NOSYS, "new mount API (also clones a mount tree)"),
    rule!(SYS_mount_setattr, NOSYS, "new mount API"),
    // Namespaces and file handles.
    rule!(SYS_setns, DENY, "enters the namespaces of another process"),
    rule!(SYS_unshare, Check::Namespaces { time: true }, "a new user namespace gives a program the capabilities of its own that open the rest of the kernel"),
    rule!(SYS_clone, Check::Namespaces { time: false }, "the same, through clone"),
    rule!(SYS_clone3, NOSYS, "its flags are in memory that a filter cannot read; ENOSYS makes the C library use clone, which is filtered"),
    rule!(SYS_open_by_handle_at, DENY, "opens a file by a handle, outside every path rule"),
    // Terminal injection and dangerous process personality.
    rule!(SYS_ioctl, Check::Ioctl, "TIOCSTI and TIOCLINUX put keystrokes into the terminal of the user"),
    rule!(SYS_personality, Check::Personality, "READ_IMPLIES_EXEC, no randomisation, page zero: they weaken the memory protections"),
];

/// Calls that exist on x86 only, and old calls that no program of today needs.
#[cfg(target_arch = "x86_64")]
const RULES_ARCH: &[Rule] = &[
    rule!(SYS_iopl, DENY, "direct access to the hardware ports"),
    rule!(SYS_ioperm, DENY, "direct access to the hardware ports"),
    rule!(SYS_uselib, DENY, "obsolete: loads a library by a path, outside the loader"),
    rule!(SYS_ustat, DENY, "obsolete"),
    rule!(SYS_sysfs, DENY, "obsolete"),
    rule!(SYS__sysctl, DENY, "obsolete and removed from the kernel"),
    rule!(SYS_nfsservctl, DENY, "obsolete and removed from the kernel"),
];
#[cfg(not(target_arch = "x86_64"))]
const RULES_ARCH: &[Rule] = &[];

/// Every rule of this architecture.
fn all_rules() -> impl Iterator<Item = &'static Rule> {
    RULES.iter().chain(RULES_ARCH.iter())
}

/// One instruction (`struct sock_filter`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Insn {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

const fn stmt(code: u16, k: u32) -> Insn {
    Insn { code, jt: 0, jf: 0, k }
}

const fn jump(code: u16, k: u32, jt: u8, jf: u8) -> Insn {
    Insn { code, jt, jf, k }
}

fn ret_errno(e: u32) -> Insn {
    stmt(BPF_RET_K, SECCOMP_RET_ERRNO | (e & 0xffff))
}

/// The program, as a list of instructions. Layout:
/// 1. a different architecture: refused (this covers the 32-bit ABIs);
/// 2. x32 (x86_64 only): refused;
/// 3. for each rule: `jeq nr -> next rule`, then the rule's own block, which always ends in a `ret`.
///    Since each block returns, no block needs to load the number again.
/// 4. allow.
pub fn build() -> Result<Vec<Insn>> {
    if NATIVE_ARCH == 0 {
        return Err(coded(ErrorCode::E3002, "the seccomp filter does not cover this architecture"));
    }
    let mut p = vec![
        stmt(BPF_LD_W_ABS, OFF_ARCH),
        jump(BPF_JEQ_K, NATIVE_ARCH, 1, 0),
        ret_errno(EPERM),
        stmt(BPF_LD_W_ABS, OFF_NR),
    ];
    #[cfg(target_arch = "x86_64")]
    {
        // JSET: the bit is set -> the call is x32. The number stays in A for the rules.
        p.push(jump(BPF_JSET_K, X32_BIT, 0, 1));
        p.push(ret_errno(EPERM));
    }
    for r in all_rules() {
        let nr = u32::try_from(r.nr).map_err(|_| coded(ErrorCode::E3002, format!("bad system call number for {}", r.name)))?;
        let block = block_of(r.check);
        let skip = u8::try_from(block.len()).map_err(|_| coded(ErrorCode::E3002, "seccomp block too long"))?;
        p.push(jump(BPF_JEQ_K, nr, 0, skip));
        p.extend(block);
    }
    p.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
    if p.len() > 4096 {
        return Err(coded(ErrorCode::E3002, "seccomp program too long"));
    }
    Ok(p)
}

/// The instructions that follow `jeq nr` for one rule. Always ends in a `ret`.
fn block_of(c: Check) -> Vec<Insn> {
    match c {
        Check::Deny(e) => vec![ret_errno(e)],
        Check::Ioctl => vec![
            stmt(BPF_LD_W_ABS, off_arg(1)),
            jump(BPF_JEQ_K, TIOCSTI, 2, 0),
            jump(BPF_JEQ_K, TIOCLINUX, 1, 0),
            stmt(BPF_RET_K, SECCOMP_RET_ALLOW),
            ret_errno(EPERM),
        ],
        Check::Personality => {
            // One compare per allowed value: equal -> allow; at the end, refuse.
            let n = PERSONALITY_OK.len();
            let mut v = vec![stmt(BPF_LD_W_ABS, off_arg(0))];
            for (i, k) in PERSONALITY_OK.iter().enumerate() {
                // jump to the `ret allow` after the last compare
                v.push(jump(BPF_JEQ_K, *k, (n - i) as u8, 0));
            }
            v.push(ret_errno(EPERM));
            v.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
            v
        }
        Check::Namespaces { time } => {
            let mask = NEW_NAMESPACES | if time { CLONE_NEWTIME } else { 0 };
            vec![
                stmt(BPF_LD_W_ABS, off_arg(0)),
                jump(BPF_JSET_K, mask, 0, 1),
                ret_errno(EPERM),
                stmt(BPF_RET_K, SECCOMP_RET_ALLOW),
            ]
        }
    }
}

/// The list as text, for `panpak doctor`: one line per call, with the error it returns and the reason.
pub fn describe() -> Vec<String> {
    all_rules()
        .map(|r| {
            let how = match r.check {
                Check::Deny(e) if e == ENOSYS => "ENOSYS",
                Check::Deny(_) => "EPERM",
                Check::Ioctl => "EPERM for TIOCSTI, TIOCLINUX",
                Check::Personality => "EPERM unless the value is a plain Linux one",
                Check::Namespaces { .. } => "EPERM when a new namespace is asked",
            };
            format!("{:<22} {:<10} {}", r.name.trim_start_matches("SYS_"), how, r.why)
        })
        .collect()
}

/// Install the filter on this process (all its threads). `no_new_privs` is set first: it is what lets a
/// process without privileges install a filter, and it stays for the app and every program it runs.
pub fn install() -> Result<()> {
    let prog = build()?;
    #[repr(C)]
    struct Fprog {
        len: u16,
        filter: *const Insn,
    }
    let fprog = Fprog { len: prog.len() as u16, filter: prog.as_ptr() };
    // SAFETY: prctl with integer arguments only.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1 as libc::c_ulong, 0, 0, 0) } != 0 {
        return Err(coded(ErrorCode::E3002, format!("no_new_privs: {}", std::io::Error::last_os_error())));
    }
    // SAFETY: `fprog` and the array it points to live to the end of the call; the kernel copies the program.
    let r = unsafe { libc::syscall(libc::SYS_seccomp, 1 /* SET_MODE_FILTER */, 1 /* FLAG_TSYNC */, &fprog as *const Fprog) };
    if r != 0 {
        let e = std::io::Error::last_os_error();
        return Err(coded(ErrorCode::E3002, format!("cannot install the seccomp filter: {e}")));
    }
    Ok(())
}

/// The mode that `/proc/self/status` reports: `(NoNewPrivs, Seccomp)`. For `doctor` and the tests.
pub fn self_status() -> Option<(u32, u32)> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let get = |key: &str| s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().parse::<u32>().ok());
    Some((get("NoNewPrivs:")?, get("Seccomp:")?))
}

/// Whether the kernel can install a filter at all: a filter that is empty of effect (`getpid` allowed) is
/// installed in a child process and the result is read. For `doctor`.
pub fn kernel_supports() -> bool {
    // SAFETY: plain prctl query; `PR_GET_SECCOMP` returns the mode, or -1/EINVAL if the kernel has no seccomp.
    let mode = unsafe { libc::prctl(libc::PR_GET_SECCOMP) };
    if mode < 0 {
        return false;
    }
    // SAFETY: a query with a null program: EFAULT means the filter mode exists, EINVAL that it does not.
    let r = unsafe { libc::syscall(libc::SYS_seccomp, 1, 0, std::ptr::null::<u8>()) };
    r != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A classic BPF interpreter for the instructions used here, to run the program on `seccomp_data`.
    fn run(prog: &[Insn], arch: u32, nr: u32, args: [u64; 6]) -> u32 {
        let load = |off: u32| -> u32 {
            match off {
                0 => nr,
                4 => arch,
                16..=63 if (off - 16) % 8 == 0 => args[((off - 16) / 8) as usize] as u32,
                _ => panic!("unexpected load at {off}"),
            }
        };
        let (mut a, mut pc) = (0u32, 0usize);
        for _ in 0..10_000 {
            let i = prog[pc];
            pc += 1;
            match i.code {
                BPF_LD_W_ABS => a = load(i.k),
                BPF_JEQ_K => pc += if a == i.k { i.jt } else { i.jf } as usize,
                BPF_JSET_K => pc += if a & i.k != 0 { i.jt } else { i.jf } as usize,
                BPF_RET_K => return i.k,
                c => panic!("unexpected instruction {c:#x}"),
            }
        }
        panic!("the program did not end");
    }

    fn judge(nr: i64, args: [u64; 6]) -> u32 {
        run(&build().unwrap(), NATIVE_ARCH, nr as u32, args)
    }

    fn errno(e: u32) -> u32 {
        SECCOMP_RET_ERRNO | e
    }

    #[test]
    fn program_is_valid_and_short() {
        let p = build().unwrap();
        assert!(p.len() < 600, "{} instructions", p.len());
        // Every jump stays inside the program, and the last instruction is a return.
        for (i, ins) in p.iter().enumerate() {
            if ins.code == BPF_JEQ_K || ins.code == BPF_JSET_K {
                assert!(i + 1 + ins.jt.max(ins.jf) as usize <= p.len() - 1, "jump out of the program at {i}");
            }
        }
        assert_eq!(p.last().unwrap().code, BPF_RET_K);
    }

    #[test]
    fn every_simple_rule_returns_its_error() {
        for r in all_rules() {
            if let Check::Deny(e) = r.check {
                assert_eq!(judge(r.nr, [0; 6]), errno(e), "{}", r.name);
            }
        }
    }

    #[test]
    fn base_list_is_covered() {
        // The names of the base list of the task: each must be on the list (by name), so that a rename or a
        // removal in `RULES` is a deliberate change of this test.
        let names: Vec<&str> = all_rules().map(|r| r.name).collect();
        for want in [
            "SYS_io_uring_setup", "SYS_io_uring_enter", "SYS_io_uring_register", "SYS_bpf", "SYS_userfaultfd", "SYS_keyctl",
            "SYS_add_key", "SYS_request_key", "SYS_perf_event_open", "SYS_ptrace", "SYS_process_vm_readv", "SYS_process_vm_writev",
            "SYS_kexec_load", "SYS_kexec_file_load", "SYS_init_module", "SYS_finit_module", "SYS_delete_module", "SYS_mount",
            "SYS_umount2", "SYS_pivot_root", "SYS_fsopen", "SYS_fsmount", "SYS_move_mount", "SYS_open_tree", "SYS_unshare",
            "SYS_clone", "SYS_clone3", "SYS_setns", "SYS_personality", "SYS_open_by_handle_at", "SYS_ioctl", "SYS_syslog",
            "SYS_acct", "SYS_swapon", "SYS_reboot",
        ] {
            assert!(names.contains(&want), "{want} is not on the list");
        }
    }

    #[test]
    fn no_duplicates_and_all_have_reasons() {
        let mut seen = std::collections::HashSet::new();
        for r in all_rules() {
            assert!(seen.insert(r.nr), "{} appears twice", r.name);
            assert!(!r.why.is_empty());
        }
    }

    #[test]
    fn errno_kinds() {
        assert_eq!(judge(libc::SYS_io_uring_setup, [0; 6]), errno(ENOSYS));
        assert_eq!(judge(libc::SYS_clone3, [0; 6]), errno(ENOSYS));
        assert_eq!(judge(libc::SYS_open_tree, [0; 6]), errno(ENOSYS));
        assert_eq!(judge(libc::SYS_bpf, [0; 6]), errno(EPERM));
        assert_eq!(judge(libc::SYS_ptrace, [0; 6]), errno(EPERM));
    }

    #[test]
    fn ordinary_calls_pass() {
        for nr in [libc::SYS_read, libc::SYS_write, libc::SYS_openat, libc::SYS_mmap, libc::SYS_futex, libc::SYS_execve,
                   libc::SYS_memfd_create, libc::SYS_seccomp, libc::SYS_landlock_create_ruleset, libc::SYS_prctl,
                   libc::SYS_socket, libc::SYS_sendmsg, libc::SYS_epoll_wait, libc::SYS_getrandom, libc::SYS_prlimit64] {
            assert_eq!(judge(nr, [0; 6]), SECCOMP_RET_ALLOW, "{nr}");
        }
        #[cfg(target_arch = "x86_64")]
        assert_eq!(judge(libc::SYS_modify_ldt, [0; 6]), SECCOMP_RET_ALLOW, "modify_ldt is needed by Wine");
    }

    #[test]
    fn ioctl_only_the_injection_requests() {
        let a = |cmd: u64| [3, cmd, 0, 0, 0, 0];
        assert_eq!(judge(libc::SYS_ioctl, a(TIOCSTI as u64)), errno(EPERM));
        assert_eq!(judge(libc::SYS_ioctl, a(TIOCLINUX as u64)), errno(EPERM));
        for ok in [libc::TCGETS, libc::TIOCGWINSZ, libc::FIONREAD, libc::TIOCSWINSZ] {
            assert_eq!(judge(libc::SYS_ioctl, a(ok as u64)), SECCOMP_RET_ALLOW, "{ok:#x}");
        }
    }

    #[test]
    fn personality_plain_linux_only() {
        let a = |v: u64| [v, 0, 0, 0, 0, 0];
        for ok in PERSONALITY_OK {
            assert_eq!(judge(libc::SYS_personality, a(*ok as u64)), SECCOMP_RET_ALLOW, "{ok:#x}");
        }
        for bad in [0x0040_0000u64 /* READ_IMPLIES_EXEC */, 0x0004_0000 /* ADDR_NO_RANDOMIZE */, 0x0010_0000 /* MMAP_PAGE_ZERO */,
                    0x0000_0001 /* PER_SVR4 */, 0x0000_0008 | 0x0040_0000] {
            assert_eq!(judge(libc::SYS_personality, a(bad)), errno(EPERM), "{bad:#x}");
        }
    }

    #[test]
    fn namespaces_refused_through_clone_and_unshare() {
        let a = |v: u64| [v, 0, 0, 0, 0, 0];
        for flag in [0x1000_0000u64 /* NEWUSER */, 0x0002_0000 /* NEWNS */, 0x4000_0000 /* NEWNET */, 0x2000_0000 /* NEWPID */,
                     0x0400_0000 /* NEWUTS */, 0x0800_0000 /* NEWIPC */, 0x0200_0000 /* NEWCGROUP */] {
            assert_eq!(judge(libc::SYS_clone, a(flag | 0x11)), errno(EPERM), "clone {flag:#x}");
            assert_eq!(judge(libc::SYS_unshare, a(flag)), errno(EPERM), "unshare {flag:#x}");
        }
        assert_eq!(judge(libc::SYS_unshare, a(CLONE_NEWTIME as u64)), errno(EPERM));
        // The flags of an ordinary thread (pthread_create) and of fork pass.
        let thread = 0x0000_0100 | 0x0000_0200 | 0x0000_0400 | 0x0000_0800 | 0x0001_0000 | 0x0004_0000 | 0x0020_0000 | 0x0040_0000;
        assert_eq!(judge(libc::SYS_clone, a(thread)), SECCOMP_RET_ALLOW);
        assert_eq!(judge(libc::SYS_clone, a(0x0120_0011 /* SIGCHLD|CLONE_CHILD_* style fork */)), SECCOMP_RET_ALLOW);
        // CLONE_FS|CLONE_FILES: not a namespace.
        assert_eq!(judge(libc::SYS_unshare, a(0x0000_0200 | 0x0000_0400)), SECCOMP_RET_ALLOW);
        // Only the low 32 bits count: the kernel takes `unsigned long` flags, but the namespace bits are all in them.
    }

    #[test]
    fn other_architectures_are_refused_whole() {
        let p = build().unwrap();
        // i386, arm 32-bit, a made-up one: every call, even a harmless one.
        for arch in [0x4000_0003u32, 0x4000_0028, 0x1234_5678, 0] {
            assert_eq!(run(&p, arch, libc::SYS_getpid as u32, [0; 6]), errno(EPERM), "{arch:#x}");
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn x32_numbers_are_refused() {
        let p = build().unwrap();
        // A harmless call number with the x32 bit, and the x86_64 architecture (which is how x32 arrives).
        assert_eq!(run(&p, NATIVE_ARCH, libc::SYS_getpid as u32 | X32_BIT, [0; 6]), errno(EPERM));
        // The x32 bit also hides a call of the list from the plain compares: the bit test comes first.
        assert_eq!(run(&p, NATIVE_ARCH, libc::SYS_ptrace as u32 | X32_BIT, [0; 6]), errno(EPERM));
        assert_eq!(run(&p, NATIVE_ARCH, libc::SYS_getpid as u32, [0; 6]), SECCOMP_RET_ALLOW);
    }
}
