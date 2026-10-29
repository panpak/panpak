#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""End-to-end test of the isolated view (bubblewrap) and of the start without it.

Usage: python3 tests/view.py [work-directory]
Build release first. Everything runs in an isolated HOME inside the work directory. The app is a
Python script that reports what it sees: the sockets of a fake session (bus, systemd, gpg-agent,
ssh-agent, Wayland, PipeWire, PulseAudio, the system bus), /tmp, name resolution, the namespaces
and the Landlock rules. Every check is made twice: in the view and, in a copy of the machine where
/usr/bin/bwrap is not usable, as before the view existed.
"""
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
import app_dirs  # noqa: E402
import isolation  # noqa: E402

PROBE = r'''#!/usr/bin/python3
import os, socket, sys
out = {}
rt = os.environ.get('XDG_RUNTIME_DIR', '')

def conn(path, kind=socket.SOCK_STREAM):
    s = socket.socket(socket.AF_UNIX, kind)
    s.settimeout(2)
    try:
        s.connect(path)
        return 'ok'
    except FileNotFoundError:
        return 'absent'
    except PermissionError:
        return 'deny'
    except OSError as e:
        return f'err{e.errno}'
    finally:
        s.close()

for name, path in {
    'bus': rt + '/bus', 'systemd_private': rt + '/systemd/private', 'gpg_agent': rt + '/gnupg/S.gpg-agent',
    'gpg_agent_ssh': rt + '/gnupg/S.gpg-agent.ssh', 'ssh_agent': rt + '/ssh-agent',
    'wayland': rt + '/wayland-0', 'pipewire': rt + '/pipewire-0', 'pulse': rt + '/pulse/native',
    'system_bus_fake': rt + '/system_bus_socket', 'system_bus': '/run/dbus/system_bus_socket',
}.items():
    out['sock_' + name] = conn(path)
out['sock_journal'] = conn('/run/systemd/journal/socket', socket.SOCK_DGRAM)
disp = os.environ.get('DISPLAY', '')
out['sock_x11'] = conn('/tmp/.X11-unix/X' + disp[1:].split('.')[0]) if disp.startswith(':') else 'nodisplay'
home = os.path.expanduser('~')
out['sock_ssh_cm'] = conn(home + '/.ssh/cm-test')
out['sock_gpg_home'] = conn(home + '/.gnupg/S.gpg-agent')
out['home_ssh_dir'] = 'present' if os.path.exists(home + '/.ssh') else 'absent'
out['home_gnupg_dir'] = 'present' if os.path.exists(home + '/.gnupg') else 'absent'
store = home + '/.local/share/panpak'
for name in ('packages', 'blobs', 'apps', 'abi-cache', 'userdata/org.other.app'):
    out['store_' + name.replace('/', '_').replace('-', '_').replace('.', '_')] = 'present' if os.path.exists(store + '/' + name) else 'absent'
out['store_trees'] = 'present' if os.path.isdir(store + '/trees') else 'absent'
out['store_farms'] = 'present' if os.path.isdir(store + '/farms') else 'absent'
out['store_runtime'] = 'present' if os.path.exists(store + '/runtimes/1/panpak-stub') else 'absent'
out['home_secret'] = 'present' if os.path.exists(home + '/.config/gh/hosts.yml') else 'absent'
out['home_xauthority'] = 'present' if os.path.exists(home + '/.Xauthority') else 'absent'
for name, path in {'gtk': home + '/.config/gtk-3.0/settings.ini', 'font': home + '/.local/share/fonts/f.txt',
                   'icon': home + '/.local/share/icons/i.txt', 'mimeapps': home + '/.config/mimeapps.list',
                   'notes': home + '/Notes/n.txt', 'docs_ro': home + '/Docs/ro.txt', 'xauthority': home + '/.Xauthority'}.items():
    try:
        open(path).read()
        out['read_' + name] = 'ok'
    except OSError as e:
        out['read_' + name] = f'err{e.errno}'
for name, path in {'gtk': home + '/.config/gtk-3.0/settings.ini', 'notes': home + '/Notes/n.txt', 'docs_ro': home + '/Docs/ro.txt',
                   'stray': home + '/stray.txt'}.items():
    try:
        open(path, 'a').write('x')
        out['write_' + name] = 'ok'
    except OSError as e:
        out['write_' + name] = f'err{e.errno}'
# The home folder as the app sees it, and the user folders of the manifest.
try:
    out['home_entries'] = ','.join(sorted(os.listdir(home)))
except OSError as e:
    out['home_entries'] = f'err{e.errno}'
for name, rel in (('music', 'Musica'), ('docs', 'Documenti'), ('pics', 'Immagini')):
    try:
        out['list_' + name] = str(len(os.listdir(home + '/' + rel)))
    except OSError as e:
        out['list_' + name] = f'err{e.errno}'
    try:
        open(home + '/' + rel + '/w.txt', 'w').write('x')
        out['write_' + name] = 'ok'
    except OSError as e:
        out['write_' + name] = f'err{e.errno}'
try:
    out['user_dirs_file'] = open(os.environ['XDG_CONFIG_HOME'] + '/user-dirs.dirs').read().replace('\n', '|')
except OSError as e:
    out['user_dirs_file'] = f'err{e.errno}'

# Opening addresses: xdg-open in the view, by name and by full path.
import shutil, subprocess, time
def op(*a, exe='xdg-open'):
    time.sleep(0.5)
    try:
        r = subprocess.run([exe, *a], capture_output=True, timeout=10, text=True)
        return str(r.returncode)
    except Exception as e:
        return 'exc'
out['open_which'] = shutil.which('xdg-open') or 'none'
out['open_https'] = op('https://example.org/a')
out['open_file'] = op('file:///etc/passwd')
out['open_mailto'] = op('mailto:a@b.org?subject=hi')
out['open_attach'] = op('mailto:a@b.org?attach=/etc/passwd')
out['open_fullpath'] = op('https://example.org/full', exe='/usr/bin/xdg-open') if os.path.exists('/usr/bin/xdg-open') else 'nofile'
out['open_option'] = op('--version')
out['open_two'] = op('https://example.org/x', 'https://example.org/y')
try:
    from gi.repository import Gio
    out['gio_https'] = Gio.AppInfo.get_default_for_uri_scheme('https').get_id()
    out['gio_mailto'] = Gio.AppInfo.get_default_for_uri_scheme('mailto').get_id()
    out['gio_launch'] = str(Gio.AppInfo.launch_default_for_uri('https://example.org/gio', None))
except Exception as e:
    out['gio_https'] = 'nogi'
out['x11_dir'] = 'present' if os.path.isdir('/tmp/.X11-unix') else 'absent'
out['run_entries'] = ','.join(sorted(os.listdir('/run')))
out['rt_entries'] = ','.join(sorted(os.listdir(rt))) if os.path.isdir(rt) else 'none'

# /tmp: a file the harness made in the real /tmp (its name is the first argument).
marker = [a for a in sys.argv[1:] if a.startswith('/tmp/')]
if marker:
    out['tmp_marker'] = 'visible' if os.path.exists(marker[0]) else 'hidden'
out['tmp_entries'] = len(os.listdir('/tmp'))
out['env_TMPDIR'] = os.environ.get('TMPDIR', 'UNSET')
out['env_SQLITE_TMPDIR'] = os.environ.get('SQLITE_TMPDIR', 'UNSET')
try:
    open(os.environ['TMPDIR'] + '/w', 'w').write('x')
    out['write_tmpdir'] = 'ok'
except OSError as e:
    out['write_tmpdir'] = f'err{e.errno}'
try:
    open(os.environ['XDG_DATA_HOME'] + '/w', 'w').write('x')
    out['write_data'] = 'ok'
except OSError as e:
    out['write_data'] = f'err{e.errno}'
try:
    open(os.environ['XDG_RUNTIME_DIR'] + '/app-socket-test', 'w').write('x')
    out['write_runtime'] = 'ok'
except OSError as e:
    out['write_runtime'] = f'err{e.errno}'

# Landlock: the file is visible in the view, and only Landlock refuses to read it.
try:
    open(os.path.expanduser('~/.config/gh/hosts.yml')).read()
    out['read_secret'] = 'ok'
except OSError as e:
    out['read_secret'] = f'err{e.errno}'
out['no_new_privs'] = [l.split()[1] for l in open('/proc/self/status') if l.startswith('NoNewPrivs')][0]

# Name resolution.
try:
    out['resolv_conf'] = 'ok' if open('/etc/resolv.conf').read().strip() else 'empty'
except OSError as e:
    out['resolv_conf'] = f'err{e.errno}'
try:
    socket.getaddrinfo('example.com', 443)
    out['dns'] = 'ok'
except OSError as e:
    out['dns'] = 'fail'

# Namespaces and processes.
for ns in ('pid', 'ipc', 'mnt', 'user', 'net'):
    out['ns_' + ns] = os.readlink(f'/proc/self/ns/{ns}')
out['procs'] = len([d for d in os.listdir('/proc') if d.isdigit()])
out['own_pid'] = os.getpid()
out['sid'] = os.getsid(0)
out['uid_map'] = ' '.join(open('/proc/self/uid_map').read().split())
out['uid'] = os.getuid()

# A file given with `panpak open`: the argument is the path.
if len(sys.argv) > 1 and sys.argv[1].endswith('.doc'):
    try:
        open(sys.argv[1], 'a').write('more\n')
        out['open_write'] = 'ok'
    except OSError as e:
        out['open_write'] = f'err{e.errno}'
    out['open_read'] = 'ok' if os.path.exists(sys.argv[1]) else 'err'

with open(os.environ['XDG_DATA_HOME'] + '/report.txt', 'w') as f:
    for k, v in out.items():
        f.write(f'{k}={v}\n')
'''


class Fake:
    """Listening sockets that play the sockets of a desktop session."""

    def __init__(self, rt, home):
        self.socks = []
        paths = [Path(rt) / rel for rel in ['wayland-0', 'pipewire-0', 'pulse/native', 'systemd/private', 'gnupg/S.gpg-agent',
                                            'gnupg/S.gpg-agent.ssh', 'ssh-agent']]
        # Sockets in the home folder: SSH ControlMaster and a gpg-agent outside /run.
        paths += [Path(home) / '.ssh/cm-test', Path(home) / '.gnupg/S.gpg-agent']
        for p in paths:
            p.parent.mkdir(parents=True, exist_ok=True)
            s = socket.socket(socket.AF_UNIX)
            s.bind(str(p))
            s.listen(16)
            self.socks.append(s)
            threading.Thread(target=self.accept, args=(s,), daemon=True).start()

    @staticmethod
    def accept(s):
        while True:
            try:
                c, _ = s.accept()
                c.close()
            except OSError:
                return


def xvfb():
    """(binary, environment) for a private X server, or None."""
    binary = os.environ.get('PANPAK_XVFB') or shutil.which('Xvfb')
    if not binary:
        return None
    env = dict(os.environ)
    libs = Path(binary).resolve().parents[1] / 'lib/x86_64-linux-gnu'
    if libs.is_dir():
        env['LD_LIBRARY_PATH'] = str(libs)
    return binary, env


def start_xvfb(display):
    x = xvfb()
    if not x:
        return None
    binary, env = x
    proc = subprocess.Popen([binary, f':{display}', '-screen', '0', '640x480x24', '-nolisten', 'tcp'], env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(50):
        if Path(f'/tmp/.X11-unix/X{display}').exists():
            return proc
        time.sleep(0.1)
    proc.kill()
    return None


def masked(lab, *cmd):
    """Run `cmd` in a copy of the machine where /usr/bin/bwrap cannot be used. Only the test does this:
    a user namespace with the real tree writable and an empty character device over the program."""
    prefix = ['/usr/bin/bwrap', '--unshare-user', '--bind', '/', '/', '--dev-bind', '/dev', '/dev', '--proc', '/proc',
              '--ro-bind', '/dev/null', '/usr/bin/bwrap', '--']
    return subprocess.run(prefix + [str(c) for c in cmd], env=lab.env, capture_output=True, text=True,
                          timeout=120, stdin=subprocess.DEVNULL)


def run_report(lab, launcher_args, mask):
    report = lab.home / f'.local/share/panpak/userdata/{app_dirs.ID}/data/report.txt'
    report.unlink(missing_ok=True)
    launcher = str(lab.home / '.local/bin' / app_dirs.ID)
    if mask:
        p = masked(lab, launcher, *launcher_args)
    else:
        p = subprocess.run([launcher, *launcher_args], env=lab.env, capture_output=True, text=True,
                           timeout=120, stdin=subprocess.DEVNULL)
    rep = dict(l.partition('=')[::2] for l in report.read_text().splitlines()) if report.exists() else {}
    return p, rep


def check(rep, **want):
    app_dirs.expect(rep, **want)


def main():
    work = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT / 'target/view-test'
    if work.exists():
        isolation.require_inside(work, work.parent, 'work')
        subprocess.run(['chmod', '-R', 'u+w', str(work)], check=True)
        isolation.safe_rmtree(work, work.parent)
    work.mkdir(parents=True)
    app_dirs.PROBE = PROBE
    lab = app_dirs.Lab(work)
    h = lab.home
    (h / '.config/gh').mkdir(parents=True)
    (h / '.config/gh/hosts.yml').write_text('oauth_token: fake\n')
    # The user's own files: settings, fonts, icons, notes and secrets, and the folder of another app.
    for rel, text in {'.config/gtk-3.0/settings.ini': '[Settings]\n', '.config/mimeapps.list': '[Default Applications]\n',
                      '.local/share/fonts/f.txt': 'f', '.local/share/icons/i.txt': 'i', 'Notes/n.txt': 'n', 'Docs/ro.txt': 'r',
                      '.local/share/panpak/userdata/org.other.app/secret': 's', '.Xauthority': ''}.items():
        (h / rel).parent.mkdir(parents=True, exist_ok=True)
        (h / rel).write_text(text)
    fake = Fake(lab.runtime, h)
    lab.env['WAYLAND_DISPLAY'] = 'wayland-0'
    marker = Path('/tmp') / f'panpak-view-marker-{os.getpid()}'
    marker.write_text('host tmp\n')
    host_ns = {ns: os.readlink(f'/proc/self/ns/{ns}') for ns in ('pid', 'ipc', 'mnt', 'user', 'net')}
    x = None
    try:
        status = subprocess.run(['/usr/bin/bwrap', '--unshare-user', '--ro-bind', '/', '/', '/usr/bin/true'],
                                capture_output=True)
        view_here = status.returncode == 0
        if not view_here:
            print('bubblewrap is not usable here: only the start without the view is tested')

        # 1. A plain app: the base case.
        p = lab.install('')
        assert p.returncode == 0, p.stdout + p.stderr
        layers = next(l for l in p.stdout.splitlines() if l.startswith('isolation layers:'))
        print(p.stdout.strip().splitlines()[-3:])
        if view_here:
            assert 'isolated view (bubblewrap)' in layers, layers
        p, rep = run_report(lab, ['--marker', str(marker)], mask=False)
        assert p.returncode == 0, p.stderr
        gone = dict(sock_ssh_cm='absent', sock_gpg_home='absent', home_ssh_dir='absent', home_gnupg_dir='absent', home_secret='absent',
                    home_xauthority='absent', store_packages='absent', store_blobs='absent', store_apps='absent',
                    store_abi_cache='absent', store_userdata_org_other_app='absent', store_trees='present',
                    store_farms='present', store_runtime='present', read_gtk='ok', read_font='ok', read_icon='ok',
                    read_mimeapps='ok', read_notes='err2', read_docs_ro='err2', write_gtk='err30', write_stray='err30',
                    sock_bus='absent', sock_systemd_private='absent', sock_gpg_agent='absent', sock_gpg_agent_ssh='absent',
                    sock_ssh_agent='absent', sock_system_bus_fake='absent', sock_system_bus='absent',
                    sock_pipewire='absent', sock_pulse='absent', sock_wayland='ok', sock_journal='ok',
                    x11_dir='absent', tmp_marker='hidden', env_TMPDIR='/tmp', env_SQLITE_TMPDIR='/tmp',
                    write_tmpdir='ok', write_data='ok', write_runtime='ok',
                    uid=str(os.getuid()), uid_map=f'{os.getuid()} {os.getuid()} 1')
        if view_here:
            check(rep, **gone)
            assert rep['rt_entries'] == 'panpak,wayland-0', rep['rt_entries']
            assert rep['sid'] != str(os.getsid(0)), 'the app is in the session of the caller (no --new-session)'
            assert 'dbus' not in rep['run_entries'].split(','), rep['run_entries']
            assert rep['tmp_entries'] == '1' or int(rep['tmp_entries']) <= 2, rep['tmp_entries']
            for ns in ('pid', 'ipc', 'mnt', 'user'):
                assert rep['ns_' + ns] != host_ns[ns], f'{ns} namespace is the host one'
            assert rep['ns_net'] == host_ns['net'], 'the network namespace must be shared'
            assert int(rep['procs']) < 12, f'the app sees {rep["procs"]} processes'
            assert rep['resolv_conf'] == 'ok'
            check(rep, read_secret='err2', no_new_privs='1')
            print('view, plain app: ok (', rep['run_entries'], '| runtime:', rep['rt_entries'], ')')
            if rep['dns'] != 'ok':
                print('note: name resolution failed in the view; checking that it fails outside too')
                out = subprocess.run(['getent', 'hosts', 'example.com'], capture_output=True)
                assert out.returncode != 0, 'DNS works on the host but not in the view'
            else:
                print('DNS in the view: ok')

        # 2. The same app where bubblewrap cannot be used: as before the view existed.
        p = masked(lab, BIN_PANPAK(), 'install', '--yes', '--accept-new-permissions',
                   '--runtime', lab.stub, lab.pack(''))
        assert p.returncode == 0, p.stdout + p.stderr
        installed = p.stdout
        assert 'isolation layers: Landlock abstract-socket scope (V6)' in installed or 'Landlock path sockets' in installed, installed
        assert 'NOTE: no isolated view: /usr/bin/bwrap is missing or cannot be run' in installed, installed
        p, rep = run_report(lab, ['--marker', str(marker)], mask=True)
        assert p.returncode == 0, p.stdout + p.stderr
        check(rep, tmp_marker='visible', sid=str(os.getsid(0)))
        assert rep['env_TMPDIR'] != '/tmp', rep['env_TMPDIR']
        assert rep['sock_bus'] in ('ok', 'deny'), 'the session bus should exist outside the view'
        assert rep['home_ssh_dir'] == 'present' and rep['home_gnupg_dir'] == 'present'
        assert rep['sock_ssh_cm'] in ('ok', 'deny') and rep['sock_gpg_home'] in ('ok', 'deny'), 'without the view the home sockets exist'
        print('no bubblewrap: starts as before (bus socket:', rep['sock_bus'], ')')
        warn = 'this app is only partially isolated'
        landlock_v9 = 'Landlock path sockets (V9)' in installed
        assert (warn in installed) != landlock_v9, installed

        # 3. Audio only with the permission.
        p = lab.install('[permissions]\naudio = true\n')
        assert 'audio: the PipeWire and PulseAudio sockets' in p.stdout, p.stdout
        if view_here:
            _, rep = run_report(lab, [], mask=False)
            check(rep, sock_pipewire='ok', sock_pulse='ok', sock_bus='absent', sock_wayland='ok')
            print('audio with the permission: ok')

        # 3b. Home paths come back into the empty home, as declared.
        p = lab.install('[permissions]\n[[permissions.home_paths]]\npath = "Notes"\naccess = "rw"\n'
                        '[[permissions.home_paths]]\npath = "Docs"\naccess = "ro"\n')
        assert 'Partial access to your home: Notes (rw), Docs (ro)' in p.stdout, p.stdout
        if view_here:
            _, rep = run_report(lab, [], mask=False)
            check(rep, read_notes='ok', write_notes='ok', read_docs_ro='ok', write_docs_ro='err30', sock_ssh_cm='absent',
                  home_ssh_dir='absent', write_stray='err30')
            assert (h / 'Notes/n.txt').read_text() == 'nx', 'the write in Notes did not reach the real folder'
            assert (h / 'Docs/ro.txt').read_text() == 'r'
            print('home paths (rw and ro) in the view: ok')

        # 4. A listed bus socket is the one thing from the session that appears.
        p = lab.install('[permissions]\nsockets_allow = ["$XDG_RUNTIME_DIR/bus"]\n')
        assert 'Unix socket: $XDG_RUNTIME_DIR/bus' in p.stdout and 'not isolated' in p.stdout, p.stdout
        if view_here:
            _, rep = run_report(lab, [], mask=False)
            check(rep, sock_bus='ok', sock_pipewire='absent', sock_ssh_agent='absent', sock_systemd_private='absent')
            print('a listed bus is visible, nothing else is: ok')

        # 5. X11 only with the permission, and the IPC namespace is shared then.
        x = start_xvfb(97)
        if x and view_here:
            lab.env['DISPLAY'] = ':97'
            p = lab.install('')
            _, rep = run_report(lab, [], mask=False)
            check(rep, x11_dir='absent', sock_x11='absent', home_xauthority='absent')
            p = lab.install('[permissions]\nx11 = true\n')
            assert 'X11 access' in p.stdout and 'abstract Unix sockets stay open' in p.stdout, p.stdout
            _, rep = run_report(lab, [], mask=False)
            check(rep, x11_dir='present', sock_x11='ok', home_xauthority='present', read_xauthority='ok')
            assert rep['ns_ipc'] == host_ns['ipc'], 'IPC namespace must be shared with X11'
            assert rep['ns_pid'] != host_ns['pid']
            print('X11 only with the permission: ok')
        else:
            print('X11 test skipped (no Xvfb; set PANPAK_XVFB)')

        # 6. A file opened with `panpak open`: read-only by default, writable with --write.
        lab.env.pop('DISPLAY', None)
        lab.install('')
        doc = h / 'note.doc'
        doc.write_text('hello\n')
        for flag, want in (([], 'err'), (['--write'], 'ok')):
            report = h / f'.local/share/panpak/userdata/{app_dirs.ID}/data/report.txt'
            report.unlink(missing_ok=True)
            q = lab.cli('open', app_dirs.ID, str(doc), *flag, check=False)
            assert q.returncode == 0, q.stdout + q.stderr
            rep = dict(l.partition('=')[::2] for l in report.read_text().splitlines())
            assert rep['open_read'] == 'ok'
            assert (rep['open_write'] == 'ok') == (want == 'ok'), (flag, rep['open_write'])
        assert doc.read_text() == 'hello\nmore\n', doc.read_text()
        print('panpak open: read-only, then writable with --write: ok')

        # 6b. User folders (xdg_dirs): the folder of user-dirs.dirs comes back into the empty home, nothing else.
        for rel in ('Musica', 'Documenti', 'Immagini'):
            (h / rel).mkdir(exist_ok=True)
        (h / 'Musica/song.txt').write_text('s')
        (h / '.config/user-dirs.dirs').write_text('XDG_MUSIC_DIR="$HOME/Musica"\nXDG_DOCUMENTS_DIR="$HOME/Documenti"\n'
                                                   'XDG_PICTURES_DIR="$HOME/Immagini"\nXDG_DOWNLOAD_DIR="$HOME/Scaricati"\n')
        p = lab.install('[permissions]\n[[permissions.xdg_dirs]]\nname = "music"\naccess = "rw"\n'
                        '[[permissions.xdg_dirs]]\nname = "documents"\naccess = "ro"\n')
        assert p.returncode == 0, p.stdout + p.stderr
        assert 'music: ~/Musica (rw)' in p.stdout and 'documents: ~/Documenti (ro)' in p.stdout, p.stdout
        pin = (h / f'.local/share/panpak/apps/{app_dirs.ID}/xdg-dirs').read_text()
        assert 'music=Musica' in pin and 'documents=Documenti' in pin, pin
        if view_here:
            _, rep = run_report(lab, [], mask=False)
            check(rep, home_entries='.cache,.config,.local,Documenti,Musica', list_music='1', write_music='ok', list_docs='0', write_docs='err30',
                  list_pics='err2', write_pics='err2')
            assert 'XDG_MUSIC_DIR="$HOME/Musica"|' in rep['user_dirs_file'] and 'XDG_PICTURES_DIR="$HOME/"|' in rep['user_dirs_file'], rep['user_dirs_file']
            assert (h / 'Musica/w.txt').read_text() == 'x', 'the write in the music folder did not reach the real folder'
            print('xdg_dirs (rw and ro) in the view: ok')
        # A folder that changed since install is refused at the start, with a warning.
        (h / '.config/user-dirs.dirs').write_text('XDG_MUSIC_DIR="$HOME/Immagini"\nXDG_DOCUMENTS_DIR="$HOME/Documenti"\n')
        _, rep, err = lab.run()
        assert 'the music folder changed since install' in err, err
        if view_here:
            check(rep, list_music='err2', write_music='err2', list_pics='err2')
            check(rep, list_docs='0')
        # A folder that is now a link is refused too.
        (h / '.config/user-dirs.dirs').write_text('XDG_MUSIC_DIR="$HOME/Musica"\nXDG_DOCUMENTS_DIR="$HOME/Documenti"\n')
        shutil.move(h / 'Musica', h / 'Musica.real')
        os.symlink(h / 'Musica.real', h / 'Musica')
        _, rep, err = lab.run()
        assert 'music folder is not granted' in err and 'symbolic link' in err, err
        os.unlink(h / 'Musica')
        shutil.move(h / 'Musica.real', h / 'Musica')
        # An hostile user-dirs.dirs is refused at install: no folder, with a warning.
        (h / '.config/user-dirs.dirs').write_text('XDG_MUSIC_DIR="/etc"\n')
        p = lab.install('[permissions]\n[[permissions.xdg_dirs]]\nname = "music"\naccess = "rw"\n')
        assert 'WARNING: music: not granted' in p.stdout, p.stdout
        assert not (h / f'.local/share/panpak/apps/{app_dirs.ID}/xdg-dirs').exists()
        (h / '.config/user-dirs.dirs').write_text('XDG_MUSIC_DIR="$HOME/Musica"\nXDG_DOCUMENTS_DIR="$HOME/Documenti"\n')
        print('xdg_dirs: changed, linked and hostile folders refused: ok')

        # 6c. Addresses: xdg-open in the view reaches the real opener only for http, https and mailto.
        binlog = work / 'open.log'
        fakebin = work / 'fakebin'
        fakebin.mkdir()
        (fakebin / 'xdg-open').write_text(f'#!/bin/sh\necho "$@" >> {binlog}\n')
        (fakebin / 'xdg-open').chmod(0o755)
        lab.env['PATH'] = f'{fakebin}:{lab.env["PATH"]}'
        lab.install('')
        if view_here:
            _, rep = run_report(lab, [], mask=False)
            want = dict(open_https='0', open_file='1', open_mailto='0', open_attach='1', open_option='1', open_two='1')
            check(rep, **want)
            assert rep['open_which'].endswith('/panpak/bin/xdg-open'), rep['open_which']
            assert rep['open_fullpath'] in ('0', 'nofile'), rep['open_fullpath']
            time.sleep(0.5)
            got = binlog.read_text().split('\n')
            assert 'https://example.org/a' in got and 'mailto:a@b.org?subject=hi' in got, got
            assert not any('file:' in g or 'attach' in g or 'passwd' in g for g in got), got
            assert rep['gio_https'] in ('panpak-opener.desktop', 'nogi'), rep['gio_https']
            if rep['gio_https'] != 'nogi':
                assert rep['gio_mailto'] == 'panpak-opener.desktop', rep['gio_mailto']
                time.sleep(0.5)
                assert 'https://example.org/gio' in binlog.read_text(), binlog.read_text()
            print('addresses: https and mailto pass, file: and attach are refused, GIO goes through the opener:', rep['gio_https'])
        lab.env['PATH'] = lab.env['PATH'].split(':', 1)[1]

        # 7. doctor tells the layers, with and without bubblewrap.
        d = lab.cli('doctor', check=False).stdout
        dm = masked(lab, BIN_PANPAK(), 'doctor').stdout
        if view_here:
            assert 'OK   isolated view:' in d, d
        assert 'NOTE isolated view: not available, /usr/bin/bwrap is missing or cannot be run' in dm, dm
        assert 'install the bubblewrap package' in dm, dm
        print('doctor: ok')
        print('all view tests passed')
    finally:
        if x:
            x.terminate()
        marker.unlink(missing_ok=True)
        for s in fake.socks:
            s.close()
        lab.bus.terminate()
        lab.sysbus.terminate()
        isolation.safe_rmtree(Path(lab.runtime), Path(lab.runtime).parent)


def BIN_PANPAK():
    return app_dirs.BIN / 'panpak'


if __name__ == '__main__':
    main()
