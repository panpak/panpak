#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""End-to-end test of protected mode (PANPAK-FORMAT.md 3.1.10): an installed version below the floor its author set.

Usage: python3 tests/protected_mode.py [work-directory]
Build release first (cargo build --release --target x86_64-unknown-linux-musl).
Everything runs in an isolated HOME inside the work directory (see tests/isolation.py). The app asks for the network (a TCP port where this script
listens, and one of its own to listen on) and for `exec_written`. It reports what it can do (TCP to the host, a server on its own loopback, a program it
wrote), and it sends a UDP datagram, a DNS query and an IPv6 datagram to sockets of this script, and asks to open an address (a fake `xdg-open` records it):
the script says whether each one arrived.

  1. not in protected mode: everything works and arrives;
  2. a package of another channel with floor 2 is refused (and the floor is seen): the app is marked and the next start is in protected mode: no host TCP,
     no UDP, no DNS, no IPv6, no address opened, the loopback inside the app works, no written program is run; a start with nobody to ask writes one line
     with no instruction; `protected --json` says the state and the actions;
     The same two starts again with no isolated view (bubblewrap masked): the network is taken away by a filter of the system calls, so that no IP socket
     is made at all (and no datagram, query or connection leaves), and the programs that the app wrote are still not run. (Without a view the app
     can run the `xdg-open` of the system itself: the addresses are not stopped there, and the test does not say they are.)
  3. a program of the terminal started from an interactive terminal asks its question with one key, and the answer is acted on; a graphical app (a window
     class in `[desktop]`) is not asked in the terminal, whatever the terminal is;
  4. `allow-internet --yes` (for scripts): the network is back (TCP, UDP, DNS, addresses), written programs still are not; `--undo` takes it away again;
  5. the fixed version (revision 2, floor 2) installed over it: the mode ends with a message, the choice is gone, everything works again.
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
port, lport, uport, dport, u6port = (int(x) for x in sys.argv[1:6])
out = []
def attempt(name, fn):
    try:
        fn()
        out.append(f'{name}=ok')
    except Exception:
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
def udp(family, addr, data):
    def f():
        s = socket.socket(family, socket.SOCK_DGRAM)
        s.sendto(data, addr)
        s.close()
    return f
def open_address():
    subprocess.run(['xdg-open', 'https://example.org/?data=secret'], check=True, timeout=10, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
attempt('host', host)
attempt('loop', loop)
attempt('written', run_written)
attempt('udp', udp(socket.AF_INET, ('127.0.0.1', uport), b'udp-datagram'))
# A name lookup that must not leave: a DNS query (id, flags, one question for example.org, type A) sent to the host, which listens where the app is told to.
query = b'\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x07example\x03org\x00\x00\x01\x00\x01'
attempt('dns', udp(socket.AF_INET, ('127.0.0.1', dport), query))
if u6port:
    attempt('udp6', udp(socket.AF_INET6, ('::1', u6port), b'udp6-datagram'))
attempt('open', open_address)
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
        # A fake `xdg-open` that the outer stub finds in its PATH: it records the address it is given.
        self.opened = work / 'opened.txt'
        fake = work / 'fakebin'
        fake.mkdir()
        (fake / 'xdg-open').write_text(f'#!/bin/sh\necho "$1" >> {self.opened}\n')
        (fake / 'xdg-open').chmod(0o755)
        self.env = {'PATH': f'{fake}:{os.environ["PATH"]}', 'HOME': str(self.home), 'XDG_RUNTIME_DIR': self.runtime, 'PANPAK_TEST_REAL_HOME': str(self.home), 'LC_ALL': 'C'}
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

        def udp_socket(family, host):
            try:
                s = socket.socket(family, socket.SOCK_DGRAM)
                s.bind((host, 0))
                s.setblocking(False)
                return s
            except OSError:
                return None
        self.udp = {'udp': udp_socket(socket.AF_INET, '127.0.0.1'), 'dns': udp_socket(socket.AF_INET, '127.0.0.1'), 'udp6': udp_socket(socket.AF_INET6, '::1')}
        self.appdir = self.home / '.local/share/panpak/apps' / ID
        self.n = 0
        self.graphical = False

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
        desktop = '[desktop]\nwm_class = "Protected"\n' if self.graphical else ''
        (tree / 'panpak.toml').write_text(
            f'[app]\nid = "{ID}"\nname = "Protected"\nversion = "{revision}"\nentry = "bin/probe"\n[release]\nrevision = {revision}\n{release_extra}{desktop}'
            f'[permissions]\nnetwork = true\nnetwork_allow_tcp = [{self.port}, {self.lport}]\nnetwork_bind_tcp = [{self.lport}]\nexec_written = true\n[update]\nkey = "{self.pub}"\n')
        pkg = self.work / f'p{self.n}.panpak'
        self.cli('pack', tree, '--stub', self.stub, '--secret-file', self.key, '-o', pkg)
        return pkg

    def report_file(self):
        return self.home / f'.local/share/panpak/userdata/{ID}/.local/share/report.txt'

    def args(self):
        u6 = self.udp['udp6'].getsockname()[1] if self.udp['udp6'] else 0
        return [str(self.port), str(self.lport), str(self.udp['udp'].getsockname()[1]), str(self.udp['dns'].getsockname()[1]), str(u6)]

    def drain(self):
        """What the sockets of this script got since the last call: the names of the datagram sockets that received one."""
        got = set()
        for name, s in self.udp.items():
            while s:
                try:
                    s.recv(2048)
                    got.add(name)
                except BlockingIOError:
                    break
        return got

    def read_report(self):
        rep = dict(l.split('=', 1) for l in self.report_file().read_text().split() if '=' in l) if self.report_file().exists() else {}
        opened = self.opened.read_text().split() if self.opened.exists() else []
        self.opened.unlink(missing_ok=True)
        rep['arrived'] = sorted(self.drain())
        rep['opened'] = opened
        return rep

    def start(self, stdin='null', stderr='pipe', keys=None, masked=False):
        """Start the app through its launcher. `stdin`/`stderr` is `null`, `pipe` or `tty` (a pseudo terminal); with a tty on both, `keys` are typed one at a
        time as the prompts come. Returns the report and the text written on the standard error (and, with a tty, on the terminal)."""
        self.report_file().unlink(missing_ok=True)
        self.drain()
        cmd = [str(self.home / '.local/bin' / ID), *self.args()]
        if masked:
            # A copy of the machine where /usr/bin/bwrap cannot be used (as tests/view.py does): the start has no isolated view.
            cmd = ['/usr/bin/bwrap', '--unshare-user', '--bind', '/', '/', '--dev-bind', '/dev', '/dev', '--proc', '/proc', '--ro-bind', '/dev/null', '/usr/bin/bwrap', '--'] + cmd
        master = slave = None
        if 'tty' in (stdin, stderr):
            master, slave = os.openpty()
        p = subprocess.Popen(cmd, env=self.env, stdin=slave if stdin == 'tty' else subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                             stderr=slave if stderr == 'tty' else subprocess.PIPE)
        text = ''
        if master is not None:
            os.close(slave)
            keys = list(keys or [])
            end = time.time() + 90
            buf, seen = b'', 0
            while time.time() < end and p.poll() is None:
                r, _, _ = select.select([master], [], [], 0.3)
                if r:
                    try:
                        data = os.read(master, 4096)
                    except OSError:
                        break
                    buf += data
                    # A question ends with its choices, `[y/N]` or `[Y/n]`, and a space: the next key is typed when one is waiting.
                    if keys and buf[seen:].rstrip().endswith(b']'):
                        os.write(master, keys.pop(0).encode())
                        seen = len(buf)
            p.wait(timeout=90)
            try:
                while True:
                    r, _, _ = select.select([master], [], [], 0.2)
                    if not r:
                        break
                    buf += os.read(master, 4096)
            except OSError:
                pass
            os.close(master)
            text = buf.decode(errors='replace')
        else:
            p.wait(timeout=120)
        if stderr == 'pipe':
            text += p.stderr.read().decode(errors='replace')
        return self.read_report(), text


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
        everything = sorted(n for n, s in lab.udp.items() if s)
        # 1. Not in protected mode: everything the package asked for works, and every datagram arrives.
        lab.cli('install', '--yes', '--trust-first-key', '--runtime', lab.stub, lab.pack(1))
        rep, err = lab.start()
        everything_opened = ['https://example.org/?data=secret']
        expect(rep, host='ok', loop='ok', written='ok', arrived=everything, opened=everything_opened)
        assert 'protected mode' not in err and 'links do not open' not in err, err
        # The same with no view: Landlock filters the TCP ports and nothing filters UDP, so the datagrams arrive.
        rep, err = lab.start(masked=True)
        expect(rep, host='ok', loop='ok', written='ok', arrived=everything)
        assert lab.cli('protected', '--json', ID).stdout.strip() == '{"protected":false}'
        p = lab.cli('allow-internet', '--yes', ID, check=False)
        assert p.returncode != 0 and 'not in protected mode' in p.stdout + p.stderr, p.stdout + p.stderr
        print('  not in protected mode: network, UDP, DNS, addresses, loopback and written programs work: ok')

        # 2. A package of another channel with floor 2: refused, but the floor is seen, and the app is marked.
        refused = lab.cli('install', '--yes', '--trust-first-key', '--runtime', lab.stub, lab.pack(2, 'floor = 2\nchannel = "beta"\n'), check=False)
        assert refused.returncode != 0 and 'PANPAK-E4005' in refused.stdout + refused.stderr, refused.stdout + refused.stderr
        assert isolation.read_floor(lab.appdir) == 2 and (lab.appdir / 'protected').is_file()
        rep, err = lab.start()
        expect(rep, host='deny', loop='ok', written='deny', arrived=[], opened=[], open='deny')
        assert 'protected mode' in err and 'Your files are safe' in err, err
        # The app tried to open an address: it is told to the person, in one line with no instruction (the program of the terminal has no window).
        assert 'is in protected mode: links do not open until you update' in err, err
        assert 'panpak update' not in err and 'allow-internet' not in err, f'an instruction in the line: {err}'
        assert len([l for l in err.splitlines() if l.strip()]) <= 2, f'the line of the start, and the one of the refused address, are the most that is said: {err!r}'
        listing = lab.cli('list').stdout
        assert 'protected mode: no internet' in listing, listing
        assert '"protected":{"internet_back_on":false}' in lab.cli('list', '--json').stdout
        assert f'({ID}) is in protected mode' in lab.cli('doctor', check=False).stdout
        import json
        state = json.loads(lab.cli('protected', '--json', ID).stdout)
        assert state['protected'] and state['kind'] == 'terminal' and state['floor'] == 2 and state['revision'] == 1, state
        assert state['update'] is None and state['actions'] == ['internet_on'] and 'protected mode' in state['texts']['title'] and 'serious problem' in state['texts']['body'], state
        assert [s['id'] for s in json.loads(lab.cli('protected', '--json').stdout)] == [ID]
        # With no view: no IP socket at all.
        rep, err = lab.start(masked=True)
        expect(rep, host='deny', loop='deny', written='deny', udp='deny', dns='deny', arrived=[])
        assert 'protected mode' in err, err
        print('  below the floor: no host TCP, UDP, DNS, IPv6 or address; loopback works; no written program; one line without an instruction; list, doctor and `protected --json` say it: ok')

        # 3. A program of the terminal, started from an interactive terminal: the question, one key, no Enter.
        rep, text = lab.start(stdin='tty', stderr='tty', keys=['n'])
        expect(rep, host='deny', loop='ok', written='deny', arrived=[])
        assert 'Turn the internet back on for Protected? [y/N]' in text and 'can be used against you' in text, text
        assert not (lab.appdir / 'protected-net').exists()
        # Asked once: after a no, the next starts have the short line only (no question, no key waits).
        rep, text = lab.start(stdin='tty', stderr='tty', keys=[])
        expect(rep, host='deny', arrived=[])
        assert '[y/N]' not in text and 'protected mode: no internet. Your files are safe.' in text and 'downloaded' not in text, text
        # A new version asks again (here the record of the question is taken away, as a new floor or a new update would make it stale; protected_ui.py
        # raises the floor).
        (lab.appdir / 'protected-asked').unlink()
        rep, text = lab.start(stdin='tty', stderr='tty', keys=['y'])
        assert (lab.appdir / 'protected-net').is_file(), text
        expect(rep, host='ok', loop='ok', written='deny', arrived=everything, open='ok', opened=everything_opened)
        # With the internet on, the short line says so, and asks nothing: it was asked.
        rep, text = lab.start(stdin='tty', stderr='tty', keys=[])
        assert '[y/N]' not in text and 'you turned the internet back on' in text, text
        (lab.appdir / 'protected-asked').unlink()
        rep, text = lab.start(stdin='tty', stderr='tty', keys=['y'])
        assert 'Turn the internet off again for Protected? [y/N]' in text, text
        assert not (lab.appdir / 'protected-net').exists()
        expect(rep, host='deny', arrived=[], opened=[])
        # No terminal on the input (a script or a pipe): no question, one line, and the app goes on in protected mode.
        rep, text = lab.start(stdin='null', stderr='tty')
        expect(rep, host='deny', arrived=[])
        assert '[y/N]' not in text and 'protected mode' in text
        print('  a program of the terminal asks one question with one key and acts on the answer; with no terminal on the input it asks nothing: ok')

        # 4. For scripts: the choice with --yes and --undo, and no answer without them.
        p = lab.cli('allow-internet', ID, check=False)
        assert p.returncode != 0 and not (lab.appdir / 'protected-net').exists(), 'no terminal and no --yes: nothing is turned on'
        lab.cli('allow-internet', '--yes', ID)
        assert (lab.appdir / 'protected-net').is_file()
        rep, _ = lab.start()
        expect(rep, host='ok', loop='ok', written='deny', arrived=everything, open='ok')
        assert 'turned the internet back on' in lab.cli('list').stdout
        assert json.loads(lab.cli('protected', '--json', ID).stdout)['actions'] == ['internet_off']
        lab.cli('allow-internet', '--undo', ID)
        rep, _ = lab.start()
        expect(rep, host='deny', loop='ok', written='deny', arrived=[], opened=[])
        print('  allow-internet for scripts: the network, UDP, DNS and addresses are back, written programs are not; --undo takes it away: ok')

        # 5. The fixed version: the mode ends by itself, with a message, and the mark and the choice go.
        lab.cli('allow-internet', '--yes', ID)
        fixed = lab.cli('install', '--yes', '--trust-first-key', '--runtime', lab.stub, lab.pack(2, 'floor = 2\n'))
        assert 'The problem is fixed and the internet works again' in fixed.stdout + fixed.stderr and 'Protected is updated' in fixed.stdout + fixed.stderr, fixed.stdout + fixed.stderr
        assert not (lab.appdir / 'protected-net').exists() and not (lab.appdir / 'protected').exists()
        rep, err = lab.start(stderr='tty')
        expect(rep, host='ok', loop='ok', written='ok', arrived=everything)
        assert 'protected mode' not in err, err
        assert 'protected mode' not in lab.cli('list').stdout
        print('  the fixed version ends the mode with a message, clears the mark and the choice, and everything works again: ok')

        # 6. A graphical app (a window class in its package) is not asked in the terminal, even from an interactive one: one line, and the notification.
        lab.graphical = True
        lab.cli('install', '--yes', '--trust-first-key', '--runtime', lab.stub, lab.pack(3))
        refused = lab.cli('install', '--yes', '--trust-first-key', '--runtime', lab.stub, lab.pack(4, 'floor = 4\nchannel = "beta"\n'), check=False)
        assert 'PANPAK-E4005' in refused.stdout + refused.stderr
        assert json.loads(lab.cli('protected', '--json', ID).stdout)['kind'] == 'graphical'
        rep, text = lab.start(stdin='tty', stderr='tty', keys=[])
        expect(rep, host='deny', arrived=[])
        assert 'protected mode' in text and '[y/N]' not in text and '[Y/n]' not in text, text
        assert 'links do not open until you update' in text, text
        entry = (lab.home / f'.local/share/applications/panpak.{ID}.desktop').read_text()
        assert 'Actions=Uninstall;Internet;' in entry and f'allow-internet --ask {ID}' in entry and 'Name=Turn the internet back on' in entry, entry
        print('  a graphical app is not asked in the terminal; its menu entry gets the action to turn the internet back on: ok')
        print('protected mode PASS')
    finally:
        lab.listener.close()
        isolation.remove_owned(lab.runtime, work)


if __name__ == '__main__':
    main()
