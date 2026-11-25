#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The private bus of an app (step 13p), end to end, against the real stub and the real sandbox.

Usage: python3 tests/privbus.py [work-directory]
       python3 tests/privbus.py --rawlat out.csv [work-directory]   (only the round trip of a Ping, with and without the layer; see docs/PRIVATE-BUS.md)
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
  D    the file dialog: open, close, save and a refused link, through the real portal and its document folder
  X    the accessibility bus is filtered: the app is found by a screen reader, cannot read or command the other applications
"""
import json
import signal
import os
from pathlib import Path
import shutil
import tempfile
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
elif mode == "dialog":
    # The file dialog as a toolkit asks for it: OpenFile or SaveFile with a handle token, the answer as a Response signal, then the file through the address given.
    # name is the method, secs unused; the host path the dialog was told to choose is in argv[4], and the text to write for a save in argv[5].
    method, host = name, sys.argv[4][2:]
    px = Gio.DBusProxy.new_sync(c, Gio.DBusProxyFlags.NONE, None, "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.FileChooser", None)
    v = px.get_cached_property("version")
    res["version"] = v.unpack() if v is not None else None
    answers = []
    path = "/org/freedesktop/portal/desktop/request/" + res["unique"][1:].replace(".", "_") + "/dlg"
    c.signal_subscribe("org.freedesktop.portal.Desktop", "org.freedesktop.portal.Request", "Response", path, None, Gio.DBusSignalFlags.NONE, lambda *a: answers.append(a[5].unpack()))
    opts = {"handle_token": GLib.Variant("s", "dlg"), "modal": GLib.Variant("b", True)}
    r = call("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.FileChooser", method, ("x11:1f", "Pick", opts), "(ssa{sv})", timeout=20000)
    res["call"] = r[:120]
    ctx = GLib.MainContext.default()
    t0 = time.time()
    while time.time() - t0 < 20 and not answers:
        ctx.iteration(False)
        time.sleep(0.01)
    res["answers"] = [[a[0], sorted(a[1].keys())] for a in answers]
    res["host_visible"] = os.path.exists(host)
    if answers and answers[0][0] == 0:
        uri = answers[0][1]["uris"][0]
        res["uri"] = uri
        local = uri[len("file://"):]
        res["in_doc_folder"] = local.startswith(os.environ["XDG_RUNTIME_DIR"] + "/doc/")
        try:
            if method == "SaveFile":
                with open(local, "w") as f:
                    f.write(sys.argv[5])
                res["wrote"] = True
            else:
                res["content"] = open(local).read()[:200]
        except OSError as e:
            res["io_error"] = str(e)[:100]
        try:
            os.rename(local, local + ".moved")
            res["rename_error"] = "none"
            os.rename(local + ".moved", local)
        except OSError as e:
            res["rename_error"] = str(e)[:60]
elif mode == "docedit":
    # What an editor does with a document it was given: read it, save over it the way GLib does (a temporary file in the folder of the document, then a rename over the
    # document), and look at the files next to it. argv: the address of the document, the new text, the name of a sibling, the path of that sibling on the host.
    local, text, sibling, host_sibling = sys.argv[4], sys.argv[5], sys.argv[6], sys.argv[7][2:]  # "h:" keeps the stub from taking it for a file to share
    if local.startswith("h:"):
        local = local[2:]
    here = os.path.dirname(local)
    def attempt(f):
        try:
            return "ok:" + f()
        except OSError as e:
            return "error:" + (e.strerror or str(e))[:60]
    def read(p):
        with open(p) as f:
            return f.read()[:200]
    res["before"] = attempt(lambda: read(local))
    def replace():
        tmp = os.path.join(here, ".goutputstream-probe")
        with open(tmp, "w") as f:
            f.write(text)
            f.flush()
            os.fsync(f.fileno())
        os.rename(tmp, local)
        return "replaced"
    res["replace"] = attempt(replace)
    res["after"] = attempt(lambda: read(local))
    res["sibling_in_folder"] = attempt(lambda: read(os.path.join(here, sibling)))
    res["sibling_on_host"] = attempt(lambda: read(host_sibling))
    res["listing"] = attempt(lambda: ",".join(sorted(os.listdir(here))))
    res["create_next_to_it"] = attempt(lambda: open(os.path.join(here, "other-new.txt"), "w").write("x") and "created")
elif mode == "args":
    # Files given at the start: the stub shares them with the portal and puts the document paths in their place.
    args = sys.argv[4:]
    res["argv"] = args
    try:
        res["first"] = open(args[0]).read() if args else None
        if len(args) > 1:
            with open(args[1], "w") as f:
                f.write("written through the document\n")
            res["wrote"] = True
    except OSError as e:
        res["io_error"] = str(e)[:100]
elif mode == "lat":
    # Round trips: Settings.ReadOne at the portal (through the router and the proxy) and Peer.Ping to a name of the app itself (answered by the router).
    request_name(name, 4)
    lat = {}
    host = ("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Settings", "ReadOne", GLib.Variant("(ss)", ("org.freedesktop.appearance", "color-scheme")))
    own = (name, "/", "org.freedesktop.DBus.Peer", "Ping", None)
    for label, (dest, path, iface, member, args) in (("host", host), ("own", own)):
        for _ in range(200):
            c.call_sync(dest, path, iface, member, args, None, Gio.DBusCallFlags.NONE, 3000, None)
        t0 = time.perf_counter()
        n = 2000
        for _ in range(n):
            c.call_sync(dest, path, iface, member, args, None, Gio.DBusCallFlags.NONE, 3000, None)
        lat[label] = round((time.perf_counter() - t0) / n * 1e6, 1)
    res["lat_us"] = lat
elif mode == "rawlat":
    # 1000 pings to the bus daemon over a raw socket, so that no client library is measured (docs/PRIVATE-BUS.md, section Measures, says how to run it without the layer).
    import socket, struct, statistics
    def msg(serial, member, iface="org.freedesktop.DBus.Peer", path="/org/freedesktop/DBus", dest="org.freedesktop.DBus"):
        f = b""
        def fld(code, sig, val):
            nonlocal f
            while len(f) % 8: f += b"\0"
            f += bytes([code, 1]) + sig.encode() + b"\0"
            while len(f) % 4: f += b"\0"
            f += struct.pack("<I", len(val)) + val.encode() + b"\0"
        fld(1, "o", path); fld(2, "s", iface); fld(3, "s", member); fld(6, "s", dest)
        h = struct.pack("<BBBBIII", ord("l"), 1, 0, 1, 0, serial, len(f)) + f
        while len(h) % 8: h += b"\0"
        return h
    time.sleep(secs)
    sk = socket.socket(socket.AF_UNIX); sk.connect(os.environ["XDG_RUNTIME_DIR"] + "/bus")
    sk.sendall(b"\0AUTH EXTERNAL " + str(os.getuid()).encode().hex().encode() + b"\r\nNEGOTIATE_UNIX_FD\r\nBEGIN\r\n" + msg(1, "Hello", "org.freedesktop.DBus", "/org/freedesktop/DBus"))
    buf = b""
    def read_msg():
        global buf
        while True:
            if b"AGREE_UNIX_FD\r\n" in buf: buf = buf.split(b"AGREE_UNIX_FD\r\n", 1)[1]
            if len(buf) >= 16 and buf[:1] == b"l":
                n_ = 16 + (-(-struct.unpack_from("<I", buf, 12)[0] // 8) * 8) + struct.unpack_from("<I", buf, 4)[0]
                if len(buf) >= n_:
                    m = buf[:n_]; buf = buf[n_:]; return m
            buf += sk.recv(65536)
    read_msg()
    ts = []
    for i in range(1100):
        t = time.perf_counter(); sk.sendall(msg(10 + i, "Ping"))
        while True:
            m = read_msg()
            if m[1] in (2, 3): break
        ts.append((time.perf_counter() - t) * 1e6)
    res["median_us"] = round(statistics.median(ts[100:]), 1)
    res["p99_us"] = round(sorted(ts[100:])[int(1000 * 0.99)], 1)
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
        r = self.cli('install', '--yes', '--trust-first-key', '--accept-new-permissions', '--runtime', self.stub, pkg, check=False)
        need(r.returncode == 0, f'install {app_id}: {r.stdout}{r.stderr}')
        self.installed.add(app_id)

    def start(self, app_id, *args):
        """Start the probe; return the process."""
        report = self.report_path(app_id, args[0])
        report.unlink(missing_ok=True)
        return subprocess.Popen([str(self.home / '.local/bin' / app_id), *map(str, args)], env=self.env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, stdin=subprocess.DEVNULL, start_new_session=True)

    def report_path(self, app_id, mode):
        return self.home / f'.local/share/panpak/userdata/{app_id}/.local/share/probe-{mode}.json'

    def finish(self, p, app_id, mode, timeout=60):
        try:
            out = p.communicate(timeout=timeout)[0]
        except subprocess.TimeoutExpired:
            isolation.stop_group(p.pid, p, grace=5)
            need(False, f'{app_id} {mode} did not end')
        self.last_out = out
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
    p1 = lab.start(pid, 'portal', '-', 1)
    time.sleep(0.8)
    p2 = lab.start(pid, 'portal', '-', 8)
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
    # The file chooser is served (version 4) when the document portal is there, and left to the host (which refuses it) when it is not.
    served = (Path(lab.sess.rt) / 'doc/by-app').is_dir()
    need(r['props'][1].startswith('ok') == served, f"L: the app read a property of the file chooser: {r['props'][1]} (document portal {served})")
    want = {'https://example.org/a': 0, 'mailto:a@b.org': 0, 'https://example.org/fail': 2, 'mailto:a@b.org?attach=/etc/passwd': 2, 'file:///etc/passwd': 2}
    for uri, code in want.items():
        got = r['links'][uri]
        need(got[0].startswith('ok ') and '/org/freedesktop/portal/desktop/request/' in got[0], f'L: {uri}: {got}')
        need(got[1] == [code], f'L: {uri}: the app was told {got[1]}, expected {code}')
    opened = lab.opened.read_text().split() if lab.opened.exists() else []
    need(opened == ['https://example.org/a', 'mailto:a@b.org', 'https://example.org/fail'], f'L: the system opener got {opened}')
    print('L   OpenURI is answered by the router and the app is told what happened: ok')


def scenario_protected_links(lab):
    """P: an app in protected mode (a floor above its revision) asks OpenURI through the router: nothing is opened, the app is told it failed (as for any
    address the system could not open), and the person is told in one line (a program of the terminal: the standard error)."""
    app = 'org.panpaktest.ProtLinks'
    lab.install(app, '[permissions]\nnotifications = true\n')
    isolation.set_floor(lab.home / '.local/share/panpak/apps' / app, 2)
    lab.opened.unlink(missing_ok=True)
    r = lab.run(app, 'link', '-')
    for uri in ('https://example.org/a', 'mailto:a@b.org', 'https://example.org/fail'):
        got = r['links'][uri]
        need(got[0].startswith('ok ') and got[1] == [2], f'P: {uri}: the app was told {got}, expected the answer 2 (it was not opened)')
    opened = lab.opened.read_text().split() if lab.opened.exists() else []
    need(opened == [], f'P: the system opener got {opened} in protected mode')
    need('is in protected mode: links do not open until you update' in lab.last_out, f'P: the person was not told: {lab.last_out[-400:]}')
    print('P   in protected mode OpenURI through the router opens nothing, the app is told, and so is the person: ok')


docshare_link_prefix = 'the file you chose is a link to '


def scenario_dialog(lab):
    """D: the file dialog of the portal, from the app to the files: the real portal front end, the real document portal, a scripted backend."""
    rt = Path(lab.sess.rt)
    lab.sess.call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus.StartServiceByName', 'org.freedesktop.portal.Documents', '0')
    for _ in range(100):
        if (rt / 'doc/by-app').is_dir():
            break
        time.sleep(0.1)
    if not (rt / 'doc/by-app').is_dir():
        return print('D   the document portal has no FUSE folder here: skipped')
    app = 'org.panpaktest.Files'
    lab.install(app)
    docs = lab.home / 'Docs'
    docs.mkdir(exist_ok=True)
    (docs / 'a.txt').write_text('hello from the host\n')
    answer = Path(str(lab.sess.log) + '.chooser')
    uri = lambda p: 'file://' + str(p)
    # Open: the app gets an address in the document folder, reads the file through it, and does not see the path of the host.
    answer.write_text(uri(docs / 'a.txt') + '\n')
    r = lab.run(app, 'dialog', 'OpenFile', 0, 'h:' + str(docs / 'a.txt'), '')
    need(r.get('version') == 4, f"D: the version of FileChooser: {r}")
    need(r['answers'] and r['answers'][0][0] == 0 and 'uris' in r['answers'][0][1], f'D: the answer: {r}; the runtime said: {lab.last_out[-600:]}')
    need(r.get('in_doc_folder') and r.get('content') == 'hello from the host\n', f'D: the app read the file through the document folder: {r}')
    need(not r['host_visible'], f'D: the path of the host is visible to the app: {r}')
    log = lab.sess.log.read_text()
    need(f'OpenFile app_id=panpak.{app} parent=x11:1f' in log, f'D: what the backend was asked: {log[-400:]}')
    # The user closes the dialog.
    answer.write_text('')
    r = lab.run(app, 'dialog', 'OpenFile', 0, 'h:' + str(docs / 'a.txt'), '')
    need(r['answers'] == [[1, ['uris']]], f"D: a closed dialog: {r['answers']}")  # code 1 and `uris` = []: what GTK survives
    # Save: a name that does not exist yet; the file appears on the host when the app writes.
    answer.write_text(uri(docs / 'new.txt') + '\n')
    r = lab.run(app, 'dialog', 'SaveFile', 0, 'h:' + str(docs / 'new.txt'), 'saved by the app\n')
    print('D   atomic save probe (rename in the document folder):', r.get('rename_error'), '| io:', r.get('io_error'))
    need(r.get('wrote') and (docs / 'new.txt').exists() and (docs / 'new.txt').read_text() == 'saved by the app\n', f'D: the save: {r}, host file: {(docs / "new.txt").exists()}; the runtime said: {lab.last_out[-600:]}')
    # A link chosen in the dialog, in a folder the app cannot write, to an ordinary file: the user chose it, the app gets the file it points to, under its own name.
    (docs / 'ln.txt').symlink_to(docs / 'a.txt')
    answer.write_text(uri(docs / 'ln.txt') + '\n')
    r = lab.run(app, 'dialog', 'OpenFile', 0, 'h:' + str(docs / 'a.txt'), '')
    need(r['answers'] and r['answers'][0][0] == 0 and r.get('content') == 'hello from the host\n', f'D: a link to a file: {r}; the runtime said: {lab.last_out[-400:]}')
    need(r.get('uri', '').endswith('/a.txt'), f'D: the app got the name of the link and not of the file it points to: {r.get("uri")}')
    # A link to a hidden place of the home folder (here a settings file): refused, the app is told "cancelled" (1), the message names the place it points to.
    (lab.home / '.hidden').mkdir(exist_ok=True)
    (lab.home / '.hidden' / 'secret.txt').write_text('secret\n')
    (docs / 'ln-hidden.txt').symlink_to(lab.home / '.hidden' / 'secret.txt')
    answer.write_text(uri(docs / 'ln-hidden.txt') + '\n')
    r = lab.run(app, 'dialog', 'OpenFile', 0, 'h:' + str(docs / 'a.txt'), '')
    need(r['answers'] == [[1, ['uris']]], f'D: a link to a hidden place: {r["answers"]}')
    need(docshare_link_prefix in lab.last_out and 'hidden place' in lab.last_out, f'D: the refusal said nothing: {lab.last_out[-400:]}')
    # Files given at the start (Open with, a double click, the command line): shared before the app runs; a link is followed in the same way, one to a hidden place is
    # dropped with the message on stderr.
    r = lab.run(app, 'args', '-', 0, docs / 'a.txt', docs / 'started.txt', docs / 'ln.txt', docs / 'ln-hidden.txt')
    doc = str(rt / 'doc') + '/'
    need(len(r['argv']) == 3 and all(a.startswith(doc) for a in r['argv']), f"D: the arguments the app got: {r['argv']}")
    need(r.get('first') == 'hello from the host\n' and r.get('wrote') and (docs / 'started.txt').read_text() == 'written through the document\n', f'D: files given at the start: {r}')
    need(docshare_link_prefix in lab.last_out and 'hidden place' in lab.last_out, f'D: the link was dropped without a word: {lab.last_out[-300:]}')
    # An app that can write the folder (a `rw` home path): what it chooses there is given as the real address, no document is made, and the app writes through that address.
    app2 = 'org.panpaktest.Files2'
    lab.install(app2, '[[permissions.home_paths]]\npath = "Docs"\naccess = "rw"\n')
    answer.write_text(uri(docs / 'a.txt') + '\n')
    r = lab.run(app2, 'dialog', 'OpenFile', 0, 'h:' + str(docs / 'a.txt'), '')
    real = os.path.realpath(docs / 'a.txt')
    need(r['answers'] and r['answers'][0][0] == 0 and r.get('uri') == 'file://' + real, f'D: an app that can write the folder got {r.get("uri")}, expected the real address: {r}; {lab.last_out[-400:]}')
    need(r.get('content') == 'hello from the host\n' and not r.get('in_doc_folder'), f'D: the app could not read the real path: {r}')
    held = lab.sess.call('org.freedesktop.portal.Documents', '/org/freedesktop/portal/documents', 'org.freedesktop.portal.Documents.List', f'panpak.{app2}')[1]
    need(held.endswith('{},)'), f'D: a document was made for something the app can write: {held}')
    answer.write_text(uri(docs / 'saved-by-app2.txt') + '\n')
    r = lab.run(app2, 'dialog', 'SaveFile', 0, 'h:' + str(docs / 'saved-by-app2.txt'), 'real path save\n')
    need(r.get('wrote') and (docs / 'saved-by-app2.txt').read_text() == 'real path save\n' and r.get('uri') == 'file://' + os.path.realpath(docs) + '/saved-by-app2.txt', f'D: the save of an app that can write the folder: {r}; {lab.last_out[-300:]}')
    # The launcher starts an app with ONE existing file as `run <app> --panpak-open <file> <file>` (the older way of giving one file). With the document portal served the file is
    # shared as a document with the other arguments, the host path gets no rule of its own and nothing is said about one.
    store = lab.home / '.local/share/panpak'
    report = lab.report_path(app, 'args')
    report.unlink(missing_ok=True)
    p = subprocess.run([str(store / 'bin/panpak-stub'), 'run', str(store / 'apps' / app / 'current'), '--panpak-open', str(docs / 'a.txt'), 'args', '-', '0', str(docs / 'a.txt')],
                       env=lab.env, capture_output=True, text=True, timeout=60, stdin=subprocess.DEVNULL)
    need(report.exists(), f'D: the start with one file left no report: {p.stdout[-300:]}{p.stderr[-300:]}')
    argv = json.loads(report.read_text())['argv']
    need(len(argv) == 1 and argv[0].startswith(doc) and argv[0].endswith('/a.txt'), f'D: the argument of a start with one file: {argv}')
    need('may read' not in p.stderr and 'read access' not in p.stderr, f'D: the old rule of the host path was added and announced: {p.stderr[-300:]}')
    # With the portal, `--write-rename` is not used (the portal gives the file with the user's rights and lets the app save by replacing it): the stub says so, and the
    # folder of the file is not opened (no warning of a grant on the folder).
    p = subprocess.run([str(store / 'bin/panpak-stub'), 'run', str(store / 'apps' / app / 'current'), '--panpak-open', str(docs / 'a.txt'), '--panpak-open-rename', 'args', '-', '0', str(docs / 'a.txt')],
                       env=lab.env, capture_output=True, text=True, timeout=60, stdin=subprocess.DEVNULL)
    need('--write-rename is not used' in p.stderr and 'folder is not opened' in p.stderr and 'WARNING: WRITE access' not in p.stderr, f'D: the flag of the folder with the portal: {p.stderr[-400:]}')
    # Removing the app takes the grants back and leaves the user's files alone.
    listing = lambda: lab.sess.call('org.freedesktop.portal.Documents', '/org/freedesktop/portal/documents', 'org.freedesktop.portal.Documents.List', f'panpak.{app}')[1]
    need(not listing().endswith('{},)'), f'D: the portal holds nothing for the app before the removal: {listing()}')
    # The menu action "Uninstall" is started by the shell in a scope called `app-gnome-panpak.<id>-N.scope`. From 1.19 or so the portals take that name as the identity of the caller (when the
    # menu entry exists), and the document portal refuses an app to revoke its own documents. First see whether the portal of this lab does that (a revoke from such a scope); then the
    # removal started from the same kind of scope must still take the grants back (it leaves the scope of the app: `remove.rs`, `leave_app_scope`).
    import re
    some = re.search(r"['\"]([A-Za-z0-9_-]{8,})['\"]", listing())
    def in_scope(unit, *argv):
        """Run argv inside the transient scope `unit` (what the shell makes for a menu action): the process waits for a line, is moved into the scope by the user's systemd
        (asked on $XDG_RUNTIME_DIR/bus, as the runtime does; `systemd-run` does not find it there), then runs argv."""
        p = subprocess.Popen(['sh', '-c', 'read _; exec "$@"', 'sh', *argv], env=lab.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        m = subprocess.run(['gdbus', 'call', '--address', f'unix:path={lab.sess.rt}/bus', '--dest', 'org.freedesktop.systemd1', '--object-path', '/org/freedesktop/systemd1',
                            '--method', 'org.freedesktop.systemd1.Manager.StartTransientUnit', unit, 'fail', f"[('Description', <'panpak test'>), ('PIDs', <[uint32 {p.pid}]>)]", '[]'],
                           capture_output=True, text=True, timeout=30)
        if m.returncode:
            p.kill()
            need(False, f'D: the user manager did not make the scope {unit}: {m.stderr[-200:]}')
        time.sleep(0.5)
        out, err = p.communicate('\n', timeout=120)
        return subprocess.CompletedProcess(argv, p.returncode, out, err)

    if some and shutil.which('gdbus'):
        t = in_scope(f'app-gnome-panpak.{app}-{os.getpid()}.scope', 'gdbus', 'call', '--session', '--dest', 'org.freedesktop.portal.Documents', '--object-path', '/org/freedesktop/portal/documents',
                     '--method', 'org.freedesktop.portal.Documents.RevokePermissions', some.group(1), f'panpak.{app}', "['read', 'write', 'grant-permissions', 'delete']")
        print('D   a revoke from the scope of the app:', 'refused (the portal takes the identity from the scope name)' if 'permission' in (t.stdout + t.stderr).lower() else 'allowed (this portal does not use the name of the scope)')
    if shutil.which('gdbus'):
        r = in_scope(f'app-gnome-panpak.{app}-{os.getpid() + 1}.scope', str(BIN / 'panpak'), 'remove', app)
        out = r.stdout
        need(r.returncode == 0, f'D: remove from the scope of the app failed: {r.stdout[-300:]}{r.stderr[-300:]}')
    else:
        out = lab.cli('remove', app).stdout
    need('file grants of the document portal' in out, f'D: remove says nothing about the grants: {out}')
    need(listing().endswith('{},)'), f'D: the grants are still there after the removal: {listing()}')
    need((docs / 'a.txt').read_text() == 'hello from the host\n' and (docs / 'new.txt').exists(), 'D: the files of the user were touched by the removal')
    print('D   the file dialog: open, close, save and a refused link, through the real portal and the document folder (a link to an ordinary file is followed, one to a hidden place is refused); remove takes the grants back: ok')


def scenario_editor(lab):
    """T: a text editor with no folder permission, through the document portal: open, change, save, save as, close and open it again from its recent files; the
    second document in the same folder stays out of reach."""
    rt = Path(lab.sess.rt)
    lab.sess.call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus.StartServiceByName', 'org.freedesktop.portal.Documents', '0')
    for _ in range(100):
        if (rt / 'doc/by-app').is_dir():
            break
        time.sleep(0.1)
    if not (rt / 'doc/by-app').is_dir():
        return print('T   the document portal has no FUSE folder here: skipped')
    app = 'org.panpaktest.Editor'
    lab.install(app)  # no xdg_dirs, no home_paths: the dialog is the only way to a file
    docs = lab.home / 'EditorDocs'
    docs.mkdir(exist_ok=True)
    (docs / 'a.txt').write_text('first document\n')
    (docs / 'b.txt').write_text('second document, not chosen\n')
    answer = Path(str(lab.sess.log) + '.chooser')
    uri = lambda p: 'file://' + str(p)
    unreachable = lambda v: str(v).startswith('error:')
    def edit(path, text):
        # The host path goes with a prefix: an argument that is an existing file would be shared by the stub (files given at the start).
        return lab.run(app, 'docedit', '-', 0, path, text, 'b.txt', 'h:' + str(docs / 'b.txt'))
    # Open a.txt with the dialog.
    answer.write_text(uri(docs / 'a.txt') + '\n')
    r = lab.run(app, 'dialog', 'OpenFile', 0, 'h:' + str(docs / 'a.txt'), '')
    need(r['answers'] and r['answers'][0][0] == 0 and r.get('in_doc_folder') and r.get('content') == 'first document\n', f'T: open: {r}; {lab.last_out[-300:]}')
    a_doc = r['uri'][len('file://'):]
    # Change and save over it (a temporary file and a rename in the folder of the document, as GLib does).
    r = edit(a_doc, 'first document, edited\n')
    need(r['before'] == 'ok:first document\n' and r['replace'] == 'ok:replaced' and r['after'] == 'ok:first document, edited\n', f'T: save over the document: {r}')
    need((docs / 'a.txt').read_text() == 'first document, edited\n', 'T: the file on the host after the save')
    # The second document in the same folder is out of reach: by the address of the document, by its path on the host, in the listing, and nothing can be made next to it.
    need(unreachable(r['sibling_in_folder']) and unreachable(r['sibling_on_host']), f'T: the second document is reachable: {r}')
    need(r['listing'] == 'ok:a.txt', f'T: the folder of the document lists more than the document: {r["listing"]}')
    need(unreachable(r['create_next_to_it']) or not (docs / 'other-new.txt').exists(), f'T: a file was made next to the document: {r["create_next_to_it"]}')
    need((docs / 'b.txt').read_text() == 'second document, not chosen\n' and not (docs / 'other-new.txt').exists(), 'T: the second document was touched')
    # Save as: a new name, chosen in the dialog, in the same folder.
    answer.write_text(uri(docs / 'c.txt') + '\n')
    r = lab.run(app, 'dialog', 'SaveFile', 0, 'h:' + str(docs / 'c.txt'), 'saved as\n')
    need(r.get('wrote') and (docs / 'c.txt').read_text() == 'saved as\n' and r.get('in_doc_folder'), f'T: save as: {r}; {lab.last_out[-300:]}')
    c_doc = r['uri'][len('file://'):]
    r = edit(c_doc, 'saved as, edited\n')
    need(r['replace'] == 'ok:replaced' and (docs / 'c.txt').read_text() == 'saved as, edited\n', f'T: save over the new file: {r}')
    need(unreachable(r['sibling_in_folder']) and unreachable(r['sibling_on_host']) and r['listing'] == 'ok:c.txt', f'T: next to the new file: {r}')
    # Close and open again from the recent files of the app (a new process with no dialog): the addresses it kept still work, and still only for these files.
    for doc, host, before, after in ((a_doc, 'a.txt', 'first document, edited\n', 'first document, edited again\n'), (c_doc, 'c.txt', 'saved as, edited\n', 'saved as, edited again\n')):
        r = edit(doc, after)
        need(r['before'] == 'ok:' + before, f'T: reopened {host} from its recent files: {r}; the runtime said: {lab.last_out[-300:]}')
        need(r['replace'] == 'ok:replaced' and (docs / host).read_text() == after, f'T: save after the reopen of {host}: {r}')
        need(unreachable(r['sibling_in_folder']) and unreachable(r['sibling_on_host']), f'T: the second document after the reopen of {host}: {r}')
    # The control that shows the test can tell: an app that was given the folder (what the old recipe did with `xdg_dirs = documents`) reads the second document and makes files.
    app2 = 'org.panpaktest.Editor2'
    lab.install(app2, '[[permissions.home_paths]]\npath = "EditorDocs"\naccess = "rw"\n')
    r = lab.run(app2, 'docedit', '-', 0, 'h:' + str(docs / 'a.txt'), 'control\n', 'b.txt', 'h:' + str(docs / 'b.txt'))
    need(r['sibling_in_folder'] == 'ok:second document, not chosen\n' and r['sibling_on_host'].startswith('ok:'), f'T: the control (an app that has the folder) does not reach the second document: {r}')
    (docs / 'a.txt').write_text('first document, edited again\n')
    (docs / 'other-new.txt').unlink(missing_ok=True)
    print('T   an editor with no folder permission: open, change and save over, save as, and open again from its recent files, through the document portal; the second document in the folder stays out of reach: ok')


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


def skip(why):
    """The lab cannot run here: a message and success, unless the job requires it (PANPAK_REQUIRE_LAB=1)."""
    print(f'skipped: {why}')
    if dbus_lab.required():
        print('FAIL: the lab is required here (PANPAK_REQUIRE_LAB=1)')
        sys.exit(1)


def bench_rawlat(lab, csv_path, rounds=3):
    """The round trip of a `Ping` to the bus daemon of the lab over a raw socket, 1000 counted pings per run: with no layer (the probe outside, on the bus
    of the lab) and through the layer (the probe in the sandbox of an app), alternating, `rounds` times, in this one session. Writes a CSV."""
    import platform
    probe = lab.work / 'rawlat-probe.py'
    probe.write_text(PROBE)
    nolayer_rt = Path(tempfile.mkdtemp(prefix='rl-', dir=f'/run/user/{os.getuid()}'))
    try:
        (nolayer_rt / 'bus').symlink_to(lab.sess.addr[len('unix:path='):])
        data = lab.work / 'rawlat-data'
        data.mkdir()
        rows = []
        for rnd in range(1, rounds + 1):
            env = dict(lab.env, XDG_RUNTIME_DIR=str(nolayer_rt), XDG_DATA_HOME=str(data))
            subprocess.run([sys.executable, str(probe), 'rawlat', 'x', '0'], env=env, check=True, timeout=120, stdin=subprocess.DEVNULL)
            r = json.loads((data / 'probe-rawlat.json').read_text())
            rows.append((rnd, 'no layer', r['median_us'], r['p99_us']))
            r = lab.run('org.panpaktest.Squat', 'rawlat', 'x', 0)
            rows.append((rnd, 'through the layer (product)', r['median_us'], r['p99_us']))
    finally:
        subprocess.run(['chmod', '-R', 'u+rwX', str(nolayer_rt)], capture_output=True)
        isolation.safe_rmtree(nolayer_rt, nolayer_rt.parent)
    cpu = next((l.split(':', 1)[1].strip() for l in open('/proc/cpuinfo') if l.startswith('model name')), '?')
    meta = f'{time.strftime("%Y-%m-%d")},{cpu} / {platform.release()} / {os.cpu_count()} threads'
    cmd = 'python3 tests/privbus.py --rawlat docs/bench-data/privbus-rawlat.csv'
    # The build the product figures belong to: the commit as `git describe` says it now ("-dirty" if the tree has changes), and the hash of the two binaries.
    # Build the release from that commit just before the series, so that the two agree.
    describe = subprocess.run(['git', 'describe', '--always', '--dirty'], cwd=ROOT, capture_output=True, text=True).stdout.strip() or '?'
    code_commit = subprocess.run(['git', 'log', '-1', '--format=%h', '--', 'panpak', 'panpak-core', 'panpak-stub', 'Cargo.toml', 'Cargo.lock'], cwd=ROOT, capture_output=True, text=True).stdout.strip() or '?'
    import hashlib
    sums = ' '.join(hashlib.sha256((BIN / b).read_bytes()).hexdigest()[:12] for b in ('panpak', 'panpak-stub'))
    with open(csv_path, 'w') as f:
        f.write('date,machine,command,build,last_code_commit,binary_sha256_panpak_and_stub,round,configuration,median_us,p99_us\n')
        for rnd, cfg, med, p99 in rows:
            f.write(f'{meta},"{cmd}",{describe},{code_commit},{sums},{rnd},{cfg},{med},{p99}\n')
    for rnd, cfg, med, p99 in rows:
        print(f'round {rnd}  {cfg:30s} median {med:6.1f} us  p99 {p99:6.1f} us')


def main():
    # `--only A,B,C,F` runs those scenarios: A, B, C, F (F11, F22 and F23), S, R (life cycle), E, I, L, M, X. Default: all.
    args = sys.argv[1:]
    only = None
    if args[:1] == ['--only']:
        only = set(args[1].split(','))
        args = args[2:]
    want = lambda k: only is None or k in only
    rawlat_csv = None
    if args[:1] == ['--rawlat']:
        rawlat_csv, args = Path(args[1]).resolve(), args[2:]
    diagnose = args[:1] == ['--diagnose']
    if diagnose:
        args = args[1:]
    why = dbus_lab.missing()
    if why:
        return skip(why)
    work = Path(args[0]).resolve() if args else ROOT / 'target/privbus-test'
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
        if diagnose:
            # What a start of the portal probe prints with the debug output of the runtime and the router (for a machine that cannot be watched).
            lab.env['PANPAK_DEBUG'] = '1'
            p = lab.start('org.panpaktest.Portal', 'portal', '-', 0)
            print(p.communicate(timeout=90)[0][-6000:])
            f = lab.report_path('org.panpaktest.Portal', 'portal')
            print('report:', f.read_text() if f.exists() else 'none')
            return
        if rawlat_csv:
            lab.run('org.panpaktest.Squat', 'rawlat', 'x', 0)  # a first start, so the caches of the runtime are warm for the series
            return bench_rawlat(lab, rawlat_csv)
        smoke = lab.run('org.panpaktest.Squat', 'call', 'org.example.Nobody')
        if 'connect_error' in smoke or not smoke.get('unique'):
            # Say why: the start again with the debug output of the runtime.
            lab.env['PANPAK_DEBUG'] = '1'
            p = lab.start('org.panpaktest.Squat', 'call', 'org.example.Nobody')
            print('--- the start again, with PANPAK_DEBUG=1:')
            print(p.communicate(timeout=60)[0][-3000:])
            return skip(f'the app got no bus here (no systemd user session for its scope, or no xdg-dbus-proxy): {smoke}')
        for key, fn in (('A', scenario_a), ('B', scenario_b), ('C', scenario_c), ('F', scenario_f22_f23_f11), ('S', scenario_single_instance),
                        ('R', scenario_lifecycle), ('E', scenario_end_race), ('I', scenario_identity), ('L', scenario_links), ('P', scenario_protected_links), ('D', scenario_dialog), ('T', scenario_editor), ('M', scenario_mpris)):
            if want(key):
                fn(lab)
    finally:
        lab.sess.close()
    if want('X'):
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
    print('all private bus tests passed' if only is None else f'private bus tests passed: {",".join(sorted(only))}')


if __name__ == '__main__':
    main()
