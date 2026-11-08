# SPDX-License-Identifier: MIT OR Apache-2.0
"""A private desktop session for the D-Bus filter tests: a session bus of its own, the real
xdg-desktop-portal and its permission store and document portal, and a fake backend (no display).
The runtime folder is private; `bus` in it is a link to the user's real systemd bus, because the
stub asks that bus for its scope (a transient scope that ends with the process).
"""
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import xml.etree.ElementTree as ET

LIBEXEC = Path('/usr/libexec')
# The backend interfaces the fake backend claims, so that the portal exports the front-end interfaces
# built on them. The fake does nothing for most: the tests only need the portal to export them.
IMPL = ['Settings', 'Notification', 'Inhibit', 'Access', 'Account', 'AppChooser', 'Background', 'Clipboard', 'DynamicLauncher', 'Email', 'FileChooser',
        'GlobalShortcuts', 'InputCapture', 'Location', 'Lockdown', 'Print', 'RemoteDesktop', 'ScreenCast', 'Screenshot', 'Secret', 'Wallpaper']


def missing():
    """Why the lab cannot run here, or None."""
    for exe in ('dbus-daemon', 'gdbus', '/usr/bin/xdg-dbus-proxy', '/usr/libexec/xdg-desktop-portal'):
        if not (shutil.which(exe) or Path(exe).exists()):
            return f'{exe} not found'
    try:
        import gi  # noqa: F401
    except ImportError:
        return 'python3 gi not found'
    uid = os.getuid()
    if not Path(f'/run/user/{uid}/bus').exists():
        return 'no user bus (systemd user manager) to make the scope'
    return None


class Session:
    def __init__(self, tests_dir, portal=True, data_home=None, bus_rel='session', fake=True):
        uid = os.getuid()
        self.rt = Path(tempfile.mkdtemp(prefix='pkd-', dir=f'/run/user/{uid}'))
        self.rt.chmod(0o700)
        (self.rt / 'bus').symlink_to(f'/run/user/{uid}/bus')
        self.home = Path(tempfile.mkdtemp(prefix='pkdh-'))
        (self.rt / bus_rel).parent.mkdir(parents=True, exist_ok=True)
        self.addr = f'unix:path={self.rt}/{bus_rel}'
        self.log = self.rt / 'fake.log'
        self.procs = []
        self.env = {'PATH': os.environ['PATH'], 'HOME': str(self.home), 'XDG_RUNTIME_DIR': str(self.rt),
                    'DBUS_SESSION_BUS_ADDRESS': self.addr, 'XDG_CURRENT_DESKTOP': 'panpaktest',
                    'XDG_CONFIG_HOME': str(self.home / '.config'), 'XDG_DATA_HOME': str(data_home or (self.home / '.local/share')),
                    'XDG_CACHE_HOME': str(self.home / '.cache'), 'LANG': 'C'}
        # The portal looks for backends in <dir>/*.portal; one for the fake backend.
        pdir = self.rt / 'portals'
        pdir.mkdir()
        (pdir / 'fake.portal').write_text(
            '[portal]\nDBusName=org.freedesktop.impl.portal.desktop.fake\n'
            'Interfaces=' + ''.join(f'org.freedesktop.impl.portal.{i};' for i in IMPL) + '\n'
            'UseIn=panpaktest\n')
        self.env['XDG_DESKTOP_PORTAL_DIR'] = str(pdir)
        self.daemon = self._spawn(['dbus-daemon', '--session', '--nofork', f'--address={self.addr}'])
        self._wait(self.rt / bus_rel)
        if fake:
            self._spawn(['python3', str(Path(tests_dir) / 'dbus_fake_backend.py'), str(self.log)])
            for _ in range(100):
                if self.log.exists() and 'ready' in self.log.read_text():
                    break
                time.sleep(0.05)
        if portal:
            # Activated by the first call; started now so that its start is not in a test's time.
            self.call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus.StartServiceByName', 'org.freedesktop.portal.Desktop', '0')

    def _spawn(self, argv):
        p = subprocess.Popen(argv, env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
        self.procs.append(p)
        return p

    @staticmethod
    def _wait(path):
        for _ in range(100):
            if Path(path).exists():
                return
            time.sleep(0.05)
        raise RuntimeError(f'{path} did not appear')

    def call(self, dest, path, method, *args, bus=None, timeout=10):
        """`gdbus call`; returns (exit status, text)."""
        argv = ['gdbus', 'call', '-a', bus or self.addr, '--dest', dest, '--object-path', path, '--method', method, *args]
        p = subprocess.run(argv, env=self.env, capture_output=True, text=True, timeout=timeout)
        return p.returncode, (p.stdout + p.stderr).strip()

    def introspect(self, dest, path, bus=None):
        rc, out = 0, subprocess.run(['gdbus', 'introspect', '-a', bus or self.addr, '--dest', dest, '--object-path', path, '--xml'],
                                    env=self.env, capture_output=True, text=True, timeout=20)
        return ET.fromstring(out.stdout) if out.returncode == 0 and out.stdout.strip() else None

    def _lab_pids(self):
        """Processes of this lab, found by their environment (`XDG_RUNTIME_DIR` is the lab's private folder), never by name: the
        services the bus daemon started on demand (portal, permission store, document portal, dconf) are not children of this script."""
        want = f'XDG_RUNTIME_DIR={self.rt}'.encode()
        pids = []
        for d in os.listdir('/proc'):
            if not d.isdigit() or int(d) == os.getpid():
                continue
            try:
                if os.stat(f'/proc/{d}').st_uid == os.getuid() and want in open(f'/proc/{d}/environ', 'rb').read().split(b'\0'):
                    pids.append(int(d))
            except OSError:
                pass
        return pids

    def close(self):
        # The document portal's FUSE mount first, then the processes (by process id), then the folder.
        subprocess.run(['fusermount3', '-u', '-z', str(self.rt / 'doc')], capture_output=True)
        for p in reversed(self.procs):
            p.terminate()
        for sig, wait in ((signal.SIGTERM, 2.0), (signal.SIGKILL, 1.0)):
            pids = self._lab_pids()
            for pid in pids:
                try:
                    os.kill(pid, sig)
                except OSError:
                    pass
            deadline = time.monotonic() + wait
            while pids and time.monotonic() < deadline:
                time.sleep(0.05)
                pids = self._lab_pids()
        for p in self.procs:
            try:
                p.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
        # The store makes its trees read-only: make them writable first, or the folder stays behind.
        subprocess.run(['chmod', '-R', 'u+rwX', str(self.rt), str(self.home)], capture_output=True)
        shutil.rmtree(self.rt, ignore_errors=True)
        shutil.rmtree(self.home, ignore_errors=True)
