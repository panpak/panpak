#!/usr/bin/env python3
"""OB2 measure driver. Usage: python3 measure.py. Writes results.json.
Environment variables (see bench/README.md):
  PANPAK_BENCH_DIR: directory with the original AppImages
  (default ~/panpak-bench).
  PANPAK_PKGS_DIR: directory with the .panpak files to install
  (default temp directory "panpak-bench-pkgs").
"""
import json
import os
import statistics
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from bench import bench, evict, BASE_ENV

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NE = os.path.join(ROOT, "target/x86_64-unknown-linux-musl/release/panpak")
SB = os.path.join(ROOT, "target/x86_64-unknown-linux-musl/release/panpak-stub")
def bench_home():
    """Dedicated bench HOME: the Rust runtime (store, launchers, desktop)
    follows $HOME, so everything stays out of the user directories."""
    return os.environ.get("PANPAK_BENCH_HOME", os.path.join(B, "home"))


def store_dir():
    """Bench store: explicit PANPAK_STORE, else
    <bench-home>/.local/share/panpak. Never the user one by default."""
    return os.environ.get("PANPAK_STORE", os.path.join(bench_home(), ".local", "share", "panpak"))


def launch_dir():
    return os.path.join(bench_home(), ".local", "bin")
B = os.environ.get("PANPAK_BENCH_DIR", os.path.expanduser("~/panpak-bench"))
PK = os.environ.get("PANPAK_PKGS_DIR", os.path.join(tempfile.gettempdir(), "panpak-bench-pkgs"))
EXT = os.environ.get("PANPAK_PKG_EXT", ".panpak")

OFF = dict(BASE_ENV)
OFF["QT_QPA_PLATFORM"] = "offscreen"

RES = {"startup": {}, "disk": {}, "update": {}, "install": {}}


def du(path):
    out = subprocess.run(["du", "-sb", path], capture_output=True, text=True).stdout
    return int(out.split()[0])


def app_files_panpak(app_id):
    import struct
    store = store_dir()
    cur = os.path.join(store, "apps", app_id, "current")
    pkg = os.path.realpath(cur)
    data = open(pkg, "rb").read()
    root = data[len(data) - 80 + 48:len(data) - 80 + 80].hex()
    return [pkg, os.path.join(store, "trees", root),
            os.path.join(store, "runtimes", "1", "panpak-stub")]


def install_panpak(pkg, keyflag="--runtime"):
    # keyflag: use local runtime (development)
    t0 = time.perf_counter()
    r = subprocess.run([NE, "install", "--yes", keyflag, SB, pkg],
                       capture_output=True, text=True, timeout=600)
    dt = time.perf_counter() - t0
    assert r.returncode == 0, r.stderr[-500:]
    return dt


def fmt(v):
    if not v["ok"]:
        return "KO"
    return "med=%.3f min=%.3f max=%.3f sd=%.3f" % (
        v["median"], v["min"], v["max"], v["stdev"])


def measure_startup(name, argv, env, files, marker=None):
    bench(name, argv, env, files, reps=2, cold=False)  # warmup discarded
    hot = bench(name, argv, env, files, reps=10, cold=False, marker=marker)
    cold = bench(name, argv, env, files, reps=10, cold=True, marker=marker)
    RES["startup"][name] = {"hot": hot, "cold": cold}
    print(f"{name}/hot: {fmt(hot)}", flush=True)
    print(f"{name}/cold: {fmt(cold)}", flush=True)


if __name__ == "__main__":
    print("driver measure pronto")
