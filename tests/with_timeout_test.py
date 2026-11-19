#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""tests/with_timeout.py: the exit status of the command passes through, and a command that hangs is stopped with a message that names it."""
import subprocess
import sys
import time
from pathlib import Path

TOOL = Path(__file__).with_name("with_timeout.py")


def run(*args):
    return subprocess.run([sys.executable, str(TOOL), *map(str, args)], capture_output=True, text=True, timeout=60)


def main():
    r = run(10, "true")
    assert r.returncode == 0, r
    r = run(10, sys.executable, "-c", "raise SystemExit(7)")
    assert r.returncode == 7, r
    # A command that hangs, with a child that holds the pipe: both go, in about the limit, and the message names the command.
    start = time.time()
    r = run(2, "sh", "-c", "sleep 300 & sleep 300")
    took = time.time() - start
    assert r.returncode == 124 and "TIMEOUT" in r.stderr and "sleep 300" in r.stderr and "2 s" in r.stderr, r
    assert took < 15, took
    # A script that is not executable is run by the interpreter, with its arguments.
    import tempfile
    with tempfile.NamedTemporaryFile('w', suffix='.py') as f:
        f.write('import sys\nraise SystemExit(int(sys.argv[1]))\n')
        f.flush()
        assert run(10, f.name, 5).returncode == 5
    assert run().returncode == 2 and run("x", "true").returncode == 2
    print("with_timeout PASS")


if __name__ == "__main__":
    main()
