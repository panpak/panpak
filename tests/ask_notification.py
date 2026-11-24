#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Questions without `zenity` or `kdialog` (PANPAK-FORMAT.md 6.10): the question of kept versions is a notification of the portal with two buttons.

Usage: python3 tests/ask_notification.py   (debug musl binaries built first; an isolated HOME; a private bus; no real window or notification)
The portal is a fake (PyGObject owns `org.freedesktop.portal.Desktop` on a private bus, records `AddNotification` and `RemoveNotification`, and sends
`ActionInvoked` when told). `PANPAK_TEST_TOOLS_DIR` is an empty folder, so no window tool exists.

  1. two failed starts of a new version: the notification is shown with the two buttons and the question is pending; the button "Go back" goes back and the
     pending file is gone;
  2. nobody presses: after the wait the notification is withdrawn, the question stays pending and nothing changes; the next start asks again, and "Try again"
     (the other button) ends the question;
  3. no bus and no tool: nothing is asked, the question stays pending, and the next start says so on its standard error.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402
from incremental import PUBLIC  # noqa: E402

BIN = ROOT / 'target/x86_64-unknown-linux-musl/debug'
ID = 'org.example.askn'

FAKE_PORTAL = r'''
import gi, json, os, sys
gi.require_version('Gio', '2.0')
from gi.repository import Gio, GLib
log, click = sys.argv[1], sys.argv[2]
XML = """<node><interface name="org.freedesktop.portal.Notification">
<method name="AddNotification"><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/></method>
<method name="RemoveNotification"><arg type="s" direction="in"/></method>
<signal name="ActionInvoked"><arg type="s"/><arg type="s"/><arg type="av"/></signal></interface></node>"""
conn_box, last = [], []
def note(d):
    with open(log, 'a') as f:
        f.write(json.dumps(d) + '\n')
def on_call(conn, sender, path, iface, method, params, inv):
    p = params.unpack()
    if method == 'AddNotification':
        last.append(p[0])
        note({'add': p[0], 'title': p[1].get('title'), 'body': p[1].get('body'), 'buttons': p[1].get('buttons'), 'default_action': 'default-action' in p[1]})
    else:
        note({'remove': p[0]})
    inv.return_value(None)
def poll():
    if os.path.exists(click) and conn_box and last:
        action = open(click).read().strip()
        os.unlink(click)
        conn_box[0].emit_signal(None, '/org/freedesktop/portal/desktop', 'org.freedesktop.portal.Notification', 'ActionInvoked', GLib.Variant('(ssav)', (last[-1], action, [])))
    return True
def acquired(conn, name):
    conn_box.append(conn)
    node = Gio.DBusNodeInfo.new_for_xml(XML)
    conn.register_object('/org/freedesktop/portal/desktop', node.interfaces[0], on_call, None, None)
    open(log + '.ready', 'w').write('1')
Gio.bus_own_name(Gio.BusType.SESSION, 'org.freedesktop.portal.Desktop', Gio.BusNameOwnerFlags.NONE, acquired, None, None)
GLib.timeout_add(100, poll)
GLib.MainLoop().run()
'''


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, f'{[str(x) for x in args][:3]} ended with status {p.returncode}\n{p.stdout}{p.stderr}'
    return p


def wait_for(cond, what, seconds=20):
    end = time.time() + seconds
    while time.time() < end:
        if cond():
            return
        time.sleep(0.1)
    raise AssertionError(f'timeout: {what}')


def records(log):
    return [json.loads(l) for l in log.read_text().splitlines()] if log.exists() else []


def main():
    base = ROOT / 'target' / 'tmp-tests'
    base.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='panpak-askn-', dir=base, ignore_cleanup_errors=True) as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        home = tmp / 'home'
        home.mkdir()
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        tools = tmp / 'notools'
        tools.mkdir()
        bus_addr = f'unix:path={tmp}/bus'
        env = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / '.local/share'), XDG_CACHE_HOME=str(home / '.cache'), XDG_CONFIG_HOME=str(home / '.config'),
                   XDG_RUNTIME_DIR=str(runtime), LC_ALL='C', PANPAK_QUICK_SECS='0.3', PANPAK_NO_BACKGROUND='1', PANPAK_TEST_TOOLS_DIR=str(tools),
                   PANPAK_TEST_ASK_SECS='60', DBUS_SESSION_BUS_ADDRESS='unix:path=' + str(tmp / 'nobus'))
        env.pop('WAYLAND_DISPLAY', None)
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        tree = tmp / 'tree'
        tree.mkdir()
        previous = None

        def release(version, body):
            nonlocal previous
            (tree / 'hello').write_text('#!/bin/sh\n' + body)
            (tree / 'hello').chmod(0o755)
            (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Ask App"\nversion="{version}"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
            out = tmp / f'{version}.panpak'
            cmd = [BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', out]
            if previous:
                cmd += ['--previous', previous]
            run(cmd, env)
            previous = out
            return out

        v1 = release('1.0', 'sleep 0.6\nexit 0\n')
        v2 = release('2.0', 'exit 3\n')
        run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', v1], env)
        store = home / '.local/share/panpak'
        appdir = store / 'apps' / ID
        start = [store / 'bin/panpak-stub', 'run', appdir / 'current']
        for _ in range(3):
            run(start, env, ok=False)
        assert (appdir / 'last_good').is_symlink()
        good = os.readlink(appdir / 'last_good')
        run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--accept-new-permissions', '--runtime', BIN / 'panpak-stub', v2], env)
        bad = os.readlink(appdir / 'current')
        assert bad != good

        # 3 first (no bus), while nothing else is going on: the question stays pending and the next start says so.
        for _ in range(2):
            run(start, env, ok=False)
        wait_for(lambda: (appdir / 'pending-question').exists(), 'the question is pending with no bus and no tool')
        time.sleep(0.5)
        assert os.readlink(appdir / 'current') == bad, 'nothing was answered, nothing changed'
        p = run(start, env, ok=False)
        assert f'panpak rollback {ID}' in p.stderr, p.stderr
        print('3. no bus, no tool: pending, said on stderr: ok')

        # The portal and the bus.
        log, click = tmp / 'portal.log', tmp / 'click'
        bus = subprocess.Popen(['dbus-daemon', f'--config-file={ROOT}/tests/bus-no-activation.conf', '--nofork', f'--address={bus_addr}'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        env['DBUS_SESSION_BUS_ADDRESS'] = bus_addr
        (tmp / 'portal.py').write_text(FAKE_PORTAL)
        portal = subprocess.Popen([sys.executable, tmp / 'portal.py', log, click], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            wait_for(lambda: Path(str(log) + '.ready').exists(), 'the fake portal is up')
            # 2. Nobody presses: the notification is withdrawn after the wait, the question stays pending.
            run(start, dict(env, PANPAK_TEST_ASK_SECS='3'), ok=False)
            wait_for(lambda: any('add' in r for r in records(log)), 'the notification is shown')
            first = records(log)[0]
            assert first['title'] == "Ask App doesn't start" and first['default_action'] is False, first
            assert [b['label'] for b in first['buttons']] == ['Go back to 1.0', 'Try again'] and [b['action'] for b in first['buttons']] == ['yes', 'no'], first
            assert 'failed to start twice' in first['body'], first
            wait_for(lambda: any(r.get('remove') == first['add'] for r in records(log)), 'the notification is withdrawn after the wait')
            assert (appdir / 'pending-question').exists() and os.readlink(appdir / 'current') == bad
            print('2. nobody presses: withdrawn, pending, nothing changed: ok')
            # "Uninstall" is not kept: with no answer nothing is removed and nothing is pending.
            p = run([BIN / 'panpak', 'remove', '--ask', ID], dict(env, PANPAK_TEST_ASK_SECS='1'))
            assert 'no answer: nothing was removed' in p.stdout, p.stdout + p.stderr
            assert appdir.exists(), 'nothing was removed'
            print('2b. uninstall with no answer: nothing removed: ok')
            # The next start asks again; the first button goes back.
            n = len([r for r in records(log) if 'add' in r])
            run(start, env, ok=False)
            wait_for(lambda: len([r for r in records(log) if 'add' in r]) > n, 'the question is shown again at the next start')
            click.write_text('yes')
            wait_for(lambda: os.readlink(appdir / 'current') == good, 'the button "Go back" goes back')
            wait_for(lambda: not (appdir / 'pending-question').exists(), 'the question is no longer pending')
            print('1. button 1: went back, not pending: ok')
            # The second button: a new bad version, the question, "Try again": the app is started again and the question ends.
            run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--accept-new-permissions', '--runtime', BIN / 'panpak-stub', v2], env)
            n = len([r for r in records(log) if 'add' in r])
            for _ in range(2):
                run(start, env, ok=False)
            wait_for(lambda: len([r for r in records(log) if 'add' in r]) > n and (appdir / 'pending-question').exists(), 'the question is shown')
            time.sleep(0.5)
            click.write_text('no')
            try:
                wait_for(lambda: not (appdir / 'pending-question').exists(), 'the second button ends the question')
            except AssertionError:
                print('DEBUG', records(log), sorted(x.name for x in appdir.iterdir()), (appdir / 'pending-question').read_text())
                raise
            assert os.readlink(appdir / 'current') == bad, 'Try again does not go back'
            print('1b. button 2: nothing changed, not pending: ok')
        finally:
            for pr in (portal, bus):
                pr.terminate()
                pr.wait()
        print('questions without a window tool PASS')
        time.sleep(1)
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
