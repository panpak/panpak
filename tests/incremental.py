#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Range integration + hostile server regression; optional real old/new packages.

Usage: python3 tests/incremental.py [old.panpak new.panpak]
Build release binaries BEFORE invoking this test. Uses an isolated HOME/store.
"""
import json
import os
from pathlib import Path
import re
import socket
import struct
import subprocess
import sys
import tempfile
import time

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / "target/x86_64-unknown-linux-musl/release"
KEY = Ed25519PrivateKey.from_private_bytes(bytes([41]) * 32)
PUBLIC = KEY.public_key().public_bytes_raw().hex()


def run(args, env, ok=True):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=40)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    else:
        assert p.returncode != 0, p.stdout
    return p


def index(data):
    footer = struct.unpack("<8sIIQQQQ32s", data[-80:])
    _, _, _, off, size, _, _, _ = footer
    p = subprocess.run(["zstd", "-dc"], input=data[off:off + size], capture_output=True, check=True)
    raw = p.stdout
    n = struct.unpack_from("<I", raw)[0]
    chunks = [struct.unpack_from("<32sQII", raw, 4 + 48 * i) for i in range(n)]
    return footer, chunks


def resign(data, version, port):
    footer, _ = index(data)
    magic, ver, flags, off, ilen, moff, mlen, root = footer
    man = data[moff:moff + mlen].decode()
    man = re.sub(r'^id = .*$', 'id = "org.example.incremental"', man, count=1, flags=re.M)
    man = re.sub(r'^version = .*$', f'version = "{version}"', man, count=1, flags=re.M)
    man = re.sub(r'^key = .*$', f'key = "ed25519:{PUBLIC}"', man, flags=re.M)
    man = re.sub(r'^url = .*\n', '', man, flags=re.M)
    man = man.replace('[update]', f'[update]\nurl = "http://127.0.0.1:{port}/remote.panpak"')
    man = man.encode()
    return data[:moff] + man + KEY.sign(man + root) + struct.pack(
        "<8sIIQQQQ32s", magic, ver, flags, off, ilen, moff, len(man), root)


def main():
    with tempfile.TemporaryDirectory(prefix="panpak-incremental-") as tmp:
        tmp = Path(tmp)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / "data"))
        env['PANPAK_RANGE_LOG'] = str(tmp / 'ranges.jsonl')
        sock = socket.socket()
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
        sock.close()
        if len(sys.argv) == 3:
            old, new = [Path(p).read_bytes() for p in sys.argv[1:]]
        else:
            tree = tmp / 'tree'
            tree.mkdir()
            (tree / 'hello').write_text('#!/bin/sh\nexit 0\n')
            (tree / 'hello').chmod(0o755)
            (tree / 'panpak.toml').write_text(
                '[app]\nid="org.example.incremental"\nname="Test"\nversion="1"\nentry="hello"\n'
                f'[update]\nkey="ed25519:{PUBLIC}"\n')
            # Distinct files keep unchanged chunks between two separated changes.
            for i in range(10):
                (tree / f'payload{i}').write_bytes(bytes([i]) * 180000)
            (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
            (tmp / 'secret.key').chmod(0o600)
            command = [BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub',
                       '--secret-file', tmp / 'secret.key', '-o', tmp / 'packed.panpak']
            run(command, env)
            old = (tmp / 'packed.panpak').read_bytes()
            for i in (2, 8):
                (tree / f'payload{i}').write_bytes(bytes([50 + i]) * 180000)
            run(command, env)
            new = (tmp / 'packed.panpak').read_bytes()
        old = resign(old, '1.0', port)
        new = resign(new, '1.1', port)
        (tmp / 'old.panpak').write_bytes(old)
        (tmp / 'remote.panpak').write_bytes(new)
        ft, chunks = index(new)
        _, old_chunks = index(old)
        available = {(h, comp, raw) for h, _, comp, raw in old_chunks}
        missing = [(off, off + comp) for h, off, comp, raw in chunks if (h, comp, raw) not in available]
        assert missing, 'test needs changed chunks'
        stub = min([c[1] for c in chunks], default=ft[3])
        old_stub = min([c[1] for c in old_chunks], default=index(old)[0][3])
        stub_reuse = old[:old_stub] == new[:stub]
        analytical = 80 + ft[4] + ft[6] + 64 + sum(end - start for start, end in missing)
        if not stub_reuse:
            analytical += stub
        store = tmp / 'data/panpak'
        current = store / 'apps/org.example.incremental/current'
        run([BIN / 'panpak', 'install', '--yes', '--runtime', BIN / 'panpak-stub', tmp / 'old.panpak'], env)
        before = current.readlink()
        for mode in ['norange', 'badrange', 'loop', 'drip', 'huge',
                     f'corrupt={ft[5] + ft[6]}', f'corrupt={ft[3]}',
                     f'corrupt={missing[0][0]}', f'truncate={missing[0][0]}', 'normal']:
            server = subprocess.Popen([sys.executable, ROOT / 'tests/range_server.py', tmp, str(port), mode],
                                      env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                time.sleep(0.3)
                log = tmp / 'ranges.jsonl'
                log.unlink(missing_ok=True)
                if mode == 'normal':
                    # Signature/root remain valid but a reused local chunk is bad.
                    reused = next(c for c in old_chunks if any(c[0] == n[0] for n in chunks))
                    local = current.resolve()
                    damaged = bytearray(local.read_bytes())
                    damaged[reused[1]] ^= 255
                    local.write_bytes(damaged)
                    run([BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '2',
                         'org.example.incremental'], env, ok=False)
                    assert current.readlink() == before
                    local.write_bytes(old)
                    time.sleep(0.1)
                    log.unlink(missing_ok=True)
                result = run([BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '2',
                              'org.example.incremental'], env, ok=mode == 'normal')
                if mode != 'normal':
                    assert current.readlink() == before, mode + ': current changed on failure'
                else:
                    rows = [json.loads(l) for l in log.read_text().splitlines()]
                    actual = sum(r['bytes'] for r in rows)
                    assert actual == analytical, (actual, analytical)
                    assert current.resolve().read_bytes() == new
                    print(json.dumps({'analytical_bytes': analytical, 'downloaded_bytes': actual,
                                      'full_package_bytes': len(new), 'requests': len(rows),
                                      'missing_chunks': len(missing), 'total_chunks': len(chunks),
                                      'stub_reused': stub_reuse}, indent=2))
                print(mode, 'PASS')
            finally:
                server.terminate()
                server.wait(timeout=5)


if __name__ == '__main__':
    main()
