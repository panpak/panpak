#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The private bus of an app (step 13p), end to end, against the real stub and the real sandbox.

Usage: python3 tests/privbus.py [work-directory]
Build release first (cargo build --release --target x86_64-unknown-linux-musl). Everything runs in a private session (tests/dbus_lab.py): a bus of
its own, the real xdg-desktop-portal, a fake backend, an isolated HOME. The only thing outside is the transient systemd scope of each start.

Regression tests of the findings of docs/SOURCE-BUILD.md section 4 and docs/PRIVATE-BUS.md:
  A    an app calls a name that a native process owns                        -> refused, the native process saw nothing
  B    an app owns the name of a native app that is not running              -> nobody outside reaches it, it gets nothing
  C    an app calls the name of a native program that the bus could start    -> refused, nothing started outside the sandbox
  F22  a native single-instance program (a GApplication) owns the app's id   -> the copy in the sandbox is a primary instance, not a remote of the native one
  F23  the copy is running and the native program starts                     -> the native program owns the id on the real bus, nothing reaches the copy
  F11  an app owns its own id with nothing declared                          -> it gets it
  S    two launches of the same app share one router                         -> the second finds the first (this FAILS with one router for each start)
  I    the portal knows every copy as panpak.<id>, also after the first copy ended, and answers reach the copy that asked
  L    a link goes to the opener and the app is told what happened
  M    a media player name gets one alias on the host bus; nothing else of the family
  R    the app cannot register another identity with the portal
  X    the accessibility bus is filtered: the app is found by a screen reader, cannot read or command the other applications
"""
import json
import signal
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
import dbus_lab  # noqa: E402
import isolation  # noqa: E402

N = 'org.keepassxc.KeePassXC'

PROBE = r'''#!/usr/bin/python3
import json, os, sys, time
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib
mode, name = sys.argv[1], sys.argv[2]
secs = float(sys.argv[3]) if len(sys.argv) > 3 else 0.0
out = os.path.join(os.environ["XDG_DATA_HOME"], "probe-%s.json" % mode)
res = {}
def done():
    with open(out, "w") as f:
        json.dump(res, f)
bus = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "")
res["bus"] = bus
try:
    c = Gio.DBusConnection.new_for_address_sync(bus, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
except Exception as e:
    res["connect_error"] = str(e)[:100]
    done()
    sys.exit(0)
res["unique"] = c.get_unique_name()

def call(dest, path, iface, member, args=None, sig=None, timeout=3000):
    try:
        r = c.call_sync(dest, path, iface, member, GLib.Variant(sig, args) if sig else None, None, Gio.DBusCallFlags.NONE, timeout, None)
        return "ok " + str(r.unpack())[:80]
    except GLib.Error as e:
        return "error " + e.message.split(":")[-1].strip()[:60]

def request_name(n, flags):
    r = c.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName", GLib.Variant("(su)", (n, flags)), None, Gio.DBusCallFlags.NONE, 3000, None)
    return r.unpack()[0]

NODE = Gio.DBusNodeInfo.new_for_xml("""<node><interface name="org.gtk.Application"><method name="Activate"><arg type="a{sv}" direction="in"/></method></interface>
<interface name="com.example.Any"><method name="Do"/></interface></node>""")
got = []
def on_call(conn, sender, path, iface, meth, params, inv):
    got.append(iface + "." + meth)
    inv.return_value(None)
def serve(secs):
    for i in NODE.interfaces:
        c.register_object("/org/gtk/Application" if i.name == "org.gtk.Application" else "/com/example/Any", i, on_call, None, None)
    loop = GLib.MainLoop()
    GLib.timeout_add(int(secs * 1000), loop.quit)
    loop.run()

if mode == "call":
    res["calls"] = [call(name, "/", "org.freedesktop.DBus.Peer", "Ping"), call(name, "/com/example/Anything", "com.example.Secret", "GetSecret"), call(name, "/org/gtk/Application", "org.gtk.Application", "Activate", ({},), "(a{sv})")]
elif mode == "own":
    res["code"] = request_name(name, 4)
    serve(secs)
    res["received"] = got
elif mode == "gapp":
    res["code"] = request_name(name, 4)
    if res["code"] == 1:
        res["role"] = "primary"
        serve(secs)
        res["activations"] = got
    else:
        res["role"] = "remote"
        res["activate"] = call(name, "/org/gtk/Application", "org.gtk.Application", "Activate", ({},), "(a{sv})")
elif mode == "portal":
    time.sleep(secs)
    path = "/org/freedesktop/portal/desktop/request/" + res["unique"][1:].replace(".", "_") + "/tk"
    resp = []
    c.signal_subscribe("org.freedesktop.portal.Desktop", "org.freedesktop.portal.Request", "Response", path, None, Gio.DBusSignalFlags.NONE, lambda *a: resp.append(a[5].unpack()))
    r = call("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Inhibit", "Inhibit", ("", 8, {"handle_token": GLib.Variant("s", "tk"), "reason": GLib.Variant("s", "t")}), "(sua{sv})")
    res["inhibit"] = r
    ctx = GLib.MainContext.default()
    t0 = time.time()
    while time.time() - t0 < 4 and not resp:
        ctx.iteration(False)
        time.sleep(0.01)
    res["response"] = resp
    res["path_matches"] = path in r
    res["register"] = call("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.host.portal.Registry", "Register", ("org.mozilla.firefox", {}), "(sa{sv})")
elif mode == "link":
    # As GTK does it: a proxy with its properties, which resolves the portal's name and then talks to the owner's unique name.
    px = Gio.DBusProxy.new_sync(c, Gio.DBusProxyFlags.NONE, None, "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.OpenURI", None)
    v = px.get_cached_property("version")
    res["proxy_version"] = v.unpack() if v is not None else None
    res["links"] = {}
    res["props"] = [call("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.DBus.Properties", "Get", ("org.freedesktop.portal.OpenURI", "version"), "(ss)")[:60],
                    call("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.DBus.Properties", "Get", ("org.freedesktop.portal.FileChooser", "version"), "(ss)")[:60]]
    answers = {}
    c.signal_subscribe("org.freedesktop.portal.Desktop", "org.freedesktop.portal.Request", "Response", None, None, Gio.DBusSignalFlags.NONE, lambda conn, snd, path, i, sig, params: answers.update({path: params.unpack()[0]}))
    ctx = GLib.MainContext.default()
    for n, uri in enumerate(("https://example.org/a", "mailto:a@b.org", "https://example.org/fail", "mailto:a@b.org?attach=/etc/passwd", "file:///etc/passwd")):
        path = "/org/freedesktop/portal/desktop/request/" + res["unique"][1:].replace(".", "_") + "/lk%d" % n
        r = call("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.OpenURI", "OpenURI", ("", uri, {"handle_token": GLib.Variant("s", "lk%d" % n)}), "(ssa{sv})")
        t0 = time.time()
        while time.time() - t0 < 4 and path not in answers:
            ctx.iteration(False)
            time.sleep(0.01)
        res["links"][uri] = [r[:90], [answers[path]] if path in answers else []]
elif mode == "mpris":
    res["codes"] = [request_name("org.mpris.MediaPlayer2.vlc", 4), request_name("org.mpris.MediaPlayer2.other", 4)]
    serve(secs)
elif mode == "a11y":
    addr = os.environ.get("AT_SPI_BUS_ADDRESS")
    res["addr"] = addr
    if addr:
        a = Gio.DBusConnection.new_for_address_sync(addr, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
        def acall(dest, path, iface, member, args=None, sig=None):
            try:
                a.call_sync(dest, path, iface, member, GLib.Variant(sig, args) if sig else None, None, Gio.DBusCallFlags.NONE, 2000, None)
                return "ok"
            except GLib.Error as e:
                return "refused" if "AccessDenied" in e.message else "error " + e.message[:40]
        R = "org.a11y.atspi.Registry"
        res["registered_events"] = acall(R, "/org/a11y/atspi/registry", R, "GetRegisteredEvents")
        res["root_children"] = acall(R, "/org/a11y/atspi/accessible/root", "org.a11y.atspi.Accessible", "GetChildren")
        res["register_event"] = acall(R, "/org/a11y/atspi/registry", R, "RegisterEvent", ("object:state-changed", [], ""), "(sass)")
        res["key_notify"] = acall(R, "/org/a11y/atspi/registry/deviceeventcontroller", "org.a11y.atspi.DeviceEventController", "NotifyListenersSync", None, None)
        res["others"] = sorted({acall(":1.%d" % n, "/org/a11y/atspi/accessible/root", "org.a11y.atspi.Accessible", "GetChildren") for n in range(1, 60)})
done()
'''


def need(cond, msg):
    if not cond:
        print(f'FAIL: {msg}')
        sys.exit(1)


class Lab:
    def __init__(self, work, fake=True, atspi=False):
        self.work = work
        self.sess = dbus_lab.Session(ROOT / 'tests', portal=True, fake=fake)
        self.home = self.sess.home
        isolation.require_isolated_home(self.home, self.sess.home.parent)
        self.env = dict(self.sess.env)
        self.stub = BIN / 'panpak-stub'
        # A stand-in for the system's xdg-open, first in the PATH of the stub: it records the address and fails for one that contains "fail".
        fake = work / 'bin'
        fake.mkdir(exist_ok=True)
        self.opened = work / 'opened.txt'
        (fake / 'xdg-open').write_text(f'#!/bin/sh\necho "$1" >> {self.opened}\ncase "$1" in *fail*) echo "no browser" >&2; exit 3;; esac\nexit 0\n')
        (fake / 'xdg-open').chmod(0o755)
        self.env['PATH'] = f'{fake}:' + self.env.get('PATH', '/usr/bin:/bin')
        self.key = work / 'secret.key'
        out = self.cli('keygen', '-o', str(self.key)).stdout
        self.pub = next(line.split()[-1] for line in out.splitlines() if line.startswith('key:'))
        self.installed = set()
        if atspi:
            # The accessibility bus of the lab (the fake backend would own org.a11y.Bus, so this lab has none).
            self.sess._spawn(['/usr/libexec/at-spi-bus-launcher', '--launch-immediately'])
            time.sleep(1.5)

    def cli(self, *args, check=True):
        p = subprocess.run([str(BIN / 'panpak'), *map(str, args)], env=self.env, capture_output=True, text=True, timeout=120, stdin=subprocess.DEVNULL)
        if check:
            need(p.returncode == 0, f'panpak {args}: {p.stdout}{p.stderr}')
        return p

    def install(self, app_id, perms=''):
        tree = self.work / f'tree-{app_id}'
        (tree / 'bin').mkdir(parents=True, exist_ok=True)
        (tree / 'bin/probe').write_text(PROBE)
        (tree / 'bin/probe').chmod(0o755)
        (tree / 'panpak.toml').write_text(f'[app]\nid = "{app_id}"\nname = "Probe"\nversion = "1"\nentry = "bin/probe"\n{perms}\n[update]\nkey = "{self.pub}"\n')
        pkg = self.work / f'{app_id}.panpak'
        self.cli('pack', tree, '--stub', self.stub, '--secret-file', self.key, '-o', pkg)
        r = self.cli('install', '--yes', '--accept-new-permissions', '--runtime', self.stub, pkg, check=False)
        need(r.returncode == 0, f'install {app_id}: {r.stdout}{r.stderr}')
        self.installed.add(app_id)

    def start(self, app_id, *args):
        """Start the probe; return the process."""
        report = self.report_path(app_id, args[0])
        report.unlink(missing_ok=True)
        return subprocess.Popen([str(self.home / '.local/bin' / app_id), *map(str, args)], env=self.env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, stdin=subprocess.DEVNULL, start_new_session=True)

    def report_path(self, app_id, mode):
        return self.home / f'.local/share/panpak/userdata/{app_id}/data/probe-{mode}.json'

    def finish(self, p, app_id, mode, timeout=60):
        try:
            out = p.communicate(timeout=timeout)[0]
        except subprocess.TimeoutExpired:
            isolation.stop_group(p.pid, p, grace=5)
            need(False, f'{app_id} {mode} did not end')
        f = self.report_path(app_id, mode)
        need(f.exists(), f'{app_id} {mode}: no report (exit {p.returncode}): {out[-400:]}')
        return json.loads(f.read_text())

    def run(self, app_id, *args, timeout=60):
        return self.finish(self.start(app_id, *args), app_id, args[0], timeout)

    def native(self, name, log):
        p = subprocess.Popen([sys.executable, str(ROOT / 'tests/fake_native.py'), name, str(log)], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.sess.procs.append(p)
        for _ in range(100):
            if log.exists() and 'owned' in log.read_text():
                return p
            time.sleep(0.1)
        need(False, 'the fake native program did not start')

    def owner(self, name):
        rc, out = self.sess.call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus.GetNameOwner', name)
        return out if rc == 0 else None


def scenario_a(lab):
    log = lab.work / 'native-a.log'
    nat = lab.native(N, log)
    r = lab.run('org.panpaktest.Squat', 'call', N)
    need(all(c.startswith('error') for c in r['calls']), f'A: the app reached the native process: {r["calls"]}')
    need(log.read_text().split() == ['owned', N], f'A: the native process saw calls: {log.read_text()}')
    nat.terminate()
    nat.wait(timeout=10)
    print('A   an app cannot call a name that a native process owns: ok')


def scenario_b(lab):
    name = 'org.example.ClosedApp'
    p = lab.start('org.panpaktest.Squat', 'own', name, 4)
    for _ in range(100):
        if lab.owner(name) is not None or p.poll() is not None:
            break
        time.sleep(0.1)
    time.sleep(1)
    need(lab.owner(name) is None, f'B: the name is owned on the host bus: {lab.owner(name)}')
    rc, out = lab.sess.call(name, '/', 'org.freedesktop.DBus.Peer.Ping')
    need(rc != 0 and 'ServiceUnknown' in out, f'B: an outside client reached the name: {out}')
    r = lab.finish(p, 'org.panpaktest.Squat', 'own')
    need(r['code'] == 1 and r['received'] == [], f'B: {r}')
    print('B   an app owns a name on its own bus; nobody outside reaches it: ok')


def scenario_c(lab):
    log = lab.work / 'activated.log'
    services = Path(lab.env['XDG_DATA_HOME']) / 'dbus-1/services'
    services.mkdir(parents=True, exist_ok=True)
    (services / 'org.example.NativeApp.service').write_text(f'[D-BUS Service]\nName=org.example.NativeApp\nExec={sys.executable} {ROOT}/tests/fake_native.py org.example.NativeApp {log}\n')
    r = lab.run('org.panpaktest.Squat', 'call', 'org.example.NativeApp')
    time.sleep(1.5)
    need(all(c.startswith('error') for c in r['calls']), f'C: {r["calls"]}')
    need(not log.exists(), 'C: the bus started the native program for the app')
    print('C   a name the bus could activate is not started for the app: ok')


def scenario_f22_f23_f11(lab):
    gid = 'org.panpaktest.Gapp'
    lab.install(gid)
    # F11: nothing declared, the id is the app's own on its bus.
    r = lab.run(gid, 'gapp', gid, 0.5)
    need(r['role'] == 'primary', f'F11: {r}')
    print('F11 an app owns its own id without declaring it: ok')
    # F22: a native program owns the same id on the host bus; the copy is not a remote instance of it.
    log = lab.work / 'native-f22.log'
    nat = lab.native(gid, log)
    r = lab.run(gid, 'gapp', gid, 0.5)
    need(r['role'] == 'primary', f'F22: the copy became a remote instance of the native program: {r}')
    need(log.read_text().split() == ['owned', gid], f'F22: the native program was called: {log.read_text()}')
    nat.terminate()
    nat.wait(timeout=10)
    time.sleep(0.5)
    # F23: the copy is running; the native program then takes the id on the host bus, and nothing reaches the copy.
    p = lab.start(gid, 'gapp', gid, 6)
    for _ in range(100):
        if lab.report_path(gid, 'gapp').exists() or p.poll() is not None:
            break
        time.sleep(0.1)
    time.sleep(1.5)
    log2 = lab.work / 'native-f23.log'
    nat2 = lab.native(gid, log2)
    need(lab.owner(gid) is not None, 'F23: the native program does not own the id on the host bus')
    rc, out = lab.sess.call(gid, '/org/gtk/Application', 'org.gtk.Application.Activate', '{}')
    r = lab.finish(p, gid, 'gapp', 40)
    need(r['role'] == 'primary' and r['activations'] == [], f'F23: the call of the native command reached the copy: {r}')
    nat2.terminate()
    nat2.wait(timeout=10)
    print('F22 a native single-instance program and the copy do not see each other: ok')
    print('F23 a native command is not routed into a copy: ok')


def scenario_single_instance(lab):
    gid = 'org.panpaktest.Gapp'
    p1 = lab.start(gid, 'gapp', gid, 5)
    time.sleep(2.5)
    r2 = lab.run(gid, 'gapp', gid, 0)
    r1 = lab.finish(p1, gid, 'gapp')
    need(r1['role'] == 'primary' and r2['role'] == 'remote', f'S: the second launch did not find the first: {r1} {r2}')
    need(r2['activate'].startswith('ok') and r1['activations'] == ['org.gtk.Application.Activate'], f'S: the activation did not arrive once: {r1} {r2}')
    print('S   two launches share one router: the second reaches the first: ok')


def routers(lab):
    """PIDs of the router processes of this lab: the program they run is the stub installed in the lab's home folder.
    (Their descriptors cannot be read from outside: they are not dumpable.)"""
    found = []
    for d in Path('/proc').iterdir():
        if not d.name.isdigit():
            continue
        try:
            cmd = (d / 'cmdline').read_bytes().split(b'\0')
        except OSError:
            continue
        if b'--panpak-busrouter' in cmd and cmd[0].startswith(str(lab.home).encode() + b'/'):
            found.append(int(d.name))
    return found


def wait_routers(lab, n, seconds):
    end = time.time() + seconds
    while time.time() < end:
        if len(routers(lab)) == n:
            return True
        time.sleep(0.2)
    return len(routers(lab)) == n


def scenario_lifecycle(lab):
    gid = 'org.panpaktest.Squat'  # not a single-instance app: both copies stay for the given seconds
    need(wait_routers(lab, 0, 8), f'R1: a router is left from an earlier test: {routers(lab)}')
    # Two copies that start together share one router; it ends after the last one.
    p1 = lab.start(gid, 'own', 'org.example.Life1', 9)
    p2 = lab.start(gid, 'own', 'org.example.Life2', 9)
    time.sleep(3)
    need(len(routers(lab)) == 1, f'R1: two copies started together have {len(routers(lab))} routers')
    # A copy that dies badly: the other one goes on, the router stays.
    os.killpg(p1.pid, signal.SIGKILL)
    p1.wait(timeout=10)
    time.sleep(1.5)
    need(len(routers(lab)) == 1, f'R2: after a copy was killed there are {len(routers(lab))} routers')
    r3 = lab.run('org.panpaktest.Portal', 'portal', 1)
    need(r3['inhibit'].startswith('ok'), f'R2: a new copy cannot use the bus after another one was killed: {r3}')
    # The last copy ends: the router ends with it.
    lab.finish(p2, gid, 'own', timeout=60)
    need(wait_routers(lab, 0, 8), f'R3: the router stayed after the last copy ended: {routers(lab)}')
    # All copies killed: no router left either.
    p4 = lab.start(gid, 'own', 'org.example.Life3', 30)
    time.sleep(2.5)
    need(len(routers(lab)) == 1, 'R4: no router for a single copy')
    os.killpg(p4.pid, signal.SIGKILL)
    p4.wait(timeout=10)
    need(wait_routers(lab, 0, 10), f'R4: the router stayed after its only copy was killed: {routers(lab)}')
    print('R   one router for copies started together, it survives a copy killed badly, and ends with the last copy (also when it is killed): ok')


def scenario_end_race(lab):
    """A copy that starts while the router is deciding to end must get a bus. The router is told to wait 1.5 s with its lock held between the
    decision and the end, so that the new copy surely arrives inside that window (it ends 1.5 s after the last copy left)."""
    gid = 'org.panpaktest.Squat'
    need(wait_routers(lab, 0, 8), f'E: a router is left from an earlier test: {routers(lab)}')
    lab.env['PANPAK_ROUTER_END_PAUSE_MS'] = '1500'
    try:
        # `run` would wait for the end of the router too (it holds the output pipe), so the copy is waited for by itself.
        pa = lab.start(gid, 'own', 'org.example.RaceA', 1)
        pa.wait(timeout=60)
        a = json.loads(lab.report_path(gid, 'own').read_text())
        need(a['code'] == 1, f'E: the first copy got no bus: {a}')
        time.sleep(1.7)
        need(len(routers(lab)) == 1, f'E: the router was not in its window to end: {routers(lab)}')
        lab.report_path(gid, 'own').unlink()
        pb = lab.start(gid, 'own', 'org.example.RaceB', 1)
        pb.wait(timeout=60)
        need(lab.report_path(gid, 'own').exists(), 'E: the copy that came while the router ended wrote no report')
        b = json.loads(lab.report_path(gid, 'own').read_text())
        need(b.get('unique') and b['code'] == 1, f'E: the copy that came while the router ended has no bus: {b}')
    finally:
        del lab.env['PANPAK_ROUTER_END_PAUSE_MS']
    need(wait_routers(lab, 0, 10), f'E: the router stayed after the last copy: {routers(lab)}')
    print('E   a copy that starts while the router decides to end gets a bus (a new router, or the old one stays)')


def scenario_identity(lab):
    pid = 'org.panpaktest.Portal'
    lab.install(pid, '[permissions]\nnotifications = true\n')
    lab.sess.log.write_text('')
    p1 = lab.start(pid, 'portal', 1)
    time.sleep(0.8)
    p2 = lab.start(pid, 'portal', 8)
    r1 = lab.finish(p1, pid, 'portal')
    # The first copy is over; the second asks the portal now.
    r2 = lab.finish(p2, pid, 'portal')
    for r, n in ((r1, 1), (r2, 2)):
        need(r['inhibit'].startswith('ok') and r['path_matches'], f'I: copy {n}: {r}')
        need(r['response'] and r['response'][0][0] == 0, f'I: copy {n} got no answer: {r}')
        need(r['register'].startswith('error'), f'I: copy {n} could register an identity: {r["register"]}')
    log = lab.sess.log.read_text()
    need(log.count(f'Inhibit app_id=panpak.{pid}') == 2, f'I: the portal did not know both copies as panpak.{pid}: {log!r}')
    print('I   the portal knows each copy as panpak.<id>, answers reach the right copy, the app cannot register an identity (R): ok')


def scenario_links(lab):
    r = lab.run('org.panpaktest.Portal', 'link', '-')
    need(r['props'][0].startswith('ok') and '3' in r['props'][0], f"L: the version of OpenURI: {r['props'][0]}")
    need(r['proxy_version'] == 3, f"L: a proxy with properties (as GTK) read the version {r['proxy_version']}")
    need(not r['props'][1].startswith('ok'), f"L: the app read a property of the file chooser: {r['props'][1]}")
    want = {'https://example.org/a': 0, 'mailto:a@b.org': 0, 'https://example.org/fail': 2, 'mailto:a@b.org?attach=/etc/passwd': 2, 'file:///etc/passwd': 2}
    for uri, code in want.items():
        got = r['links'][uri]
        need(got[0].startswith('ok ') and '/org/freedesktop/portal/desktop/request/' in got[0], f'L: {uri}: {got}')
        need(got[1] == [code], f'L: {uri}: the app was told {got[1]}, expected {code}')
    opened = lab.opened.read_text().split() if lab.opened.exists() else []
    need(opened == ['https://example.org/a', 'mailto:a@b.org', 'https://example.org/fail'], f'L: the system opener got {opened}')
    print('L   OpenURI is answered by the router and the app is told what happened: ok')


def scenario_mpris(lab):
    mid = 'org.panpaktest.Player'
    lab.install(mid, '[permissions]\nmpris = true\n')
    alias = 'org.mpris.MediaPlayer2.panpak.org_panpaktest_Player'
    p = lab.start(mid, 'mpris', '-', 5)
    for _ in range(100):
        if lab.owner(alias) is not None or p.poll() is not None:
            break
        time.sleep(0.1)
    need(lab.owner(alias) is not None, 'M: no alias on the host bus')
    need(lab.owner('org.mpris.MediaPlayer2.vlc') is None and lab.owner('org.mpris.MediaPlayer2.other') is None, 'M: a name the app chose is on the host bus')
    r = lab.finish(p, mid, 'mpris')
    need(r['codes'] == [1, 1], f'M: {r}')
    print('M   a media player gets one alias with the id, never its own name: ok')


def scenario_a11y(lab):
    r = lab.run('org.panpaktest.Portal', 'a11y', '-')
    if not r.get('addr'):
        print('X   no accessibility bus on this machine: skipped')
        return
    need(r['registered_events'] == 'ok', f'X: {r}')
    need(r['root_children'] == 'refused' and r['register_event'] == 'refused' and r['key_notify'] == 'refused', f'X: the app could do more than it should: {r}')
    need('ok' not in r['others'], f'X: other applications could be called: {r["others"]}')
    print('X   the app finds the accessibility bus and cannot read or command other applications: ok')


def main():
    why = dbus_lab.missing()
    if why:
        print(f'skipped: {why}')
        return
    work = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT / 'target/privbus-test'
    if work.exists():
        isolation.require_inside(work, work.parent, 'work')
        subprocess.run(['chmod', '-R', 'u+w', str(work)], check=True)
        isolation.safe_rmtree(work, work.parent)
    work.mkdir(parents=True)
    (work / 'one').mkdir()
    lab = Lab(work / 'one')
    try:
        lab.install('org.panpaktest.Squat')
        lab.install('org.panpaktest.Portal', '[permissions]\nnotifications = true\n')
        smoke = lab.run('org.panpaktest.Squat', 'call', 'org.example.Nobody')
        if 'connect_error' in smoke or not smoke.get('unique'):
            print('skipped: the app got no bus here (no systemd user session for its scope, or no xdg-dbus-proxy):', smoke)
            return
        scenario_a(lab)
        scenario_b(lab)
        scenario_c(lab)
        scenario_f22_f23_f11(lab)
        scenario_single_instance(lab)
        scenario_lifecycle(lab)
        scenario_end_race(lab)
        scenario_identity(lab)
        scenario_links(lab)
        scenario_mpris(lab)
    finally:
        lab.sess.close()
    if Path('/usr/libexec/at-spi-bus-launcher').exists():
        (work / 'two').mkdir()
        lab = Lab(work / 'two', fake=False, atspi=True)
        try:
            lab.install('org.panpaktest.Portal')
            scenario_a11y(lab)
        finally:
            lab.sess.close()
    else:
        print('X   at-spi-bus-launcher is not installed: skipped')
    print('all private bus tests passed')


if __name__ == '__main__':
    main()
