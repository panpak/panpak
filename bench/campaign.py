#!/usr/bin/env python3
"""OB2 campaign: per version: install (timed) -> du -> startup bench.
Usage: python3 campaign.py [--yes] 2>&1 | tee campaign.log ; results in results.json
Importing executes nothing (everything in main()). Wiping the bench store
requires explicit confirmation (--yes or PANPAK_BENCH_WIPE=1) and uses a
dedicated directory (PANPAK_STORE or <bench-home>/.local/share/panpak),
never the user's ~/.local/share/panpak.
Environment variables (see bench/README.md): PANPAK_BENCH_DIR,
PANPAK_PKGS_DIR, PANPAK_PKG_EXT, PANPAK_STORE, PANPAK_BENCH_HOME,
PANPAK_BENCH_WIPE, PANPAK_APPIMAGE_DIR.
The bench HOME, the bench store and the AppImage copy directory MUST be inside
PANPAK_BENCH_DIR (the work directory) and the HOME must not be your real home;
PANPAK_ALLOW_REAL_HOME=1 skips only the HOME check (see tests/isolation.py).
"""
import argparse
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tests"))
import isolation
import measure as M
from measure import bench, du, install_panpak

B = os.environ.get("PANPAK_BENCH_DIR", os.path.expanduser("~/panpak-bench"))
PK = M.PK
EXT = M.EXT
BASE_ENV = M.BASE_ENV
OFF = M.OFF


def wipe_store(store):
    # Never delete outside the work directory.
    isolation.require_inside(store, B, "the bench store")
    # Read-only trees: chmod first.
    for d in ["apps", "packages", "trees", "blobs", "chunks", "farms"]:
        p = os.path.join(store, d)
        subprocess.run(["chmod", "-R", "u+w", p], capture_output=True)
        subprocess.run(["rm", "-rf", p], capture_output=True)
        os.makedirs(p, exist_ok=True)


def main():
    ap = argparse.ArgumentParser(description="benchmark panpak vs AppImage (deletes and reuses the bench store)")
    ap.add_argument("--yes", action="store_true",
                    help="confirm deletion of the dedicated bench store")
    args = ap.parse_args()

    # Dedicated bench HOME BEFORE any user path: Rust runtime
    # (store, launchers, desktop) and Python paths all follow from here.
    os.environ["HOME"] = os.environ.get("PANPAK_BENCH_HOME", os.path.join(B, "home"))
    isolation.require_isolated_home(os.environ["HOME"], B)
    store = M.store_dir()
    isolation.require_inside(store, B, "the bench store")
    # AppImage copies go inside the work directory, never to ~/Applications.
    APPIMG_DIR = os.environ.get("PANPAK_APPIMAGE_DIR", os.path.join(B, "appimages"))
    isolation.require_inside(APPIMG_DIR, B, "the AppImage directory")
    launch = M.launch_dir()
    confirmed = args.yes or os.environ.get("PANPAK_BENCH_WIPE") == "1"
    if not confirmed:
        print(f"refused: this would delete the bench store {store}")
        print("Rerun with --yes or PANPAK_BENCH_WIPE=1 (dedicated store only, never the user store)")
        return 2

    RES = {"startup": {}, "disk": {}, "update": {}, "install": {},
           "env": {"kernel": os.uname().release,
                   "cpu": open("/proc/cpuinfo").read().split("model name")[1].split(":")[1].split("\n")[0].strip(),
                   "fs_home": subprocess.run(["stat", "-f", "-c", "%T", os.path.expanduser("~")], capture_output=True, text=True).stdout.strip(),
                   "rustc": "1.99.1", "abi": "Landlock V8"}}

    wipe_store(store)
    RES["disk"]["store_base_runtimesolo"] = du(store)
    print("store base:", RES["disk"]["store_base_runtimesolo"], flush=True)

    PLAN = [
        ("ail22", f"{PK}/ail-2.2.0{EXT}", "org.example.ail",
         [f"{launch}/org.example.ail", "--help"], dict(BASE_ENV),
         f"{B}/appimagelauncher-lite-2.2.0-travis995-0f91801-x86_64.AppImage"),
        ("ail30", f"{PK}/ail-3.0.0{EXT}", "org.example.ail",
         [f"{launch}/org.example.ail", "--help"], dict(BASE_ENV),
         f"{B}/appimagelauncher-lite-3.0.0-beta-2-gha287-x86_64.AppImage"),
        ("qon8", f"{PK}/qon-26.10.8{EXT}", "org.example.qownnotes",
         [f"{launch}/org.example.qownnotes", "--version"], dict(OFF),
         f"{B}/QON-v26.10.8-QOwnNotes-x86_64.AppImage"),
        ("qon9", f"{PK}/qon-26.10.9{EXT}", "org.example.qownnotes",
         [f"{launch}/org.example.qownnotes", "--version"], dict(OFF),
         f"{B}/QON-v26.10.9-QOwnNotes-x86_64.AppImage"),
    ]

    # install 10x per app (panpak) + appimage-cp 10x
    import shutil
    os.makedirs(APPIMG_DIR, exist_ok=True)

    INSTALL_PLAN = [
        ("ail30", f"{PK}/ail-3.0.0{EXT}",
         f"{B}/appimagelauncher-lite-3.0.0-beta-2-gha287-x86_64.AppImage", "org.example.ail"),
        ("qon9", f"{PK}/qon-26.10.9{EXT}",
         f"{B}/QON-v26.10.9-QOwnNotes-x86_64.AppImage", "org.example.qownnotes"),
    ]
    for tag, pkg, ai, appid in INSTALL_PLAN:
        ts = []
        for _ in range(10):
            t0 = time.perf_counter()
            install_panpak(pkg)
            ts.append(time.perf_counter() - t0)
        ts.sort()
        RES["install"][f"{tag}_panpak_10x"] = {"median": ts[5], "min": ts[0], "max": ts[-1]}
        print(f"install {tag} panpak 10x med={ts[5]:.2f}s", flush=True)
        # appimage: cp + chmod + desktop
        name = os.path.basename(ai)
        ts = []
        for _ in range(10):
            t0 = time.perf_counter()
            shutil.copy(ai, os.path.join(APPIMG_DIR, name))
            os.chmod(os.path.join(APPIMG_DIR, name), 0o755)
            open(os.path.join(APPIMG_DIR, name + ".desktop"), "w").write(
                f"[Desktop Entry]\nType=Application\nName={tag}\nExec={APPIMG_DIR}/{name}\n")
            ts.append(time.perf_counter() - t0)
        ts.sort()
        RES["install"][f"{tag}_appimage_10x"] = {"median": ts[5], "min": ts[0], "max": ts[-1]}
        print(f"install {tag} appimage 10x med={ts[5]:.2f}s", flush=True)
        json.dump(RES, open(f"{B}/results.json", "w"), indent=1)

    json.dump(RES, open(f"{B}/results.json", "w"), indent=1)

    for tag, pkg, appid, argv, env, ai in PLAN:
        dt = install_panpak(pkg)
        RES["install"][f"{tag}_panpak"] = dt
        RES["disk"][f"store_after_{tag}"] = du(store)
        print(f"install {tag}: {dt:.2f}s store={RES['disk'][f'store_after_{tag}']}", flush=True)
        M.measure_startup(f"{tag}-panpak", argv, env, M.app_files_panpak(appid))
        RES["startup"].update(M.RES["startup"])
        M.measure_startup(f"{tag}-appimage", [ai] + argv[1:], env, [ai])
        RES["startup"].update(M.RES["startup"])

    # SN: AppImage marker only (Panpak/SN does not start headless here)
    sn_ai = f"{B}/SN-Simplenote-linux-2.27.1-x86_64.AppImage"
    bench("sn-appimage-marker-warmup", [sn_ai, "--no-sandbox", "--headless"], dict(BASE_ENV), [sn_ai], marker="Checking for update", reps=2, cold=False)
    m = bench("sn-appimage-marker", [sn_ai, "--no-sandbox", "--headless"], dict(BASE_ENV), [sn_ai], marker="Checking for update", reps=10, cold=False)
    RES["startup"]["sn-appimage-marker"] = {"hot": m, "cold": None}
    print(f"sn-appimage/marker: med={m['median']:.3f} min={m['min']:.3f} max={m['max']:.3f} sd={m['stdev']:.3f}" if m["ok"] else "sn marker KO", flush=True)
    json.dump(RES, open(f"{B}/results.json", "w"), indent=1)

    # SN: only install+disk (GUI start not comparable here, see report)
    for tag, pkg in [("sn0", f"{PK}/sn-2.27.0{EXT}"), ("sn1", f"{PK}/sn-2.27.1{EXT}")]:
        dt = install_panpak(pkg)
        RES["install"][f"{tag}_panpak"] = dt
        RES["disk"][f"store_after_{tag}"] = du(store)
        print(f"install {tag}: {dt:.2f}s store={RES['disk'][f'store_after_{tag}']}", flush=True)

    json.dump(RES, open(f"{B}/results.json", "w"), indent=1)
    print("campaign done")
    return 0


if __name__ == "__main__":
    sys.exit(main())
