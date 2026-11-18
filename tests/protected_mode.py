#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""End-to-end test of protected mode (PANPAK-FORMAT.md 3.1.10): an installed version below the floor its author set.

Usage: python3 tests/protected_mode.py [work-directory]
Build release first (cargo build --release --target x86_64-unknown-linux-musl).
Everything runs in an isolated HOME inside the work directory (see tests/isolation.py). The app asks for the network (one port, where this script
listens) and for `exec_written`. It reports whether it can reach that port, reach a server of its own on its loopback, and run a program that it wrote.

  1. version 1 installed: all three work;
  2. a package of another channel with floor 2 is refused (and the floor is seen): the next start is in protected mode: no host network, the
     loopback inside the app works, no written program is run; the text is on the terminal (a pty), and nothing is written to a log without a terminal;
  3. `allow-internet --yes`: the host port is reachable again, the written program still is not; `--undo` takes it away again;
  4. the fixed version (revision 2, floor 2) installed over it: the mode ends, with the message; everything works again and the choice is gone.
"""
import os
from pathlib import Path
import select
import socket
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402

ID = 'org.example.protected'
VIEW = subprocess.run(['/usr/bin/bwrap', '--unshare-user', '--ro-bind', '/', '/', '/usr/bin/true'], capture_output=True).returncode == 0
PROBE = r'''#!/bin/sh
exec /usr/bin/python3 -I - "$@" <<'PY'
import os, socket, subprocess, shutil, sys
port, lport = int(sys.argv[1]), int(sys.argv[2])
out = []
def attempt(name, fn):
    try:
        fn()
        out.append(f'{name}=ok')
    except Exception as e:
        out.append(f'{name}=deny')
def host():
    s = socket.create_connection(('127.0.0.1', port), timeout=3)
    s.close()
def loop():
    srv = socket.socket()
    srv.bind(('127.0.0.1', lport))
    srv.listen(1)
    c = socket.create_connection(('127.0.0.1', lport), timeout=3)
    c.close()
    srv.close()
def run_written():
    tool = os.path.join(os.environ['XDG_DATA_HOME'], 'tool')
    shutil.copy('/usr/bin/true', tool)
    os.chmod(tool, 0o755)
    subprocess.run([tool], check=True)
attempt('host', host)
attempt('loop', loop)
attempt('written', run_written)
open(os.path.join(os.environ['XDG_DATA_HOME'], 'report.txt'), 'w').write('\n'.join(out) + '\n')
PY
'''


class Lab:
    def __init__(self, work):
        self.work = work
        self.home = work / 'home'
        isolation.require_isolated_home(self.home, work)
        self.home.mkdir(parents=True)
        self.runtime = isolation.owned_mkdtemp('prot-rt-')
        self.env = {'PATH': os.environ['PATH'], 'HOME': str(self.home), 'XDG_RUNTIME_DIR': self.runtime, 'PANPAK_TEST_REAL_HOME': str(self.home), 'LC_ALL': 'C'}
        self.key = work / 'secret.key'
        out = self.cli('keygen', '-o', str(self.key)).stdout
        self.pub = next(l.split()[-1] for l in out.splitlines() if l.startswith('key:'))
        self.stub = BIN / 'panpak-stub'
        self.listener = socket.socket()
        self.listener.bind(('127.0.0.1', 0))
        self.listener.listen(16)
        self.port = self.listener.getsockname()[1]
        spare = socket.socket()
        spare.bind(('127.0.0.1', 0))
        self.lport = spare.getsockname()[1]
        spare.close()
        self.appdir = self.home / '.local/share/panpak/apps' / ID
        self.n = 0

    def cli(self, *args, check=True):
        p = subprocess.run([str(BIN / 'panpak'), *map(str, args)], env=self.env, capture_output=True, text=True, timeout=120, stdin=subprocess.DEVNULL)
        if check:
            assert p.returncode == 0, f'{args}: {p.stdout}{p.stderr}'
        return p

    def pack(self, revision, release_extra=''):
        self.n += 1
        tree = self.work / f'tree{self.n}'
        (tree / 'bin').mkdir(parents=True)
        probe = tree / 'bin/probe'
        probe.write_text(PROBE)
        probe.chmod(0o755)
        (tree / 'panpak.toml').write_text(
            f'[app]\nid = "{ID}"\nname = "Protected"\nversion = "{revision}"\nentry = "bin/probe"\n[release]\nrevision = {revision}\n{release_extra}'
            f'[permissions]\nnetwork = true\nnetwork_allow_tcp = [{self.port}, {self.lport}]\nnetwork_bind_tcp = [{self.lport}]\nexec_written = true\n[update]\nkey = "{self.pub}"\n')
        pkg = self.work / f'p{self.n}.panpak'
        self.cli('pack', tree, '--stub', self.stub, '--secret-file', self.key, '-o', pkg)
        return pkg

    def report(self):
        return self.home / f'.local/share/panpak/userdata/{ID}/.local/share/report.txt'

    def start(self, tty=False):
        """Start the app through its launcher. Returns the report and the text that the start wrote on its standard error."""
        self.report().unlink(missing_ok=True)
        cmd = [str(self.home / '.local/bin' / ID), str(self.port), str(self.lport)]
        if tty:
            master, slave = os.openpty()
            p = subprocess.Popen(cmd, env=self.env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=slave)
            os.close(slave)
            text = b''
            end = time.time() + 60
            while time.time() < end:
                r, _, _ = select.select([master], [], [], 0.5)
                if r:
                    try:
                        data = os.read(master, 4096)
                    except OSError:
                        break
                    if not data:
                        break
                    text += data
                elif p.poll() is not None:
                    break
            p.wait(timeout=60)
            os.close(master)
            err = text.decode(errors='replace')
        else:
            p = subprocess.run(cmd, env=self.env, capture_output=True, text=True, timeout=120, stdin=subprocess.DEVNULL)
            err = p.stderr
        rep = dict(l.split('=', 1) for l in self.report().read_text().split() if '=' in l) if self.report().exists() else {}
        return rep, err


def expect(rep, **want):
    bad = {k: (rep.get(k), v) for k, v in want.items() if rep.get(k) != v}
    assert not bad, f'unexpected (got, wanted): {bad}; report {rep}'


def main():
    if not VIEW:
        print('skipped: protected mode keeps the loopback of the app only in the isolated view (bubblewrap with user namespaces)')
        return
    work = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT / 'target/protected-mode'
    if work.exists():
        isolation.require_inside(work, work.parent, 'work')
        isolation.safe_rmtree(work, work.parent)
    work.mkdir(parents=True)
    lab = Lab(work)
    try:
        # 1. Not in protected mode: everything the package asked for works.
        lab.cli('install', '--yes', '--runtime', lab.stub, lab.pack(1))
        rep, err = lab.start()
        expect(rep, host='ok', loop='ok', written='ok')
        assert 'protected mode' not in err, err
        p = lab.cli('allow-internet', '--yes', ID, check=False)
        assert p.returncode != 0 and 'not in protected mode' in p.stdout + p.stderr, p.stdout + p.stderr
        print('  not in protected mode: network, loopback and written programs work; allow-internet says there is nothing to do: ok')

        # 2. A package of another channel with floor 2: refused, but the floor is seen.
        refused = lab.cli('install', '--yes', '--runtime', lab.stub, lab.pack(2, 'floor = 2\nchannel = "beta"\n'), check=False)
        assert refused.returncode != 0 and 'PANPAK-E4005' in refused.stdout + refused.stderr, refused.stdout + refused.stderr
        assert (lab.appdir / 'floor').read_text().strip() == '2'
        rep, err = lab.start()
        expect(rep, host='deny', loop='ok', written='deny')
        assert 'protected mode' not in err, f'text on a start with no terminal: {err}'
        rep, err = lab.start(tty=True)
        expect(rep, host='deny', loop='ok', written='deny')
        assert 'protected mode' in err and 'Your files are safe' in err and f'panpak update {ID}' in err, err
        listing = lab.cli('list').stdout
        assert 'protected mode: no internet' in listing, listing
        assert '"protected":{"internet_back_on":false}' in lab.cli('list', '--json').stdout
        assert f'({ID}) is in protected mode' in lab.cli('doctor', check=False).stdout
        print('  below the floor: no host network, loopback works, no written program; text on a terminal; list and doctor say it: ok')

        # 3. The internet back on, and cancelled.
        p = lab.cli('allow-internet', ID, check=False)
        assert p.returncode != 0 and not (lab.appdir / 'protected-net').exists(), 'no terminal and no --yes: nothing is turned on'
        lab.cli('allow-internet', '--yes', ID)
        assert (lab.appdir / 'protected-net').is_file()
        rep, _ = lab.start()
        expect(rep, host='ok', loop='ok', written='deny')
        assert 'turned the internet back on' in lab.cli('list').stdout
        lab.cli('allow-internet', '--undo', ID)
        rep, _ = lab.start()
        expect(rep, host='deny', loop='ok', written='deny')
        print('  allow-internet: the host port is reachable again, written programs are not; --undo takes it away: ok')

        # 4. The fixed version: the mode ends by itself, with a message.
        lab.cli('allow-internet', '--yes', ID)
        fixed = lab.cli('install', '--yes', '--runtime', lab.stub, lab.pack(2, 'floor = 2\n'))
        assert 'Protected mode is over' in fixed.stdout + fixed.stderr, fixed.stdout + fixed.stderr
        assert not (lab.appdir / 'protected-net').exists()
        rep, err = lab.start(tty=True)
        expect(rep, host='ok', loop='ok', written='ok')
        assert 'protected mode' not in err, err
        assert 'protected mode' not in lab.cli('list').stdout
        print('  the fixed version ends the mode with a message, clears the choice, and everything works again: ok')
        print('protected mode PASS')
    finally:
        lab.listener.close()
        isolation.remove_owned(lab.runtime, work)


if __name__ == '__main__':
    main()
