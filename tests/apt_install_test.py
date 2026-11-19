#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""scripts/apt-install.sh: a first attempt that hangs on the first mirror is stopped, the mirror is replaced, and the second attempt installs."""
import os
import subprocess
import tempfile
import time
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "scripts" / "apt-install.sh"


def run(tmp, mirror, apt_body, limit=2):
    sources = tmp / "ubuntu.sources"
    sources.write_text(f"URIs: http://{mirror}/ubuntu/\n")
    fake = tmp / "apt-get"
    fake.write_text(f"#!/bin/sh\necho \"$@\" >> '{tmp}/calls'\n{apt_body.replace('SRC', str(sources))}\n")
    fake.chmod(0o755)
    env = dict(os.environ, APT_GET=str(fake), APT_SOURCES=str(sources), APT_SUDO="", APT_LIMIT=str(limit))
    start = time.time()
    r = subprocess.run([str(SCRIPT), "pkg-a", "pkg-b"], env=env, capture_output=True, text=True, timeout=60)
    return r, time.time() - start, sources.read_text()


def main():
    with tempfile.TemporaryDirectory() as t:
        tmp = Path(t)
        # The first mirror hangs, the second answers.
        hang_on_azure = 'if grep -q azure SRC; then sleep 300; fi; exit 0'
        r, took, text = run(tmp, "azure.archive.ubuntu.com", hang_on_azure)
        assert r.returncode == 0, r
        assert "trying archive.ubuntu.com" in r.stderr and "archive.ubuntu.com/ubuntu/" in text and "azure" not in text, (r.stderr, text)
        assert took < 30, took
        assert "install -y -qq pkg-a pkg-b" in (tmp / "calls").read_text()
    with tempfile.TemporaryDirectory() as t:
        tmp = Path(t)
        # A good first mirror: one attempt, nothing rewritten.
        r, _, text = run(tmp, "azure.archive.ubuntu.com", "exit 0")
        assert r.returncode == 0 and "trying" not in r.stderr and "azure" in text, (r.stderr, text)
    with tempfile.TemporaryDirectory() as t:
        # Both fail: it says so and fails.
        r, _, _ = run(Path(t), "azure.archive.ubuntu.com", "exit 100")
        assert r.returncode == 1 and "could not be installed" in r.stderr, r
    print("apt_install PASS")


if __name__ == "__main__":
    main()
