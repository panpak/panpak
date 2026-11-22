#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A test of silverblue_bench.py with fakes of `flatpak` and `panpak` and a fake window log, in a home of its own: it shows that the timing, the counting and the tables
do what they say. It runs nothing of the real systems. Needs `systemd-run --user` and no network (the byte counter reads the default interface, which only has to exist)."""
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent


def main():
    with tempfile.TemporaryDirectory(prefix='bench-selftest-') as tmp:
        tmp = Path(tmp)
        bins = tmp / 'bin'
        bins.mkdir()
        win = tmp / '.cache/panpak-bench/windows.jsonl'
        win.parent.mkdir(parents=True)
        win.write_text(json.dumps({'t': 1.0, 'event': 'enabled'}) + '\n')
        # A fake `flatpak`: run writes the first frame of a window 0.3 s after it starts; kill and install do nothing.
        (bins / 'flatpak').write_text(f'''#!/bin/sh
case "$1" in
  run) sleep 0.3
       python3 - <<PY
import json, time
open("{win}", "a").write(json.dumps({{"t": time.time(), "event": "first-frame", "wm_class": "org.gimp.GIMP", "app_id": "org.gimp.GIMP", "title": "GNU Image Manipulation Program", "pid": __import__("os").getppid()}}) + "\\n")
PY
       sleep 30 ;;
  install) sleep 0.2; mkdir -p "$HOME/.local/share/flatpak"; head -c 3000000 /dev/zero > "$HOME/.local/share/flatpak/blob" ;;
  remotes) echo flathub ;;
  list) echo 'org.gnome.TextEditor\t51.0\t10 MB\tfedora' ;;
  info) echo "info $*" ;;
  *) : ;;
esac
''')
        (bins / 'flatpak').chmod(0o755)
        env = dict(os.environ, HOME=str(tmp), PATH=f'{bins}:{os.environ["PATH"]}', BENCH_RESULTS=str(tmp / 'results/gimp-kdenlive-onlyoffice-qb'))
        run = lambda *a: subprocess.run([sys.executable, str(HERE / 'silverblue_bench.py'), *a], env=env, capture_output=True, text=True, timeout=120)
        p = run('install', '--system', 'flatpak', '--app', 'gimp')
        assert p.returncode == 0, p.stdout + p.stderr
        row = json.loads(p.stdout.splitlines()[-1])
        assert row['kind'] == 'install' and row['space_bytes'] >= 3000000 and row['rx_bytes'] >= 0, row
        p = run('start', '--system', 'flatpak', '--app', 'gimp', '--trials', '3', '--gap', '0.2', '--kind', 'warm')
        assert p.returncode == 0, p.stdout + p.stderr
        rows = [json.loads(l) for l in p.stdout.splitlines() if l.startswith('{')]
        assert len(rows) == 3 and all(r['ok'] and 0.25 < r['window_s'] < 1.5 for r in rows), rows
        p = run('summarize', '--dir', str(tmp / 'results/gimp-kdenlive-onlyoffice-qb'))
        assert p.returncode == 0 and '| gimp-kdenlive-onlyoffice-qb | flatpak | gimp | warm | 3 |' in p.stdout and 'install' not in p.stdout.split('### Installs')[0], p.stdout
        assert '| gimp-kdenlive-onlyoffice-qb | flatpak | gimp | - | 1 |' in p.stdout, p.stdout
        # panpak: the install is made with --expect-key and the key given, never with --trust-first-key; without the key it is refused before anything is downloaded.
        argsf = tmp / 'panpak-args.txt'
        (bins / 'panpak').write_text(f'#!/bin/sh\necho "$@" >> {argsf}\nmkdir -p "$HOME/.local/share/panpak"\nhead -c 1000000 /dev/zero > "$HOME/.local/share/panpak/blob"\n')
        (bins / 'panpak').chmod(0o755)
        (bins / 'curl').write_text('#!/bin/sh\nwhile [ "$1" != "-o" ]; do shift; done\nhead -c 1000 /dev/zero > "$2"\n')
        (bins / 'curl').chmod(0o755)
        env['PANPAK'] = str(bins / 'panpak')
        key = 'ed25519:' + 'ab' * 32
        p = run('install', '--system', 'panpak', '--app', 'gimp', '--pkg-url', 'http://127.0.0.1:1/x.panpak')
        assert p.returncode != 0 and '--expect-key' in (p.stdout + p.stderr) and not argsf.exists(), p.stdout + p.stderr
        p = run('install', '--system', 'panpak', '--app', 'gimp', '--pkg-url', 'http://127.0.0.1:1/x.panpak', '--keys', f'gimp={key}')
        assert p.returncode == 0, p.stdout + p.stderr
        got = argsf.read_text()
        assert f'install --yes --expect-key {key} ' in got and '--trust-first-key' not in got, got
        p = run('install', '--system', 'panpak', '--app', 'gimp', '--pkg-url', 'http://127.0.0.1:1/x.panpak', '--keys', 'gimp=ed25519:xyz')
        assert p.returncode != 0 and 'is not <app>=ed25519' in (p.stdout + p.stderr), p.stdout + p.stderr
        # The Text Editor is not an app of the series: nothing is installed or measured for it; `permissions` and `base` write what the system says.
        p = run('install', '--system', 'flatpak', '--app', 'te')
        assert p.returncode != 0 and 'not an app of the series' in (p.stdout + p.stderr), p.stdout + p.stderr
        p = run('start', '--system', 'flatpak', '--app', 'te', '--trials', '1')
        assert p.returncode != 0 and 'not an app of the series' in (p.stdout + p.stderr), p.stdout + p.stderr
        p = run('permissions', '--system', 'flatpak', '--app', 'te')
        assert p.returncode == 0 and 'info --show-permissions org.gnome.TextEditor' in (tmp / 'results/gimp-kdenlive-onlyoffice-qb/permissions-flatpak-te.txt').read_text(), p.stdout + p.stderr
        p = run('base', '--system', 'flatpak')
        assert p.returncode == 0 and 'org.gnome.TextEditor' in (tmp / 'results/gimp-kdenlive-onlyoffice-qb/base-flatpak.txt').read_text(), p.stdout + p.stderr
        # `*=` gives one key to every app (the converter signs every conversion with one key); the sandbox of an app is read from outside: the processes below the window.
        argsf.unlink()
        p = run('install', '--system', 'panpak', '--app', 'kdenlive', '--pkg-url', 'https://127.0.0.1:1/x.panpak', '--keys', f'*={key}', '--ca', str(tmp / 'ca.pem'))
        assert p.returncode == 0 and f'install --yes --expect-key {key} ' in argsf.read_text(), p.stdout + p.stderr
        p = run('sandbox', '--system', 'flatpak', '--app', 'gimp', '--settle', '0.2')
        assert p.returncode == 0, p.stdout + p.stderr
        row = json.loads(p.stdout.splitlines()[-1])
        assert row['kind'] == 'sandbox' and row['processes'] and all('seccomp' in f and 'shares_with_host' in f for f in row['processes']), row
        # An app with no previous version in both formats has no update measured.
        p = run('update', '--system', 'flatpak', '--app', 'gimp', '--trials', '1')
        assert p.returncode != 0 and 'no previous version in both formats' in (p.stdout + p.stderr), p.stdout + p.stderr
        # No window extension: refused with the way out.
        win.write_text('')
        p = run('start', '--system', 'flatpak', '--app', 'gimp', '--trials', '1')
        assert p.returncode != 0 and 'winwatch@panpak.bench' in (p.stdout + p.stderr), p.stdout + p.stderr
    # A series of reboots needs the automatic login of GDM: without it the machine would stop at the login screen after the first reboot; nothing is armed.
    envr = dict(os.environ, HOME=str(tempfile.mkdtemp()), BENCH_RESULTS=str(tempfile.mkdtemp()))
    envr.pop('BENCH_SKIP_AUTOLOGIN_CHECK', None)
    p = subprocess.run([sys.executable, str(HERE / 'silverblue_bench.py'), 'reboot-series', 'start', '--system', 'flatpak', '--apps', 'gimp', '--count', '1'], capture_output=True, text=True, env=envr)
    if not Path('/etc/gdm/custom.conf').exists() or 'AutomaticLoginEnable' not in Path('/etc/gdm/custom.conf').read_text():
        assert p.returncode != 0 and 'automatic login' in (p.stdout + p.stderr), p.stdout + p.stderr
        assert not (Path(envr['HOME']) / '.config/systemd/user/panpak-bench-reboot.service').exists()
    # The window that counts: the main window of the app, never its screen of start or a dialog of the first start.
    sys.path.insert(0, str(HERE))
    import silverblue_bench as sb
    ff = lambda cls, title, app_id='': {'event': 'first-frame', 'wm_class': cls, 'app_id': app_id, 'title': title}
    assert not sb.is_main_window('gimp', ff('org.gimp.GIMP', 'Starting GIMP', 'org.gimp.GIMP'))
    assert sb.is_main_window('gimp', ff('org.gimp.GIMP', 'GNU Image Manipulation Program', 'org.gimp.GIMP'))
    assert not sb.is_main_window('kdenlive', ff('org.kde.kdenlive', 'Splash Screen \u2014 Kdenlive'))
    assert not sb.is_main_window('kdenlive', ff('org.kde.kdenlive', 'Welcome to Kdenlive'))
    assert sb.is_main_window('kdenlive', ff('org.kde.kdenlive', 'Untitled / HD 1080p 25 fps \u2014 Kdenlive'))
    assert sb.is_main_window('kdenlive', ff('org.kde.kdenlive', 'Untitled / HD 1080p 25 fps \u2014 Kdenlive'))
    assert sb.is_main_window('onlyoffice', ff('ONLYOFFICE', 'ONLYOFFICE'))
    assert not sb.is_main_window('qb', ff('org.qbittorrent.qBittorrent', 'Legal Notice', 'org.qbittorrent.qBittorrent'))
    assert sb.is_main_window('qb', ff('org.qbittorrent.qBittorrent', 'qBittorrent v5.3.0', 'org.qbittorrent.qBittorrent'))
    assert not sb.is_main_window('gimp', {'event': 'created', 'wm_class': 'org.gimp.GIMP', 'title': 'GNU Image Manipulation Program'})
    # The close test takes the closing of the MAIN window, not the screen of start that the app closes by itself.
    sb.APPS['kdenlive']['title']
    ev = [{'event': 'closed', 'pid': 7, 't': 100.0, 'title': 'Splash Screen \u2014 Kdenlive'}, {'event': 'closed', 'pid': 7, 't': 101.0, 'title': 'Welcome to Kdenlive'},
          {'event': 'closed', 'pid': 7, 't': 150.0, 'title': 'Untitled / HD 1080p 25 fps \u2014 Kdenlive'}]
    import re as _re
    hit = [r for r in ev if r['event'] == 'closed' and r['pid'] == 7 and r['t'] > 99.0 and _re.search(sb.APPS['kdenlive']['title'], r['title'])]
    assert [r['t'] for r in hit] == [150.0], hit
    # The end of an app: one signal for both formats, a wait until nothing is left, and "not clean" written when something stays; the second failed start of a series stops the series.
    import types
    calls = []
    sb.sh = lambda cmd, **k: (calls.append(cmd), subprocess.CompletedProcess(cmd, 0, '', ''))[1]
    sb.scopes_of = lambda system, app, unit: [f'{unit}.scope']
    sb.scope_active = lambda name: False
    r = sb.stop('flatpak', 'gimp', 'u1', wait=2)
    assert r['exit_clean'] and r['left'] == [] and any('SIGTERM' in ' '.join(c) for c in calls) and not any('SIGKILL' in ' '.join(c) or c[:2] == ['flatpak', 'kill'] for c in calls), (r, calls)
    calls.clear()
    sb.scope_active = lambda name: True
    r = sb.stop('panpak', 'gimp', 'u2', wait=1)
    assert not r['exit_clean'] and r['left'] == ['u2.scope'] and any('SIGKILL' in ' '.join(c) for c in calls), (r, calls)
    sb.RESULTS = Path(tempfile.mkdtemp())
    sb.count_failure({'app': 'gimp', 'kind': 'warm', 'trial': 0, 'note': 'x'})
    try:
        sb.count_failure({'app': 'gimp', 'kind': 'warm', 'trial': 1, 'note': 'x'})
        raise AssertionError('the second failure did not stop the series')
    except SystemExit as e:
        assert 'STOP' in str(e), e
    # The controlled peer of the qBittorrent operation: metadata by magnet and every piece, checked; and the checklist has its link.
    p = subprocess.run([sys.executable, str(HERE / 'seed_peer.py'), '--self-test'], capture_output=True, text=True, timeout=120)
    assert p.returncode == 0 and 'PASS' in p.stdout, p.stdout + p.stderr
    p = subprocess.run([sys.executable, str(HERE / 'seed_peer.py'), '--address', '0.0.0.0', '--print-magnet'], capture_output=True, text=True)
    assert p.returncode != 0 and 'every interface' in p.stderr + p.stdout
    out = Path(tempfile.mkdtemp()) / 'c.html'
    subprocess.run([sys.executable, str(HERE / 'make_checklist.py'), str(out)], check=True, capture_output=True)
    assert 'x.pe=192.168.122.1:6891' in out.read_text()
    print('selftest PASS')


if __name__ == '__main__':
    main()
