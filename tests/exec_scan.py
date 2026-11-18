#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Does an app run programs that it wrote itself? A measurement by hand, not a test.

usage: exec_scan.py SECONDS OUT.json

Run it while an app works. Every 0.2 s it reads, for each process that is inside a view (the ones with the seccomp
filter of the inner stub), the program it runs (`/proc/<pid>/exe`) and the files mapped with the execute bit
(`/proc/<pid>/maps`), and keeps the ones that lie in a folder the app can write: its private home (but not the store
of programs below `.local/share/panpak`), `/tmp`, `/var/tmp` and `/dev/shm`. The paths are as the app sees them.

Two kinds are told apart: `exec` (a program started from such a folder: `execve` would be refused by the rule
"nothing written is executed") and `map` (a file mapped executable: allowed by that rule, listed to know). A mapping of a
memory file (`/memfd:`) or of a deleted file is listed as `memfd` and `deleted`.
"""
import json
import os
import sys
import time


def app_of(pid):
    try:
        env = open(f'/proc/{pid}/environ', 'rb').read().split(b'\0')
    except OSError:
        return None, None
    app = home = None
    for e in env:
        if e.startswith(b'PANPAK_APP_ID='):
            app = e.split(b'=', 1)[1].decode()
        elif e.startswith(b'HOME='):
            home = e.split(b'=', 1)[1].decode()
    return app, home


def in_view(pid):
    try:
        if 'Seccomp:\t2' not in open(f'/proc/{pid}/status').read():
            return False
        root, host = os.stat(f'/proc/{pid}/root/'), os.stat('/')
        return (root.st_dev, root.st_ino) != (host.st_dev, host.st_ino)
    except OSError:
        return False


def writable(path, home):
    """Is the path in a folder the app may write? The store of programs below the home is not."""
    if path.startswith(('/tmp/', '/var/tmp/', '/dev/shm/')):
        return True
    if home and path.startswith(home + '/'):
        rest = path[len(home) + 1:]
        return not rest.startswith('.local/share/panpak/')
    return False


def main():
    end = time.time() + float(sys.argv[1])
    out = sys.argv[2]
    found = {}
    info = {}
    procs = {}
    while time.time() < end:
        for p in filter(str.isdigit, os.listdir('/proc')):
            if p not in info:
                info[p] = app_of(p)
            app, home = info[p]
            if not app or not in_view(p):
                continue
            procs.setdefault(app, set()).add(p)
            try:
                exe = os.readlink(f'/proc/{p}/exe')
            except OSError:
                exe = ''
            if writable(exe.removesuffix(' (deleted)'), home):
                found.setdefault(app, {}).setdefault('exec', set()).add(exe)
            try:
                lines = open(f'/proc/{p}/maps').read().splitlines()
            except OSError:
                continue
            for line in lines:
                f = line.split(None, 5)
                if len(f) < 6 or 'x' not in f[1]:
                    continue
                path = f[5]
                if path.startswith('/memfd:'):
                    found.setdefault(app, {}).setdefault('memfd', set()).add(path)
                elif path.endswith(' (deleted)') and writable(path.removesuffix(' (deleted)'), home):
                    found.setdefault(app, {}).setdefault('deleted', set()).add(path)
                elif writable(path, home):
                    found.setdefault(app, {}).setdefault('map', set()).add(path)
        time.sleep(0.2)
    with open(out, 'w') as fh:
        json.dump({'processes seen': {a: len(v) for a, v in procs.items()}, **{a: {k: sorted(v) for k, v in d.items()} for a, d in found.items()}}, fh, indent=1)


if __name__ == '__main__':
    main()
