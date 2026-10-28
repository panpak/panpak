#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Warm cache never bypasses signature/stub/pin; inventory invalidates it."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN, PUBLIC, run


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-cache-e2e-') as tmp:
        tmp = Path(tmp)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), PANPAK_ABI_TRACE='1')
        tree = tmp / 'tree'
        tree.mkdir()
        (tree / 'go').write_text('#!/bin/sh\necho verified-app\n')
        (tree / 'go').chmod(0o755)
        (tree / 'panpak.toml').write_text('[app]\nid="org.example.cache"\nname="cache"\nversion="1"\nentry="go"\n'
                                        f'[update]\nkey="ed25519:{PUBLIC}"\n')
        pkg = tmp / 'cache.panpak'
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', pkg], env)
        run([BIN / 'panpak', 'install', '--yes', '--runtime', BIN / 'panpak-stub', pkg], env)
        # Same mapping, changed ldconfig bytes -> miss without touching real host.
        inventory = subprocess.run(['ldconfig', '-p'], capture_output=True, check=True).stdout
        (tmp / 'inventory').write_bytes(inventory)
        tools = tmp / 'tools'
        tools.mkdir()
        (tools / 'ldconfig').write_text(f'#!/bin/sh\nexec /bin/cat "{tmp}/inventory"\n')
        (tools / 'ldconfig').chmod(0o755)
        env['PATH'] = str(tools) + ':' + env['PATH']
        command = [BIN / 'panpak-stub', 'run', pkg]
        assert 'cache miss' in run(command, env).stderr
        assert 'cache hit' in run(command, env).stderr
        with (tmp / 'inventory').open('ab') as f: f.write(b'\n')
        assert 'cache miss' in run(command, env).stderr
        assert 'cache hit' in run(command, env).stderr
        data = pkg.read_bytes()
        damaged = bytearray(data)
        damaged[-81] ^= 1  # signature
        pkg.write_bytes(damaged)
        p = run(command, env, ok=False)
        assert 'cache hit' not in p.stderr and 'verified-app' not in p.stdout
        damaged = bytearray(data)
        damaged[1] ^= 1  # stub
        pkg.write_bytes(damaged)
        p = run(command, env, ok=False)
        assert 'cache hit' not in p.stderr and 'verified-app' not in p.stdout
        pkg.write_bytes(data)
        assert 'cache hit' in run(command, env).stderr
        pin = tmp / 'data/panpak/apps/org.example.cache/pinned-key'
        pin.write_text('different-key')
        p = run(command, env, ok=False)
        assert 'cache hit' not in p.stderr and 'verified-app' not in p.stdout
        print('ABI cache: miss/hit/inventory/signature/stub/pin PASS')


if __name__ == '__main__':
    main()
