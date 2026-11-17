#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A change of the user's theme or settings is seen at once by an app that is running in the view.

Usage: python3 tests/env_live.py     (build release first; needs Xvfb and python3-gi with GTK 3)
The environment of the user is mounted read-only at the real paths (PANPAK-FORMAT.md section 9.1). An app is started in a new HOME, in the
view, and the test changes the folders of the user while it runs:
- GTK 3: an icon is added to an icon theme of the user and `gtk-3.0/settings.ini` is replaced (the theme is switched). The app is a GTK
  app on a dedicated Xvfb and watches the two folders with GIO file monitors, which is how GTK follows its files; it must be told of both.
  (A change of the colour scheme or of the GTK theme made in the desktop settings goes through the portal and dconf, not through these
  files: `tests/dbus_filter.py` checks that path, "theme change: the app sees SettingChanged through the proxy".)
- Qt and KDE read their settings from `~/.config/qt6ct`, `~/.config/qt5ct` and `~/.config/kdeglobals` and watch them with inotify
  (`QFileSystemWatcher`, `KConfigWatcher`). There is no Qt binding in the test image, so the app watches the same folder with inotify itself
  and must be told of a file that is written and of a file that is replaced by a rename (how an editor saves).
- The limit that is documented: a single mounted file (`kdeglobals`) that is replaced by a rename is not seen until the next start; one that
  is written in place is.
Everything runs in an isolated HOME with a dedicated Xvfb; the desktop of the user is not touched.
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402
from incremental import PUBLIC, run  # noqa: E402

APP = r'''#!/usr/bin/python3
import ctypes, os, struct, sys, time
import gi
gi.require_version('Gtk', '3.0')
from gi.repository import Gtk, GLib
home = os.path.expanduser('~')
from gi.repository import Gio
# GTK's file based settings (icon themes, themes, gtk.css, settings.ini) are watched with GIO's file monitors, which are inotify
gtk_events = set()
def on_change(monitor, file, other, event):
    gtk_events.add((file.get_basename(), event.value_nick))
mons = []
for d in ('/.local/share/icons/LiveTheme/16x16/apps', '/.config/gtk-3.0'):
    m = Gio.File.new_for_path(home + d).monitor_directory(Gio.FileMonitorFlags.NONE, None)
    m.connect('changed', on_change)
    mons.append(m)
print('gtk_icon_before=False', flush=True)
# inotify on the Qt settings folder, and on the two ways of changing kdeglobals
libc = ctypes.CDLL(None, use_errno=True)
fd = libc.inotify_init1(os.O_NONBLOCK)
IN_MODIFY, IN_CLOSE_WRITE, IN_MOVED_TO, IN_CREATE = 0x2, 0x8, 0x80, 0x100
libc.inotify_add_watch(fd, (home + '/.config/qt6ct').encode(), IN_CLOSE_WRITE | IN_MOVED_TO | IN_CREATE)
wd_k = libc.inotify_add_watch(fd, (home + '/.config/kdeglobals').encode(), IN_MODIFY | IN_CLOSE_WRITE)
print('ready', flush=True)
seen = {'gtk': False, 'qt_write': False, 'qt_rename': False, 'kde_inplace': False}
kde_rename_text = None
end = time.time() + 20
while time.time() < end:
    try:
        data = os.read(fd, 4096)
    except BlockingIOError:
        data = b''
    i = 0
    while i < len(data):
        wd, mask, cookie, n = struct.unpack_from('iIII', data, i)
        name = data[i + 16:i + 16 + n].rstrip(b'\0').decode()
        i += 16 + n
        if name == 'qt6ct.conf' and mask & IN_MOVED_TO:
            seen['qt_rename'] = True
        elif name in ('extra.conf',) and mask & (IN_CLOSE_WRITE | IN_CREATE):
            seen['qt_write'] = True
        elif wd == wd_k:
            seen['kde_inplace'] = True
    if any(n == 'live-icon.png' for n, e in gtk_events) and any(n == 'settings.ini' for n, e in gtk_events):
        seen['gtk'] = True
    ctx = GLib.MainContext.default()
    while ctx.pending():
        ctx.iteration(False)
    if all(seen.values()) and os.path.exists(home + '/.config/done-marker-from-host') is False:
        pass
    if os.path.exists('/tmp/host-done') or all(seen.values()):
        break
    time.sleep(0.2)
# kdeglobals as the app reads it now: written in place yes, replaced by a rename (a new file) no
text = open(home + '/.config/kdeglobals').read()
print('gtk_icon_seen=' + str(seen['gtk']))
print('qt_write_seen=' + str(seen['qt_write']))
print('qt_rename_seen=' + str(seen['qt_rename']))
print('kde_inplace_seen=' + str(seen['kde_inplace']))
print('kde_text=' + text.replace('\n', '|'))
'''

PNG = bytes.fromhex('89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c6360000002000001e221bc330000000049454e44ae426082')


def main():
    xvfb = shutil.which('Xvfb')
    assert xvfb, 'Xvfb is needed'
    with tempfile.TemporaryDirectory(prefix='panpak-envlive-', dir=ROOT / 'target') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        home = tmp / 'home'
        home.mkdir()
        # The user's theme, fonts and settings.
        theme = home / '.local/share/icons/LiveTheme'
        (theme / '16x16/apps').mkdir(parents=True)
        (theme / 'index.theme').write_text('[Icon Theme]\nName=LiveTheme\nDirectories=16x16/apps\n\n[16x16/apps]\nSize=16\nType=Fixed\n')
        (theme / '16x16/apps/first.png').write_bytes(PNG)
        (home / '.config/gtk-3.0').mkdir(parents=True)
        (home / '.config/gtk-3.0/settings.ini').write_text('[Settings]\ngtk-theme-name=Adwaita\n')
        (home / '.config/qt6ct').mkdir(parents=True)
        (home / '.config/qt6ct/qt6ct.conf').write_text('[Appearance]\nstyle=Fusion\n')
        (home / '.config/kdeglobals').write_text('[General]\nColorScheme=Light\n')
        fd = os.open(tmp / 'display', os.O_CREAT | os.O_RDWR, 0o600)
        server = subprocess.Popen([xvfb, '-displayfd', str(fd), '-ac', '-screen', '0', '800x600x24'], pass_fds=[fd],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            text = ''
            for _ in range(100):
                text = (tmp / 'display').read_text().strip()
                if text:
                    break
                time.sleep(0.1)
            assert text, 'Xvfb did not start'
            env = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / '.local/share'), XDG_CONFIG_HOME=str(home / '.config'),
                       XDG_CACHE_HOME=str(home / '.cache'), DISPLAY=':' + text, LC_ALL='C', PANPAK_NO_BACKGROUND='1')
            for k in ('XAUTHORITY', 'WAYLAND_DISPLAY'):
                env.pop(k, None)
            tree = tmp / 'tree'
            tree.mkdir()
            (tree / 'watch').write_text(APP)
            (tree / 'watch').chmod(0o755)
            (tree / 'panpak.toml').write_text(f'[app]\nid="org.example.envlive"\nname="Live"\nversion="1"\nentry="watch"\n[permissions]\nx11=true\n[update]\nkey="ed25519:{PUBLIC}"\n')
            (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
            (tmp / 'secret.key').chmod(0o600)
            run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', tmp / 'a.panpak'], env)
            run([BIN / 'panpak', 'install', '--yes', '--accept-new-permissions', '--runtime', BIN / 'panpak-stub', tmp / 'a.panpak'], env)
            def scenario(cmd, what):
                for f in (theme / '16x16/apps/live-icon.png', home / '.config/qt6ct/extra.conf'):
                    f.unlink(missing_ok=True)
                (home / '.config/qt6ct/qt6ct.conf').write_text('[Appearance]\nstyle=Fusion\n')
                (home / '.config/kdeglobals').write_text('[General]\nColorScheme=Light\n')
                p = subprocess.Popen(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                lines = []
                t0 = time.time()
                while 'ready' not in lines:
                    line = p.stdout.readline()
                    assert line, f'{what}: the app ended before it was ready:\n' + p.stderr.read()
                    lines.append(line.strip())
                    assert time.time() - t0 < 60
                assert 'gtk_icon_before=False' in lines, lines
                # The user changes the look while the app runs.
                (theme / '16x16/apps/live-icon.png').write_bytes(PNG)                 # a new icon in a theme folder
                (home / '.config/gtk-3.0/settings.ini.new').write_text('[Settings]\ngtk-theme-name=Adwaita-dark\n')
                os.rename(home / '.config/gtk-3.0/settings.ini.new', home / '.config/gtk-3.0/settings.ini')   # the theme is switched
                (home / '.config/qt6ct/extra.conf').write_text('[Fonts]\nfixed="Mono"\n')   # a new file in a Qt settings folder
                (home / '.config/qt6ct/qt6ct.conf.new').write_text('[Appearance]\nstyle=Breeze\n')
                os.rename(home / '.config/qt6ct/qt6ct.conf.new', home / '.config/qt6ct/qt6ct.conf')   # an editor saves by rename
                with open(home / '.config/kdeglobals', 'r+') as f:                      # a program writes the file in place
                    f.write('[General]\nColorScheme=Dark \n')
                out, err = p.communicate(timeout=60)
                got = dict(l.split('=', 1) for l in out.splitlines() if '=' in l)
                assert p.returncode == 0, (what, out, err)
                assert got['gtk_icon_seen'] == 'True', (what, 'the GTK app was not told of the new icon and the new settings.ini', got, err[-300:])
                assert got['qt_write_seen'] == 'True' and got['qt_rename_seen'] == 'True', (what, 'the watcher of the Qt settings folder missed a change', got)
                assert got['kde_inplace_seen'] == 'True' and 'Dark' in got['kde_text'], (what, 'a file written in place is not seen', got)
                return got

            # Control: the same app, with no panpak. If this fails the test, not the view, is wrong.
            scenario([sys.executable, str(tree / 'watch')], 'control without panpak')
            print('control: the app and the changes, with no panpak, see everything PASS')
            scenario([str(home / '.local/bin/org.example.envlive')], 'in the view')
            print('in the view: the GTK 3 app finds an icon added to a theme of the user while it runs; the Qt settings folder shows a new file and a rename; a file written in place is seen PASS')
        finally:
            os.close(fd)
            server.terminate()
            server.wait(timeout=5)
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
