#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The filtered session bus (PANPAK-FORMAT.md §9.5), end to end.

Usage: python3 tests/dbus_filter.py [work-directory]
Build release first. Everything runs in a private session (tests/dbus_lab.py): a bus of its own,
the real xdg-desktop-portal, a fake backend, an isolated HOME. The only thing outside is the
transient systemd scope of each start, which ends with the process.

1. The closed list. Every interface and method the portal exports is called through a proxy with
   the rules the product builds for the package (`panpak dbus-rules`). The ones the rules allow
   reach the portal; every other is refused by the proxy. Names of other services too.
2. An app in the isolated view: portal settings and their change, the identity the portal sees
   (`panpak.<id>`), the notification permission, the names it may own, a second copy that finds
   the first by D-Bus (single instance), the document folder, what the app is told in its environment.
"""
import fnmatch
from pathlib import Path
import os
import re
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
import app_dirs  # noqa: E402
import dbus_lab  # noqa: E402
import isolation  # noqa: E402

PROBE = r'''#!/usr/bin/python3
import os, subprocess, sys, time
out = {}
rt = os.environ.get('XDG_RUNTIME_DIR', '')

def gd(*a, timeout=10):
    p = subprocess.run(['gdbus', 'call', '--session', *a], capture_output=True, text=True, timeout=timeout)
    t = (p.stdout + p.stderr).strip().replace('\n', ' ')
    if p.returncode == 0:
        return 'ok ' + t
    # The proxy answers AccessDenied for a name it shows and ServiceUnknown for one it hides: both are a refusal.
    return 'denied' if any(w in t for w in ('AccessDenied', 'ServiceUnknown', 'NameHasNoOwner')) else 'err ' + t[:120]

PORTAL = ['--dest', 'org.freedesktop.portal.Desktop', '--object-path', '/org/freedesktop/portal/desktop', '--method']
BUS = ['--dest', 'org.freedesktop.DBus', '--object-path', '/org/freedesktop/DBus', '--method']
out['env_bus'] = os.environ.get('DBUS_SESSION_BUS_ADDRESS', 'UNSET')
out['env_gtk_portal'] = os.environ.get('GTK_USE_PORTAL', 'UNSET')
out['env_qt_theme'] = os.environ.get('QT_QPA_PLATFORMTHEME', 'UNSET')
out['bus_socket'] = 'yes' if os.path.exists(rt + '/bus') else 'no'
out['pid'] = os.getpid()
try:
    out['cgroup'] = open('/proc/self/cgroup').read().strip().split('/')[-1]
except OSError as e:
    out['cgroup'] = f'err{e.errno}'
if '--no-bus' in sys.argv:
    pass
else:
    out['settings'] = gd(*PORTAL, 'org.freedesktop.portal.Settings.ReadOne', 'org.freedesktop.appearance', 'color-scheme')
    out['names'] = gd(*BUS, 'org.freedesktop.DBus.ListNames')
    out['location'] = gd(*PORTAL, 'org.freedesktop.portal.Location.CreateSession', '{}')
    out['registry'] = gd('--dest', 'org.freedesktop.portal.Desktop', '--object-path', '/org/freedesktop/host/portal/Registry', '--method', 'org.freedesktop.host.portal.Registry.Register', 'org.example.fake', '{}')
    out['documents'] = gd('--dest', 'org.freedesktop.portal.Documents', '--object-path', '/org/freedesktop/portal/documents', '--method', 'org.freedesktop.portal.Documents.GetMountPoint')
    out['properties'] = gd(*PORTAL, 'org.freedesktop.DBus.Properties.Get', 'org.freedesktop.portal.Settings', 'version')
    out['introspect'] = gd(*PORTAL, 'org.freedesktop.DBus.Introspectable.Introspect')
    out['secrets'] = gd('--dest', 'org.freedesktop.secrets', '--object-path', '/org/freedesktop/secrets', '--method', 'org.freedesktop.Secret.Service.OpenSession', 'plain', '<""')
    out['systemd'] = gd('--dest', 'org.freedesktop.systemd1', '--object-path', '/org/freedesktop/systemd1', '--method', 'org.freedesktop.systemd1.Manager.ListUnits')
    out['old_notify'] = gd('--dest', 'org.freedesktop.Notifications', '--object-path', '/org/freedesktop/Notifications', '--method', 'org.freedesktop.Notifications.GetServerInformation')
    out['portal_notify'] = gd(*PORTAL, 'org.freedesktop.portal.Notification.AddNotification', 'n1', '{"title": <"hello">, "body": <"world">}')
    out['inhibit'] = gd(*PORTAL, 'org.freedesktop.portal.Inhibit.Inhibit', '', 'uint32 4', '{"reason": <"test">}')
    out['own_self'] = gd(*BUS, 'org.freedesktop.DBus.RequestName', os.environ.get('PANPAK_APP_ID', ''), '4')
    out['own_other'] = gd(*BUS, 'org.freedesktop.DBus.RequestName', 'org.example.notmine.Name', '4')
    out['own_notif'] = gd(*BUS, 'org.freedesktop.DBus.RequestName', 'org.freedesktop.Notifications', '4')
    out['own_tray'] = gd(*BUS, 'org.freedesktop.DBus.RequestName', 'org.kde.StatusNotifierItem-2-1', '4')
    out['own_declared'] = gd(*BUS, 'org.freedesktop.DBus.RequestName', 'org.example.dirs.Second', '4')
    out['talk_tray'] = gd('--dest', 'org.kde.StatusNotifierWatcher', '--object-path', '/StatusNotifierWatcher', '--method', 'org.freedesktop.DBus.Peer.Ping')
    if '--ping-first' in sys.argv:
        out['ping_first'] = gd('--dest', os.environ.get('PANPAK_APP_ID', ''), '--object-path', '/', '--method', 'org.freedesktop.DBus.Peer.Ping')
    if '--watch-theme' in sys.argv:
        # Listen for the portal's SettingChanged for a few seconds; the harness changes the theme meanwhile.
        mon = subprocess.Popen(['gdbus', 'monitor', '--session', '--dest', 'org.freedesktop.portal.Desktop', '--object-path', '/org/freedesktop/portal/desktop'],
                               stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        open(os.environ['XDG_DATA_HOME'] + '/watching', 'w').write('x')
        import select
        end = time.time() + 12
        out['setting_changed'] = 'none'
        while time.time() < end:
            r, _, _ = select.select([mon.stdout], [], [], 0.5)
            if r:
                line = mon.stdout.readline()
                if 'SettingChanged' in line and 'color-scheme' in line:
                    out['setting_changed'] = line.strip()[-60:]
                    break
        mon.kill()
    if '--hold-name' in sys.argv:
        # Keep the name until the harness leaves a mark in the data folder (the harness sees it; /tmp is the view's).
        import gi
        gi.require_version('Gio', '2.0')
        from gi.repository import Gio, GLib
        loop = GLib.MainLoop()
        done = []
        def on_name(c, n):
            open(os.environ['XDG_DATA_HOME'] + '/name-held', 'w').write('x')
        Gio.bus_own_name(Gio.BusType.SESSION, os.environ['PANPAK_APP_ID'], Gio.BusNameOwnerFlags.NONE, None, on_name, None)
        def poll():
            if os.path.exists(os.environ['XDG_DATA_HOME'] + '/name-release'):
                loop.quit()
                return False
            return True
        GLib.timeout_add(100, poll)
        GLib.timeout_add_seconds(40, loop.quit)
        loop.run()
try:
    out['doc_dir'] = ','.join(sorted(os.listdir(rt + '/doc'))) or 'empty'
except OSError as e:
    out['doc_dir'] = f'err{e.errno}'
with open(os.environ['XDG_DATA_HOME'] + '/report.txt', 'w') as f:
    for k, v in out.items():
        f.write(f'{k}={v}\n')
'''


def need(cond, msg):
    if not cond:
        raise SystemExit(f'FAIL: {msg}')


def rules_of(lab, pkg):
    return lab.cli('dbus-rules', pkg).stdout.split()


def parse_rules(args):
    """(calls, broadcasts) as lists of (name, interface pattern, path pattern), and the policies."""
    calls, own, talk = [], [], []
    for a in args:
        m = re.match(r'--call=([^=]+)=([^@]*)@(.*)$', a)
        if m:
            calls.append(m.groups())
        elif a.startswith('--own='):
            own.append(a[6:])
        elif a.startswith('--talk='):
            talk.append(a[7:])
    return calls, own, talk


def allowed_by(calls, name, path, iface, method):
    for n, rule, p in calls:
        if n != name:
            continue
        if p.endswith('/*'):
            if not (path == p[:-2] or path.startswith(p[:-1])):
                continue
        elif p != path:
            continue
        full = f'{iface}.{method}'
        if rule == '*' or rule == full or (rule.endswith('.*') and (iface == rule[:-2] or full.startswith(rule[:-1]))):
            return True
    return False


def walk(sess, dest, path, seen, out):
    """Every (path, interface, method) the object tree under `path` exports (no recursion into requests)."""
    node = sess.introspect(dest, path)
    if node is None:
        return
    for i in node.findall('interface'):
        for m in i.findall('method'):
            out.append((path, i.get('name'), m.get('name')))
    for n in node.findall('node'):
        child = (path.rstrip('/') + '/' + n.get('name'))
        if child not in seen:
            seen.add(child)
            walk(sess, dest, child, seen, out)


def proxy_for(sess, rules, work, name):
    sock = Path(sess.rt) / f'{name}.sock'
    proc = subprocess.Popen(['/usr/bin/xdg-dbus-proxy', sess.addr, str(sock), *rules], env={}, stdin=subprocess.DEVNULL)
    sess.procs.append(proc)
    for _ in range(100):
        if sock.exists():
            return f'unix:path={sock}'
        time.sleep(0.05)
    raise SystemExit('FAIL: the proxy made no socket')


def closed_list(sess, lab, pkg, extra_allowed=()):
    rules = rules_of(lab, pkg)
    need(rules[0] == '--filter', f'rules do not start with --filter: {rules[:2]}')
    need(not any(r.startswith('--talk=org.freedesktop') for r in rules), 'a --talk to an org.freedesktop name')
    calls, own, talk = parse_rules(rules)
    objects = []
    walk(sess, 'org.freedesktop.portal.Desktop', '/', {'/'}, objects)
    objects = [o for o in objects if '/request/' not in o[0] and '/session/' not in o[0]]
    ifaces = sorted({i for _, i, _ in objects})
    for want in ('org.freedesktop.portal.Settings', 'org.freedesktop.portal.Inhibit', 'org.freedesktop.portal.Notification', 'org.freedesktop.host.portal.Registry',
                 'org.freedesktop.portal.Location', 'org.freedesktop.portal.Background', 'org.freedesktop.portal.Screenshot', 'org.freedesktop.portal.ScreenCast',
                 'org.freedesktop.portal.RemoteDesktop', 'org.freedesktop.portal.Secret', 'org.freedesktop.portal.FileChooser',
                 'org.freedesktop.portal.OpenURI', 'org.freedesktop.portal.Account'):
        need(want in ifaces, f'the portal of this machine does not export {want}: the test would not cover it (exports: {ifaces})')
    if 'org.freedesktop.portal.Camera' not in ifaces:
        print('note: this portal does not export Camera here (no PipeWire camera support); Camera is not covered on this machine')
    bus = proxy_for(sess, rules, None, 'closed-' + str(abs(hash(tuple(rules))) % 10**6))
    allowed, denied, wrong = [], [], []
    for path, iface, method in objects:
        # Call with no arguments: the proxy decides on the header alone. A service that gets it
        # answers with something else (invalid arguments, unknown object); the proxy says AccessDenied
        # with the error name as text.
        rc, text = sess.call('org.freedesktop.portal.Desktop', path, f'{iface}.{method}', bus=bus, timeout=15)
        proxy_denied = 'GDBus.Error:org.freedesktop.DBus.Error.AccessDenied: org.freedesktop.DBus.Error.AccessDenied' in text
        want = allowed_by(calls, 'org.freedesktop.portal.Desktop', path, iface, method)
        (denied if proxy_denied else allowed).append((iface, method))
        if want == proxy_denied:
            wrong.append((path, iface, method, 'allowed by the rules but refused' if want else 'not in the rules but it got through', text[:100]))
    need(not wrong, f'closed list broken: {wrong[:6]}')
    # The same call on the unfiltered bus is not an AccessDenied from the proxy: the refusals above are the proxy's.
    rc, text = sess.call('org.freedesktop.portal.Desktop', '/org/freedesktop/portal/desktop', 'org.freedesktop.portal.Location.CreateSession')
    need('GDBus.Error:org.freedesktop.DBus.Error.AccessDenied: org.freedesktop.DBus.Error.AccessDenied' not in text, f'control call also refused without the proxy: {text}')
    # Other services: the fake backend owns their names and answers anything, so a refusal is the proxy's.
    for dest, path, method in [('org.freedesktop.secrets', '/org/freedesktop/secrets', 'org.freedesktop.Secret.Service.OpenSession'),
                               ('org.freedesktop.systemd1', '/org/freedesktop/systemd1', 'org.freedesktop.systemd1.Manager.ListUnits'),
                               ('org.freedesktop.Notifications', '/org/freedesktop/Notifications', 'org.freedesktop.Notifications.GetServerInformation'), ('org.freedesktop.Notifications', '/org/freedesktop/Notifications', 'org.freedesktop.Notifications.ListCapabilitiesNot'),
                               ('org.gnome.keyring', '/', 'org.freedesktop.DBus.Peer.Ping'), ('ca.desrt.dconf', '/', 'org.freedesktop.DBus.Peer.Ping'),
                               ('org.gnome.Shell', '/org/gnome/Shell', 'org.gnome.Shell.Eval'), ('org.a11y.Bus', '/org/a11y/bus', 'org.a11y.Bus.GetAddress'),
                               ('org.freedesktop.login1', '/org/freedesktop/login1', 'org.freedesktop.login1.Manager.PowerOff'),
                               ('org.freedesktop.PolicyKit1', '/', 'org.freedesktop.DBus.Peer.Ping')]:
        rc, direct = sess.call(dest, path, method)
        need(rc == 0 and 'reached' in direct, f'control: {dest} does not answer on the bus itself: {direct}')
        rc, text = sess.call(dest, path, method, bus=bus)
        iface, member = method.rsplit('.', 1)
        if allowed_by(calls, dest, path, iface, member) or dest in own:
            need(rc == 0 and 'reached' in text, f'{dest} {method} is in the rules but did not get through: {text}')
        else:
            need(rc != 0 and 'reached' not in text and ('AccessDenied' in text or 'ServiceUnknown' in text or 'NameHasNoOwner' in text), f'{dest} {method} got through the proxy: {text}')
    # A name with a rule still allows only the methods of the rule.
    rc, text = sess.call('org.freedesktop.Notifications', '/org/freedesktop/Notifications', 'org.freedesktop.DBus.Properties.GetAll', bus=bus)
    need(rc != 0 and 'reached' not in text, f'Properties on the notification service got through: {text}')
    print(f'closed list: {len(objects)} methods of {len(ifaces)} portal interfaces called through the proxy; {len(allowed)} reached the portal, {len(denied)} refused')
    return len(objects), len(allowed), len(denied)


def main():
    why = dbus_lab.missing()
    if why:
        print(f'skipped: {why}')
        return
    work = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT / 'target/dbus-filter-test'
    if work.exists():
        isolation.require_inside(work, work.parent, 'work')
        subprocess.run(['chmod', '-R', 'u+w', str(work)], check=True)
        isolation.safe_rmtree(work, work.parent)
    work.mkdir(parents=True)
    app_dirs.PROBE = PROBE
    lab = app_dirs.Lab(work)
    # The portal looks for `panpak.<id>.desktop` in the data folder of the app's user: the lab's home.
    sess = dbus_lab.Session(ROOT / 'tests', portal=True, data_home=lab.home / '.local/share')
    try:
        # The lab's own buses are not used; its commands run in the private session.
        lab.env.update({k: sess.env[k] for k in ('XDG_RUNTIME_DIR', 'DBUS_SESSION_BUS_ADDRESS', 'XDG_CURRENT_DESKTOP')})
        run_all(lab, sess, work)
    finally:
        sess.close()
        for p in (lab.bus, lab.sysbus):
            p.terminate()
        shutil.rmtree(lab.runtime, ignore_errors=True)


def report(lab, args=()):
    rc, rep, err = lab.run(*args)
    need(rc == 0, f'the app failed: {err}')
    return rep


def run_all(lab, sess, work):
    # --- 1. the closed list, with and without the optional permissions
    plain = lab.pack('')
    both = lab.pack('[permissions]\nnotifications = true\ntray = true\ndbus_own = ["org.example.dirs.Second"]\n')
    closed_list(sess, lab, plain)
    closed_list(sess, lab, both)
    # --- 2. an app in the view
    need(lab.install('[permissions]\nnotifications = true\ndbus_own = ["org.example.dirs.Second"]\n').returncode == 0, 'install failed')
    sess.log.write_text('')
    rep = report(lab)
    pid = f'panpak.{app_dirs.ID}'
    env_bus = f'unix:path={sess.rt}/bus'
    for k, want in {'env_bus': env_bus, 'env_gtk_portal': '1', 'env_qt_theme': 'xdgdesktopportal', 'bus_socket': 'yes'}.items():
        need(rep.get(k) == want, f'{k}: {rep.get(k)!r}, wanted {want!r}')
    need(re.fullmatch(r'app-panpak\.org\.example\.dirs-[0-9a-f]{8}\.scope', rep.get('cgroup', '')), f'the app is not in its own scope: {rep.get("cgroup")}')
    need('uint32 1' in rep['settings'], f'portal settings: {rep["settings"]}')
    names = rep['names']
    for bad in ('secrets', 'systemd1', 'gnome', 'kde', 'dconf', 'Documents'):
        need(bad not in names, f'ListNames shows {bad}: {names}')
    need('org.freedesktop.portal.Desktop' in names and 'org.freedesktop.Notifications' in names, f'ListNames: {names}')
    for k in ('location', 'registry', 'documents', 'properties', 'introspect', 'secrets', 'systemd', 'own_other', 'own_notif', 'own_tray', 'talk_tray'):
        need(rep[k] == 'denied', f'{k}: {rep[k]}')
    for k in ('portal_notify', 'inhibit', 'old_notify'):
        need(rep[k].startswith('ok'), f'{k}: {rep[k]}')
    need(rep['own_self'].startswith('ok') and '(uint32 1,)' in rep['own_self'], f'own id: {rep["own_self"]}')
    need(rep['own_declared'].startswith('ok'), f'declared name: {rep["own_declared"]}')
    log = sess.log.read_text()
    need(f'AddNotification app_id={pid} id=n1' in log, f'the portal did not know the app as {pid}: {log!r}')
    need(f'Inhibit app_id={pid}' in log, f'inhibit identity: {log!r}')
    need(rep['doc_dir'] == 'empty', f'document folder: {rep["doc_dir"]}')
    print('app in the view: scope, identity', pid, ', settings, refusals, own names: ok')
    # --- 3. without the permission: no notification, no declared name
    need(lab.install('').returncode == 0, 'install failed')
    rep = report(lab)
    for k in ('portal_notify', 'own_declared', 'old_notify'):
        need(rep[k] == 'denied', f'without the permission {k}: {rep[k]}')
    need('org.freedesktop.Notifications' not in rep['names'], f'the notification service is visible without the permission: {rep["names"]}')
    need(rep['inhibit'].startswith('ok') and rep['own_self'].startswith('ok'), 'base rules lost')
    print('without the permissions: notification and declared name refused, base rules kept: ok')
    data = lab.home / f'.local/share/panpak/userdata/{app_dirs.ID}/data'
    # --- 3b. the tray: the watcher, and the item names of the view (the app is process 2 there)
    need(lab.install('[permissions]\ntray = true\n').returncode == 0, 'install failed')
    rep = report(lab)
    need(rep['pid'] == '2', f'the app is process {rep["pid"]} in the view, the tray names assume 2')
    need(rep['own_tray'].startswith('ok') and rep['talk_tray'].startswith('ok'), f'tray: {rep["own_tray"]} / {rep["talk_tray"]}')
    need(rep['own_other'] == 'denied' and rep['own_notif'] == 'denied', 'tray permission opened other names')
    print('tray permission: item name of the view and the watcher reachable, other names still refused: ok')
    need(lab.install('').returncode == 0, 'install failed')
    # --- 4. single instance across two starts
    first = subprocess.Popen([str(lab.home / '.local/bin' / app_dirs.ID), '--hold-name'], env=lab.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(200):
            if (data / 'name-held').exists():
                break
            time.sleep(0.1)
        need((data / 'name-held').exists(), 'the first copy never held the name')
        second = report(lab, ['--ping-first'])
        need(second['ping_first'].startswith('ok'), f'the second copy cannot reach the first: {second["ping_first"]}')
        print('single instance: the second copy reaches the first through the bus: ok')
    finally:
        (data / 'name-release').write_text('x')
        first.wait(timeout=60)
        for f in ('name-held', 'name-release'):
            (data / f).unlink(missing_ok=True)
    # --- 5. the theme changes while the app runs: the signal crosses the proxy
    (data / 'watching').unlink(missing_ok=True)
    watcher = subprocess.Popen([str(lab.home / '.local/bin' / app_dirs.ID), '--watch-theme'], env=lab.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(200):
            if (data / 'watching').exists():
                break
            time.sleep(0.1)
        need((data / 'watching').exists(), 'the app never started to watch')
        time.sleep(1)
        rc, text = sess.call('org.freedesktop.impl.portal.desktop.fake', '/test/fake', 'test.Fake.SetColorScheme', 'uint32 2')
        need(rc == 0, f'cannot change the theme: {text}')
        watcher.wait(timeout=60)
    finally:
        (data / 'watching').unlink(missing_ok=True)
    changed = (lab.home / f'.local/share/panpak/userdata/{app_dirs.ID}/data/report.txt').read_text()
    need('setting_changed=' in changed and 'none' not in changed.split('setting_changed=')[1].splitlines()[0], f'the app did not see the theme change: {changed[-300:]}')
    print('theme change: the app sees SettingChanged through the proxy: ok')
    print('all D-Bus filter tests passed')


if __name__ == '__main__':
    main()
