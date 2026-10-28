#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""End-to-end test of the socket probe (PANPAK_SOCKET_PROBE), a measurement mode.

Usage: python3 tests/socket_probe.py [work-directory]
Build release first. Runs a Python app in an isolated HOME under the Landlock sandbox, with and
without the probe, and checks the log, the exit code, the signal forwarding and that the probe is
off unless asked for.
"""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
import app_dirs  # noqa: E402
import isolation  # noqa: E402

APP = r'''#!/usr/bin/python3
import ctypes, json, os, socket, struct, subprocess, sys, threading, time
out = {}
rt = os.environ['XDG_RUNTIME_DIR']
def tried(name, f):
    try:
        f(); out[name] = 'ok'
    except OSError as e:
        out[name] = e.errno
s = socket.socket(socket.AF_UNIX)
tried('unix_connect', lambda: s.connect(rt + '/probe-sock'))
t = threading.Thread(target=lambda: tried('unix_connect_thread', lambda: socket.socket(socket.AF_UNIX).connect(rt + '/probe-sock')))
t.start(); t.join()
tried('abstract_connect', lambda: socket.socket(socket.AF_UNIX).connect('\0probe-abstract'))
tried('inet_connect', lambda: socket.socket().connect(('127.0.0.1', 4242)))
tried('udp_sendto', lambda: socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(b'x', ('127.0.0.1', 9)))
tried('udp6_sendto', lambda: socket.socket(socket.AF_INET6, socket.SOCK_DGRAM).sendto(b'x', ('::1', 9)))
tried('unix_dgram_sendto', lambda: socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM).sendto(b'x', rt + '/probe-dgram'))
a, b = socket.socketpair()
tried('scm_rights', lambda: a.sendmsg([b'x'], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, struct.pack('ii', 0, 1))]))
tried('sendmsg_plain', lambda: a.sendmsg([b'y']))
tried('send_connected', lambda: a.send(b'z'))
libc = ctypes.CDLL(None, use_errno=True)
out['unshare_newuser'] = 'ok' if libc.syscall(272, 0x10000000) == 0 else ctypes.get_errno()
subprocess.run(['true'])
out['seccomp'] = int(next(l for l in open('/proc/self/status') if l.startswith('Seccomp:')).split()[1])
out['env_probe'] = os.environ.get('PANPAK_SOCKET_PROBE')
json.dump(out, open(os.environ['XDG_DATA_HOME'] + '/report.json', 'w'))
mode = sys.argv[1] if len(sys.argv) > 1 else ''
if mode == 'exit7':
    sys.exit(7)
if mode == 'sleep':
    open(os.environ['XDG_DATA_HOME'] + '/ready', 'w').close()
    time.sleep(60)
'''


def main():
    work = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT / 'target/socket-probe'
    if work.exists():
        isolation.require_inside(work, work.parent, 'work')
        subprocess.run(['chmod', '-R', 'u+w', str(work)], check=True)
        isolation.safe_rmtree(work, work.parent)
    work.mkdir(parents=True)
    app_dirs.PROBE = APP
    lab = app_dirs.Lab(work)
    try:
        run(lab, work)
    finally:
        for bus in (lab.bus, lab.sysbus):
            bus.terminate()
            bus.wait(timeout=5)
        isolation.safe_rmtree(lab.runtime, work)


def run(lab, work):
    p = lab.install('')
    assert p.returncode == 0, p.stdout + p.stderr
    userdata = lab.home / f'.local/share/panpak/userdata/{app_dirs.ID}/data'
    rt = lab.runtime
    # A listening socket for the app to find, and a datagram one.
    srv = socket.socket(socket.AF_UNIX)
    srv.bind(f'{rt}/probe-sock')
    srv.listen(8)
    dg = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
    dg.bind(f'{rt}/probe-dgram')
    launcher = str(lab.home / '.local/bin' / app_dirs.ID)

    def start(*args, probe=None):
        env = dict(lab.env)
        if probe:
            env['PANPAK_SOCKET_PROBE'] = str(probe)
        return subprocess.Popen([launcher, *args], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True)

    def report():
        return json.loads((userdata / 'report.json').read_text())

    # Off by default: no filter in the app, no log.
    pr = start('exit7')
    out, err = pr.communicate(timeout=60)
    assert pr.returncode == 7, (pr.returncode, err)
    rep = report()
    assert rep['seccomp'] == 0 and 'SOCKET PROBE' not in err, (rep, err)
    (userdata / 'report.json').unlink()
    print('probe off by default: ok')

    # On: exit code, log, filter active, variable hidden from the app.
    log = work / 'probe.jsonl'
    pr = start('exit7', probe=log)
    out, err = pr.communicate(timeout=120)
    assert pr.returncode == 7, (pr.returncode, err)
    assert 'SOCKET PROBE is on' in err, err
    rep = report()
    assert rep['seccomp'] == 2 and rep['env_probe'] is None, rep
    assert rep['unix_connect'] == 'ok' and rep['unix_connect_thread'] == 'ok', rep  # CONTINUE: the kernel runs the call
    assert oct(log.stat().st_mode & 0o777) == '0o600'
    lines = [json.loads(l) for l in log.read_text().splitlines()]
    calls = [l for l in lines if 'syscall' in l]
    summary = [l['summary'] for l in lines if 'summary' in l][-1]  # the last one counts

    def find(syscall, **kw):
        return [c for c in calls if c['syscall'] == syscall and all(c.get(k) == v for k, v in kw.items())]

    sock = f'{rt}/probe-sock'
    assert find('connect', family='AF_UNIX', path=sock), 'unix connect missing'
    assert len({c['tid'] for c in find('connect', family='AF_UNIX', path=sock)}) == 2, 'the thread was not seen'
    assert find('connect', family='AF_UNIX', path='@probe-abstract')
    assert find('connect', family='AF_INET', addr='127.0.0.1', port=4242)
    assert find('sendto', family='AF_INET', addr='127.0.0.1', port=9)
    assert find('sendto', family='AF_INET6', addr='::1', port=9)
    assert find('sendto', family='AF_UNIX', path=f'{rt}/probe-dgram')
    rights = [c for c in calls if c['syscall'] == 'sendmsg' and c.get('scm_rights')]
    assert rights and rights[0]['fds'] == 2 and rights[0]['control'], rights
    assert find('unshare', newuser=True), 'unshare with CLONE_NEWUSER not seen'
    # Calls with neither address nor control data are counted, not logged.
    assert summary.get('sendmsg:noaddr', 0) >= 1, summary
    assert not any(c['syscall'] == 'sendmsg' and 'family' not in c for c in calls)
    assert summary['connect:AF_UNIX:addr'] >= 3 and summary['sendmsg:scm_rights'] >= 1, summary
    assert 'stale_reads' not in summary, summary
    print('log: ok', {k: v for k, v in summary.items() if not k.startswith('clone')})

    # The log is appended to, and never through a link.
    pr = start('exit7', probe=log)
    pr.communicate(timeout=120)
    assert len(log.read_text().splitlines()) > len(lines)
    link = work / 'link.jsonl'
    link.symlink_to(work / 'elsewhere')
    pr = start('exit7', probe=link)
    out, err = pr.communicate(timeout=60)
    assert pr.returncode != 0 and not (work / 'elsewhere').exists(), err
    print('log file: appended, links refused')

    # A signal sent to the supervisor reaches the app, and the exit code follows (128 + 15).
    (userdata / 'ready').unlink(missing_ok=True)
    pr = start('sleep', probe=work / 'probe2.jsonl')
    for _ in range(300):
        if (userdata / 'ready').exists():
            break
        time.sleep(0.1)
    assert (userdata / 'ready').exists(), 'the app did not start'
    pr.send_signal(signal.SIGTERM)
    out, err = pr.communicate(timeout=30)
    assert pr.returncode == 128 + signal.SIGTERM, (pr.returncode, err)
    print('signals and exit code: ok')

    srv.close()
    dg.close()
    print('all ok')


if __name__ == '__main__':
    main()
