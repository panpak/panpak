#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A tiny FUSE server for the tests: an empty, readable and writable-looking folder, standing for `gvfsd-fuse`.

Usage as a module: `mount(path)` returns the server process (or None when the machine cannot make a FUSE mount: no `fusermount3`, no `/dev/fuse`, or the mount is refused);
`unmount(path, proc)` ends it. It speaks the raw protocol of /dev/fuse (INIT, GETATTR, LOOKUP, OPENDIR, READDIR, RELEASEDIR, STATFS) and nothing else.
Run as a program, `fuse_server.py MOUNTPOINT` serves until it is stopped; it prints `mounted` when the mount is there.
"""
import array
import os
import shutil
import socket
import struct
import subprocess
import sys
import time

DIR_MODE = 0o40755


def serve(mnt):
    a, b = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    env = dict(os.environ, _FUSE_COMMFD=str(b.fileno()))
    p = subprocess.Popen(['fusermount3', '-o', 'fsname=panpak-test,subtype=panpak-test', '--', mnt], env=env, pass_fds=[b.fileno()],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    b.close()
    a.settimeout(10)
    try:
        _, anc, _, _ = a.recvmsg(1, socket.CMSG_LEN(struct.calcsize('i')))
        fd = array.array('i', anc[0][2][:4])[0]
    except (OSError, IndexError):
        p.wait()
        return 1
    if p.wait() != 0:
        return 1
    print('mounted', flush=True)

    def reply(unique, err, body=b''):
        os.write(fd, struct.pack('<IiQ', 16 + len(body), err, unique) + body)

    attr = struct.pack('<QQQQQQIIIIIIIIII', 1, 0, 0, 0, 0, 0, 0, 0, 0, DIR_MODE, 2, os.getuid(), os.getgid(), 0, 4096, 0)
    while True:
        try:
            req = os.read(fd, 1 << 20)
        except OSError:
            return 0
        _, op, unique = struct.unpack('<IIQ', req[:16])
        if op == 26:  # INIT
            reply(unique, 0, (struct.pack('<IIIIHHIIHHI', 7, 31, 0, 0, 0, 0, 0, 0, 0, 0, 0) + bytes(64))[:64])
        elif op == 3:  # GETATTR
            reply(unique, 0, struct.pack('<QII', 1, 0, 0) + attr)
        elif op == 27:  # OPENDIR
            reply(unique, 0, struct.pack('<QII', 1, 0, 0))
        elif op == 28:  # READDIR: nothing in it
            reply(unique, 0)
        elif op in (29, 25):  # RELEASEDIR, RELEASE
            reply(unique, 0)
        elif op == 17:  # STATFS
            reply(unique, 0, bytes(80))
        elif op == 2:  # FORGET: no reply
            pass
        elif op == 1:  # LOOKUP
            reply(unique, -2)
        else:
            reply(unique, -38)


def mount(path):
    """Mount the server at `path` (a folder that exists). None if this machine cannot."""
    if not (shutil.which('fusermount3') and os.path.exists('/dev/fuse')):
        return None
    proc = subprocess.Popen([sys.executable, os.path.abspath(__file__), str(path)], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    line = proc.stdout.readline()
    if line.strip() != 'mounted':
        proc.kill()
        proc.wait()
        return None
    return proc


def unmount(path, proc):
    subprocess.run(['fusermount3', '-u', '-z', str(path)], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if proc is not None:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
    for _ in range(20):
        if not os.path.ismount(str(path)):
            return
        time.sleep(0.1)


if __name__ == '__main__':
    sys.exit(serve(sys.argv[1]))
