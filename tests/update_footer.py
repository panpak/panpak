#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""`panpak update` against a server whose footer lies (PP-03), with real binaries.

The footer of the remote file was parsed but not checked for layout (`check_layout`): the index and the manifest had to be adjacent and the
manifest and signature had to end where the footer begins, and none of it was asked. With a manifest length of 0 the range that was asked for
(`end = off + len - 1`) was empty or wrapped around, the size that the body should have was taken as "no limit", and the body was read to the end of the
connection before the signature was looked at.

Checked, for several footers (zero lengths, a layout that does not close, lengths that point past the file): the update fails with a clear error and the server
saw one request, the footer. No body is asked for, so none is read.
"""
import http.server
import os
from pathlib import Path
import random
import socket
import struct
import subprocess
import sys
import tempfile
import threading

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN, PUBLIC, resign  # noqa: E402

ID = 'org.example.incremental'
BIG = 48 << 20   # what a hostile server would be happy to send for any range


def footer(index_off, index_len, man_off, man_len):
    return struct.pack('<8sIIQQQQ32s', b'PANP\0\1\0\0', 1, 0, index_off, index_len, man_off, man_len, bytes(32))


# (name, footer bytes, total size of the remote file)
CASES = [
    ('zero lengths that close', footer(0, 0, 0, 0), 72 + 80),
    ('manifest of length 0 after an index', footer(0, 100, 100, 0), 100 + 72 + 80),
    ('index of length 0', footer(0, 0, 0, 40), 40 + 72 + 80),
    ('index and manifest not adjacent', footer(0, 10, 500, 40), 1000),
    ('the signature block is over 64 KiB', footer(0, 10, 10, 40), 100000),
    ('the signature block is shorter than one signature', footer(0, 10, 10, 40), 10 + 40 + 71 + 80),
    ('offsets past the end of the file', footer(1 << 40, 10, (1 << 40) + 10, 40), 1000),
    ('lengths that overflow', footer(0, 2 ** 64 - 1, 5, 2 ** 64 - 1), 1000),
]


class Hostile(http.server.BaseHTTPRequestHandler):
    log = []
    case = None

    def log_message(self, *a):
        pass

    def do_GET(self):
        rng = self.headers.get('Range', '')
        Hostile.log.append(rng)
        name, foot, total = Hostile.case
        if rng.startswith('bytes=-'):
            n = int(rng[7:])
            body = foot[-n:]
            self.send_response(206)
            self.send_header('Content-Range', f'bytes {total - n}-{total - 1}/{total}')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        # Any other range: a body far bigger than anything the client has agreed to, with a Content-Range that fits it.
        self.send_response(206)
        self.send_header('Content-Range', f'bytes 0-{BIG - 1}/{max(total, BIG)}')
        self.send_header('Content-Length', str(BIG))
        self.end_headers()
        sent = 0
        try:
            while sent < BIG:
                self.wfile.write(b'\0' * (1 << 20))
                sent += 1 << 20
        except OSError:
            pass
        Hostile.sent = sent


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    return p


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-footer-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), XDG_CONFIG_HOME=str(tmp / 'config'),
                   XDG_RUNTIME_DIR=str(runtime), LC_ALL='C', PANPAK_NO_BACKGROUND='1')
        env.pop('WAYLAND_DISPLAY', None)
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        sock = socket.socket()
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
        sock.close()
        tree = tmp / 'tree'
        tree.mkdir()
        (tree / 'hello').write_text('#!/bin/sh\necho hi\n')
        (tree / 'hello').chmod(0o755)
        (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="T"\nversion="1"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
        (tree / 'data.bin').write_bytes(random.Random(5).randbytes(50_000))
        out = tmp / 'v1.panpak'
        run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', out], env)
        out.write_bytes(resign(out.read_bytes(), '1.0', port))
        run([BIN / 'panpak', 'install', '--yes', '--runtime', BIN / 'panpak-stub', out], env)
        httpd = http.server.ThreadingHTTPServer(('127.0.0.1', port), Hostile)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        failures = []
        try:
            for case in CASES:
                Hostile.case = case
                Hostile.log = []
                Hostile.sent = 0
                p = run([BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '10', ID], env, ok=False)
                assert p.returncode != 0, f'{case[0]}: the update did not fail: {p.stdout}'
                if len(Hostile.log) != 1 or 'panic' in p.stderr:
                    failures.append(f'{case[0]}: the client asked for a body of a file whose footer is not coherent: {Hostile.log} ({Hostile.sent >> 20} MiB sent) {p.stderr.strip()[-80:]}')
                    continue
                print(f'  {case[0]}: refused after the footer ({p.stderr.strip().splitlines()[-1][:110]})')
            assert not failures, 'FAILED:\n' + '\n'.join(failures)
            print(f'{len(CASES)} footers that do not close or that are empty: the update stops after the footer, no body is asked for PASS')
        finally:
            httpd.shutdown()
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
