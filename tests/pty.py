#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A pty in the isolated view: terminals, IDEs and editors with a terminal open one, so every app has one.

Usage: python3 tests/pty.py [work-directory]
Build first: `cargo build --release --target x86_64-unknown-linux-musl`. Everything runs in an isolated HOME inside the work
directory (tests/isolation.py), through the real launcher, stub and view.

The view has a devpts of its own, so the ptys are the app's and never the user's terminals. The app (the host's Python) checks,
from inside the sandbox:
- `openpty` works, and `/dev/pts` is another file system than the host's (another `st_dev`);
- an interactive shell on a pty answers, with a controlling terminal;
- the window size follows `TIOCSWINSZ` (the shell sees it with `stty size`, after a second change as well);
- job control: Ctrl+Z stops a job, `jobs` lists it, `fg` brings it back, Ctrl+C ends it;
- a full-screen program (`less`) draws on the alternate screen and leaves it on `q`;
- TIOCSTI on the pty is still refused by the seccomp filter.
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REL = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402

ID = 'org.example.pty'
APP = r'''#!/usr/bin/python3
import fcntl, json, os, pty, select, signal, struct, sys, termios, time
out = {}
out['pts_dev'] = os.stat('/dev/pts').st_dev
try:
    m, s = pty.openpty(); out['openpty'] = True
    out['sti'] = None
    try:
        fcntl.ioctl(s, termios.TIOCSTI, b'x'); out['sti'] = 'allowed'
    except OSError as e:
        out['sti'] = e.errno
    os.close(m); os.close(s)
except OSError as e:
    out['openpty'] = repr(e)
    open(os.environ['XDG_DATA_HOME'] + '/result.json', 'w').write(json.dumps(out)); sys.exit(0)

def session(argv, env=None):
    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe(argv[0], argv, env or dict(os.environ, TERM='xterm', PS1='$ '))
    return pid, fd

def read(fd, until=None, timeout=5.0):
    buf = b''; end = time.time() + timeout
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            try:
                d = os.read(fd, 65536)
            except OSError:
                break
            if not d:
                break
            buf += d
            if until and until.encode() in buf:
                break
    return buf.decode('utf-8', 'replace')

def size(fd, rows, cols):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))

pid, fd = session(['/bin/bash', '--norc', '--noprofile', '-i'])
size(fd, 40, 100)
read(fd, '$ ', 3)
os.write(fd, b'echo $((6*7))\n'); out['shell'] = '42' in read(fd, '42', 5)
os.write(fd, b'stty size\n'); out['size1'] = '40 100' in read(fd, '40 100', 5)
size(fd, 30, 90); time.sleep(0.3)
os.write(fd, b'stty size\n'); out['size2'] = '30 90' in read(fd, '30 90', 5)
os.write(fd, b'tty\n'); t = read(fd, '/dev/pts/', 5); out['tty'] = '/dev/pts/' in t
# job control
os.write(fd, b'sleep 30\n'); time.sleep(0.5); os.write(fd, b'\x1a')
r = read(fd, 'Stopped', 5); out['stopped'] = 'Stopped' in r
os.write(fd, b'jobs\n'); out['jobs'] = 'sleep 30' in read(fd, 'sleep 30', 5)
os.write(fd, b'fg\n'); time.sleep(0.5); os.write(fd, b'\x03'); time.sleep(0.3)
os.write(fd, b'echo after-$((1+1))\n'); out['fg_ctrl_c'] = 'after-2' in read(fd, 'after-2', 5)
os.write(fd, b'exit\n'); read(fd, None, 1)
try:
    os.kill(pid, signal.SIGKILL)
except OSError:
    pass
os.waitpid(pid, 0)
# a full-screen program
pid, fd = session(['/usr/bin/less', '/etc/os-release'], dict(os.environ, TERM='xterm', LESS=''))
size(fd, 24, 80)
screen = read(fd, 'NAME', 5)
out['less_alt_screen'] = '\x1b[?1049h' in screen or '\x1b[?47h' in screen
out['less_text'] = 'NAME' in screen
os.write(fd, b'q'); left = read(fd, None, 2)
out['less_left'] = '\x1b[?1049l' in left or '\x1b[?47l' in left
try:
    os.waitpid(pid, 0)
except OSError:
    pass
open(os.environ['XDG_DATA_HOME'] + '/result.json', 'w').write(json.dumps(out))
'''


def need(cond, msg):
    if not cond:
        print('FAIL:', msg)
        sys.exit(1)


def main():
    for b in (REL / 'panpak', REL / 'panpak-stub'):
        need(b.exists(), f'{b} is missing: build first (see the top of this file)')
    if not Path('/usr/bin/less').exists():
        print('SKIP: /usr/bin/less is not installed')
        return
    work = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / 'target/pty-test'
    isolation.safe_rmtree(work, work.parent)
    home = work / 'home'
    isolation.require_isolated_home(home, work)
    home.mkdir(parents=True)
    runtime = Path(isolation.owned_mkdtemp('pty-rt-'))
    env = {'PATH': os.environ['PATH'], 'HOME': str(home), 'XDG_RUNTIME_DIR': str(runtime)}

    def run(binary, *args):
        return subprocess.run([str(binary), *map(str, args)], env=env, capture_output=True, text=True, timeout=180)

    key = work / 'secret.key'
    out = run(REL / 'panpak', 'keygen', '-o', key).stdout
    pub = next(line.split()[-1] for line in out.splitlines() if line.startswith('key:'))
    tree = work / 'tree'
    (tree / 'bin').mkdir(parents=True)
    (tree / 'bin/app').write_text(APP)
    (tree / 'bin/app').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid = "{ID}"\nname = "Pty"\nversion = "1"\nentry = "bin/app"\n[update]\nkey = "{pub}"\n')
    pkg = work / 'pty.panpak'
    p = run(REL / 'panpak', 'pack', tree, '--stub', REL / 'panpak-stub', '--secret-file', key, '-o', pkg)
    need(p.returncode == 0, p.stderr)
    p = run(REL / 'panpak', 'install', '--yes', '--runtime', REL / 'panpak-stub', pkg)
    need(p.returncode == 0, p.stdout + p.stderr)
    p = run(home / '.local/bin' / ID)
    need(p.returncode == 0, f'the app failed ({p.returncode}): {p.stdout}{p.stderr}')
    result = home / f'.local/share/panpak/userdata/{ID}/data/result.json'
    need(result.exists(), 'the app wrote no result: ' + p.stderr)
    res = json.loads(result.read_text())
    need(res['openpty'] is True, f'openpty: {res["openpty"]}')
    need(res['pts_dev'] != os.stat('/dev/pts').st_dev, "/dev/pts in the view is the host's devpts: the app could see the terminals of the user")
    print('openpty works; /dev/pts in the view is a devpts of its own (another st_dev than the host\'s)')
    for k, what in (('shell', 'an interactive shell answers'), ('tty', 'the shell has a controlling terminal on /dev/pts'),
                    ('size1', 'the window size is seen by the shell'), ('size2', 'the size follows a second change'),
                    ('stopped', 'Ctrl+Z stops a job'), ('jobs', '`jobs` lists it'), ('fg_ctrl_c', '`fg`, then Ctrl+C ends it and the shell goes on'),
                    ('less_text', 'less draws the file'), ('less_alt_screen', 'less uses the alternate screen'), ('less_left', 'less leaves it on q')):
        need(res[k] is True, f'{what}: no ({k}) {res}')
        print(what + ': ok')
    need(res['sti'] == 1, f'TIOCSTI on the pty must be refused with EPERM (1): {res["sti"]}')
    print('TIOCSTI on the pty: refused by the seccomp filter')
    run(REL / 'panpak', 'remove', ID)
    isolation.remove_owned(runtime, work)
    isolation.safe_rmtree(work, work.parent)
    print('all pty tests passed')


if __name__ == '__main__':
    main()
