#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""How much do the running apps keep in /tmp, /dev/shm and TMPDIR of their view? A measurement by hand, not a test.

usage: tmp_use.py SECONDS OUT.json

Run it while an app does the work to measure (an export in OpenShot or Audacity, a long session of an Electron app). Every 0.5 s
it reads the files below /proc/<pid>/root of the processes that are inside a view (the ones with the seccomp filter of the inner
stub), per app id, in disk blocks, and writes the largest value seen for each of /tmp, /dev/shm and /var/tmp (TMPDIR).
The cap of /tmp is 256 MiB (view::TMP_SIZE): a value near it means the cap is too small for that work.
"""
import json
import os
import sys
import time


def app_of(pid):
    try:
        env = open(f'/proc/{pid}/environ', 'rb').read().split(b'\0')
    except OSError:
        return None
    for e in env:
        if e.startswith(b'PANPAK_APP_ID='):
            return e.split(b'=', 1)[1].decode()
    return None


def in_view(pid):
    """A process of the app itself: it has the seccomp filter of the inner stub, and its root is not the host's."""
    try:
        if 'Seccomp:\t2' not in open(f'/proc/{pid}/status').read():
            return False
        root, host = os.stat(f'/proc/{pid}/root/'), os.stat('/')
        return (root.st_dev, root.st_ino) != (host.st_dev, host.st_ino)
    except OSError:
        return False


def used(path):
    total = 0
    for dp, _, files in os.walk(path, onerror=lambda e: None):
        for f in files:
            try:
                total += os.lstat(os.path.join(dp, f)).st_blocks * 512
            except OSError:
                pass
    return total


def main():
    end = time.time() + float(sys.argv[1])
    out = sys.argv[2]
    best = {}
    apps = {}
    while time.time() < end:
        for p in filter(str.isdigit, os.listdir('/proc')):
            apps.setdefault(p, app_of(p))
            app = apps[p]
            if not app or not in_view(p):
                continue
            for what, path in (('tmp', 'tmp'), ('shm', 'dev/shm'), ('tmpdir', 'var/tmp')):
                k = f'{app}:{what}'
                best[k] = max(best.get(k, 0), used(f'/proc/{p}/root/{path}'))
        time.sleep(0.5)
        with open(out, 'w') as f:
            json.dump({k: round(v / 1024) for k, v in best.items()}, f, indent=1)


if __name__ == '__main__':
    main()
