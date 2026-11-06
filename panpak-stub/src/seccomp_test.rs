// SPDX-License-Identifier: MIT OR Apache-2.0
//! The seccomp filter in the real kernel (the program itself is tested in `panpak_core::seccomp`).
use panpak_core::seccomp::{install, self_status};

/// The real kernel: in a child process the filter is installed, then each kind of call is tried. The child
/// is a fork of the test process, so the test runs alone (`testutil::alone`).
#[test]
fn kernel_enforces_the_filter() {
    if !crate::testutil::alone() {
        return;
    }
    // SAFETY: the process is alone and single-threaded here (`alone`); the child only makes system calls
    // and `_exit`.
    unsafe {
        let pid = libc::fork();
        assert!(pid >= 0);
        if pid == 0 {
            let code = (|| -> i32 {
                if install().is_err() {
                    return 10;
                }
                let call = |nr: i64, a: [libc::c_long; 3]| {
                    *libc::__errno_location() = 0;
                    let r = libc::syscall(nr, a[0], a[1], a[2]);
                    (r, *libc::__errno_location())
                };
                if call(libc::SYS_io_uring_setup, [1, 0, 0]) != (-1, libc::ENOSYS) {
                    return 11;
                }
                if call(libc::SYS_bpf, [0, 0, 0]) != (-1, libc::EPERM) {
                    return 12;
                }
                if call(libc::SYS_unshare, [0x1000_0000, 0, 0]) != (-1, libc::EPERM) {
                    return 13;
                }
                if call(libc::SYS_clone3, [0, 0, 0]) != (-1, libc::ENOSYS) {
                    return 14;
                }
                if call(libc::SYS_ptrace, [0, 0, 0]) != (-1, libc::EPERM) {
                    return 15;
                }
                if call(libc::SYS_ioctl, [0, 0x5412, 0]) != (-1, libc::EPERM) {
                    return 16;
                }
                // Allowed calls still work, including a new thread's clone flags through the C library.
                if libc::getpid() <= 0 {
                    return 17;
                }
                let mut tid = std::mem::zeroed::<libc::pthread_t>();
                extern "C" fn nop(_: *mut libc::c_void) -> *mut libc::c_void {
                    std::ptr::null_mut()
                }
                if libc::pthread_create(&mut tid, std::ptr::null(), nop, std::ptr::null_mut()) != 0 {
                    return 18;
                }
                libc::pthread_join(tid, std::ptr::null_mut());
                let Some((nnp, mode)) = self_status() else { return 19 };
                if (nnp, mode) != (1, 2) {
                    return 20;
                }
                0
            })();
            libc::_exit(code);
        }
        let mut st = 0;
        libc::waitpid(pid, &mut st, 0);
        assert!(libc::WIFEXITED(st), "child ended abnormally: {st:#x}");
        assert_eq!(libc::WEXITSTATUS(st), 0, "step {} of the child failed", libc::WEXITSTATUS(st));
    }
}
