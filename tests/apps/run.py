#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The app harness: does `panpak` work with real apps, and how long does a start take?

Usage: python3 tests/apps/run.py --apps FILE [--full] [--group g,g] [--list] [--no-cache] [--work DIR] [--bin DIR]
                                 [--memory-cap 10G|none] [--swap-cap 6G] [--json FILE] [name ...]

For each app of the list (a TOML file that is not in the repository, `--apps FILE` or PANPAK_APPS; `apps.example.toml` is an example):
get a package (an AppImage is downloaded and converted, a recipe is built with `panpak build`; the package is kept, and made again only when the AppImage, the recipe, `panpak`,
`panpak-stub` or the options of the app change), install it in an isolated HOME, start it on a display of the harness (Xvfb or a headless Sway) and time the first window.
  fast (default): one start per app: does it start, is there an icon, how long to the window.
  --full:         a first start, then five more; the time is the median of the five (the first start, which makes the tree in the store, is left out).
The whole run is in a systemd scope with a memory cap (default MemoryMax 10G, MemorySwapMax 6G): a build that runs the machine out of memory is closed, not the desktop.
At the end: the table, the duration of each phase, the total and the peak memory. Exit status 1 when an app did not start. docs/APP-HARNESS.md explains the numbers.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
import appcfg  # noqa: E402
import display  # noqa: E402
import isolation  # noqa: E402
import pipeline  # noqa: E402

ROOT = HERE.parents[1]
SCOPE_ENV = "PANPAK_APPS_IN_SCOPE"


def own_peak():
    """memory.peak of the cgroup this process is in (the scope), in bytes; None when it cannot be read."""
    try:
        line = next(ln for ln in Path("/proc/self/cgroup").read_text().splitlines() if ln.startswith("0::"))
        return int((Path("/sys/fs/cgroup") / line[4:].lstrip("/") / "memory.peak").read_text())
    except (OSError, StopIteration, ValueError):
        return None


def gib(n):
    return f"{n / (1 << 30):.1f}G"


def parse_args(argv):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0], formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("names", nargs="*", help="apps to run (default: all, or those of --group)")
    ap.add_argument("--apps", default=os.environ.get("PANPAK_APPS"), help="the list of apps (TOML); default: $PANPAK_APPS")
    ap.add_argument("--full", action="store_true", help="a first start and the median of five more")
    ap.add_argument("--group", default="", help="groups to run, separated by commas (gtk, qt, electron, cli, source, appimage, ...)")
    ap.add_argument("--list", action="store_true", help="print the apps and their groups, and stop")
    ap.add_argument("--no-cache", action="store_true", help="make every package again")
    ap.add_argument("--work", default=os.environ.get("PANPAK_APPS_WORK", str(ROOT / "target/apps-harness")), help="the folder of the harness (packages, downloads, runs)")
    ap.add_argument("--bin", default=os.environ.get("PANPAK_APPS_BIN", str(ROOT / "target/x86_64-unknown-linux-musl/release")), help="the folder with panpak and panpak-stub")
    ap.add_argument("--memory-cap", default="10G", help="MemoryMax of the scope, or none")
    ap.add_argument("--swap-cap", default="6G", help="MemorySwapMax of the scope")
    ap.add_argument("--json", help="write the results to this file")
    return ap.parse_args(argv)


def in_scope(args, argv):
    """Run again inside a systemd user scope with a memory cap (once). Returns the exit status, or None when the run is to go on here."""
    if args.memory_cap == "none" or os.environ.get(SCOPE_ENV):
        return None
    if not shutil.which("systemd-run") or subprocess.run(["systemd-run", "--user", "--scope", "--quiet", "--collect", "--", "true"], capture_output=True).returncode != 0:
        print("WARNING: no systemd user scope here: the run has no memory cap (--memory-cap none says it on purpose)", file=sys.stderr)
        return None
    cmd = ["systemd-run", "--user", "--scope", "--quiet", "--collect", "-p", f"MemoryMax={args.memory_cap}", "-p", f"MemorySwapMax={args.swap_cap}", "--", sys.executable, str(Path(__file__).resolve()), *argv]
    return subprocess.run(cmd, env={**os.environ, SCOPE_ENV: "1"}).returncode


def one(ctx, app, disp_of, full, use_cache):
    """All the phases for one app; returns the row."""
    row = dict(app=app.name, id=app.id, display=app.display, starts=False, icon=False, window_s=None, package_s=None, cached=None, install_s=None, starts_s=None, notes=[], error=None)
    try:
        pkg, secs, cached, notes = pipeline.ensure_package(ctx, app, use_cache)
        row.update(package_s=round(secs, 1), cached=cached, notes=list(notes))
        disp = disp_of(app.display)
        run = pipeline.Run(ctx, app)
        run.prepare()
        t0 = time.monotonic()
        run.install(pkg, disp)
        run.seed_files()
        row["install_s"] = round(time.monotonic() - t0, 1)
        row["icon"] = run.icon_ok()
        t0 = time.monotonic()
        if app.cli:
            times = run.time_cli(disp, 6 if full else 1)
            row.update(starts=True, window_s=round(pipeline.median_after_first(times) if full else times[0], 3))
        else:
            times = []
            for _ in range(6 if full else 1):
                w, rc = run.start_once(disp)
                times.append(w)
                if w is None:
                    break
            ok = all(t is not None for t in times)
            row["starts"] = ok
            if ok:
                row["window_s"] = round(pipeline.median_after_first(times) if full else times[0], 3)
            else:
                row["error"] = "no window appeared"
        row["starts_s"] = round(time.monotonic() - t0, 1)
    except (pipeline.StepError, display.Missing) as e:
        row["error"] = str(e)
    return row


def table(rows, full, total, phases):
    lines = [f"mode: {'full (median of 5 starts, the first left out)' if full else 'fast (1 start per app)'}; {len(rows)} app(s)", ""]
    hdr = f"{'app':<16}{'starts':<8}{'icon':<6}{'window s':>9}{'package':>11}{'install':>9}{'starts':>9}"
    lines.append(hdr)
    for r in rows:
        pk = "cached" if r["cached"] else (f"{r['package_s']:.1f}s" if r["package_s"] is not None else "-")
        f = lambda v, u="s": "-" if v is None else f"{v}{u}"  # noqa: E731
        lines.append(f"{r['app']:<16}{'yes' if r['starts'] else 'NO':<8}{'yes' if r['icon'] else 'no':<6}{f(r['window_s'], ''):>9}{pk:>11}{f(r['install_s']):>9}{f(r['starts_s']):>9}")
    lines.append("")
    lines.append(f"phases: package {phases['package']:.1f}s, install {phases['install']:.1f}s, starts {phases['starts']:.1f}s; total {total:.1f}s")
    for r in rows:
        for n in r["notes"]:
            lines.append(f"  {r['app']}: {n}")
        if r["error"]:
            lines.append(f"  {r['app']}: FAILED: {r['error']}")
    return "\n".join(lines)


def main(argv):
    args = parse_args(argv)
    if not args.apps:
        print("no list of apps: give --apps FILE or set PANPAK_APPS (tests/apps/apps.example.toml is an example)", file=sys.stderr)
        return 2
    try:
        apps, settings = appcfg.load(args.apps)
        chosen = appcfg.select(apps, [g for g in args.group.split(",") if g], args.names)
    except appcfg.ConfigError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    if args.list:
        for a in chosen:
            print(f"{a.name:<16}{'recipe' if a.source else 'AppImage':<10}{a.display:<9}{','.join(sorted(a.groups))}")
        return 0
    code = in_scope(args, argv)
    if code is not None:
        return code
    t_all = time.monotonic()
    try:
        ctx = pipeline.Ctx(args.work, args.bin, os.environ.get("PANPAK_APPS_DOWNLOADS") or settings.get("download_dir"))
    except pipeline.StepError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    displays = {}

    def disp_of(kind):
        if kind not in displays:
            displays[kind] = display.X11Display() if kind == "x11" else display.WaylandDisplay()
        return displays[kind]

    rows = []
    try:
        for app in chosen:
            print(f"-- {app.name}", file=sys.stderr, flush=True)
            r = one(ctx, app, disp_of, args.full, not args.no_cache)
            rows.append(r)
            print(f"   {'ok' if r['starts'] else 'FAILED'} window {r['window_s']}", file=sys.stderr, flush=True)
    finally:
        for d in displays.values():
            d.close()
    phases = dict(package=sum(r["package_s"] or 0 for r in rows), install=sum(r["install_s"] or 0 for r in rows), starts=sum(r["starts_s"] or 0 for r in rows))
    total = time.monotonic() - t_all
    print(table(rows, args.full, total, phases))
    peak = own_peak()
    if peak:
        print(f"peak memory of the run: {gib(peak)} (the scope, page cache included)")
    if args.json:
        Path(args.json).write_text(json.dumps(dict(full=args.full, rows=rows, phases=phases, total_s=round(total, 1), peak_memory=peak), indent=1))
    return 0 if all(r["starts"] and r["error"] is None for r in rows) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
