#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Run a command with a time limit, and say which one went over it.

Usage: with_timeout.py <seconds> <command|script.py> [args...]

The command also runs with a **canary** in place of the session bus: `DBUS_SESSION_BUS_ADDRESS` and `XDG_RUNTIME_DIR` lead to a socket that nobody serves. A test that
uses the bus of the person at the keyboard (notifications, calls) is a test that does not give itself a bus of its own (`tests/dbus_lab.py`, a private `dbus-daemon`, or
none: an address that leads nowhere); here it would connect to the canary, and the run fails and says so. A test that sets its own bus and runtime folder never meets it.

A test that hangs (on a bus, a pipe, a socket that nobody answers) would hold a CI job until the limit of the job, 30 or 40 minutes, and the
log would only say that the job was cancelled. Here the whole process group is stopped after <seconds> and the message names the command.
Exit status: the one of the command, or 124 when the limit was reached. A command that moves to a session of its own escapes the group; the limit
of the step in the workflow is the second net.
"""
import os
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time


# The variables that lead to the desktop of the person at the keyboard. No test run starts with them: a window, a notification or a clipboard write of a test
# must not be able to reach that desktop, even by mistake (a test that needs a display or a bus makes its own and puts it in the environment it hands out).
DESKTOP_VARS = ('DISPLAY', 'WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'XAUTHORITY')


class Canary:
    """A socket named `bus` in a folder of its own that counts the connections it gets: the stand-in for the session bus of the real user."""

    def __init__(self):
        self.dir = tempfile.mkdtemp(prefix='panpak-canary-')
        os.chmod(self.dir, 0o700)
        self.hits = 0
        self.who = []
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.bind(os.path.join(self.dir, 'bus'))
        self.sock.listen(16)
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def env(self):
        return {'XDG_RUNTIME_DIR': self.dir, 'DBUS_SESSION_BUS_ADDRESS': f'unix:path={self.dir}/bus', 'PANPAK_CANARY_BUS': '1'}

    def _serve(self):
        while True:
            try:
                c, _ = self.sock.accept()
            except OSError:
                return
            self.hits += 1
            # Who it was, as far as the system still says (the program may be gone already).
            try:
                pid = struct.unpack('3i', c.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize('3i')))[0]
                with open(f'/proc/{pid}/cmdline', 'rb') as f:
                    who = f.read().replace(b'\0', b' ').decode(errors='replace').strip()
                self.who.append(f'{pid}: {who}')
            except (OSError, struct.error):
                self.who.append('(gone before it could be read)')
            c.close()

    def names(self):
        """Who connected, each program once with how many times (at most eight), the process numbers left out."""
        counts = {}
        for w in self.who:
            k = w.split(': ', 1)[-1]
            counts[k] = counts.get(k, 0) + 1
        shown = sorted(counts.items(), key=lambda kv: -kv[1])[:8]
        return '; '.join(f'{k} x{n}' for k, n in shown)

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass
        shutil.rmtree(self.dir, ignore_errors=True)


def main(argv):
    if len(argv) < 2 or not argv[0].isdigit() or int(argv[0]) < 1:
        print(__doc__, file=sys.stderr)
        return 2
    limit = int(argv[0])
    cmd = argv[1:]
    if cmd[0].endswith('.py'):
        # A script of tests/ is run by the interpreter that runs this one (the files are not executable).
        cmd = [sys.executable, *cmd]
    canary = Canary()
    env = dict(os.environ, **canary.env())
    for name in DESKTOP_VARS:
        env.pop(name, None)
    p = subprocess.Popen(cmd, start_new_session=True, env=env)
    try:
        status = p.wait(timeout=limit)
        time.sleep(0.2)  # a connection that was just made is counted
        canary.close()
        if canary.hits:
            print(f"::error::BUS: `{' '.join(cmd)}` connected {canary.hits} time(s) to the session bus (the canary stands in for the real one; {canary.names()}): a test gets a bus of its own or none (tests/dbus_lab.py, or DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent)", file=sys.stderr, flush=True)
            return status or 1
        return status
    except subprocess.TimeoutExpired:
        canary.close()
        print(f"::error::TIMEOUT: `{' '.join(cmd)}` was still running after {limit} s and was stopped (the limit is in .github/workflows/ci.yml)", file=sys.stderr, flush=True)
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(p.pid, sig)
            except ProcessLookupError:
                break
            end = time.time() + 5
            while time.time() < end and p.poll() is None:
                time.sleep(0.1)
            if p.poll() is not None:
                break
        p.wait()
        return 124


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
