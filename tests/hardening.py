#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The hardening of an app that runs through the real launcher, stub and view: the seccomp filter and the
system font cache.

Usage: python3 tests/hardening.py [work-directory]
Build first: `cargo build --release --target x86_64-unknown-linux-musl`. Everything runs in an isolated HOME
inside the work directory (tests/isolation.py).

The app is a Python program (the host's `python3`) that checks, from inside the sandbox:
- the process is in filter mode (`Seccomp: 2`) with `no_new_privs`;
- the refused calls return the documented error number (not a kill): io_uring_setup, clone3, the new mount API
  -> ENOSYS; bpf, unshare(CLONE_NEWUSER), ptrace, TIOCSTI, personality(READ_IMPLIES_EXEC), an x32 number -> EPERM;
- the calls a program needs still work: threads, fork + exec, pipes, memfd_create, a Landlock query.
"""
import ctypes
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REL = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402

ID = 'org.example.hardening'
APP = r'''#!/usr/bin/python3
import ctypes, errno, json, os, subprocess, threading
libc = ctypes.CDLL(None, use_errno=True)
out = {}
def sc(nr, *args):
    ctypes.set_errno(0)
    r = libc.syscall(ctypes.c_long(nr), *[ctypes.c_long(a) for a in args])
    return -1 if r == -1 else 0, ctypes.get_errno()
status = dict(l.split(':\t', 1) for l in open('/proc/self/status').read().splitlines() if ':\t' in l)
out['status'] = {'NoNewPrivs': status['NoNewPrivs'].strip(), 'Seccomp': status['Seccomp'].strip()}
N = {'io_uring_setup': 425, 'clone3': 435, 'fsopen': 430, 'open_tree': 428, 'move_mount': 429, 'bpf': 321,
     'unshare': 272, 'ptrace': 101, 'ioctl': 16, 'personality': 135, 'userfaultfd': 323, 'perf_event_open': 298,
     'keyctl': 250, 'mount': 165, 'setns': 308, 'getpid': 39}
out['io_uring_setup'] = sc(N['io_uring_setup'], 1, 0)[1]
out['clone3'] = sc(N['clone3'], 0, 0)[1]
out['fsopen'] = sc(N['fsopen'], 0, 0)[1]
out['open_tree'] = sc(N['open_tree'], -100, 0, 0)[1]
out['bpf'] = sc(N['bpf'], 0, 0, 0)[1]
out['unshare_newuser'] = sc(N['unshare'], 0x10000000)[1]
out['ptrace'] = sc(N['ptrace'], 0, 0, 0, 0)[1]
out['tiocsti'] = sc(N['ioctl'], 0, 0x5412, 0)[1]
out['personality_rix'] = sc(N['personality'], 0x400000)[1]
out['userfaultfd'] = sc(N['userfaultfd'], 0)[1]
out['perf_event_open'] = sc(N['perf_event_open'], 0, 0, -1, -1, 0)[1]
out['keyctl'] = sc(N['keyctl'], 0, 0)[1]
out['mount'] = sc(N['mount'], 0, 0, 0, 0, 0)[1]
out['setns'] = sc(N['setns'], -1, 0)[1]
out['x32_getpid'] = sc(0x40000000 | N['getpid'])[1]
# What must keep working.
t = []
th = threading.Thread(target=lambda: t.append(1)); th.start(); th.join()
out['thread'] = t == [1]
out['fork_exec'] = subprocess.run(['/bin/true']).returncode == 0
r, w = os.pipe(); os.write(w, b'x'); out['pipe'] = os.read(r, 1) == b'x'
out['memfd'] = libc.syscall(319, b'a', 0) >= 0
out['getpid'] = sc(N['getpid'])[0] == 0
out['ioctl_ok'] = sc(N['ioctl'], 0, 0x5401, 0)[1] != errno.EPERM  # TCGETS: ENOTTY or 0, never EPERM from the filter
out['personality_query'] = sc(N['personality'], 0xffffffff)[0] == 0
# The system font cache: readable, not writable.
fc = '/var/cache/fontconfig'
out['fontcache_read'] = len(os.listdir(fc)) > 0 if os.path.isdir(fc) else None
try:
    open(fc + '/x', 'w'); out['fontcache_write'] = True
except OSError:
    out['fontcache_write'] = False
open(os.environ['XDG_DATA_HOME'] + '/result.json', 'w').write(json.dumps(out))
'''

WANT = {
    'io_uring_setup': 'ENOSYS', 'clone3': 'ENOSYS', 'fsopen': 'ENOSYS', 'open_tree': 'ENOSYS',
    'bpf': 'EPERM', 'unshare_newuser': 'EPERM', 'ptrace': 'EPERM', 'tiocsti': 'EPERM', 'personality_rix': 'EPERM',
    'userfaultfd': 'EPERM', 'perf_event_open': 'EPERM', 'keyctl': 'EPERM', 'mount': 'EPERM', 'setns': 'EPERM',
    'x32_getpid': 'EPERM',
}


def need(cond, msg):
    if not cond:
        print('FAIL:', msg)
        sys.exit(1)


def main():
    for b in (REL / 'panpak', REL / 'panpak-stub'):
        need(b.exists(), f'{b} is missing: build first (see the top of this file)')
    work = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / 'target/hardening-test'
    isolation.safe_rmtree(work, work.parent)
    home = work / 'home'
    isolation.require_isolated_home(home, work)
    home.mkdir(parents=True)
    runtime = Path(isolation.owned_mkdtemp('hardening-rt-'))
    env = {'PATH': os.environ['PATH'], 'HOME': str(home), 'XDG_RUNTIME_DIR': str(runtime)}

    def run(binary, *args):
        return subprocess.run([str(binary), *map(str, args)], env=env, capture_output=True, text=True, timeout=120)

    key = work / 'secret.key'
    out = run(REL / 'panpak', 'keygen', '-o', key).stdout
    pub = next(line.split()[-1] for line in out.splitlines() if line.startswith('key:'))
    tree = work / 'tree'
    (tree / 'bin').mkdir(parents=True)
    (tree / 'bin/app').write_text(APP)
    (tree / 'bin/app').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid = "{ID}"\nname = "Hardening"\nversion = "1"\nentry = "bin/app"\n[update]\nkey = "{pub}"\n')
    pkg = work / 'hardening.panpak'
    p = run(REL / 'panpak', 'pack', tree, '--stub', REL / 'panpak-stub', '--secret-file', key, '-o', pkg)
    need(p.returncode == 0, p.stderr)
    p = run(REL / 'panpak', 'install', '--yes', '--runtime', REL / 'panpak-stub', pkg)
    need(p.returncode == 0, p.stdout + p.stderr)
    p = run(home / '.local/bin' / ID)
    need(p.returncode == 0, f'the app failed ({p.returncode}): {p.stdout}{p.stderr}')
    res = json.loads((home / f'.local/share/panpak/userdata/{ID}/data/result.json').read_text())

    need(res['status'] == {'NoNewPrivs': '1', 'Seccomp': '2'}, f'process mode: {res["status"]}')
    import errno
    for k, want in WANT.items():
        got = errno.errorcode.get(res[k], res[k])
        need(got == want, f'{k}: expected {want}, got {got}')
    for k in ('thread', 'fork_exec', 'pipe', 'memfd', 'getpid', 'ioctl_ok', 'personality_query'):
        need(res[k] is True, f'{k} no longer works under the filter')
    host_has_cache = Path('/var/cache/fontconfig').is_dir()
    need(res['fontcache_read'] == (True if host_has_cache else None), f'font cache: read {res["fontcache_read"]}, host has one: {host_has_cache}')
    need(res['fontcache_write'] is False, 'the system font cache is writable')
    print('system font cache:', 'readable, not writable' if host_has_cache else 'absent on this host (not checked)')
    print('seccomp in the real launcher: filter mode with no_new_privs; ENOSYS for', ', '.join(k for k, v in WANT.items() if v == 'ENOSYS'))
    print('  EPERM for', ', '.join(k for k, v in WANT.items() if v == 'EPERM'))
    print('  threads, fork+exec, pipes, memfd_create, ordinary ioctl and personality query keep working: ok')
    run(REL / 'panpak', 'remove', ID)  # the trees are read-only: remove takes them away properly
    isolation.remove_owned(runtime, work)
    isolation.safe_rmtree(work, work.parent)
    print('all hardening tests passed')


if __name__ == '__main__':
    main()
