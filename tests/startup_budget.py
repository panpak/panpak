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
import re
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


STAGE = re.compile(r'debug: \+\s*([0-9.]+) ms  (.*)')


def stage_times(stderr):
    return {m.group(2).strip(): float(m.group(1)) for m in map(STAGE.search, stderr.splitlines()) if m}


def menu_entry_cost(env, launcher, entry, runs, budget, csv=None, label=''):
    """What the lazy update of the menu entry costs at the start (docs/DESKTOP-INTEGRATION.md, section 3), measured by
    the stages that PANPAK_DEBUG prints: from "package checked" to "menu entry checked". Two cases: the entry is current
    (nothing to do: one read of 128 bytes), and the entry is older (it is rewritten, with its launcher)."""
    denv = dict(env, PANPAK_DEBUG='1')
    current = entry.read_text()

    def one():
        r = subprocess.run([str(launcher)], env=denv, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, timeout=120)
        t = stage_times(r.stderr)
        if 'menu entry checked' not in t:
            sys.exit('the stage "menu entry checked" is missing from the debug output:\n' + r.stderr[-600:])
        return t['menu entry checked'] - t['package checked']

    for _ in range(3):
        one()
    nothing = [one() for _ in range(runs)]
    # An entry as the first version wrote it: no version line.
    old = '\n'.join(l for l in current.splitlines() if not l.startswith('X-Panpak-Desktop-Version=')) + '\n'
    rewrite = []
    for _ in range(max(5, runs // 4)):
        entry.write_text(old)
        rewrite.append(one())
    if entry.read_text() != current:
        sys.exit('the older entry was not rewritten to the current one')
    mn, mr = statistics.median(nothing), statistics.median(rewrite)
    print(f'menu entry check: nothing to do: median {mn:.2f} ms, p90 {pct(nothing, 90):.2f} ms (n={len(nothing)}); older entry rewritten: median {mr:.2f} ms, p90 {pct(rewrite, 90):.2f} ms (n={len(rewrite)})')
    if csv:
        new = not csv.exists()
        with open(csv, 'a') as f:
            if new:
                f.writelines(meta_lines('the same script app, started through the installed launcher with PANPAK_DEBUG=1; the time is read from the stages "package checked" and "menu entry checked"',
                                        'nothing_* is the check when the entry is current; rewrite_* is the check when the entry has no version and entry and launcher are rewritten'))
                f.write('label,nothing_runs,nothing_median_ms,nothing_p90_ms,rewrite_runs,rewrite_median_ms,rewrite_p90_ms\n')
            f.write(f'{label},{len(nothing)},{mn:.2f},{pct(nothing, 90):.2f},{len(rewrite)},{mr:.2f},{pct(rewrite, 90):.2f}\n')
    if mn > budget:
        print(f'MENU ENTRY CHECK OVER BUDGET: {mn:.2f} ms > {budget} ms')
        return False
    return True


def meta_lines(app, columns):
    """The comment lines at the top of a new CSV file: the machine, the system, the versions and the date."""
    import datetime
    import platform

    def sh(*argv):
        return subprocess.run(argv, capture_output=True, text=True).stdout.strip()

    cpu = next((l.split(':', 1)[1].strip() for l in open('/proc/cpuinfo') if l.startswith('model name')), '?')
    mem = next((int(l.split()[1]) for l in open('/proc/meminfo') if l.startswith('MemTotal')), 0)
    osr = dict(l.rstrip().split('=', 1) for l in open('/etc/os-release') if '=' in l)
    meta = [('date_utc', datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')), ('cpu', cpu), ('threads', os.cpu_count()),
            ('ram_mib', mem // 1024), ('kernel', platform.release()), ('os', osr.get('PRETTY_NAME', '?').strip('"')),
            ('bubblewrap', sh('/usr/bin/bwrap', '--version')), ('script', 'tests/startup_budget.py'), ('app', app), ('columns', columns)]
    return [f'# {k}: {v}\n' for k, v in meta]


def write_csv(path, label, rel, runs, dbus, view, t_direct, t_launch, t_bare):
    import datetime
    import platform

    def sh(*argv):
        return subprocess.run(argv, capture_output=True, text=True).stdout.strip()

    new = not path.exists()
    with open(path, 'a') as f:
        if new:
            f.writelines(meta_lines('a shell script that exits at once, started directly and through the installed launcher, in turn',
                                    'cost_ms is median(launcher) - median(direct); dbus is the filtered session bus layer; view is whether bubblewrap could make the view'))
            f.write('label,panpak_sha256,stub_sha256,runs,dbus,view,direct_median_ms,direct_p90_ms,launcher_median_ms,launcher_p90_ms,bare_bwrap_median_ms,cost_ms\n')
        h = lambda name: sh('sha256sum', str(rel / name)).split()[0][:16]
        md, ml = statistics.median(t_direct), statistics.median(t_launch)
        mb = f'{statistics.median(t_bare):.1f}' if t_bare else ''
        f.write(f'{label},{h("panpak")},{h("panpak-stub")},{runs},{"yes" if dbus else "no"},{"yes" if view else "no"},{md:.1f},{pct(t_direct, 90):.1f},{ml:.1f},{pct(t_launch, 90):.1f},{mb},{ml - md:.1f}\n')


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--runs', type=int, default=40)
    ap.add_argument('--budget-ms', type=float, default=None, help='default: 30 ms; 45 ms with --dbus (systemd needs 10 to 20 ms to make the scope)')
    ap.add_argument('--dbus', action='store_true', help='ask for notifications, so that the filtered session bus is started')
    ap.add_argument('--entry-budget-ms', type=float, default=0.3, help='what the check of the menu entry may cost when there is nothing to do')
    ap.add_argument('--entry-csv', help='append one row with the cost of the check of the menu entry to this file')
    ap.add_argument('--advisory', action='store_true', help='report only; never fail on the budget')
    ap.add_argument('--bin-dir', default=str(REL), help='folder with panpak and panpak-stub')
    ap.add_argument('--csv', help='append one row to this file (the machine and the versions are written at the top of a new file)')
    ap.add_argument('--label', default='', help='name of this run in the CSV row (for example the commit or the branch)')
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
    entry_ok = menu_entry_cost(env, launcher, home / f'.local/share/applications/panpak.{ID}.desktop', a.runs, a.entry_budget_ms, Path(a.entry_csv) if a.entry_csv else None, a.label)
    if a.csv:
        write_csv(Path(a.csv), a.label, rel, a.runs, a.dbus, view, t_direct, t_launch, t_bare)
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
    if not entry_ok and not a.advisory:
        sys.exit(1)
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
