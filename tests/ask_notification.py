#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Questions without `zenity` or `kdialog` (PANPAK-FORMAT.md 6.10): the question of kept versions is a notification with two buttons, of the notification
service of the desktop (`org.freedesktop.Notifications`) when there is one and of the portal when there is not.

Usage: python3 tests/ask_notification.py   (debug musl binaries built first; an isolated HOME; a private bus; no real window or notification)
The portal is a fake (PyGObject owns `org.freedesktop.portal.Desktop` on a private bus, records `AddNotification` and `RemoveNotification`, and sends
`ActionInvoked` when told). `PANPAK_TEST_TOOLS_DIR` is an empty folder, so no window tool exists.

  1. two failed starts of a new version: the notification is shown with the two buttons and the question is pending; the button "Go back" goes back and the
     pending file is gone;
  2. nobody presses: after the wait the notification is withdrawn, the question stays pending and nothing changes; the next start asks again, and "Try again"
     (the other button) ends the question;
  3. no bus and no tool: nothing is asked, the question stays pending, and the next start says so on its standard error;
  4. the service of the desktop (a fake that owns `org.freedesktop.Notifications` next to the fake portal): the question goes to the service and not to the portal,
     with the two buttons and a `default` action that is no answer, the hint `resident`; a button of the service answers (as the broadcast that GNOME Shell sends); the same
     signal sent by another client is not an answer; a notification that the person dismisses ends the wait at once and leaves the question pending; with no answer
     the notification is closed after the wait; a click on the body (the key `default`) is no answer and withdraws nothing; a second question shows the same
     notification again (`replaces_id`) and makes no new one; a service that does not list "actions" is not used (the portal is, and its notification is added
     again under the same name); out of a scope of an app (`PANPAK_TEST_CGROUP`) the portal is not asked and the way to answer is told on the standard error.
"""
import json
import os
import re
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402
from incremental import PUBLIC  # noqa: E402

# A process in the scope of an app: the portal can tell which app it is (the portal is asked only then).
APP_SCOPE = '0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-test-1.scope\n'
# A process that a menu entry started in the service of the shell: the portal refuses it in silence.
SHELL_SERVICE = '0::/user.slice/user-1000.slice/user@1000.service/session.slice/org.gnome.Shell@user.service\n'
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
    d['ts'] = round(__import__('time').time(), 3)
    with open(log, 'a') as f:
        f.write(json.dumps(d) + '\n')
def on_call(conn, sender, path, iface, method, params, inv):
    p = params.unpack()
    if method == 'AddNotification':
        last.append(p[0])
        note({'add': p[0], 'title': p[1].get('title'), 'body': p[1].get('body'), 'buttons': p[1].get('buttons'), 'default_action': p[1].get('default-action')})
    else:
        note({'remove': p[0]})
    inv.return_value(None)
def poll():
    if os.path.exists(click) and conn_box and last:
        action = open(click).read().strip()
        if not action:
            return True
        os.unlink(click)
        note({'click': action, 'to': last[-1]})
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


# A fake notification service: it owns `org.freedesktop.Notifications`, records `Notify` and `CloseNotification`, and sends `ActionInvoked` and `NotificationClosed`
# when told, as broadcasts (no destination), as GNOME Shell does.
FAKE_SERVICE = r'''
import gi, json, os, sys, time
gi.require_version('Gio', '2.0')
from gi.repository import Gio, GLib
log, cmd = sys.argv[1], sys.argv[2]
XML = """<node><interface name="org.freedesktop.Notifications">
<method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/><arg type="a{sv}" direction="in"/><arg type="i" direction="in"/><arg type="u" direction="out"/></method>
<method name="CloseNotification"><arg type="u" direction="in"/></method>
<method name="GetCapabilities"><arg type="as" direction="out"/></method>
<signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal>
<signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal></interface></node>"""
conn_box, last = [], []
def note(d):
    d['ts'] = round(time.time(), 3)
    with open(log, 'a') as f:
        f.write(json.dumps(d) + '\n')
def on_call(conn, sender, path, iface, method, params, inv):
    p = params.unpack()
    if method == 'Notify':
        n = p[1] if p[1] else len(last) + 100
        last.append(n)
        note({'notify': n, 'app': p[0], 'replaces': p[1], 'title': p[3], 'body': p[4], 'actions': list(p[5]), 'hints': dict(p[6]), 'expire': p[7]})
        inv.return_value(GLib.Variant('(u)', (n,)))
    elif method == 'CloseNotification':
        note({'close': p[0]})
        inv.return_value(None)
    else:
        caps = open(log + '.caps').read().split() if os.path.exists(log + '.caps') else ['actions', 'body']
        inv.return_value(GLib.Variant('(as)', (caps,)))
def poll():
    if os.path.exists(cmd) and conn_box and last:
        text = open(cmd).read().strip()
        if not text:
            return True
        os.unlink(cmd)
        what, _, arg = text.partition(' ')
        note({'send': text, 'to': last[-1]})
        if what == 'action':
            conn_box[0].emit_signal(None, '/org/freedesktop/Notifications', 'org.freedesktop.Notifications', 'ActionInvoked', GLib.Variant('(us)', (last[-1], arg)))
        elif what == 'closed':
            conn_box[0].emit_signal(None, '/org/freedesktop/Notifications', 'org.freedesktop.Notifications', 'NotificationClosed', GLib.Variant('(uu)', (last[-1], int(arg))))
    return True
def acquired(conn, name):
    conn_box.append(conn)
    node = Gio.DBusNodeInfo.new_for_xml(XML)
    conn.register_object('/org/freedesktop/Notifications', node.interfaces[0], on_call, None, None)
    open(log + '.ready', 'w').write('1')
Gio.bus_own_name(Gio.BusType.SESSION, 'org.freedesktop.Notifications', Gio.BusNameOwnerFlags.NONE, acquired, None, None)
GLib.timeout_add(100, poll)
GLib.MainLoop().run()
'''

# Another client of the bus sends the signal of the service with the number of the notification: its sender is its own name, so it is not an answer.
SPOOFER = r'''
import gi, sys
gi.require_version('Gio', '2.0')
from gi.repository import Gio, GLib
number, key = int(sys.argv[1]), sys.argv[2]
conn = Gio.bus_get_sync(Gio.BusType.SESSION, None)
conn.emit_signal(None, '/org/freedesktop/Notifications', 'org.freedesktop.Notifications', 'ActionInvoked', GLib.Variant('(us)', (number, key)))
conn.flush_sync(None)
'''

# What every command printed, shown when a step fails (a flaw of this test showed in the CI and not locally: see the pull request that fixed it).
RUNLOG = []


def run(args, env, ok=True, timeout=60):
    t0 = time.time()
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    RUNLOG.append((t0, time.time(), [Path(str(x)).name if i == 0 else str(x)[-60:] for i, x in enumerate(args)][:6], p.returncode, p.stdout[-600:], p.stderr[-600:]))
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



def poll(cond, what, seconds=5.0, step=0.05):
    """Check `cond` every `step` seconds until it holds, at most `seconds`; fail with a clear message when the time is up."""
    end = time.time() + seconds
    while True:
        if cond():
            return
        if time.time() >= end:
            raise AssertionError(f'not settled after {seconds} s: {what}')
        time.sleep(step)


def askers():
    """How many processes `panpak rollback --ask <id>` there are: one holds the lock of the pending question and waits; the others, started by the same start, try the lock and end."""
    n = 0
    for d in Path('/proc').iterdir():
        if d.name.isdigit():
            try:
                argv = (d / 'cmdline').read_bytes().split(b'\0')
            except OSError:
                continue
            if b'rollback' in argv and b'--ask' in argv and ID.encode() in argv:
                n += 1
    return n


def settled_askers():
    """Wait until the processes that ask have tried the lock: only the one that holds it is left. An answer before that would be taken by a process that has not tried the lock yet,
    and it would ask the question again."""
    poll(lambda: askers() <= 1, 'more than one process "panpak rollback --ask" is still there')

def put_click(click, action):
    # Written whole and then renamed, so the fake portal never reads a half-written file.
    tmp = click.with_name(click.name + '.new')
    tmp.write_text(action)
    os.replace(tmp, click)


def failed_starts(appdir):
    """The failed starts in a row that the file `starts` of the app holds."""
    m = re.search(r'failed=(\d+)', (appdir / 'starts').read_text())
    return int(m.group(1)) if m else None


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
                   PANPAK_TEST_ASK_SECS='60', PANPAK_TEST_CGROUP=APP_SCOPE, DBUS_SESSION_BUS_ADDRESS='unix:path=' + str(tmp / 'nobus'))
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
        procs = [portal, bus]
        slog = tmp / 'service.log'
        try:
            wait_for(lambda: Path(str(log) + '.ready').exists(), 'the fake portal is up')
            # 2. Nobody presses: the notification is withdrawn after the wait, the question stays pending.
            run(start, dict(env, PANPAK_TEST_ASK_SECS='3'), ok=False)
            wait_for(lambda: any('add' in r for r in records(log)), 'the notification is shown')
            first = records(log)[0]
            assert first['title'] == "Ask App doesn't start" and first['default_action'] == 'ignore', first
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
            put_click(click, 'yes')
            wait_for(lambda: os.readlink(appdir / 'current') == good, 'the button "Go back" goes back')
            wait_for(lambda: not (appdir / 'pending-question').exists(), 'the question is no longer pending')
            # A rollback forgets the failed starts: a start of the other version that is counted late does not count (its version is not the installed one any more).
            time.sleep(0.5)
            assert failed_starts(appdir) == 0, f'a rollback forgets the failed starts: {failed_starts(appdir)}'
            print('1. button 1: went back, not pending, the failed starts forgotten: ok')
            # The bad package is installed again (the same root as before): one failed start is counted, and one failure does not ask.
            install_v2 = [BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--accept-new-permissions', '--runtime', BIN / 'panpak-stub', v2]
            run(install_v2, env)
            assert failed_starts(appdir) == 0, f'an install forgets the failed starts: {failed_starts(appdir)}'
            n = len([r for r in records(log) if 'add' in r])
            run(start, env, ok=False)
            time.sleep(1.0)
            assert failed_starts(appdir) == 1, f'one failed start is one: {failed_starts(appdir)}'
            assert len([r for r in records(log) if 'add' in r]) == n and not (appdir / 'pending-question').exists(), 'the question is asked after one failure'
            # An install of the same package forgets that failure (the same root: the counter would stay otherwise).
            run(install_v2, env)
            assert failed_starts(appdir) == 0, f'an install of the same package forgets the failed starts: {failed_starts(appdir)}'
            print('1a. an install forgets the failed starts; one failure does not ask: ok')
            # Two failures in a row ask, once.
            for _ in range(2):
                run(start, env, ok=False)
            wait_for(lambda: len([r for r in records(log) if 'add' in r]) > n and (appdir / 'pending-question').exists(), 'the question is shown after two failures')
            assert len([r for r in records(log) if 'add' in r]) == n + 1, 'the question is asked once'
            # The second button. While the question waits one more start fails (one failure is counted); "Try again" forgets it, so the start that follows is the first failure and
            # does not ask. Without that, the two would make a pair and the question would come back at once.
            run(start, env, ok=False)
            assert failed_starts(appdir) == 1, f'a start while the question waits is counted: {failed_starts(appdir)}'
            settled_askers()
            put_click(click, 'no')
            wait_for(lambda: not (appdir / 'pending-question').exists(), 'the second button ends the question')
            assert os.readlink(appdir / 'current') == bad, 'Try again does not go back'
            n2 = len([r for r in records(log) if 'add' in r])
            time.sleep(1.0)
            assert len([r for r in records(log) if 'add' in r]) == n2, 'a new notification appeared after Try again'
            assert not (appdir / 'pending-question').exists(), 'the question came back after Try again'
            assert failed_starts(appdir) == 1, f'after Try again the start that follows is the first failure: {failed_starts(appdir)}'
            print('1b. button 2: nothing changed, not pending, the failed starts forgotten, no new question for a second: ok')
            # One more failed start (a second in a row) asks again; the answer "Go back" ends it.
            run(start, env, ok=False)
            wait_for(lambda: len([r for r in records(log) if 'add' in r]) > n2 and (appdir / 'pending-question').exists(), 'the question comes back after two failures in a row')
            time.sleep(0.5)
            put_click(click, 'yes')
            wait_for(lambda: os.readlink(appdir / 'current') == good and not (appdir / 'pending-question').exists(), 'the last question is answered')
            print('1c. two failures in a row ask again: ok')
            # 4. The service of the desktop. It is started while the fake portal is still there: the question must go to the service and not to the portal.
            slog, scmd = tmp / 'service.log', tmp / 'service-cmd'
            (tmp / 'service.py').write_text(FAKE_SERVICE)
            (tmp / 'spoof.py').write_text(SPOOFER)
            service = subprocess.Popen([sys.executable, tmp / 'service.py', slog, scmd], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            procs.append(service)
            wait_for(lambda: Path(str(slog) + '.ready').exists(), 'the fake notification service is up')

            def notifies():
                # the new notifications; a notification shown again (`replaces` is its number) is in `replays()`
                return [r for r in records(slog) if 'notify' in r and not r['replaces']]

            def replays():
                return [r for r in records(slog) if 'notify' in r and r['replaces']]

            def send(text):
                put_click(scmd, text)
                wait_for(lambda: not scmd.exists(), 'the fake service took the command')

            def ask_again():
                # The bad version again (the same root: the counter is forgotten), then two failures in a row ask.
                run(install_v2, env)
                n = len(notifies())
                for _ in range(2):
                    run(start, env, ok=False)
                wait_for(lambda: len(notifies()) > n and (appdir / 'pending-question').exists(), 'the question is shown by the service')
                return notifies()[-1]

            portal_adds = len([r for r in records(log) if 'add' in r])
            first = ask_again()
            assert first['app'] == 'Panpak', first
            assert first['actions'] == ['default', '', 'yes', 'Go back to 1.0', 'no', 'Try again'], first
            assert first['hints'] == {'resident': True} and first['expire'] == 0, first
            assert first['title'] == "Ask App doesn't start" and 'failed to start twice' in first['body'], first
            assert len([r for r in records(log) if 'add' in r]) == portal_adds, 'the portal was asked although the service is there'
            print('4a. the question goes to the service, the two buttons and a default that is no answer, resident, the portal not asked: ok')
            # The same signal from another client is not an answer; the answer of the service is.
            subprocess.run([sys.executable, tmp / 'spoof.py', str(first['notify']), 'yes'], env=env, check=True, timeout=30)
            time.sleep(1.0)
            assert os.readlink(appdir / 'current') == bad and (appdir / 'pending-question').exists(), 'a signal of another client answered the question'
            # A click on the body (the key `default`) is no answer and the notification stays: nothing is closed, nothing changes.
            settled_askers()
            send('action default')
            time.sleep(1.0)
            assert os.readlink(appdir / 'current') == bad and (appdir / 'pending-question').exists(), 'a click on the body answered the question'
            assert not any(r.get('close') == first['notify'] for r in records(slog)), 'a click on the body withdrew the notification'
            send('action yes')
            wait_for(lambda: os.readlink(appdir / 'current') == good and not (appdir / 'pending-question').exists(), 'the button of the service goes back')
            wait_for(lambda: any(r.get('close') == first['notify'] for r in records(slog)), 'the notification is closed after the answer')
            print("4b. another client's signal ignored; a click on the body is no answer and withdraws nothing; the button of the service goes back; the notification is closed: ok")
            # The person dismisses the notification: the wait ends at once (the test waits up to 60 s), the question stays pending, nothing changes.
            second = ask_again()
            send('closed 2')
            wait_for(lambda: any(r.get('close') == second['notify'] for r in records(slog)), 'the process stops waiting when the notification is dismissed', seconds=10)
            assert (appdir / 'pending-question').exists() and os.readlink(appdir / 'current') == bad, 'a dismissal answered the question'
            print('4c. dismissed: the wait ends at once, pending, nothing changed: ok')
            # Nobody presses: after the wait the notification is closed, the question stays pending.
            n = len(notifies())
            run(start, dict(env, PANPAK_TEST_ASK_SECS='3'), ok=False)
            wait_for(lambda: len(notifies()) > n, 'the question is asked again at the next start')
            third = notifies()[-1]
            wait_for(lambda: any(r.get('close') == third['notify'] for r in records(slog)), 'the notification is closed after the wait', seconds=15)
            assert (appdir / 'pending-question').exists() and os.readlink(appdir / 'current') == bad
            print('4d. nobody presses: closed after the wait, pending: ok')
            # The second button of the service ends the question and changes nothing.
            n = len(notifies())
            run(start, env, ok=False)
            wait_for(lambda: len(notifies()) > n, 'the question is asked again')
            settled_askers()
            send('action no')
            wait_for(lambda: not (appdir / 'pending-question').exists(), 'the second button of the service ends the question')
            assert os.readlink(appdir / 'current') == bad
            print('4e. the second button of the service: nothing changed, not pending: ok')
            # Another process asks the same question while the first waits: it shows the notification of the first again (the same number, `replaces`), a second
            # one is not made, and the answer still goes to the first. Within 5 s of the first notification it does not, because the same failed start makes two of them.
            third_q = ask_again()
            settled_askers()
            r0 = len(replays())
            subprocess.run([BIN / 'panpak', 'rollback', '--ask', ID], env=env, check=True, timeout=30, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            time.sleep(0.5)
            assert len(replays()) == r0 or time.time() - third_q['ts'] >= 5, 'shown again too soon'
            time.sleep(max(0.0, 5.5 - (time.time() - third_q['ts'])))
            n_new = len(notifies())
            subprocess.run([BIN / 'panpak', 'rollback', '--ask', ID], env=env, check=True, timeout=30, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait_for(lambda: len(replays()) > r0, 'the notification is shown again by the second process')
            again = replays()[-1]
            assert again['replaces'] == third_q['notify'] and again['actions'] == third_q['actions'] and again['title'] == third_q['title'], (again, third_q)
            assert len(notifies()) == n_new, 'a second notification was made'
            assert (appdir / 'pending-question').exists() and os.readlink(appdir / 'current') == bad
            put_pending = (appdir / 'pending-question').read_text()
            assert 'via=service' in put_pending and f"id={third_q['notify']}" in put_pending, put_pending
            send('action yes')
            wait_for(lambda: os.readlink(appdir / 'current') == good and not (appdir / 'pending-question').exists(), 'the first process gets the answer to the notification that was shown again')
            print('4h. a second question shows the same notification again (the same number), makes no new one, and the answer goes to the first: ok')
            # A service that does not list "actions" shows no buttons: the question goes to the portal and the service is not asked.
            caps = Path(str(slog) + '.caps')
            caps.write_text('body persistence')
            n, portal_adds = len(notifies()), len([r for r in records(log) if 'add' in r])
            run(install_v2, env)
            for _ in range(2):
                run(start, env, ok=False)
            wait_for(lambda: len([r for r in records(log) if 'add' in r]) > portal_adds and (appdir / 'pending-question').exists(), 'the question goes to the portal')
            assert len(notifies()) == n, 'the service without "actions" was asked'
            settled_askers()
            # The same question again after 5 s: the notification of the portal is added again with the same name (it replaces the first), and the answer goes to the first.
            adds = [r for r in records(log) if 'add' in r]
            time.sleep(5.5)
            subprocess.run([BIN / 'panpak', 'rollback', '--ask', ID], env=env, check=True, timeout=30, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait_for(lambda: len([r for r in records(log) if 'add' in r]) > len(adds), 'the notification of the portal is shown again')
            assert [r for r in records(log) if 'add' in r][-1]['add'] == adds[-1]['add'], 'the portal notification was shown again under another name'
            assert 'via=portal' in (appdir / 'pending-question').read_text()
            put_click(click, 'yes')
            wait_for(lambda: os.readlink(appdir / 'current') == good and not (appdir / 'pending-question').exists(), 'the portal button goes back')
            assert all(r['default_action'] == 'ignore' for r in records(log) if 'add' in r), 'a portal notification without a default action that is no answer'
            print('4f. a service without "actions" is not used: the portal is (shown again under the same name, a default action that is no answer): ok')
            # The portal is not asked from a process that is in no scope of an app (a menu entry in the service of the shell): it would refuse in silence. Nothing is
            # asked, the question stays pending and the next start says how to answer.
            env_shell = dict(env, PANPAK_TEST_CGROUP=SHELL_SERVICE)
            n, portal_adds = len(notifies()), len([r for r in records(log) if 'add' in r])
            run(install_v2, env_shell)
            for _ in range(2):
                run(start, env_shell, ok=False)
            wait_for(lambda: (appdir / 'pending-question').exists(), 'the question is pending')
            time.sleep(1.5)
            assert len([r for r in records(log) if 'add' in r]) == portal_adds, 'the portal was asked from the service of the shell'
            assert len(notifies()) == n and os.readlink(appdir / 'current') == bad
            p = run(start, env_shell, ok=False)
            assert f'panpak rollback {ID}' in p.stderr, p.stderr
            caps.unlink()
            print('4i. out of a scope of an app the portal is not asked: nothing shown, pending, said on stderr: ok')
            # "Uninstall" through the service: "Cancel" removes nothing; the answer "Remove" removes the app (the last thing of this test).
            for key, removed in (('no', False), ('yes', True)):
                time.sleep(3)  # the router of the private bus of the app stays 1.5 s after the last copy, and an app that is open is not removed
                n = len(notifies())
                pr = subprocess.Popen([BIN / 'panpak', 'remove', '--ask', ID], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                wait_for(lambda: len(notifies()) > n, 'the question to remove is shown by the service')
                q = notifies()[-1]
                assert q['app'] == 'Panpak' and q['actions'][:5] == ['default', '', 'yes', q['actions'][3], 'no'], q
                send('action ' + key)
                out, err = pr.communicate(timeout=60)
                assert pr.returncode == 0, (out, err)
                assert appdir.exists() != removed, f'{key}: removed={not appdir.exists()}'
                if not removed:
                    assert 'cancelled: nothing was removed' in out, out
            print('4j. uninstall through the service: Cancel removes nothing, Remove removes: ok')
            if os.environ.get('PANPAK_ASKN_DUMP_TEST'):
                raise AssertionError('forced, to see the diagnostics')
        except BaseException:
            print('=== DIAGNOSTICS (the test failed) ===')
            print('--- commands run by the test (start, end, command, status, stdout, stderr)')
            for t0, t1, cmd, rc, out, err in RUNLOG:
                print(f'{t0:.3f}-{t1:.3f} {cmd} rc={rc} out={out!r} err={err!r}')
            print('--- the service log')
            for r in records(slog):
                print(json.dumps(r))
            print('--- the portal log')
            for r in records(log):
                print(json.dumps({k: v for k, v in r.items() if k not in ('body', 'buttons')}))
            print('--- files of the app')
            for f in sorted(appdir.iterdir()):
                print(f.name, '->', os.readlink(f) if f.is_symlink() else (f.read_text()[-200:] if f.is_file() else 'dir'))
            raise
        finally:
            for pr in reversed(procs):
                pr.terminate()
                pr.wait()
        print('questions without a window tool PASS')
        time.sleep(1)
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
