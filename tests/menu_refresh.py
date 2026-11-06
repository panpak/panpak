#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Does GNOME Shell notice a changed menu entry or icon without a new login? (docs/DESKTOP-INTEGRATION.md, section 4)

Runs ON a machine with a GNOME session, as the logged-in user, with a Shell extension that logs what the Shell knows of an
entry: `~/panpak-probe.log` gets one JSON line when the state of an id in `~/panpak-probe.watch` changes (found, actions,
icon, running). The extension is a test tool (see the report); nothing here needs unsafe mode.

The extension is in tests/menu_probe_extension/ (copy it to ~/.local/share/gnome-shell/extensions/panpak-probe@local, add the uuid to
enabled-extensions, log in again; it does not work while the screen is locked).

Usage: python3 menu_refresh.py <dir with panpak and panpak-stub> <dir with the PNG fixtures>
It installs a test app into the real HOME of that user (the Shell only looks there), and removes it at the end.
"""
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

BIN, FIX = Path(sys.argv[1]), Path(sys.argv[2])
HOME = Path.home()
ID = 'org.example.menu'
DESK = f'panpak.{ID}.desktop'
LOG = HOME / 'panpak-probe.log'
APPS = HOME / '.local/share/applications'
ENV = dict(os.environ, XDG_RUNTIME_DIR=f'/run/user/{os.getuid()}', DBUS_SESSION_BUS_ADDRESS=f'unix:path=/run/user/{os.getuid()}/bus')
WAIT = 8.0


def run(*a, **k):
    return subprocess.run([str(x) for x in a], env=ENV, capture_output=True, text=True, **k)


def state():
    last = None
    for l in LOG.read_text().splitlines():
        o = json.loads(l)
        if o.get('id') == f'panpak.{ID}.desktop':
            last = o
    return last


def wait_for(pred, what):
    t0 = time.time()
    while time.time() - t0 < WAIT:
        s = state()
        if s and pred(s):
            return round(time.time() - t0, 1), s
        time.sleep(0.25)
    return None, state()


def set_favorites(on):
    cur = eval(run('gsettings', 'get', 'org.gnome.shell', 'favorite-apps').stdout)
    cur = [c for c in cur if c != DESK] + ([DESK] if on else [])
    run('gsettings', 'set', 'org.gnome.shell', 'favorite-apps', str(cur))


def old_entry():
    """The entry of the first version: no actions, no icon, no version."""
    return f'[Desktop Entry]\nType=Application\nName=Menu Test\nExec="{HOME}/.local/bin/{ID}" %U\nTerminal=false\nCategories=Utility;\nX-Panpak-App-Id={ID}\n'


def put(text):
    """Replace the entry the way panpak does: a hidden temporary file in the folder, then a rename."""
    tmp = APPS / f'.{DESK}.test-tmp'
    tmp.write_text(text)
    os.replace(tmp, APPS / DESK)


def pack(version, icons):
    tree = HOME / 'panpak-menu/tree'
    shutil.rmtree(tree, ignore_errors=True)
    (tree / 'bin').mkdir(parents=True)
    (tree / 'bin/app').write_text('#!/bin/sh\nexit 0\n')
    (tree / 'bin/app').chmod(0o755)
    toml = f'[app]\nid = "{ID}"\nname = "Menu Test"\nversion = "{version}"\nentry = "bin/app"\n'
    if icons:
        toml += '[desktop]\nicons = ["i16.png", "i48.png"]\n'
        shutil.copy(FIX / 'ok-16-rgba.png', tree / 'i16.png')
        shutil.copy(FIX / 'ok-48-rgba.png', tree / 'i48.png')
    key = HOME / 'panpak-menu/secret.key'
    if not key.exists():
        out = run(BIN / 'panpak', 'keygen', '-o', key).stdout
        key.with_suffix('.pub').write_text(next(l.split()[-1] for l in out.splitlines() if l.startswith('key:')))
    (tree / 'panpak.toml').write_text(toml + f'[update]\nkey = "{key.with_suffix(".pub").read_text()}"\n')
    pkg = HOME / f'panpak-menu/{version}.panpak'
    p = run(BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', key, '-o', pkg)
    assert p.returncode == 0, p.stderr
    return pkg


def cleanup():
    set_favorites(False)
    run(BIN / 'panpak', 'remove', ID)
    (APPS / DESK).unlink(missing_ok=True)


results = []


def report(case, step, got):
    secs, s = got
    ok = secs is not None
    results.append((case, step, ok))
    shown = {k: s.get(k) for k in ('found', 'actions', 'icon', 'running')} if s else None
    print(f'  {case:9} {step:44} {"seen after " + str(secs) + " s" if ok else "NOT SEEN in " + str(WAIT) + " s"}  {shown}')


def main():
    cleanup()
    run(BIN / 'panpak', 'install-runtime', BIN / 'panpak-stub', '--allow-unsigned')
    p = run(BIN / 'panpak', 'install', '--yes', pack('1', False))
    assert p.returncode == 0, p.stdout + p.stderr
    for case in ('neither', 'favourite'):
        cleanup()
        run(BIN / 'panpak', 'install', '--yes', pack('1', False))
        set_favorites(case == 'favourite')
        # 1. An entry of the first version, as the Shell finds it.
        put(old_entry())
        report(case, '1 old entry (no actions)', wait_for(lambda s: s['found'] and s['actions'] == [], ''))
        # 2. The app starts: the stub rewrites the entry and launcher (a rename in the same folder).
        r = run(HOME / '.local/bin' / ID)
        assert r.returncode == 0, r.stderr
        text = (APPS / DESK).read_text()
        assert 'X-Panpak-Desktop-Version=' in text and 'Actions=Uninstall;' in text, text
        got = wait_for(lambda s: s['actions'] == ['Uninstall'], '')
        report(case, '2 lazy rewrite by the stub: action appears', got)
        if got[0] is None:
            os.utime(APPS)
            report(case, '2b ... then touch the applications folder', wait_for(lambda s: s['actions'] == ['Uninstall'], ''))
            run('update-desktop-database', APPS)
            report(case, '2c ... then update-desktop-database', wait_for(lambda s: s['actions'] == ['Uninstall'], ''))
        # 3. A new version with icons: install writes the icon files, then the entry.
        run(BIN / 'panpak', 'install', '--yes', pack('2', True))
        report(case, '3 install with icons: Icon= changes', wait_for(lambda s: s['icon'] and 'panpak.' + ID in s['icon'], ''))
    cleanup()
    print('summary:', 'all seen' if all(r[2] for r in results) else 'some changes were NOT seen: ' + '; '.join(f'{c}/{s}' for c, s, ok in results if not ok))


if __name__ == '__main__':
    main()
