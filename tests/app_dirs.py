#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""End-to-end test of the app folders and the sandbox permissions.

Usage: python3 tests/app_dirs.py [work-directory]
Build release first (cargo build --release --target x86_64-unknown-linux-musl).
Everything runs in an isolated HOME inside the work directory (see
tests/isolation.py). The app is a shell script that tries a list of reads and
writes and reports each result; this script checks the report.
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

ID = 'org.example.dirs'
PROBE = r'''#!/bin/sh
# Reports "name=ok" or "name=deny" for each attempt, then the environment.
R="$XDG_DATA_HOME/report.txt"
try() { if ( eval "$2" ) 2>/dev/null; then echo "$1=ok"; else echo "$1=deny"; fi; }
{
try write_config 'echo x > "$XDG_CONFIG_HOME/c.txt"'
try write_data 'echo x > "$XDG_DATA_HOME/d.txt"'
try write_state 'echo x > "$XDG_STATE_HOME/s.txt"'
try write_cache 'echo x > "$XDG_CACHE_HOME/k.txt"'
try write_tmpdir 'echo x > "$TMPDIR/t.txt"'
try write_home_config 'mkdir -p "$HOME/.config/something" && echo x > "$HOME/.config/something/x"'
try write_home_file 'echo x > "$HOME/stray.txt"'
try read_gtk 'cat "$HOME/.config/gtk-3.0/settings.ini" >/dev/null'
try write_gtk 'echo x >> "$HOME/.config/gtk-3.0/settings.ini"'
try read_gh 'cat "$HOME/.config/gh/hosts.yml" >/dev/null'
try list_config 'ls "$HOME/.config" >/dev/null'
try read_icons 'ls "$HOME/.local/share/icons" >/dev/null'
try write_icons 'echo x > "$HOME/.local/share/icons/x"'
try read_old_appdata 'ls "$HOME/.local/share/'"$PANPAK_APP_ID"'" >/dev/null'
try read_other_userdata 'ls "$HOME/.local/share/panpak" >/dev/null'
try read_notes 'ls "$HOME/Notes" >/dev/null'
try write_notes 'echo x > "$HOME/Notes/n.txt"'
try read_docs 'cat "$HOME/Docs/ro.txt" >/dev/null'
try write_docs 'echo x > "$HOME/Docs/new.txt"'
try write_tmp 'echo x > /tmp/panpak-dirs-test-$$ && rm /tmp/panpak-dirs-test-$$'
echo "args=$*"
for v in HOME XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME XDG_CACHE_HOME TMPDIR SQLITE_TMPDIR XDG_RUNTIME_DIR XDG_CONFIG_DIRS XDG_DATA_DIRS GTK_THEME FOO IPYTHONDIR; do
  eval "echo env_$v=\${$v-UNSET}"
done
fc-list 2>/dev/null | grep -c "$HOME/.local/share/fonts/panpak-test-font" | sed 's/^/fonts_found=/'
} > "$R"
'''


class Lab:
    def __init__(self, work):
        self.work = work
        self.home = work / 'home'
        isolation.require_isolated_home(self.home, work)
        self.home.mkdir(parents=True)
        self.runtime = isolation.owned_mkdtemp('dirs-rt-')
        self.env = {'PATH': os.environ['PATH'], 'HOME': str(self.home), 'XDG_RUNTIME_DIR': self.runtime}
        self.key = work / 'secret.key'
        out = self.cli('keygen', '-o', str(self.key)).stdout
        self.pub = next(l.split()[-1] for l in out.splitlines() if l.startswith('key:'))
        self.stub = BIN / 'panpak-stub'
        self.n = 0

    def cli(self, *args, check=True, **kw):
        p = subprocess.run([str(BIN / 'panpak'), *map(str, args)], env=self.env, capture_output=True,
                           text=True, timeout=120, stdin=subprocess.DEVNULL, **kw)
        if check:
            assert p.returncode == 0, f'{args}: {p.stdout}{p.stderr}'
        return p

    def pack(self, toml_extra, url=None):
        """Pack the probe app with the extra manifest text; return the package path."""
        self.n += 1
        tree = self.work / f'tree{self.n}'
        (tree / 'bin').mkdir(parents=True)
        probe = tree / 'bin/probe'
        probe.write_text(PROBE)
        probe.chmod(0o755)
        (tree / 'panpak.toml').write_text(
            f'[app]\nid = "{ID}"\nname = "Dirs"\nversion = "{self.n}"\nentry = "bin/probe"\n'
            f'{toml_extra}\n[update]\n{f"url = {chr(34)}{url}{chr(34)}" if url else ""}\nkey = "{self.pub}"\n')
        pkg = self.work / f'p{self.n}.panpak'
        self.cli('pack', tree, '--stub', self.stub, '--secret-file', self.key, '-o', pkg)
        return pkg

    def install(self, toml_extra):
        """Pack and install the probe app with the extra manifest text."""
        pkg = self.pack(toml_extra)
        return self.cli('install', '--yes', '--runtime', self.stub, pkg, check=False)

    def run(self, *args):
        """Run the launcher; return (returncode, report dict, stderr)."""
        report = self.home / f'.local/share/panpak/userdata/{ID}/data/report.txt'
        report.unlink(missing_ok=True)
        p = subprocess.run([str(self.home / '.local/bin' / ID), *args], env=self.env, capture_output=True,
                           text=True, timeout=120, stdin=subprocess.DEVNULL)
        rep = {}
        if report.exists():
            for line in report.read_text().splitlines():
                k, _, v = line.partition('=')
                rep[k] = v
        return p.returncode, rep, p.stderr


def expect(rep, **want):
    bad = {k: (rep.get(k), v) for k, v in want.items() if rep.get(k) != v}
    assert not bad, f'unexpected results (got, wanted): {bad}\nfull report: {rep}'


def main():
    work = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT / 'target/app-dirs'
    if work.exists():
        # Store trees are read-only: make them removable first (inside the work directory only).
        isolation.require_inside(work, work.parent, 'work')
        subprocess.run(['chmod', '-R', 'u+w', str(work)], check=True)
        isolation.safe_rmtree(work, work.parent)
    work.mkdir(parents=True)
    lab = Lab(work)
    h = lab.home
    # The user's own files: theme, a secret, icons, notes, a font, an old app folder.
    (h / '.config/gtk-3.0').mkdir(parents=True)
    (h / '.config/gtk-3.0/settings.ini').write_text('[Settings]\ngtk-application-prefer-dark-theme=1\n')
    (h / '.config/gh').mkdir()
    (h / '.config/gh/hosts.yml').write_text('oauth_token: fake\n')
    (h / '.local/share/icons').mkdir(parents=True)
    (h / '.local/share/fonts').mkdir(parents=True)
    fonts = subprocess.run(['fc-list', ':file'], capture_output=True, text=True).stdout.split(':')
    font = next((f.strip() for f in fonts if f.strip().endswith('.ttf') and Path(f.strip()).is_file()), None)
    assert font, 'no system TTF font to copy'
    shutil.copy(font, h / '.local/share/fonts/panpak-test-font.ttf')
    (h / 'Notes').mkdir()
    (h / 'Docs').mkdir()
    (h / 'Docs/ro.txt').write_text('read me\n')
    old = h / f'.local/share/{ID}'
    old.mkdir(parents=True)
    (old / 'legacy.txt').write_text('old data\n')

    # 1. No launch fields, no home paths: the base layout.
    p = lab.install('')
    assert p.returncode == 0, p.stdout + p.stderr
    rc, rep, err = lab.run('one', 'two')
    assert rc == 0, err
    userdata = h / f'.local/share/panpak/userdata/{ID}'
    cache = h / f'.cache/panpak/{ID}'
    expect(rep, write_config='ok', write_data='ok', write_state='ok', write_cache='ok', write_tmpdir='ok',
           write_home_config='deny', write_home_file='deny', read_gtk='ok', write_gtk='deny', read_gh='deny',
           list_config='deny', read_icons='ok', write_icons='deny', read_old_appdata='deny',
           read_other_userdata='deny', read_notes='deny', write_notes='deny', read_docs='deny', write_docs='deny',
           write_tmp='ok', args='one two', fonts_found='1')
    assert (userdata / 'config/c.txt').exists() and (userdata / 'data/d.txt').exists()
    assert (userdata / 'state/s.txt').exists() and (cache / 'k.txt').exists() and (userdata / 'tmp/t.txt').exists()
    assert not (h / '.config/something').exists() and not (h / 'stray.txt').exists()
    assert (old / 'legacy.txt').read_text() == 'old data\n', 'old app folder was touched'
    for d in [userdata, userdata / 'config', userdata / 'data', userdata / 'state', userdata / 'tmp', cache]:
        assert d.stat().st_mode & 0o777 == 0o700, d
    assert rep['env_HOME'] == str(h) and rep['env_XDG_RUNTIME_DIR'] == lab.runtime
    assert rep['env_XDG_CONFIG_HOME'] == str(userdata / 'config')
    assert rep['env_XDG_DATA_HOME'] == str(userdata / 'data')
    assert rep['env_XDG_STATE_HOME'] == str(userdata / 'state')
    assert rep['env_XDG_CACHE_HOME'] == str(cache)
    assert rep['env_TMPDIR'] == str(userdata / 'tmp') and rep['env_SQLITE_TMPDIR'] == str(userdata / 'tmp')
    assert rep['env_XDG_CONFIG_DIRS'] == f'/etc/xdg:{h}/.config', rep['env_XDG_CONFIG_DIRS']
    assert rep['env_XDG_DATA_DIRS'] == f'/usr/local/share:/usr/share:{h}/.local/share', rep['env_XDG_DATA_DIRS']
    fonts_conf = (userdata / 'config/fontconfig/fonts.conf').read_text()
    assert 'X-Panpak-Generated' in fonts_conf and f'<dir>{h}/.local/share/fonts</dir>' in fonts_conf
    print('base layout: ok')

    # 2. The generated fonts.conf is replaced when stale, kept when the user owns it.
    (userdata / 'config/fontconfig/fonts.conf').write_text('<!-- X-Panpak-Generated -->\n<fontconfig/>\n')
    lab.run()
    assert f'<dir>{h}/.local/share/fonts</dir>' in (userdata / 'config/fontconfig/fonts.conf').read_text()
    (userdata / 'config/fontconfig/fonts.conf').write_text('<fontconfig/>\n')
    lab.run()
    assert (userdata / 'config/fontconfig/fonts.conf').read_text() == '<fontconfig/>\n', 'user fonts.conf overwritten'
    print('fonts.conf marker: ok')

    # 2b. Dark theme for GTK3: from gsettings, once, before the sandbox; never an error.
    fake = work / 'fakebin'
    fake.mkdir()
    gs = fake / 'gsettings'

    def fake_gsettings(scheme, theme, extra=''):
        gs.write_text(f'#!/bin/sh\n{extra}\ncase "$3" in color-scheme) echo "{scheme}";; gtk-theme) echo "{theme}";; esac\n')
        gs.chmod(0o755)

    path = lab.env['PATH']
    lab.env['PATH'] = f'{fake}:{path}'
    fake_gsettings("'prefer-dark'", "'Yaru'")
    expect(lab.run()[1], env_GTK_THEME='Yaru:dark')
    fake_gsettings("'prefer-dark'", "''")
    expect(lab.run()[1], env_GTK_THEME='Adwaita:dark')
    fake_gsettings("'default'", "'Yaru'")
    expect(lab.run()[1], env_GTK_THEME='UNSET')
    fake_gsettings("'prefer-dark'", "'Yaru'", 'sleep 5')
    t0 = time.monotonic()
    rc, rep, err = lab.run()
    assert rc == 0 and rep['env_GTK_THEME'] == 'UNSET' and time.monotonic() - t0 < 3, 'slow gsettings must not block the start'
    fake_gsettings("'prefer-dark'", "'Yaru'")
    lab.env['GTK_THEME'] = 'Custom'
    expect(lab.run()[1], env_GTK_THEME='Custom')
    del lab.env['GTK_THEME']
    gs.unlink()
    rc, rep, err = lab.run()
    assert rc == 0, err
    lab.env['PATH'] = path
    print('dark theme: ok')

    # 3. Missing optional paths (no qt5ct, no ~/.fonts, no kdeglobals) do not stop the start: case 1 had none.
    # 4. Launch arguments and environment with placeholders; home paths ro and rw.
    p = lab.install(
        '[launch]\nargs = ["--profile", "${XDG_CONFIG_HOME}/prof", "--tmp=${TMPDIR}"]\n'
        '[launch.env]\nFOO = "${XDG_STATE_HOME}/foo"\nIPYTHONDIR = "${XDG_CONFIG_HOME}/ipython"\n'
        '[permissions]\n'
        '[[permissions.home_paths]]\npath = "Notes"\naccess = "rw"\n'
        '[[permissions.home_paths]]\npath = "Docs"\naccess = "ro"\n'
        '[[permissions.home_paths]]\npath = "Missing/Dir"\naccess = "rw"\n'
        '[[permissions.home_paths]]\npath = "Absent"\naccess = "ro"\n')
    assert p.returncode == 0, p.stdout + p.stderr
    assert 'Partial access to your home: Notes (rw), Docs (ro), Missing/Dir (rw), Absent (ro)' in p.stdout, p.stdout
    rc, rep, err = lab.run('tail')
    assert rc == 0, err
    expect(rep, write_notes='ok', read_notes='ok', read_docs='ok', write_docs='deny', write_home_file='deny',
           read_gh='deny', args=f'--profile {userdata}/config/prof --tmp={userdata}/tmp tail',
           env_FOO=f'{userdata}/state/foo', env_IPYTHONDIR=f'{userdata}/config/ipython')
    assert (h / 'Notes/n.txt').exists() and not (h / 'Docs/new.txt').exists()
    assert (h / 'Missing/Dir').is_dir(), 'rw home path was not created'
    assert not (h / 'Absent').exists(), 'ro home path was created'
    print('launch fields and home paths: ok')

    # 5. A home path that is a link, or a link inside the path, blocks the start.
    outside = work / 'outside'
    outside.mkdir()
    for name, setup in [('Notes', lambda: ((h / 'Notes').rename(h / 'Notes.real'), (h / 'Notes').symlink_to(outside))),
                        ('Missing/Dir', lambda: ((h / 'Missing').rename(h / 'Missing.real'), (h / 'Missing').symlink_to(outside)))]:
        setup()
        rc, rep, err = lab.run()
        assert rc != 0 and 'PANPAK-E3000' in err and 'symbolic link' in err, (name, rc, err)
        assert not rep, 'the app started although a home path is a link'
        assert list(outside.iterdir()) == [], 'something was created through the link'
        (h / name.split('/')[0]).unlink()
        (h / (name.split('/')[0] + '.real')).rename(h / name.split('/')[0])
    # rw on a file is refused.
    (h / 'Notes').rmdir() if not any((h / 'Notes').iterdir()) else (h / 'Notes/n.txt').unlink()
    (h / 'Notes').rmdir()
    (h / 'Notes').write_text('a file\n')
    rc, rep, err = lab.run()
    assert rc != 0 and 'PANPAK-E3000' in err and 'rw needs a folder' in err, (rc, err)
    (h / 'Notes').unlink()
    (h / 'Notes').mkdir()
    print('links and files as home paths: refused')

    # 5b. A package that asks for the bus socket gets a plain warning at install.
    p = lab.install('[permissions]\nsockets_allow = ["$XDG_RUNTIME_DIR/bus"]\n')
    assert p.returncode == 0 and 'WARNING: Full access to desktop services (D-Bus): this app is not isolated' in p.stdout, p.stdout
    lab.install('')
    print('bus warning: ok')

    # 6. Install shows permissions; refused fields do not install.
    for extra, code in [('[launch.env]\nLD_PRELOAD = "/x"\n', 'PANPAK-E0007'),
                        ('[launch]\nargs = ["${HOME}"]\n', 'PANPAK-E0007'),
                        ('[[permissions.home_paths]]\npath = ".config"\naccess = "rw"\n', 'PANPAK-E0007'),
                        ('[[permissions.home_paths]]\npath = ".config/autostart"\naccess = "rw"\n', 'PANPAK-E0007'),
                        ('[[permissions.home_paths]]\npath = ".local/bin"\naccess = "ro"\n', 'PANPAK-E0007'),
                        ('[[permissions.home_paths]]\npath = "a/../b"\naccess = "ro"\n', 'PANPAK-E0007')]:
        tree = work / 'bad-tree'
        shutil.rmtree(tree, ignore_errors=True)
        (tree / 'bin').mkdir(parents=True)
        (tree / 'bin/probe').write_text('#!/bin/sh\n')
        (tree / 'bin/probe').chmod(0o755)
        (tree / 'panpak.toml').write_text(
            f'[app]\nid = "{ID}"\nname = "Dirs"\nversion = "9"\nentry = "bin/probe"\n{extra}\n[update]\nkey = "{lab.pub}"\n')
        p = lab.cli('pack', tree, '--stub', lab.stub, '--secret-file', lab.key, '-o', work / 'bad.panpak', check=False)
        assert p.returncode != 0 and code in p.stderr, (extra, p.stdout, p.stderr)
    print('hostile launch fields and home paths: refused at pack')

    # 7. data path / data clear.
    p = lab.cli('data', 'path', ID)
    assert str(userdata) in p.stdout and str(cache) in p.stdout, p.stdout
    (userdata / 'data/link-out').symlink_to(outside)
    (outside / 'keep.txt').write_text('keep\n')
    p = lab.cli('data', 'clear', ID, check=False)
    assert p.returncode != 0 and 'PANPAK-E5004' in p.stderr and userdata.exists(), 'clear without --yes must not delete'
    p = lab.cli('data', 'clear', '--yes', ID)
    assert not userdata.exists() and not cache.exists(), p.stdout
    assert (outside / 'keep.txt').read_text() == 'keep\n', 'clear followed a link'
    assert (old / 'legacy.txt').exists(), 'clear touched the old app folder'
    assert (h / 'Notes').is_dir() and (h / 'Docs/ro.txt').exists(), 'clear touched home paths'
    assert (h / '.local/share/panpak/apps' / ID / 'current').exists(), 'clear touched the store'
    p = lab.cli('data', 'clear', '--yes', ID)
    assert 'deleted 0 folder' in p.stdout
    p = lab.cli('data', 'clear', '--yes', '../x', check=False)
    assert p.returncode != 0 and 'PANPAK-E0006' in p.stderr
    # A link in place of a root is refused.
    userdata.parent.mkdir(parents=True, exist_ok=True)
    userdata.symlink_to(outside)
    p = lab.cli('data', 'clear', '--yes', ID, check=False)
    assert p.returncode != 0 and (outside / 'keep.txt').exists()
    userdata.unlink()
    print('data path/clear: ok')

    # 8. An update that asks for new permissions needs a yes; one that does not asks nothing.
    import socket
    sock = socket.socket()
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]
    sock.close()
    serve = work / 'serve'
    serve.mkdir()
    url = f'http://127.0.0.1:{port}/remote.panpak'
    p = lab.cli('install', '--yes', '--runtime', lab.stub, lab.pack('', url), check=False)
    assert p.returncode == 0, p.stdout + p.stderr
    assert 'permissions: none beyond the private app folders' in p.stdout, p.stdout
    server = subprocess.Popen([sys.executable, str(ROOT / 'tests/range_server.py'), str(serve), str(port), 'normal'],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        time.sleep(0.5)
        current = h / f'.local/share/panpak/apps/{ID}/current'
        before = current.readlink()
        more = '[permissions]\nnetwork = true\nnetwork_bind_tcp = [2658]\n[[permissions.home_paths]]\npath = "Notes"\naccess = "rw"\n'
        shutil.copy(lab.pack(more, url), serve / 'remote.panpak')
        update = ['update', '--allow-insecure-http', '--timeout', '10']
        p = lab.cli(*update, ID, check=False)
        assert p.returncode != 0 and 'PANPAK-E5004' in p.stderr, p.stdout + p.stderr
        assert 'asks for permissions' in p.stdout and 'listen on TCP port 2658' in p.stdout and 'access to Notes in your home (rw)' in p.stdout, p.stdout
        assert current.readlink() == before, 'update was published without a yes'
        p = lab.cli(*update, '--yes', ID)
        assert current.readlink() != before and 'Partial access to your home: Notes (rw)' in p.stdout, p.stdout
        before = current.readlink()
        shutil.copy(lab.pack(more, url), serve / 'remote.panpak')
        p = lab.cli(*update, ID)
        assert current.readlink() != before and 'asks for permissions' not in p.stdout, p.stdout
    finally:
        server.terminate()
        server.wait(timeout=5)
    print('update confirmation: ok')
    print('all ok')
    isolation.safe_rmtree(lab.runtime, work)


if __name__ == '__main__':
    main()
