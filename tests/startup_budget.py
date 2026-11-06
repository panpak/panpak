#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The fixed cost of starting an app through panpak, against running the same program directly.

Usage: python3 tests/startup_budget.py [--runs N] [--budget-ms MS] [--dbus] [--advisory] [--work DIR]
Build first: `cargo build --release --target x86_64-unknown-linux-musl`.

The app is a two-line shell script that ends at once. It is started
- directly (`/bin/sh <script>`): the baseline;
- through the installed launcher: wrapper, outer stub, package check, view (bubblewrap), inner stub, Landlock and
  seccomp, and with `--dbus` the filtered session bus (xdg-dbus-proxy and a systemd scope) as well;
- as `bwrap --unshare-all ... true`, to show how much of the cost is bubblewrap itself (only reported).
The three are run in turn, N times each, and the medians are compared: the cost is
`median(launcher) - median(direct)`. The medians, the 90th percentiles and the verdict are printed. Exit status 1 when
the cost is over the budget (30 ms by default, 45 ms with `--dbus`), unless `--advisory`, which only reports.

When the view cannot be made (a runner whose kernel refuses user namespaces to bubblewrap), the script says so
and measures the start without the view; that number is not comparable with the budget and is marked.
"""
import argparse
import os
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REL = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402

ID = 'org.example.budget'
APP = '#!/bin/sh\nexit 0\n'


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(p / 100 * (len(xs) - 1))))]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--runs', type=int, default=40)
    ap.add_argument('--budget-ms', type=float, default=None, help='default: 30 ms; 45 ms with --dbus (systemd needs 10 to 20 ms to make the scope)')
    ap.add_argument('--dbus', action='store_true', help='ask for notifications, so that the filtered session bus is started')
    ap.add_argument('--advisory', action='store_true', help='report only; never fail on the budget')
    ap.add_argument('--bin-dir', default=str(REL), help='folder with panpak and panpak-stub')
    ap.add_argument('--work', default=str(ROOT / 'target/startup-budget'))
    a = ap.parse_args()
    if a.budget_ms is None:
        a.budget_ms = 45.0 if a.dbus else 30.0
    rel = Path(a.bin_dir)
    for b in (rel / 'panpak', rel / 'panpak-stub'):
        if not b.exists():
            sys.exit(f'{b} is missing: build first (see the top of this file)')
    work = Path(a.work)
    isolation.safe_rmtree(work, work.parent)
    home = work / 'home'
    isolation.require_isolated_home(home, work)
    home.mkdir(parents=True)
    runtime = Path(isolation.owned_mkdtemp('budget-rt-'))
    env = {'PATH': os.environ['PATH'], 'HOME': str(home), 'XDG_RUNTIME_DIR': str(runtime)}
    for k in ('DBUS_SESSION_BUS_ADDRESS', 'WAYLAND_DISPLAY', 'DISPLAY', 'LANG'):
        if k in os.environ:
            env[k] = os.environ[k]
    if a.dbus:
        # The user bus of the real session, so that the proxy has something to filter. Read-only use.
        real_rt = f'/run/user/{os.getuid()}'
        if not os.path.exists(f'{real_rt}/bus'):
            sys.exit('--dbus needs a user session bus')
        env['XDG_RUNTIME_DIR'] = real_rt
        env['DBUS_SESSION_BUS_ADDRESS'] = f'unix:path={real_rt}/bus'

    def run(binary, *args):
        return subprocess.run([str(binary), *map(str, args)], env=env, capture_output=True, text=True, timeout=120)

    key = work / 'secret.key'
    out = run(rel / 'panpak', 'keygen', '-o', key).stdout
    pub = next(line.split()[-1] for line in out.splitlines() if line.startswith('key:'))
    tree = work / 'tree'
    (tree / 'bin').mkdir(parents=True)
    script = tree / 'bin/app'
    script.write_text(APP)
    script.chmod(0o755)
    perms = '[permissions]\nnotifications = true\n' if a.dbus else ''
    (tree / 'panpak.toml').write_text(f'[app]\nid = "{ID}"\nname = "Budget"\nversion = "1"\nentry = "bin/app"\n[update]\nkey = "{pub}"\n{perms}')
    pkg = work / 'budget.panpak'
    p = run(rel / 'panpak', 'pack', tree, '--stub', rel / 'panpak-stub', '--secret-file', key, '-o', pkg)
    if p.returncode:
        sys.exit(p.stderr)
    p = run(rel / 'panpak', 'install', '--yes', '--runtime', rel / 'panpak-stub', pkg)
    if p.returncode:
        sys.exit(p.stdout + p.stderr)
    launcher = home / '.local/bin' / ID

    direct = ['/bin/sh', str(script)]
    bare = ['/usr/bin/bwrap', '--unshare-all', '--ro-bind', '/usr', '/usr', '--symlink', 'usr/bin', '/bin', '--symlink', 'usr/lib', '/lib',
            '--symlink', 'usr/lib64', '/lib64', '/bin/true']
    view_ok = subprocess.run(bare, capture_output=True).returncode == 0
    d = run(rel / 'panpak', 'doctor')
    view_line = next((l for l in d.stdout.splitlines() if 'isolated view' in l), '?')
    view = 'isolated view: not available' not in view_line and view_ok and 'NOTE isolated view' not in d.stdout

    def once(cmd):
        t = time.perf_counter()
        r = subprocess.run(cmd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        ms = (time.perf_counter() - t) * 1000
        if r.returncode:
            sys.exit(f'{cmd[0]} failed ({r.returncode}): {r.stderr.decode(errors="replace")[:300]}')
        return ms

    for _ in range(3):  # warm the page cache and the first-run caches
        once(direct); once([str(launcher)])
        if view_ok:
            once(bare)
    t_direct, t_launch, t_bare = [], [], []
    for _ in range(a.runs):
        t_direct.append(once(direct))
        t_launch.append(once([str(launcher)]))
        if view_ok:
            t_bare.append(once(bare))

    md, ml = statistics.median(t_direct), statistics.median(t_launch)
    cost = ml - md
    print(f'runs: {a.runs} each, interleaved; session bus layer: {"on" if a.dbus else "off"}; isolated view: {"yes" if view else "NO (start without the view)"}')
    print(f'direct   (/bin/sh script):  median {md:6.1f} ms   p90 {pct(t_direct, 90):6.1f} ms')
    print(f'launcher (panpak):          median {ml:6.1f} ms   p90 {pct(t_launch, 90):6.1f} ms')
    if t_bare:
        mb = statistics.median(t_bare)
        print(f'bare bwrap (true):          median {mb:6.1f} ms   p90 {pct(t_bare, 90):6.1f} ms   (bubblewrap alone; panpak adds {ml - mb - md:+.1f} ms to it)')
    print(f'fixed cost of panpak: {cost:.1f} ms (budget {a.budget_ms:.0f} ms)')
    run(rel / 'panpak', 'remove', ID)
    if a.dbus:
        # The runtime folder is the real one: take away only the folder of this test app.
        mine = Path(env['XDG_RUNTIME_DIR']) / 'panpak' / ID
        if mine.is_dir() and not mine.is_symlink():
            shutil.rmtree(mine, ignore_errors=True)
        isolation.remove_owned(runtime, work)
    else:
        isolation.remove_owned(runtime, work)
    isolation.safe_rmtree(work, work.parent)
    if not view:
        print('NOTE: no isolated view here, so this is the cost of the start without it; not compared with the budget')
        return
    if cost > a.budget_ms:
        print(f'OVER BUDGET by {cost - a.budget_ms:.1f} ms' + (' (advisory: not failing)' if a.advisory else ''))
        if not a.advisory:
            sys.exit(1)
    else:
        print('within budget')


if __name__ == '__main__':
    main()
