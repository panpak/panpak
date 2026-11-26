#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The ways protected mode is shown and answered (PANPAK-FORMAT.md 3.1.10): the notification and its button, the windows of `zenity` (the new permissions,
the risk of the internet), the question of a program of the terminal, the menu action, and the actions that a front end of its own would call.

Usage: python3 tests/protected_ui.py [work-directory]
Build the debug binaries first (cargo build -p panpak -p panpak-stub --target x86_64-unknown-linux-musl): the tools that `panpak` calls (`zenity`,
`notify-send`) are looked for in `PANPAK_TEST_TOOLS_DIR` in a debug build, where this script puts fakes that record what they are asked and answer as told. The
notification service is a fake too (PyGObject owns `org.freedesktop.Notifications` on a private bus, records `Notify` and sends `ActionInvoked` when told).
Everything runs in an isolated HOME. An update is made ready with `panpak update --stage` from a local server.

  1. an update that is ready and asks for a permission: the state says so (`protected --json`), with the permissions and the actions;
  2. the notification has the button only when an update is ready; pressing it opens a window with the new permissions: No leaves everything as it is, Yes
     updates and ends the mode with a message; with no dialog tool nothing is accepted and the person is told, with no command to type;
  3. the same window and the same update when a copy of the app is running: it is accepted, waits, and the next start applies it without asking;
  4. the menu action: the window that explains the risk (and the one that turns the internet off again), No changes nothing, Yes does; with no window tool the question is a
     notification with two buttons (the fake service), and with no tool and no service nothing is changed and the way is told;
  5. a program of the terminal that is run from an interactive terminal is asked: the permissions and the question for the update, then the internet;
  6. the actions as JSON for a front end;
  7. an address that a graphical app tries to open in protected mode is not opened, and the person is told in a notification (not a silent no).
"""
import json
import os
from pathlib import Path
import select
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/x86_64-unknown-linux-musl/debug'
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402

ID = 'org.example.prui'
PUBLIC = None

FAKE_DAEMON = r'''
import gi, json, os, sys
gi.require_version('Gio', '2.0')
from gi.repository import Gio, GLib
log, click = sys.argv[1], sys.argv[2]
XML = """<node><interface name="org.freedesktop.Notifications">
<method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/><arg type="a{sv}" direction="in"/><arg type="i" direction="in"/><arg type="u" direction="out"/></method>
<method name="GetCapabilities"><arg type="as" direction="out"/></method>
<signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal></interface></node>"""
conn_box = []
def on_call(conn, sender, path, iface, method, params, inv):
    if method == 'GetCapabilities':
        inv.return_value(GLib.Variant('(as)', (['actions', 'body'],)))
        return
    p = params.unpack()
    with open(log, 'a') as f:
        f.write(json.dumps({'summary': p[3], 'body': p[4], 'actions': p[5]}) + '\n')
    inv.return_value(GLib.Variant('(u)', (7,)))
def poll():
    if os.path.exists(click) and conn_box:
        os.unlink(click)
        conn_box[0].emit_signal(None, '/org/freedesktop/Notifications', 'org.freedesktop.Notifications', 'ActionInvoked', GLib.Variant('(us)', (7, 'update')))
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


def run(args, env, ok=True, timeout=120, stdin=subprocess.DEVNULL):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout, stdin=stdin)
    if ok is True:
        assert p.returncode == 0, f'{args}: {p.stdout}{p.stderr}'
    elif ok is False:
        assert p.returncode != 0, f'{args}: {p.stdout}{p.stderr}'
    return p


def wait_for(cond, what, seconds=15):
    end = time.time() + seconds
    while time.time() < end:
        if cond():
            return
        time.sleep(0.1)
    raise AssertionError(f'timeout: {what}')


class Lab:
    def __init__(self, tmp):
        self.tmp = tmp
        isolation.require_isolated_home(tmp, tmp)
        self.runtime = isolation.owned_mkdtemp('prui-rt-')
        self.tools = tmp / 'tools'
        self.tools.mkdir()
        self.zlog, self.nlog, self.answer = tmp / 'zenity.log', tmp / 'notify.log', tmp / 'zenity.answer'
        self.zenity = self.tools / 'zenity'
        self.zenity.write_text(f'#!/bin/sh\nprintf "%s" "$*" | tr "\\n" " " >> {self.zlog}\necho >> {self.zlog}\nexit $(cat {self.answer} 2>/dev/null || echo 0)\n')
        self.zenity.chmod(0o755)
        (self.tools / 'notify-send').write_text(f'#!/bin/sh\necho "$@" >> {self.nlog}\n')
        (self.tools / 'notify-send').chmod(0o755)
        # A fake `xdg-open` that the outer stub finds in its PATH: it records the address it is given.
        self.opened = tmp / 'opened.txt'
        fakebin = tmp / 'fakebin'
        fakebin.mkdir()
        (fakebin / 'xdg-open').write_text(f'#!/bin/sh\necho "$1" >> {self.opened}\n')
        (fakebin / 'xdg-open').chmod(0o755)
        self.env = {'PATH': f'{fakebin}:{os.environ["PATH"]}', 'HOME': str(tmp), 'XDG_DATA_HOME': str(tmp / 'data'), 'XDG_CACHE_HOME': str(tmp / 'cache'), 'XDG_CONFIG_HOME': str(tmp / 'config'),
                    'XDG_RUNTIME_DIR': self.runtime, 'LC_ALL': 'C', 'PANPAK_TEST_REAL_HOME': str(tmp), 'PANPAK_TEST_TOOLS_DIR': str(self.tools), 'PANPAK_NO_BACKGROUND': '1'}
        out = self.cli('keygen', '-o', str(tmp / 'secret.key')).stdout
        self.pub = next(l.split()[-1] for l in out.splitlines() if l.startswith('key:'))
        s = socket.socket()
        s.bind(('127.0.0.1', 0))
        self.port = s.getsockname()[1]
        s.close()
        self.serve = tmp / 'serve'
        self.serve.mkdir()
        self.appdir = tmp / 'data/panpak/apps' / ID
        self.server = subprocess.Popen([sys.executable, ROOT / 'tests/range_server.py', self.serve, str(self.port), 'normal'], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(0.4)

    def cli(self, *args, ok=True, stdin=subprocess.DEVNULL):
        return run([BIN / 'panpak', *args], self.env, ok=ok, stdin=stdin)

    def pack(self, name, release, permissions='', desktop='', body='exit 0'):
        tree = self.tmp / f'tree-{name}'
        tree.mkdir()
        (tree / 'hello').write_text(f'#!/bin/sh\n{body}\n')
        (tree / 'hello').chmod(0o755)
        (tree / 'panpak.toml').write_text(
            f'[app]\nid = "{ID}"\nname = "Prui"\nversion = "{name}"\nentry = "hello"\n{release}{desktop}{permissions}'
            f'[update]\nurl = "http://127.0.0.1:{self.port}/remote.panpak"\nkey = "{self.pub}"\n')
        out = self.tmp / f'{name}.panpak'
        run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', self.tmp / 'secret.key', '-o', out], self.env)
        return out

    def stage(self, pkg):
        (self.serve / 'remote.panpak').write_bytes(Path(pkg).read_bytes())
        return self.cli('update', '--stage', '--allow-insecure-http', '--timeout', '5', ID)

    def zenity_calls(self):
        return self.zlog.read_text().splitlines() if self.zlog.exists() else []

    def notices(self):
        return self.nlog.read_text().splitlines() if self.nlog.exists() else []

    def state(self):
        return json.loads(self.cli('protected', '--json', ID).stdout)

    def current_version(self):
        return self.cli('inspect', self.appdir / 'current').stdout


def terminal(lab, args, keys):
    """Run `panpak args` with a pseudo terminal on its standard input and error, typing `keys` as the questions come. Returns the text it wrote."""
    master, slave = os.openpty()
    p = subprocess.Popen([str(BIN / 'panpak'), *args], env=lab.env, stdin=slave, stdout=subprocess.DEVNULL, stderr=slave)
    os.close(slave)
    keys, buf, seen, end = list(keys), b'', 0, time.time() + 60
    while time.time() < end and p.poll() is None:
        r, _, _ = select.select([master], [], [], 0.3)
        if r:
            try:
                buf += os.read(master, 4096)
            except OSError:
                break
            if keys and buf[seen:].rstrip().endswith(b']'):
                os.write(master, keys.pop(0).encode())
                seen = len(buf)
    p.wait(timeout=60)
    try:
        while select.select([master], [], [], 0.2)[0]:
            buf += os.read(master, 4096)
    except OSError:
        pass
    os.close(master)
    assert not keys, f'a question was not asked: {keys}: {buf.decode(errors="replace")}'
    return buf.decode(errors='replace')


def main():
    work = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else None
    with tempfile.TemporaryDirectory(prefix='panpak-prui-') as tmpname:
        tmp = Path(tmpname) if work is None else work
        tmp.mkdir(parents=True, exist_ok=True)
        lab = Lab(tmp)
        bus_addr = f'unix:path={lab.runtime}/bus'
        bus = subprocess.Popen(['dbus-daemon', f'--config-file={ROOT}/tests/bus-no-activation.conf', '--nofork', f'--address={bus_addr}'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        wait_for(lambda: (Path(lab.runtime) / 'bus').exists(), 'the private bus')
        lab.env_bus = dict(lab.env, DBUS_SESSION_BUS_ADDRESS=bus_addr)
        lab.nlog_daemon, lab.click = tmp / 'daemon.log', tmp / 'click'
        daemon = subprocess.Popen([sys.executable, '-c', FAKE_DAEMON, str(lab.nlog_daemon), str(lab.click)], env=lab.env_bus, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            wait_for(lambda: Path(str(lab.nlog_daemon) + '.ready').exists(), 'the fake notification service')
            main_inner(lab, tmp)
        finally:
            for proc in (daemon, bus, lab.server):
                proc.terminate()
                proc.wait(timeout=5)
            isolation.remove_owned(lab.runtime, tmp)


def main_inner(lab, tmp):
    graphical = '[desktop]\nwm_class = "Prui"\n'
    v1 = lab.pack('v1', '[release]\nrevision = 1\n', desktop=graphical)
    # The fixed version asks for a permission the first one did not have, and its floor is its own revision.
    v2 = lab.pack('v2', '[release]\nrevision = 2\nfloor = 2\n', desktop=graphical, permissions='[permissions]\nnetwork = true\nnetwork_allow_tcp = [443]\n')
    lab.cli('install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', v1)

    # 1. A floor that this machine saw, with nothing staged: protected, with no update and no `update` action.
    isolation.set_floor(lab.appdir, 2)
    state = lab.state()
    assert state['protected'] and state['update'] is None and state['actions'] == ['internet_on'], state
    # With no update ready the notification is the text and nothing else: no button.
    run([BIN / 'panpak', 'protected-start', ID], lab.env_bus)
    wait_for(lambda: lab.nlog_daemon.exists(), 'the notification without a button')
    n = json.loads(lab.nlog_daemon.read_text().splitlines()[-1])
    assert n['actions'] == [] and n['summary'] == 'Prui is running in protected mode', n
    # It says that the fixed version comes by itself, and nothing about downloaded programs.
    assert 'Panpak will download the fixed version as soon as it can, and the mode will end by itself.' in n['body'], n
    assert 'downloaded' not in n['body'] and 'programs' not in n['body'], n
    lab.nlog_daemon.unlink()
    # Staged: the update is ready, with its new permission.
    lab.stage(v2)
    state = lab.state()
    assert state['protected'] and state['kind'] == 'graphical' and state['floor'] == 2, state
    assert state['update']['version'] == 'v2' and len(state['update']['new_permissions']) >= 1 and state['update']['accepted'] is False, state
    assert state['actions'] == ['update', 'internet_on'], state
    assert (lab.appdir / 'protected').is_file()
    print('  an update that is ready: the state has its version, the permissions it asks for and the two actions: ok')

    # 2. The notification: with the button, because an update is ready. (The fake notification service runs on a private bus for the whole test.)
    env_bus, nlog, click = lab.env_bus, lab.nlog_daemon, lab.click

    wait_for(lambda: Path(str(nlog) + '.ready').exists(), 'the fake notification service')

    def last_notice():
        return json.loads(nlog.read_text().splitlines()[-1])

    # The start of a graphical app with nobody to ask starts the notice by itself: the title, the body and the button "Update now".
    run([BIN / 'panpak', 'protected-start', ID], env_bus)
    wait_for(lambda: nlog.exists(), 'the notification')
    n = last_notice()
    assert n['summary'] == 'Prui is running in protected mode' and 'Your files are safe' in n['body'] and n['actions'] == ['update', 'Update now'], n
    assert 'panpak update' not in n['body'] and 'allow-internet' not in n['body'], n
    assert 'will download the fixed version' not in n['body'] and 'downloaded' not in n['body'], n
    # The window of the new permissions: No leaves everything as it is.
    lab.answer.write_text('1')
    click.write_text('')
    wait_for(lambda: len(lab.zenity_calls()) >= 1, 'the window of the permissions')
    z = lab.zenity_calls()[0]
    assert '--question' in z and 'Update Prui' in z and 'asks for more than the version you have' in z and '--ok-label Update' in z and '--cancel-label Not now' in z, z
    time.sleep(1.5)
    assert 'v1' in lab.current_version() and (lab.appdir / 'pending').exists(), 'No: nothing changed'
    assert lab.state()['update']['accepted'] is False
    print('  the notification has the button when an update is ready, and the window shows the permissions; No changes nothing: ok')
    # The button without the service (what a front end with a button of its own calls): Yes updates, and the end of the mode is told.
    lab.answer.write_text('0')
    lab.zlog.unlink()
    run([BIN / 'panpak', 'protected-notice', '--pressed', ID], lab.env)
    assert len(lab.zenity_calls()) == 1
    assert 'v2' in lab.current_version(), lab.current_version()
    assert not (lab.appdir / 'protected').exists() and lab.state()['protected'] is False
    assert any('Prui is updated' in l and 'The problem is fixed and the internet works again' in l for l in lab.notices()), lab.notices()
    print('  Yes in the window updates, ends the mode (the mark goes) and says so in a notification: ok')

    # 3. A copy of the app is running: accepted, it waits, and the next start applies it with no question.
    lab.cli('remove', '--yes', ID, ok=None)
    lab.nlog.unlink(missing_ok=True)
    lab.zlog.unlink(missing_ok=True)
    long_running = lab.pack('w1', '[release]\nrevision = 1\n', desktop=graphical, body='sleep 6')
    w2 = lab.pack('w2', '[release]\nrevision = 2\nfloor = 2\n', desktop=graphical, permissions='[permissions]\nnetwork = true\nnetwork_allow_tcp = [443]\n', body='exit 0')
    lab.cli('install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', long_running)
    (lab.serve / 'remote.panpak').write_bytes(Path(w2).read_bytes())
    lab.cli('update', '--stage', '--allow-insecure-http', '--timeout', '5', ID)
    launcher = lab.tmp / '.local/bin' / ID
    app = subprocess.Popen([str(launcher)], env=lab.env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2.0)
    run([BIN / 'panpak', 'protected-notice', '--pressed', ID], lab.env)
    assert any('applies the next time it starts' in l for l in lab.notices()), lab.notices()
    assert lab.state()['update']['accepted'] is True and 'w1' in lab.current_version()
    app.wait(timeout=30)
    run([launcher], lab.env)
    assert 'w2' in lab.current_version(), 'the next start applied it without asking'
    assert not (lab.appdir / 'protected').exists()
    print('  with a copy running the update is accepted and waits; the next start applies it with no question: ok')

    # 4. The menu action: the window that explains the risk.
    lab.cli('remove', '--yes', ID, ok=None)
    lab.zlog.unlink(missing_ok=True)
    lab.nlog.unlink(missing_ok=True)
    lab.cli('install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', v1)
    isolation.set_floor(lab.appdir, 3)
    run([BIN / 'panpak', 'update', '--stage', '--allow-insecure-http', '--timeout', '5', ID], lab.env, ok=None)
    lab.cli('protected', ID)
    assert lab.state()['protected']
    lab.answer.write_text('1')
    lab.cli('allow-internet', '--ask', ID)
    z = lab.zenity_calls()[-1]
    assert 'Turn the internet back on for Prui?' in z and 'can be used against you' in z and '--ok-label Turn the internet on' in z, z
    assert not (lab.appdir / 'protected-net').exists(), 'No changes nothing'
    lab.answer.write_text('0')
    lab.cli('allow-internet', '--ask', ID)
    assert (lab.appdir / 'protected-net').is_file()
    assert any('Close and reopen Prui to use the internet' in l for l in lab.notices()), lab.notices()
    lab.cli('allow-internet', '--ask', ID)
    assert 'Turn the internet off again for Prui?' in lab.zenity_calls()[-1] and not (lab.appdir / 'protected-net').exists()
    # No dialog tool but a notification service (the fake one): the question is a notification with the two buttons, and with no answer nothing is changed.
    lab.zenity.rename(lab.tools / 'zenity.away')
    asked = len(nlog.read_text().splitlines())
    out = run([BIN / 'panpak', 'allow-internet', '--ask', ID], dict(lab.env, PANPAK_TEST_ASK_SECS='1'))
    lines = nlog.read_text().splitlines()
    assert len(lines) == asked + 1, 'the question was not sent to the notification service'
    n = json.loads(lines[-1])
    assert n['summary'] == 'Turn the internet back on for Prui?' and n['actions'] == ['default', '', 'yes', 'Turn the internet on', 'no', 'Not now'], n
    assert not (lab.appdir / 'protected-net').exists(), 'no answer changes nothing'
    # No dialog tool and no notification service (a bus with nobody on it): nothing is changed, and the person is told that there is nothing to answer with.
    nobody = dict(lab.env, DBUS_SESSION_BUS_ADDRESS='unix:path=' + str(lab.tmp / 'no-such-bus'))
    run([BIN / 'panpak', 'allow-internet', '--ask', ID], nobody)
    assert not (lab.appdir / 'protected-net').exists() and any(f'Open a terminal and type: panpak allow-internet {ID}' in l for l in lab.notices()), lab.notices()
    # The same for the update that asks for permissions: no window to show them, so nothing is accepted and the way is told.
    lab.stage(lab.pack('v3', '[release]\nrevision = 3\nfloor = 3\n', desktop=graphical, permissions='[permissions]\nnetwork = true\nnetwork_allow_tcp = [443]\n'))
    run([BIN / 'panpak', 'protected-notice', '--pressed', ID], nobody)
    assert any(f'asks for new permissions, but there is no window to show them to you. To update, open a terminal and type: panpak update {ID}' in l for l in lab.notices()), lab.notices()
    assert 'v1' in lab.current_version(), 'nothing was accepted'
    lab.zenity.with_name('zenity.away').rename(lab.zenity)
    print('  the menu action: the window explains the risk, No changes nothing, Yes turns it on, the same action turns it off; no tool, no change: ok')

    # 5. A program of the terminal from an interactive terminal: the permissions and the question for the update, then the internet.
    lab.cli('remove', '--yes', ID, ok=None)
    t1 = lab.pack('t1', '[release]\nrevision = 1\n')
    t2 = lab.pack('t2', '[release]\nrevision = 2\nfloor = 2\n', permissions='[permissions]\nnetwork = true\nnetwork_allow_tcp = [443]\n')
    lab.cli('install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', t1)
    lab.stage(t2)
    assert lab.state()['kind'] == 'terminal'
    text = terminal(lab, ['protected-start', ID], ['n', 'n'])
    assert 'asks for more than the version you have' in text and 'Update Prui now and accept these permissions? [y/N]' in text, text
    assert 'Turn the internet back on for Prui? [y/N]' in text and 'can be used against you' in text, text
    assert 'panpak ' not in text.replace('panpak:', ''), f'an instruction to type: {text}'
    assert 't1' in lab.current_version() and not (lab.appdir / 'protected-net').exists()
    # Asked once: the next start, with the same floor and the same update, has the short line and no question.
    text = terminal(lab, ['protected-start', ID], [])
    assert '[y/N]' not in text and '[Y/n]' not in text and 'protected mode: no internet. Your files are safe.' in text and 'downloaded' not in text, text
    assert 't1' in lab.current_version()
    # A new version is a new question: another fix is ready (revision 3), so it asks again.
    lab.stage(lab.pack('t3', '[release]\nrevision = 3\n', permissions='[permissions]\nnetwork = true\nnetwork_allow_tcp = [443]\n'))
    text = terminal(lab, ['protected-start', ID], ['y'])
    assert 't3' in lab.current_version() and not (lab.appdir / 'protected').exists(), lab.current_version()
    print('  a program of the terminal is asked where the person is: the permissions, the update, then the internet; one key each: ok')

    # 6. The actions as JSON, for a front end of its own.
    isolation.set_floor(lab.appdir, 9)
    r = json.loads(lab.cli('protected-action', 'internet-on', ID).stdout)
    assert r == {'result': 'done', 'why': None} and (lab.appdir / 'protected-net').is_file(), r
    r = json.loads(lab.cli('protected-action', 'internet-off', ID).stdout)
    assert r['result'] == 'done' and not (lab.appdir / 'protected-net').exists()
    isolation.set_floor(lab.appdir, 9)
    r = json.loads(lab.cli('protected-action', 'update', ID).stdout)
    assert r['result'] == 'nothing_ready', r
    print('  the actions answer in JSON: ok')

    # 7. A link that a graphical app tries to open in protected mode: refused, and the person is told.
    lab.cli('remove', '--yes', ID, ok=None)
    lab.opened.unlink(missing_ok=True)
    x1 = lab.pack('x1', '[release]\nrevision = 1\n', desktop=graphical, body='xdg-open "https://example.org/?data=secret" || true')
    lab.cli('install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', x1)
    run([launcher], lab.env_bus)
    assert lab.opened.exists(), 'outside protected mode the address is opened'
    lab.opened.unlink(missing_ok=True)
    lab.nlog_daemon.unlink(missing_ok=True)
    isolation.set_floor(lab.appdir, 2)
    (lab.appdir / 'protected').write_text('2\n')
    run([launcher], lab.env_bus)
    wait_for(lambda: lab.nlog_daemon.exists() and 'links do not open until you update' in lab.nlog_daemon.read_text(), 'the notification about the refused link')
    notices = [json.loads(l) for l in lab.nlog_daemon.read_text().splitlines()]
    link = [n for n in notices if 'links do not open' in n['body']][0]
    assert link['summary'] == 'Prui is running in protected mode' and link['body'] == 'Prui is in protected mode: links do not open until you update.' and link['actions'] == [], link
    assert not lab.opened.exists(), 'the address was not opened'
    print('  an address that a graphical app tries to open is refused, and the person is told in a notification: ok')
    print('protected ui PASS')


if __name__ == '__main__':
    main()
