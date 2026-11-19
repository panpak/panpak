#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Run a command with a time limit, and say which one went over it.

Usage: with_timeout.py <seconds> <command|script.py> [args...]

A test that hangs (on a bus, a pipe, a socket that nobody answers) would hold a CI job until the limit of the job, 30 or 40 minutes, and the
log would only say that the job was cancelled. Here the whole process group is stopped after <seconds> and the message names the command.
Exit status: the one of the command, or 124 when the limit was reached. A command that moves to a session of its own escapes the group; the limit
of the step in the workflow is the second net.
"""
import os
import signal
import subprocess
import sys
import time


def main(argv):
    if len(argv) < 2 or not argv[0].isdigit() or int(argv[0]) < 1:
        print(__doc__, file=sys.stderr)
        return 2
    limit = int(argv[0])
    cmd = argv[1:]
    if cmd[0].endswith('.py'):
        # A script of tests/ is run by the interpreter that runs this one (the files are not executable).
        cmd = [sys.executable, *cmd]
    p = subprocess.Popen(cmd, start_new_session=True)
    try:
        return p.wait(timeout=limit)
    except subprocess.TimeoutExpired:
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
