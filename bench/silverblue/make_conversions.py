#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Step 9b: the conversions of the series, made with `panpak convert` from the official AppImages of the authors.

Usage: python3 make_conversions.py --appimages DIR --out DIR --key FILE --update-url-base URL [--panpak DIR] [APP ...]

These are **conversions made by us, not packages of the authors**: every package is signed with one key, the key of the converter (`--key`), and says so (`[origin]`: the file it was made from, its SHA-256,
`signed_by_upstream = false`). Each AppImage is checked against the SHA-256 that its author publishes before it is converted; a file that does not match is refused. For each app two versions are converted (the
previous one and the current one: the pair of the measure of an update) with the permissions of `conversions/<app>.toml` (the functions that the Flatpak gives, as far as Panpak has an equivalent), and the
current one follows the previous one (`--previous`, so the revision goes up). The files are `<id>.prev.panpak` and `<id>.panpak`; `[update].url` of both is `<update-url-base>/<id>.panpak`.
Runs in a home of its own, with no session bus.
"""
import argparse
import hashlib
import os
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]

# (id, name, permissions file, [(version, file name, SHA-256 published by the author)], entry or None, window class) oldest first. The window class is the one the windows of the app have for the desktop
# (the Wayland app id or the X11 class, read from a real start: the `first-frame` lines of the window extension); the desktop groups the windows under the entry of the menu and shows its icon by it, and the
# one in the desktop file of an AppImage is often not it (GIMP's names `org.gimp.GIMP.Stable`, its windows are `gimp`). The entry is given when the AppImage names a link as its program
# (GIMP: its AppRun sets the environment that the program needs (GIO_MODULE_DIR for TLS, the pixbuf loaders, the typelibs, the bundled loader for its plug-ins), so it is the entry: the program started by itself loses all of it).
# Text that the conversion adds at the end of a file of the program (`convert --append <path in the tree>=<file>`): GIMP's system gimprc, where its check for updates is turned off
# (the updates come from Panpak, so the program does not ask a server of its own). The user's own gimprc is read after it and can say otherwise.
APPENDS = {
    'gimp': [('usr/etc/gimp/3.0/gimprc', 'conversions/gimp-gimprc.append')],
}

APPS = {
    'gimp': ('org.gimp.GIMP', 'GIMP', 'conversions/gimp.toml', [
        ('3.2.6', 'GIMP-3.2.6-x86_64.AppImage', '79ea41bc9b06f78fda181849a9ca8e42d83f1124dedb756f4d52352e2465010f'),
        ('3.2.8', 'GIMP-3.2.8-x86_64.AppImage', '8b43dc23e58a1b66ace9dd6d4d5f7e8532f3398e6109f95a1ec0b6ad036aa250')], 'AppRun', 'gimp'),
    'kdenlive': ('org.kde.kdenlive', 'Kdenlive', 'conversions/kdenlive.toml', [
        ('26.08.2', 'kdenlive-26.08.2-x86_64.AppImage', '3194c0979bcfb7ec4a0b33ebc05178a51cffdf5b180c3a89bcf4da94e025bc78'),
        ('26.08.3', 'kdenlive-26.08.3-x86_64.AppImage', '2c8322edfc86605e4c98a78a019310d9b4fc46bb8e94e6fe313dc7dd987e7671')], None, 'org.kde.kdenlive'),
    'onlyoffice': ('org.onlyoffice.desktopeditors', 'ONLYOFFICE Desktop Editors', 'conversions/onlyoffice.toml', [
        ('9.4.0', 'DesktopEditors-9.4.0-x86_64.AppImage', 'f5faf24552665262fe94486a510b997d2e5a24bde14df94c802c79bf17f9254c'),
        ('10.0.0', 'DesktopEditors-10.0.0-x86_64.AppImage', '3e4668d44f6881d1d8873b38ea9c9fd81b9c77132facdb735bfebfe960edb745')], None, 'ONLYOFFICE'),
}


def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--appimages', required=True)
    ap.add_argument('--out', required=True)
    ap.add_argument('--key', required=True, help='the signing key of the converter (`panpak keygen -o`), one for every conversion')
    ap.add_argument('--update-url-base', required=True, help='the address (https) that serves <id>.panpak, for example https://192.168.122.1:8443')
    ap.add_argument('--level', type=int, default=19, help='the zstd level of the chunks (`convert --level`); the series are publications, so 19 as `pack` uses')
    ap.add_argument('--panpak', default=str(ROOT / 'target/x86_64-unknown-linux-musl/release'), help='the folder with panpak and panpak-stub')
    ap.add_argument('apps', nargs='*', help=f'which of {sorted(APPS)} (default: all)')
    a = ap.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    home = out / '.home'
    (home / 'run').mkdir(parents=True, exist_ok=True)
    (home / 'run').chmod(0o700)
    env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / 'config'), XDG_DATA_HOME=str(home / 'data'), XDG_CACHE_HOME=str(home / 'cache'), XDG_RUNTIME_DIR=str(home / 'run'),
               DBUS_SESSION_BUS_ADDRESS='unix:path=/nonexistent/panpak-no-bus', LC_ALL='C', PANPAK_NO_BACKGROUND='1')
    panpak, stub = Path(a.panpak) / 'panpak', Path(a.panpak) / 'panpak-stub'
    for name in a.apps or sorted(APPS):
        app_id, title, perms, versions, entry, wm_class = APPS[name]
        previous = None
        for i, (version, fname, want) in enumerate(versions):
            src = Path(a.appimages) / fname
            got = sha256(src)
            if got != want:
                sys.exit(f'{src}: SHA-256 {got} is not the one the author publishes ({want}): refused')
            dest = out / (f'{app_id}.prev.panpak' if i == 0 else f'{app_id}.panpak')
            cmd = [panpak, 'convert', src, '--id', app_id, '--name', title, '--version', version, '--stub', stub, '--secret-file', a.key, '--permissions', HERE / perms, '--wm-class', wm_class,
                   '--update-url', f'{a.update_url_base.rstrip("/")}/{app_id}.panpak', '--level', str(a.level), '-o', dest]
            if entry:
                cmd += ['--entry', entry]
            for path_in_tree, text_file in APPENDS.get(name, []):
                cmd += ['--append', f'{path_in_tree}={HERE / text_file}']
            if previous:
                cmd += ['--previous', previous]
            p = subprocess.run([str(c) for c in cmd], env=env, capture_output=True, text=True)
            sys.stdout.write(f'{app_id} {version}: ' + ('ok' if p.returncode == 0 else 'FAILED') + f' {dest.name}\n')
            if p.returncode != 0:
                sys.exit(p.stdout + p.stderr)
            for line in (p.stdout + p.stderr).splitlines():
                if 'WARNING' in line or 'note:' in line or 'appended' in line:
                    print('   ', line)
            previous = dest
    print('done: conversions made by us from the AppImages of the authors, signed with the key of the converter')


if __name__ == '__main__':
    main()
