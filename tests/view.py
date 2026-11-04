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
import atexit
import os
from pathlib import Path
import shutil
import signal
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
out['sock_journal_devlog'] = conn('/run/systemd/journal/dev-log', socket.SOCK_DGRAM)
try:
    _d = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
    _d.sendto(b'<14>panpak view probe', '/run/systemd/journal/dev-log')
    out['sock_journal_sendto'] = 'ok'
except FileNotFoundError:
    out['sock_journal_sendto'] = 'absent'
except OSError as e:
    out['sock_journal_sendto'] = 'deny' if e.errno == 13 else f'err{e.errno}'
# Sockets of container managers and of snap: /run is a new tmpfs in the view, so those below it are gone. The LXD
# paths below /var are not under /run: they are reported, whether or not the machine has LXD (it may not).
for name, path in {
    'docker': '/run/docker.sock', 'docker_var': '/var/run/docker.sock', 'containerd': '/run/containerd/containerd.sock',
    'podman': '/run/podman/podman.sock', 'snapd': '/run/snapd.socket', 'libvirt': '/run/libvirt/libvirt-sock',
    'lxd_snap': '/var/snap/lxd/common/lxd/unix.socket', 'lxd_lib': '/var/lib/lxd/unix.socket',
}.items():
    out['sock_' + name] = conn(path)
# A socket the harness made outside /run, /tmp and the home folder, standing for any such system socket.
fake = [a for a in sys.argv[1:] if a.startswith('/var/tmp/')]
if fake:
    out['sock_outside_run'] = conn(fake[0])
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
for name, rel in (('music', 'Music'), ('docs', 'Documents'), ('pics', 'Pictures')):
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
out['args'] = ' '.join(sys.argv[1:])
out['x11_dir'] = 'present' if os.path.isdir('/tmp/.X11-unix') else 'absent'
out['run_entries'] = ','.join(sorted(os.listdir('/run')))
out['rt_entries'] = ','.join(sorted(os.listdir(rt))) if os.path.isdir(rt) else 'none'

# /tmp: a file the harness made in the real /tmp (its name is the first argument).
marker = [a for a in sys.argv[1:] if a.startswith('/tmp/')]
if marker:
    out['tmp_marker'] = 'visible' if os.path.exists(marker[0]) else 'hidden'
if '--hold' in sys.argv:
    # A first copy of the app: leave marks in /tmp and /dev/shm and wait for the harness to let go.
    import time as _t
    open('/tmp/copy-held', 'w').write('x')
    open('/dev/shm/copy-shm', 'w').write('x')
    for _ in range(300):
        if os.path.exists('/tmp/copy-release'):
            break
        _t.sleep(0.1)
out['tmp_copy'] = ','.join(sorted(n for n in os.listdir('/tmp') if n.startswith('copy-')))
out['shm_copy'] = ','.join(sorted(n for n in os.listdir('/dev/shm') if n.startswith('copy-')))
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

# Network: a datagram and a connection to the listeners the harness keeps on the loopback of the host.
net_port = [a.split('=')[1] for a in sys.argv[1:] if a.startswith('--net-port=')]
if net_port:
    try:
        u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        u.sendto(b'udp-from-the-app', ('127.0.0.1', int(net_port[0])))
        out['udp_loopback'] = 'sent'
    except OSError as e:
        out['udp_loopback'] = f'err{e.errno}'
    try:
        t = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        t.settimeout(2)
        t.connect(('127.0.0.1', int(net_port[0])))
        out['tcp_loopback'] = 'ok'
    except OSError as e:
        out['tcp_loopback'] = f'err{e.errno}'
    # /proc/net/dev lists the interfaces of the network namespace of the process.
    out['net_ifaces'] = ','.join(sorted(l.split(':')[0].strip() for l in open('/proc/net/dev').read().splitlines()[2:]))

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


def stub_memory(launcher_pid):
    """Memory of the outer stub (the resident process of a start in the view) below the launcher."""
    pids, stack = [], [launcher_pid]
    while stack:
        pid = stack.pop()
        pids.append(pid)
        try:
            stack += [int(c) for c in Path(f'/proc/{pid}/task/{pid}/children').read_text().split()]
        except OSError:
            pass
    for pid in pids:
        try:
            cmd = Path(f'/proc/{pid}/cmdline').read_bytes().split(b'\0')
            if Path(f'/proc/{pid}/exe').resolve().name == 'panpak-stub' and b'--panpak-inner' not in cmd:
                status = dict(l.split(':', 1) for l in Path(f'/proc/{pid}/status').read_text().splitlines())
                return {k: int(status[k].split()[0]) for k in ('VmRSS', 'VmHWM', 'Threads')}
        except OSError:
            continue
    raise AssertionError('the outer stub is not running')


def teardown(lab):
    """Stop the two buses and remove the runtime folder. Safe to call twice."""
    for bus in (lab.bus, lab.sysbus):
        if bus.poll() is None:
            bus.terminate()
            try:
                bus.wait(timeout=10)
            except subprocess.TimeoutExpired:
                bus.kill()
                bus.wait()
    # Helpers that the private bus started (dconf-service, for one) write into the runtime folder when the
    # bus goes away: stop what still has this folder in its environment, then remove the folder.
    wanted = f'XDG_RUNTIME_DIR={lab.runtime}'.encode()
    for _ in range(30):
        mine = []
        for d in os.listdir('/proc'):
            if d.isdigit() and int(d) != os.getpid():
                try:
                    if wanted in Path(f'/proc/{d}/environ').read_bytes().split(b'\0'):
                        mine.append(int(d))
                except OSError:
                    pass
        if not mine:
            break
        for pid in mine:
            try:
                os.kill(pid, signal.SIGTERM)
            except OSError:
                pass
        time.sleep(0.1)
    isolation.remove_owned(lab.runtime, Path(lab.runtime).parent)


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
    # The runtime folder goes away on every way out: normal end, a failed check, an error before the try below.
    atexit.register(teardown, lab)
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
    # A listening socket in /var/tmp: outside the folders the view replaces (home, /tmp, /run).
    outside = Path('/var/tmp') / f'panpak-view-outside-{os.getpid()}.sock'
    outside.unlink(missing_ok=True)
    outside_srv = socket.socket(socket.AF_UNIX)
    outside_srv.bind(str(outside))
    outside_srv.listen(4)
    threading.Thread(target=Fake.accept, args=(outside_srv,), daemon=True).start()
    host_ns = {ns: os.readlink(f'/proc/self/ns/{ns}') for ns in ('pid', 'ipc', 'mnt', 'user', 'net')}
    # Listeners on the loopback of the host (UDP and TCP, same port): what the app can reach on the network.
    net_got = []
    net_udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    net_udp.bind(('127.0.0.1', 0))
    net_port = net_udp.getsockname()[1]
    net_tcp = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    net_tcp.bind(('127.0.0.1', net_port))
    net_tcp.listen(4)

    def net_listen():
        while True:
            try:
                data, _ = net_udp.recvfrom(100)
                net_got.append('udp')
            except OSError:
                return

    def net_accept():
        while True:
            try:
                c, _ = net_tcp.accept()
                net_got.append('tcp')
                c.close()
            except OSError:
                return

    threading.Thread(target=net_listen, daemon=True).start()
    threading.Thread(target=net_accept, daemon=True).start()
    net_arg = f'--net-port={net_port}'
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
        p, rep = run_report(lab, ['--marker', str(marker), str(outside), net_arg], mask=False)
        assert p.returncode == 0, p.stderr
        gone = dict(sock_ssh_cm='absent', sock_gpg_home='absent', home_ssh_dir='absent', home_gnupg_dir='absent', home_secret='absent',
                    home_xauthority='absent', store_packages='absent', store_blobs='absent', store_apps='absent',
                    store_abi_cache='absent', store_userdata_org_other_app='absent', store_trees='present',
                    store_farms='present', store_runtime='present', read_gtk='ok', read_font='ok', read_icon='ok',
                    read_mimeapps='ok', read_notes='err2', read_docs_ro='err2', write_gtk='err30', write_stray='err30',
                    sock_bus='absent', sock_systemd_private='absent', sock_gpg_agent='absent', sock_gpg_agent_ssh='absent',
                    sock_ssh_agent='absent', sock_system_bus_fake='absent', sock_system_bus='absent',
                    sock_pipewire='absent', sock_pulse='absent', sock_wayland='ok', sock_journal='ok', sock_journal_devlog='ok', sock_journal_sendto='ok',
                    sock_docker='absent', sock_docker_var='absent', sock_containerd='absent', sock_podman='absent',
                    sock_snapd='absent', sock_libvirt='absent',
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
            # No network asked for: the view has a network namespace of its own with only the loopback interface.
            assert rep['ns_net'] != host_ns['net'], 'an app with no network shares the network namespace of the host'
            assert rep['net_ifaces'] == 'lo', rep['net_ifaces']
            time.sleep(0.3)
            assert net_got == [], f'an app with no network reached the host: {net_got}'
            assert rep['tcp_loopback'] != 'ok'
            assert int(rep['procs']) < 12, f'the app sees {rep["procs"]} processes'
            assert rep['resolv_conf'] == 'ok'
            check(rep, read_secret='err2', no_new_privs='1')
            check(rep, sock_lxd_snap='absent', sock_lxd_lib='absent', sock_outside_run='absent')
            print('sockets of containers, snap and LXD, and one in /var/tmp: not reachable')
            print('view, plain app: ok (', rep['run_entries'], '| runtime:', rep['rt_entries'], ')')
            assert rep['dns'] == 'fail', 'name resolution works for an app with no network'
            print('no network: no interface but lo, no datagram or connection reached the host, no name resolution')

        # 2. The same app where bubblewrap cannot be used: as before the view existed.
        p = masked(lab, BIN_PANPAK(), 'install', '--yes', '--accept-new-permissions',
                   '--runtime', lab.stub, lab.pack(''))
        assert p.returncode == 0, p.stdout + p.stderr
        installed = p.stdout
        assert 'isolation layers: Landlock abstract-socket scope (V6)' in installed or 'Landlock path sockets' in installed, installed
        assert 'NOTE: no isolated view: /usr/bin/bwrap is missing or cannot be run' in installed, installed
        net_got.clear()
        p, rep = run_report(lab, ['--marker', str(marker), str(outside), net_arg], mask=True)
        assert p.returncode == 0, p.stdout + p.stderr
        # Without the view, Landlock stops TCP only: a datagram still reaches the host (documented limit).
        time.sleep(0.3)
        assert net_got == ['udp'], f'without the view: {net_got}'
        assert rep['tcp_loopback'] != 'ok'
        print('without the view: TCP is refused and UDP gets through (known limit, THREAT-MODEL.md)')
        check(rep, tmp_marker='visible', sid=str(os.getsid(0)))
        # Control: without the view the socket in /var/tmp is reachable, except where Landlock V9 closes path sockets.
        if 'Landlock path sockets (V9)' in installed:
            assert rep['sock_outside_run'] == 'deny', rep['sock_outside_run']
            print('control: without the view the socket in /var/tmp is closed by Landlock V9')
        else:
            assert rep['sock_outside_run'] == 'ok'
            print('control: without the view the socket in /var/tmp is reachable')
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
        for rel in ('Music', 'Documents', 'Pictures'):
            (h / rel).mkdir(exist_ok=True)
        (h / 'Music/song.txt').write_text('s')
        (h / '.config/user-dirs.dirs').write_text('XDG_MUSIC_DIR="$HOME/Music"\nXDG_DOCUMENTS_DIR="$HOME/Documents"\n'
                                                   'XDG_PICTURES_DIR="$HOME/Pictures"\nXDG_DOWNLOAD_DIR="$HOME/Downloads"\n')
        p = lab.install('[permissions]\n[[permissions.xdg_dirs]]\nname = "music"\naccess = "rw"\n'
                        '[[permissions.xdg_dirs]]\nname = "documents"\naccess = "ro"\n')
        assert p.returncode == 0, p.stdout + p.stderr
        assert 'music: ~/Music (rw)' in p.stdout and 'documents: ~/Documents (ro)' in p.stdout, p.stdout
        pin = (h / f'.local/share/panpak/apps/{app_dirs.ID}/xdg-dirs').read_text()
        assert 'music=Music' in pin and 'documents=Documents' in pin, pin
        if view_here:
            _, rep = run_report(lab, [], mask=False)
            check(rep, home_entries='.cache,.config,.local,Documents,Music', list_music='1', write_music='ok', list_docs='0', write_docs='err30',
                  list_pics='err2', write_pics='err2')
            assert 'XDG_MUSIC_DIR="$HOME/Music"|' in rep['user_dirs_file'] and 'XDG_PICTURES_DIR="$HOME/"|' in rep['user_dirs_file'], rep['user_dirs_file']
            assert (h / 'Music/w.txt').read_text() == 'x', 'the write in the music folder did not reach the real folder'
            print('xdg_dirs (rw and ro) in the view: ok')
        # A folder that changed since install is refused at the start, with a warning.
        (h / '.config/user-dirs.dirs').write_text('XDG_MUSIC_DIR="$HOME/Pictures"\nXDG_DOCUMENTS_DIR="$HOME/Documents"\n')
        _, rep, err = lab.run()
        assert 'the music folder changed since install' in err, err
        if view_here:
            check(rep, list_music='err2', write_music='err2', list_pics='err2')
            check(rep, list_docs='0')
        # A folder that is now a link is refused too.
        (h / '.config/user-dirs.dirs').write_text('XDG_MUSIC_DIR="$HOME/Music"\nXDG_DOCUMENTS_DIR="$HOME/Documents"\n')
        shutil.move(h / 'Music', h / 'Music.real')
        os.symlink(h / 'Music.real', h / 'Music')
        _, rep, err = lab.run()
        assert 'music folder is not granted' in err and 'symbolic link' in err, err
        os.unlink(h / 'Music')
        shutil.move(h / 'Music.real', h / 'Music')
        # An hostile user-dirs.dirs is refused at install: no folder, with a warning.
        (h / '.config/user-dirs.dirs').write_text('XDG_MUSIC_DIR="/etc"\n')
        p = lab.install('[permissions]\n[[permissions.xdg_dirs]]\nname = "music"\naccess = "rw"\n')
        assert 'WARNING: music: not granted' in p.stdout, p.stdout
        assert not (h / f'.local/share/panpak/apps/{app_dirs.ID}/xdg-dirs').exists()
        (h / '.config/user-dirs.dirs').write_text('XDG_MUSIC_DIR="$HOME/Music"\nXDG_DOCUMENTS_DIR="$HOME/Documents"\n')
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

        # 6d. Copies of one app that run together share /tmp and /dev/shm; a start with no copy alive begins empty.
        if view_here:
            lab.install('')
            shared = Path(lab.runtime) / 'panpak' / app_dirs.ID
            launcher = str(h / '.local/bin' / app_dirs.ID)
            first_err = work / 'first.err'
            first = subprocess.Popen([launcher, '--hold'], env=lab.env, stdout=subprocess.DEVNULL, stderr=first_err.open('wb'))
            t0 = time.time()
            for _ in range(300):
                if (shared / 'tmp/copy-held').exists() and (shared / 'shm/copy-shm').exists():
                    break
                time.sleep(0.1)
            else:
                listing = sorted(str(q.relative_to(shared)) for q in shared.rglob('*')) if shared.exists() else 'no shared folder'
                raise AssertionError(f'the first copy did not start (exit {first.poll()}): {first_err.read_text()} {listing}')
            print(f'first copy ready after {time.time() - t0:.1f} s')
            assert oct((shared / 'tmp').stat().st_mode & 0o777) == '0o700'
            rss = stub_memory(first.pid)
            print('outer stub while the app runs: VmRSS', rss['VmRSS'], 'kB, VmHWM', rss['VmHWM'], 'kB, threads', rss['Threads'])
            assert rss['VmRSS'] < 32 * 1024, rss
            _, rep = run_report(lab, [], mask=False)
            check(rep, tmp_copy='copy-held', shm_copy='copy-shm')
            (shared / 'tmp/copy-release').write_text('x')
            assert first.wait(timeout=60) == 0
            assert (shared / 'tmp/copy-held').exists(), 'nothing may empty the folder of a copy that has just left'
            _, rep = run_report(lab, [], mask=False)
            check(rep, tmp_copy='', shm_copy='')
            assert not (shared / 'tmp/copy-held').exists() and not (shared / 'shm/copy-shm').exists()
            real_shm = [n for n in os.listdir('/dev/shm') if n.startswith('copy-')]
            assert not real_shm, f'the app wrote in the real /dev/shm: {real_shm}'
            print('copies of one app share /tmp and /dev/shm, a start with none alive begins empty: ok')

            # 6e. Stale lock files with a process id: kept while a copy is alive, removed when none is, and never
            # a link's target or anything outside the private folders.
            cfg = h / '.local/share/panpak/userdata' / app_dirs.ID / 'config'
            decoy = h / 'Notes/decoy.lock'
            decoy.write_text('4242\nqbittorrent\nhost\n')
            bystander = h / 'Notes/bystander.txt'
            bystander.write_text('keep\n')

            def plant():
                (cfg / 'App').mkdir(parents=True, exist_ok=True)
                for link in ('SingletonLock', 'SingletonSocket', 'SingletonCookie'):
                    (cfg / 'App' / link).unlink(missing_ok=True)
                (cfg / 'App/SingletonLock').symlink_to('somehost-4242')
                (cfg / 'App/SingletonSocket').symlink_to('/tmp/org.chromium.Chromium.x/SingletonSocket')
                (cfg / 'App/SingletonCookie').symlink_to('123456')
                (cfg / 'qt.lock').write_text('4242\nqbittorrent\nsomehost\n')
                (cfg / 'App/lock').unlink(missing_ok=True)
                (cfg / 'App/lock').symlink_to(str(bystander))   # a link named like a lock that points at a file: not a lock
                (cfg / 'App/other.lock').unlink(missing_ok=True)
                (cfg / 'App/other.lock').symlink_to(str(decoy))  # a link to a real lock-like file outside: never followed
            def hold():
                p = subprocess.Popen([launcher, '--hold'], env=lab.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                for _ in range(300):
                    if (shared / 'tmp/copy-held').exists():
                        return p
                    time.sleep(0.1)
                raise AssertionError('the copy did not start')

            def stale_left():
                return [g for g in ('App/SingletonLock', 'App/SingletonSocket', 'App/SingletonCookie', 'qt.lock') if os.path.lexists(cfg / g)]

            marker = h / '.local/share/panpak/userdata' / (app_dirs.ID + '.in-use')
            # Normal end: the mark is gone, so the next start does not even look (the planted locks stay).
            first = hold()
            assert marker.exists(), 'no mark "in use" while the app runs'
            (shared / 'tmp/copy-release').write_text('x')
            assert first.wait(timeout=60) == 0
            assert not marker.exists(), 'the mark stays after a normal end'
            plant()
            _, rep = run_report(lab, [], mask=False)
            assert len(stale_left()) == 4, f'a normal end was followed by a search: {stale_left()}'
            # Another copy is alive: nothing is removed, and the mark stays until the last copy ends normally.
            first = hold()
            _, rep = run_report(lab, [], mask=False)
            assert len(stale_left()) == 4, 'a lock was removed while a copy is alive'
            assert marker.exists()
            # The app is killed (SIGKILL on the stub; bubblewrap ends the rest): the mark is left, the next start searches.
            first.kill()
            first.wait()
            for _ in range(100):
                if not any(str(h) in Path(f'/proc/{d}/cmdline').read_text(errors='ignore') for d in os.listdir('/proc') if d.isdigit() and os.path.exists(f'/proc/{d}/cmdline') and d != str(os.getpid())):
                    break
                time.sleep(0.1)
            assert marker.exists(), 'a killed run must leave the mark'
            (shared / 'tmp/copy-release').unlink(missing_ok=True)
            _, rep = run_report(lab, [], mask=False)
            for gone in ('App/SingletonLock', 'App/SingletonSocket', 'App/SingletonCookie', 'qt.lock'):
                assert not os.path.lexists(cfg / gone), f'{gone} is stale and was not removed'
            assert os.path.lexists(cfg / 'App/lock') and os.path.lexists(cfg / 'App/other.lock'), 'a link that is not a lock was removed'
            assert decoy.exists() and bystander.read_text() == 'keep\n', 'something outside the private folders was touched'
            assert not marker.exists(), 'the search was followed by a normal end'
            print('stale lock files: no search after a normal end or while a copy is alive, search after SIGKILL, links not followed, nothing else touched: ok')

        # 6e. URL schemes: the desktop entry declares them, the desktop learns of them, the launcher gets the address.
        p = lab.install('[desktop]\nschemes = ["panpaktest", "panpak-two"]\n')
        assert p.returncode == 0, p.stdout + p.stderr
        assert 'URL scheme panpaktest' in p.stdout and 'URL scheme panpak-two' in p.stdout, p.stdout
        entry = (h / f'.local/share/applications/panpak.{app_dirs.ID}.desktop').read_text()
        assert 'MimeType=x-scheme-handler/panpaktest;x-scheme-handler/panpak-two;\n' in entry, entry
        exec_line = next(l for l in entry.splitlines() if l.startswith('Exec='))
        assert exec_line.endswith(' %U') and str(h / '.local/bin' / app_dirs.ID) in exec_line, exec_line
        if shutil.which('update-desktop-database') and shutil.which('xdg-mime'):
            q = subprocess.run(['xdg-mime', 'query', 'default', 'x-scheme-handler/panpaktest'], env=lab.env, capture_output=True, text=True)
            assert q.stdout.strip() == f'panpak.{app_dirs.ID}.desktop', (q.stdout, q.stderr)
        _, rep = run_report(lab, ['panpaktest://open/1?x=y'], mask=False)
        check(rep, args='panpaktest://open/1?x=y')
        try:
            lab.pack('[desktop]\nschemes = ["https"]\n')
            raise SystemExit('a reserved scheme was packed')
        except AssertionError as e:
            assert 'PANPAK-E0007' in str(e) and 'reserved scheme' in str(e), e
        lab.install('')
        assert 'MimeType' not in (h / f'.local/share/applications/panpak.{app_dirs.ID}.desktop').read_text()
        print('URL schemes: declared in the entry, known to the desktop, the address reaches the launcher: ok')

        # 6f. A browser started through the opener belongs to nobody: it is in a session of its own, has no
        # inherited descriptors, and lives on when the app and the stub are gone.
        if view_here:
            lab.install('')
            slow = work / 'slowbin'
            slow.mkdir()
            pids = work / 'browser-pids'
            # The script starts a new shell with -c so that its own script descriptor is not in the list.
            (slow / 'xdg-open').write_text(
                '#!/bin/sh\nexec /bin/sh -c \'echo "$$ $(awk "{print \\$6}" /proc/$$/stat) $(ls /proc/$$/fd | tr "\\n" ,) $1" >> '
                + str(pids) + '; exec sleep 300\' sh "$1"\n')
            (slow / 'xdg-open').chmod(0o755)
            saved = lab.env['PATH']
            lab.env['PATH'] = f'{slow}:{saved}'
            t0 = time.monotonic()
            p, rep = run_report(lab, [], mask=False)
            took = time.monotonic() - t0
            lab.env['PATH'] = saved
            started = [l.split() for l in pids.read_text().splitlines()] if pids.exists() else []
            try:
                assert p.returncode == 0, p.stderr
                assert len(started) >= 2, started
                launcher_sid = os.getsid(0)
                for pid, sid, fds, url in started:
                    assert Path(f'/proc/{pid}').exists(), f'the browser {pid} ended with the app'
                    assert sid == pid and int(sid) != launcher_sid, (pid, sid)
                    assert set(fds.strip(',').split(',')) <= {'0', '1', '2', '3'}, fds
                assert took < 30, took
            finally:
                for pid, *_ in started:
                    try:
                        os.kill(int(pid), 9)
                    except OSError:
                        pass
            print('browser started by the opener: own session, no inherited descriptors, alive after the app ended:', len(started), 'process(es)')

        # 6h. An app that asks for network keeps the network namespace of the host: its datagrams get out.
        if view_here:
            lab.install('[permissions]\nnetwork = true\n')
            net_got.clear()
            _, rep = run_report(lab, [net_arg], mask=False)
            time.sleep(0.3)
            assert rep['ns_net'] == host_ns['net'], 'an app with network does not share the network namespace'
            assert 'udp' in net_got, net_got
            # Names: the resolver socket is in the view for an app that asks for network.
            if subprocess.run(['getent', 'hosts', 'example.com'], capture_output=True).returncode == 0:
                assert rep['dns'] == 'ok', 'name resolution fails for an app with network'
            print('network = true: same network namespace, a datagram reaches the host, names resolve')

        # 6g. Several first starts at once, on a store without the tree: all of them start (defect 19).
        lab.install('')
        trees = h / '.local/share/panpak/trees'
        subprocess.run(['chmod', '-R', 'u+w', str(trees)], check=True)
        isolation.safe_rmtree(trees, work)
        launcher = str(h / '.local/bin' / app_dirs.ID)
        procs = [subprocess.Popen([launcher], env=lab.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, stdin=subprocess.DEVNULL, text=True)
                 for _ in range(6)]
        results = [(q.wait(timeout=120), q.stderr.read()) for q in procs]
        bad = [r for r in results if r[0] != 0]
        assert not bad, bad
        assert len([d for d in os.listdir(trees) if d.endswith('.partial')]) == 0
        print('6 first starts at once on an empty store: all started')

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
        outside.unlink(missing_ok=True)
        outside_srv.close()
        for s in fake.socks:
            s.close()
        teardown(lab)


def BIN_PANPAK():
    return app_dirs.BIN / 'panpak'


if __name__ == '__main__':
    main()
