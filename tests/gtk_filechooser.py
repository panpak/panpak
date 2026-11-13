#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A real GTK 4 program against the answers of the file chooser that the router gives (docs/FILE-CHOOSER.md section 5).

Usage: python3 tests/gtk_filechooser.py [work-directory]
Needs `sway`, `dbus-daemon` and python3-gi with the GTK 4 typelib; prints SKIP and exits 0 when one is missing.

`Gtk.FileDialog.save()` is asked in a headless Sway (so the program has a window and gets a parent handle, as on a desktop), with a fake
`org.freedesktop.portal.Desktop` that answers the dialog by hand. What the router sends when a dialog gives no file is the answer "cancelled" (1) with
`uris` = an empty list (route.rs, `choose_done`; its unit tests check the bytes), so that is what is tried here:
  - code 1 and code 2 with `uris` = []   -> the program is told the dialog was dismissed and ends normally;
  - code 0 with one address               -> the program gets that file.
GTK 4.18 (GNOME on the VM) and 4.22 crash on a Save whose answer has no `uris` key, with any code (found on the VM: SIGSEGV in `g_vfs_get_file_for_uri`
called from GTK): that answer is tried as a control, and only reported, because other versions of GTK may not crash on it.
"""
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402

PORTAL = r'''
import sys, gi
gi.require_version('Gio', '2.0')
from gi.repository import Gio, GLib
code, mode = int(sys.argv[1]), sys.argv[2]
XML = """<node><interface name='org.freedesktop.portal.FileChooser'>
<method name='OpenFile'><arg type='s' direction='in'/><arg type='s' direction='in'/><arg type='a{sv}' direction='in'/><arg type='o' direction='out'/></method>
<method name='SaveFile'><arg type='s' direction='in'/><arg type='s' direction='in'/><arg type='a{sv}' direction='in'/><arg type='o' direction='out'/></method>
<property name='version' type='u' access='read'/></interface></node>"""
info = Gio.DBusNodeInfo.new_for_xml(XML)
def answer():
    if mode == 'none':
        return {}
    if mode == 'file':
        return {'uris': GLib.Variant('as', ['file:///run/user/1000/doc/ab12/chosen.txt'])}
    return {'uris': GLib.Variant('as', [])}
def call(conn, sender, path, iface, method, params, inv):
    parent, title, opts = params.unpack()
    print('call', method, 'parent=' + parent, flush=True)
    handle = '/org/freedesktop/portal/desktop/request/%s/%s' % (sender.lstrip(':').replace('.', '_'), opts.get('handle_token', 'x'))
    inv.return_value(GLib.Variant('(o)', (handle,)))
    def respond():
        conn.emit_signal(sender, handle, 'org.freedesktop.portal.Request', 'Response', GLib.Variant('(ua{sv})', (code, answer())))
        return False
    GLib.timeout_add(1500, respond)
def acquired(conn, name):
    conn.register_object('/org/freedesktop/portal/desktop', info.interfaces[0], call, lambda *a: GLib.Variant('u', 4), None)
Gio.bus_own_name(Gio.BusType.SESSION, 'org.freedesktop.portal.Desktop', Gio.BusNameOwnerFlags.NONE, acquired, None, None)
GLib.MainLoop().run()
'''

APP = r'''
import gi
gi.require_version('Gtk', '4.0')
from gi.repository import Gtk, GLib, Gio
# The program listens for the answers itself, as a desktop app with a session bus connection of its own does. Without this, a GTK that waits for the answer
# on a connection that nobody else uses was seen to wait for ever on this fake portal.
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
bus.signal_subscribe(None, 'org.freedesktop.portal.Request', 'Response', None, None, 0, lambda *a: None)
def on_activate(app):
    win = Gtk.ApplicationWindow(application=app)
    win.present()
    def go():
        d = Gtk.FileDialog()
        def done(dlg, r):
            try:
                f = dlg.save_finish(r)
                print('RESULT file', f.get_uri(), flush=True)
            except GLib.Error as e:
                print('RESULT error', e.message, flush=True)
            GLib.timeout_add(200, lambda: (app.quit(), False)[1])
        d.save(win, None, done)
        return False
    GLib.timeout_add(1000, go)
app = Gtk.Application(application_id='org.example.GtkSave')
app.connect('activate', on_activate)
app.run([])
print('EXIT clean', flush=True)
'''


def skip(why):
    print('SKIP:', why)
    sys.exit(0)


def main():
    try:
        import gi
        gi.require_version('Gtk', '4.0')
        from gi.repository import Gtk  # noqa: F401
    except Exception as e:  # missing module or typelib
        skip(f'python3-gi with GTK 4 is not here ({e})')
    sway, bus = shutil.which('sway'), shutil.which('dbus-daemon')
    if not sway or not bus:
        skip('sway or dbus-daemon is not installed')
    work = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / 'target/gtk-filechooser'
    isolation.safe_rmtree(work, work.parent)
    work.mkdir(parents=True)
    (work / 'portal.py').write_text(PORTAL)
    (work / 'app.py').write_text(APP)
    runtime = Path(isolation.owned_mkdtemp('gtkfc-rt-'))
    home = work / 'home'
    isolation.require_isolated_home(home, work)
    home.mkdir()
    env = {'PATH': os.environ['PATH'], 'HOME': str(home), 'XDG_RUNTIME_DIR': str(runtime), 'LANG': 'C.UTF-8', 'GTK_USE_PORTAL': '1',
           'DBUS_SESSION_BUS_ADDRESS': f'unix:path={runtime}/bus'}
    (work / 'sway.conf').write_text('output HEADLESS-1 resolution 800x600\n')
    started = []

    def spawn(argv, **kw):
        p = subprocess.Popen(argv, env={**env, **kw.pop('extra', {})}, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, start_new_session=True, **kw)
        started.append(p)
        return p

    failures = []
    try:
        comp = spawn([sway, '-c', str(work / 'sway.conf')], extra={'WLR_BACKENDS': 'headless', 'WLR_LIBINPUT_NO_DEVICES': '1', 'WLR_RENDERER': 'pixman'})
        sock = None
        for _ in range(100):
            found = [f for f in runtime.glob('wayland-*') if not f.name.endswith('.lock')]
            if found:
                sock = found[0]
                break
            if comp.poll() is not None:
                break
            time.sleep(0.1)
        if sock is None:
            skip('sway did not start here')
        env['WAYLAND_DISPLAY'] = sock.name
        spawn([bus, '--session', f'--address=unix:path={runtime}/bus', '--nofork', '--nopidfile'])
        time.sleep(0.5)

        def attempt(code, mode):
            """Run the program once against a portal that answers (code, mode); returns (exit code, output of the program, line of the portal)."""
            portal = spawn([sys.executable, str(work / 'portal.py'), str(code), mode])
            time.sleep(1.0)
            r = subprocess.run([sys.executable, str(work / 'app.py')], env=env, capture_output=True, text=True, timeout=40)
            lines = [l for l in r.stdout.splitlines() if l.startswith(('RESULT', 'EXIT'))]
            os.killpg(portal.pid, signal.SIGTERM)
            portal.wait(timeout=10)
            return r.returncode, lines

        for code in (1, 2):
            rc, lines = attempt(code, 'empty')
            ok = rc == 0 and any(l.startswith('RESULT error') for l in lines) and 'EXIT clean' in lines
            print(f"GTK  Response {code} with uris = []: exit {rc}, {lines} -> {'ok' if ok else 'FAILED'}")
            if not ok:
                failures.append(f'code {code} with uris = []')
        rc, lines = attempt(0, 'file')
        ok = rc == 0 and 'RESULT file file:///run/user/1000/doc/ab12/chosen.txt' in lines
        print(f"GTK  Response 0 with one address: exit {rc}, {lines} -> {'ok' if ok else 'FAILED'}")
        if not ok:
            failures.append('code 0 with one address')
        rc, lines = attempt(1, 'none')
        print(f"GTK  control, Response 1 with an empty dictionary (not what the router sends): exit {rc}, {lines} -> {'GTK survives it' if rc == 0 else 'GTK crashes on it, as on the VM'}")
    finally:
        for p in started:
            try:
                os.killpg(p.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        for p in started:
            try:
                p.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
        isolation.safe_rmtree(runtime, runtime.parent)
    if failures:
        print('FAILED:', ', '.join(failures))
        sys.exit(1)
    print('gtk file chooser answers: ok')


if __name__ == '__main__':
    main()
