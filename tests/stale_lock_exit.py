#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A lock file left by an app that stops on a signal (PANPAK-FORMAT.md §9.4), through the real launcher, stub and view.

Usage: python3 tests/stale_lock_exit.py [work-directory]
Build first: `cargo build --release --target x86_64-unknown-linux-musl`. Everything runs in an isolated HOME.

The app is a Python program that behaves like a Qt application with a `QLockFile`: it writes a file with its process id, a name and
a host, and it refuses to start when that file names a process that exists. In the view process ids start at 1, so the file left by
a copy that was stopped always names a live process. It does not remove the file when a signal stops it.
Cases, and what must happen:
- SIGTERM to the app only, SIGKILL to the app only: the runtime that stays removes the lock file when the last copy ends, and the next
  start works;
- SIGKILL to every process of the run (the runtime too): the mark "in use" stays, the next start removes the file and works;
- two copies: when the first ends, the lock file of the second (and the first's own) are not touched; the last one to end removes both.
"""
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REL = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402

ID = 'org.example.stalelock'
APP = r'''#!/usr/bin/python3
import os, sys, time
second = '--second' in sys.argv
lock = os.environ['XDG_CONFIG_HOME'] + ('/lockfile2' if second else '/lockfile')
data = os.environ['XDG_DATA_HOME']
if os.path.exists(lock):
    try:
        pid = int(open(lock).read().split('\n')[0])
    except ValueError:
        pid = 0
    # A QLockFile takes a process id that exists for a live copy.
    if pid and os.path.exists(f'/proc/{pid}'):
        open(data + '/refused', 'a').write('x')
        sys.exit(3)
open(lock, 'w').write(f'{os.getpid()}\nfakeapp\nhost\n\nid-1234\n')
open(data + ('/started2' if second else '/started'), 'w').write('1')
time.sleep(60)
'''


def need(cond, msg):
    """`msg` may be a function, so that it is only evaluated (a process read, a pipe drained) when the check fails."""
    if not cond:
        print('FAIL:', msg() if callable(msg) else msg)
        sys.exit(1)


def main():
    for b in (REL / 'panpak', REL / 'panpak-stub'):
        need(b.exists(), f'{b} is missing: build first (see the top of this file)')
    work = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / 'target/stale-lock-test'
    isolation.safe_rmtree(work, work.parent)
    home = work / 'home'
    isolation.require_isolated_home(home, work)
    home.mkdir(parents=True)
    runtime = Path(isolation.owned_mkdtemp('stalelock-rt-'))
    env = {'PATH': os.environ['PATH'], 'HOME': str(home), 'XDG_RUNTIME_DIR': str(runtime), 'PANPAK_DEBUG': '1'}

    def run(binary, *args):
        return subprocess.run([str(binary), *map(str, args)], env=env, capture_output=True, text=True, timeout=120)

    key = work / 'secret.key'
    out = run(REL / 'panpak', 'keygen', '-o', key).stdout
    pub = next(line.split()[-1] for line in out.splitlines() if line.startswith('key:'))
    tree = work / 'tree'
    (tree / 'bin').mkdir(parents=True)
    (tree / 'bin/app').write_text(APP)
    (tree / 'bin/app').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid = "{ID}"\nname = "Stale lock"\nversion = "1"\nentry = "bin/app"\n[update]\nkey = "{pub}"\n')
    pkg = work / 'app.panpak'
    p = run(REL / 'panpak', 'pack', tree, '--stub', REL / 'panpak-stub', '--secret-file', key, '-o', pkg)
    need(p.returncode == 0, p.stderr)
    p = run(REL / 'panpak', 'install', '--yes', '--runtime', REL / 'panpak-stub', pkg)
    need(p.returncode == 0, p.stdout + p.stderr)
    if 'isolated view: not available' in run(REL / 'panpak', 'doctor').stdout or 'NOTE isolated view' in run(REL / 'panpak', 'doctor').stdout:
        print('SKIP: no isolated view here (bubblewrap cannot run), so the runtime does not stay to see the app end')
        return
    base = home / f'.local/share/panpak/userdata/{ID}'
    lock, lock2 = base / 'config/lockfile', base / 'config/lockfile2'
    data = base / 'data'
    launcher = str(home / '.local/bin' / ID)
    token = [0]

    def start(*extra):
        token[0] += 1
        tag = f'--token={os.getpid()}-{token[0]}'
        flag = data / ('started2' if '--second' in extra else 'started')
        flag.unlink(missing_ok=True)
        proc = subprocess.Popen([launcher, tag, *extra], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, start_new_session=True)
        for _ in range(300):
            if flag.exists() or proc.poll() is not None:
                break
            time.sleep(0.05)
        return proc, tag, flag.exists()

    def app_pid(tag):
        for pid in filter(str.isdigit, os.listdir('/proc')):
            try:
                argv = Path(f'/proc/{pid}/cmdline').read_bytes().split(b'\0')
                # The app itself: the interpreter is the first word (the runtimes and bubblewrap have the token further in).
                if tag.encode() in argv and b'python' in argv[0]:
                    return int(pid)
            except OSError:
                pass
        return None

    def run_pids():
        key = str(home).encode()
        out = []
        for pid in filter(str.isdigit, os.listdir('/proc')):
            try:
                if int(pid) != os.getpid() and key in Path(f'/proc/{pid}/cmdline').read_bytes():
                    out.append(int(pid))
            except OSError:
                pass
        return out

    def finish(proc):
        try:
            return proc.communicate(timeout=30)[1]
        except subprocess.TimeoutExpired:
            proc.kill()
            return proc.communicate()[1]

    def next_start_works(case):
        (data / 'refused').unlink(missing_ok=True)
        proc, tag, up = start()
        refused = (data / 'refused').exists()
        pid = app_pid(tag)
        if pid:
            os.kill(pid, signal.SIGTERM)
        err = finish(proc)
        need(up and not refused, f'{case}: the next start with the runtime alone did not work (started {up}, refused {refused}): {err[-300:]}')
        return err

    costs = []
    for case, sig, everything in (('SIGTERM to the app only', signal.SIGTERM, False), ('SIGKILL to the app only', signal.SIGKILL, False), ('SIGKILL to every process of the run', signal.SIGKILL, True)):
        proc, tag, up = start()
        need(up and lock.exists(), lambda: f'{case}: the app did not start: {finish(proc)[-300:]}')
        if everything:
            for pid in run_pids():
                try:
                    os.kill(pid, signal.SIGKILL)
                except OSError:
                    pass
            proc.wait(timeout=10)
            err = proc.stderr.read()
            need(lock.exists(), f'{case}: nothing could have removed the lock file, and it is gone')
            print(f'  {case}: the lock file stays (no runtime left to remove it), the mark stays;', end=' ')
            err2 = next_start_works(case)
            need('removed a stale lock file:' in err2 and 'at exit' not in err2.split('removed a stale lock file:')[0][-40:], f'{case}: the next start did not report the search: {err2[-300:]}')
            print('the next start removed it and worked')
        else:
            pid = app_pid(tag)
            need(pid, f'{case}: cannot find the app')
            os.kill(pid, sig)
            err = finish(proc)
            need(not lock.exists(), f'{case}: the lock file is still there after the last copy ended')
            m = re.search(r'exit: is the last copy, stale lock search ([0-9.]+) ms, (\d+) removed', err)
            need(m and m.group(2) == '1' and 'removed a stale lock file at exit' in err, f'{case}: the runtime did not report the search at exit: {err[-300:]}')
            costs.append(float(m.group(1)))
            print(f'  {case}: the runtime removed the lock file when the last copy ended ({m.group(1)} ms at exit);', end=' ')
            next_start_works(case)
            print('the next start worked')
        time.sleep(0.3)

    # Two copies: the end of the first must not touch the lock files of either copy.
    p1, t1, up1 = start()
    p2, t2, up2 = start('--second')
    need(up1 and up2 and lock.exists() and lock2.exists(), lambda: f'two copies: they did not start ({up1}, {up2}): {finish(p2)[-200:]}')
    first, second = app_pid(t1), app_pid(t2)
    need(first and second, 'two copies: cannot find the apps')
    os.kill(first, signal.SIGTERM)
    err1 = finish(p1)
    need(lock.exists() and lock2.exists(), 'two copies: the first to end removed a lock file while another copy is alive')
    m = re.search(r'exit: is not the last copy', err1)
    need(m and 'removed a stale lock file' not in err1, f'two copies: the first to end should have found it was not the last: {err1[-300:]}')
    need(Path(f'/proc/{second}').exists(), 'two copies: the second copy died with the first')
    os.kill(second, signal.SIGTERM)
    err2 = finish(p2)
    need(not lock.exists() and not lock2.exists(), 'two copies: the last copy to end left a lock file')
    need('is the last copy' in err2, f'two copies: {err2[-300:]}')
    print('  two copies: the first to end touched nothing; the last removed both lock files')
    next_start_works('after two copies')
    print(f'cost at exit of the search for stale locks (private folders of this test app): {max(costs):.2f} ms at most; none at start')
    run(REL / 'panpak', 'remove', ID)
    isolation.remove_owned(runtime, work)
    isolation.safe_rmtree(work, work.parent)
    print('all stale lock tests passed')


if __name__ == '__main__':
    main()
