#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The Wayland security context against a real compositor: Sway on the headless backend of wlroots.

Usage: python3 tests/wayland_sway.py [work-directory]
Needs `sway` (1.9 or newer) and the isolated view (bubblewrap); prints SKIP and exits 0 when either is missing.
Build first: `cargo build --release --target x86_64-unknown-linux-musl`. Everything runs in an isolated HOME and a
runtime folder of its own, with a Sway of its own.

1. The list of globals that the compositor offers on its normal socket is read: it has `zwlr_screencopy_manager_v1`
   (screen capture), a protocol that Sway refuses to a sandboxed client.
2. An app is started through the real launcher with `WAYLAND_DISPLAY` set to that Sway. It lists the globals on the
   socket it was given: the ordinary ones (`wl_compositor`, `xdg_wm_base`) are there and the privileged ones are not.
3. While the app runs, the compositor sees a context for it (`panpak.<id>`): Sway hides the privileged globals only from
   connections that have one, so the difference of 2 is the proof.
4. A second socket in the runtime folder (`panpak/<id>/wayland-*`) exists while the app runs and is gone after.
"""
import json
import os
import shutil
import signal
import struct
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REL = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402

ID = 'org.example.waysway'
CLIENT = r'''
import os, socket, struct, sys

def globals_of(path):
    s = socket.socket(socket.AF_UNIX); s.settimeout(3); s.connect(path)
    # wl_display.get_registry(2), wl_display.sync(3)
    s.sendall(struct.pack('=IIII', 1, (12 << 16) | 1, 2, 0)[:12] + struct.pack('=III', 1, (12 << 16) | 0, 3))
    buf = b''; out = []
    while True:
        buf += s.recv(65536)
        while len(buf) >= 8:
            obj, so = struct.unpack('=II', buf[:8]); size, op = so >> 16, so & 0xffff
            if len(buf) < size: break
            body = buf[8:size]; buf = buf[size:]
            if obj == 2 and op == 0:
                name, n = struct.unpack('=II', body[:8]); iface = body[8:8 + n - 1].decode()
                out.append(iface)
            if obj == 3 and op == 0:
                return out

if __name__ == '__main__':
    rt = os.environ['XDG_RUNTIME_DIR']
    path = os.path.join(rt, os.environ['WAYLAND_DISPLAY'])
    g = globals_of(path)
    open(os.environ['XDG_DATA_HOME'] + '/globals.json', 'w').write(__import__('json').dumps(g))
    if '--hold' in sys.argv:
        __import__('time').sleep(6)
'''
APP = '#!/usr/bin/python3\n' + CLIENT


def need(cond, msg):
    if not cond:
        print('FAIL:', msg)
        sys.exit(1)


def main():
    sway = shutil.which('sway')
    if not sway:
        print('SKIP: sway is not installed')
        return
    if subprocess.run(['/usr/bin/bwrap', '--unshare-all', '--ro-bind', '/usr', '/usr', '--symlink', 'usr/bin', '/bin', '--symlink', 'usr/lib', '/lib', '--symlink', 'usr/lib64', '/lib64', '/bin/true'], capture_output=True).returncode:
        print('SKIP: bubblewrap cannot make the view here')
        return
    for b in (REL / 'panpak', REL / 'panpak-stub'):
        need(b.exists(), f'{b} is missing: build first (see the top of this file)')
    work = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / 'target/wayland-sway'
    isolation.safe_rmtree(work, work.parent)
    home = work / 'home'
    isolation.require_isolated_home(home, work)
    home.mkdir(parents=True)
    runtime = Path(isolation.owned_mkdtemp('waysway-rt-'))
    env = {'PATH': os.environ['PATH'], 'HOME': str(home), 'XDG_RUNTIME_DIR': str(runtime), 'LANG': 'C.UTF-8'}

    def run(binary, *args, **kw):
        return subprocess.run([str(binary), *map(str, args)], env={**env, **kw.pop('extra', {})}, capture_output=True, text=True, timeout=120)

    conf = work / 'sway.conf'
    conf.write_text('output HEADLESS-1 resolution 800x600\n')
    comp = subprocess.Popen([sway, '-c', str(conf)], env={**env, 'WLR_BACKENDS': 'headless', 'WLR_LIBINPUT_NO_DEVICES': '1', 'WLR_RENDERER': 'pixman'},
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, start_new_session=True)
    try:
        sock = None
        for _ in range(100):
            found = sorted(runtime.glob('wayland-*'))
            found = [f for f in found if not f.name.endswith('.lock')]
            if found:
                sock = found[0]
                break
            if comp.poll() is not None:
                break
            time.sleep(0.1)
        if sock is None:
            print('SKIP: sway did not start here:', (comp.stdout.read() if comp.poll() is not None else '')[-300:])
            return
        time.sleep(0.5)
        sys.path.insert(0, str(work))
        (work / 'wlclient.py').write_text(CLIENT)
        import importlib
        wl = importlib.import_module('wlclient')
        normal = wl.globals_of(str(sock))
        need('zwlr_screencopy_manager_v1' in normal, f'the headless Sway does not offer screencopy to a normal client: {normal}')
        need('wp_security_context_manager_v1' in normal, 'this Sway has no security context protocol (needs 1.9 or newer)')

        key = work / 'secret.key'
        out = run(REL / 'panpak', 'keygen', '-o', key).stdout
        pub = next(line.split()[-1] for line in out.splitlines() if line.startswith('key:'))
        tree = work / 'tree'
        (tree / 'bin').mkdir(parents=True)
        (tree / 'bin/app').write_text(APP)
        (tree / 'bin/app').chmod(0o755)
        (tree / 'panpak.toml').write_text(f'[app]\nid = "{ID}"\nname = "Waysway"\nversion = "1"\nentry = "bin/app"\n[update]\nkey = "{pub}"\n')
        pkg = work / 'waysway.panpak'
        p = run(REL / 'panpak', 'pack', tree, '--stub', REL / 'panpak-stub', '--secret-file', key, '-o', pkg)
        need(p.returncode == 0, p.stderr)
        p = run(REL / 'panpak', 'install', '--yes', '--runtime', REL / 'panpak-stub', pkg)
        need(p.returncode == 0, p.stdout + p.stderr)
        launcher = home / '.local/bin' / ID
        result = home / f'.local/share/panpak/userdata/{ID}/data/globals.json'
        wenv = {'WAYLAND_DISPLAY': sock.name}

        held = subprocess.Popen([str(launcher), '--hold'], env={**env, **wenv}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        for _ in range(200):
            if result.exists() and result.stat().st_size:
                break
            time.sleep(0.05)
        need(result.exists(), 'the app did not start or did not reach the compositor: ' + (held.poll() is not None and held.communicate()[1] or ''))
        inside = json.loads(result.read_text())
        extra_sock = sorted((runtime / 'panpak' / ID).glob('wayland-*'))
        need(len(extra_sock) == 1, f'expected one socket of the context while the app runs: {extra_sock}')
        need('wl_compositor' in inside and 'xdg_wm_base' in inside, f'the ordinary globals are missing: {inside}')
        hidden = sorted(set(normal) - set(inside))
        need('zwlr_screencopy_manager_v1' not in inside, 'the app can capture the screen: the context was not applied')
        print('normal client sees', len(normal), 'globals; the app, through the security context, sees', len(inside))
        print('hidden from the app:', ', '.join(hidden))
        held.terminate()
        held.communicate(timeout=30)
        time.sleep(0.3)
        need(not list((runtime / 'panpak' / ID).glob('wayland-*')), 'the socket of the context is still there after the app ended')
        print('the socket of the context is removed when the app ends: ok')
        run(REL / 'panpak', 'remove', ID)
        print('all wayland (sway) tests passed')
    finally:
        try:
            os.killpg(comp.pid, signal.SIGTERM)
        except OSError:
            pass
        try:
            comp.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(comp.pid, signal.SIGKILL)
        isolation.remove_owned(runtime, work)
        isolation.safe_rmtree(work, work.parent)


if __name__ == '__main__':
    main()
