#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A test never reaches the session bus of the real user (a notification would arrive on the real desktop).

The runner `tests/with_timeout.py` gives every command a canary in place of the session bus and fails the run if anything connects to it. This test checks the runner:
a command that connects fails with the message, one that does not passes, and one that gives itself a bus of its own is not counted. It also checks that a test that
starts from `isolation.require_isolated_home` has no address that leads to the real bus when it is not run by the runner.
"""
import os
import socket
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RUNNER = ROOT / 'tests/with_timeout.py'


def run(code, env=None):
    return subprocess.run([sys.executable, str(RUNNER), '30', sys.executable, '-c', code], capture_output=True, text=True, env=env or os.environ)


def main():
    connect = "import os, socket; s = socket.socket(socket.AF_UNIX); s.connect(os.environ['DBUS_SESSION_BUS_ADDRESS'][len('unix:path='):])"
    p = run(connect)
    assert p.returncode != 0 and 'connected 1 time(s) to the session bus' in p.stderr, (p.returncode, p.stderr)
    print('a command that connects to the session bus fails the run PASS')
    p = run('print("quiet")')
    assert p.returncode == 0 and 'BUS' not in p.stderr, p.stderr
    print('a command that does not connect passes PASS')
    # A test with a bus of its own, and a runtime folder of its own, never meets the canary.
    own = (
        "import os, socket, tempfile\n"
        "d = tempfile.mkdtemp(prefix='own-bus-')\n"
        "srv = socket.socket(socket.AF_UNIX); srv.bind(d + '/bus'); srv.listen(1)\n"
        "c = socket.socket(socket.AF_UNIX); c.connect(d + '/bus')\n"
        "os.unlink(d + '/bus'); os.rmdir(d)\n"
    )
    p = run(own)
    assert p.returncode == 0 and 'BUS' not in p.stderr, p.stderr
    print('a test with a bus of its own is not counted PASS')
    # Run by hand (no runner), a test of the suite has an address that leads nowhere.
    env = {k: v for k, v in os.environ.items() if k != 'PANPAK_CANARY_BUS'}
    env['DBUS_SESSION_BUS_ADDRESS'] = f'unix:path=/run/user/{os.getuid()}/bus'
    with tempfile.TemporaryDirectory(prefix='bus-guard-') as d:
        code = (
            "import sys; sys.path.insert(0, 'tests'); import isolation, os; "
            f"isolation.require_isolated_home({d!r} + '/home', {d!r}); print(os.environ['DBUS_SESSION_BUS_ADDRESS'])"
        )
        p = subprocess.run([sys.executable, '-c', code], capture_output=True, text=True, env=env, cwd=ROOT)
        assert p.returncode == 0 and p.stdout.strip() == 'unix:path=/nonexistent/panpak-test-no-bus', (p.stdout, p.stderr)
    print('run by hand, a test has no way to the real bus PASS')
    print('bus guard PASS')


if __name__ == '__main__':
    main()
