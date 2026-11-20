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
open("{win}", "a").write(json.dumps({{"t": time.time(), "event": "first-frame", "wm_class": "org.gnome.Sudoku", "app_id": "org.gnome.Sudoku", "title": "x", "pid": 1}}) + "\\n")
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
        env = dict(os.environ, HOME=str(tmp), PATH=f'{bins}:{os.environ["PATH"]}', BENCH_RESULTS=str(tmp / 'results/su-mj-qb'))
        run = lambda *a: subprocess.run([sys.executable, str(HERE / 'silverblue_bench.py'), *a], env=env, capture_output=True, text=True, timeout=120)
        p = run('install', '--system', 'flatpak', '--app', 'su')
        assert p.returncode == 0, p.stdout + p.stderr
        row = json.loads(p.stdout.splitlines()[-1])
        assert row['kind'] == 'install' and row['space_bytes'] >= 3000000 and row['rx_bytes'] >= 0, row
        p = run('start', '--system', 'flatpak', '--app', 'su', '--trials', '3', '--gap', '0.2', '--kind', 'warm')
        assert p.returncode == 0, p.stdout + p.stderr
        rows = [json.loads(l) for l in p.stdout.splitlines() if l.startswith('{')]
        assert len(rows) == 3 and all(r['ok'] and 0.25 < r['window_s'] < 1.5 for r in rows), rows
        p = run('summarize', '--dir', str(tmp / 'results/su-mj-qb'))
        assert p.returncode == 0 and '| su-mj-qb | flatpak | su | warm | 3 |' in p.stdout and 'install' not in p.stdout.split('### Installs')[0], p.stdout
        assert '| su-mj-qb | flatpak | su | - | 1 |' in p.stdout, p.stdout
        # panpak: the install is made with --expect-key and the key given, never with --trust-first-key; without the key it is refused before anything is downloaded.
        argsf = tmp / 'panpak-args.txt'
        (bins / 'panpak').write_text(f'#!/bin/sh\necho "$@" >> {argsf}\nmkdir -p "$HOME/.local/share/panpak"\nhead -c 1000000 /dev/zero > "$HOME/.local/share/panpak/blob"\n')
        (bins / 'panpak').chmod(0o755)
        (bins / 'curl').write_text('#!/bin/sh\nwhile [ "$1" != "-o" ]; do shift; done\nhead -c 1000 /dev/zero > "$2"\n')
        (bins / 'curl').chmod(0o755)
        env['PANPAK'] = str(bins / 'panpak')
        key = 'ed25519:' + 'ab' * 32
        p = run('install', '--system', 'panpak', '--app', 'su', '--pkg-url', 'http://127.0.0.1:1/x.panpak')
        assert p.returncode != 0 and '--expect-key' in (p.stdout + p.stderr) and not argsf.exists(), p.stdout + p.stderr
        p = run('install', '--system', 'panpak', '--app', 'su', '--pkg-url', 'http://127.0.0.1:1/x.panpak', '--keys', f'su={key}')
        assert p.returncode == 0, p.stdout + p.stderr
        got = argsf.read_text()
        assert f'install --yes --expect-key {key} ' in got and '--trust-first-key' not in got, got
        p = run('install', '--system', 'panpak', '--app', 'su', '--pkg-url', 'http://127.0.0.1:1/x.panpak', '--keys', 'su=ed25519:xyz')
        assert p.returncode != 0 and 'is not <app>=ed25519' in (p.stdout + p.stderr), p.stdout + p.stderr
        # The Text Editor is not an app of the series: nothing is installed or measured for it; `permissions` and `base` write what the system says.
        p = run('install', '--system', 'flatpak', '--app', 'te')
        assert p.returncode != 0 and 'not an app of the series' in (p.stdout + p.stderr), p.stdout + p.stderr
        p = run('start', '--system', 'flatpak', '--app', 'te', '--trials', '1')
        assert p.returncode != 0 and 'not an app of the series' in (p.stdout + p.stderr), p.stdout + p.stderr
        p = run('permissions', '--system', 'flatpak', '--app', 'te')
        assert p.returncode == 0 and 'info --show-permissions org.gnome.TextEditor' in (tmp / 'results/su-mj-qb/permissions-flatpak-te.txt').read_text(), p.stdout + p.stderr
        p = run('base', '--system', 'flatpak')
        assert p.returncode == 0 and 'org.gnome.TextEditor' in (tmp / 'results/su-mj-qb/base-flatpak.txt').read_text(), p.stdout + p.stderr
        # No window extension: refused with the way out.
        win.write_text('')
        p = run('start', '--system', 'flatpak', '--app', 'su', '--trials', '1')
        assert p.returncode != 0 and 'winwatch@panpak.bench' in (p.stdout + p.stderr), p.stdout + p.stderr
    print('selftest PASS')


if __name__ == '__main__':
    main()
