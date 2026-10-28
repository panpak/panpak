#!/usr/bin/env python3
"""Panpak vs AppImage measures: warm/cold start, disk, update, install.
Each timing measure: 10 repetitions, median + min/max + stdev.
Cold = POSIX_FADV_DONTNEED on the involved files before each run.
Environment variables (see bench/README.md):
  PANPAK_FUSE_LIB: userspace libfuse2 for the original AppImages
  (default ~/fuse2/lib/x86_64-linux-gnu).
"""
import json
import os
import statistics
import subprocess
import sys
import time

FUSE_LIB = os.environ.get("PANPAK_FUSE_LIB", os.path.expanduser("~/fuse2/lib/x86_64-linux-gnu"))
BASE_ENV = dict(os.environ)
BASE_ENV["LD_LIBRARY_PATH"] = FUSE_LIB + ":" + BASE_ENV.get("LD_LIBRARY_PATH", "")


def evict(paths):
    for p in paths:
        if os.path.isdir(p):
            for root, _, files in os.walk(p):
                for fn in files:
                    evict([os.path.join(root, fn)])
        elif os.path.isfile(p):
            try:
                fd = os.open(p, os.O_RDONLY)
                try:
                    os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
                finally:
                    os.close(fd)
            except OSError:
                pass


def run_once(argv, env, marker=None, timeout=120):
    """Returns (seconds, ok). With marker: time until the marker line."""
    t0 = time.perf_counter()
    try:
        p = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             text=True, bufsize=1, env=env)
    except OSError as e:
        return None, False
    if marker is None:
        try:
            p.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            p.kill()
            return None, False
        dt = time.perf_counter() - t0
        return (dt, True) if p.returncode == 0 else (None, False)
    # marker mode: kill at the first line containing the marker
    dt = None
    try:
        for line in p.stdout:
            if marker in line:
                dt = time.perf_counter() - t0
                break
    finally:
        p.kill()
        try:
            p.wait(timeout=10)
        except Exception:
            pass
    return (dt, True) if dt is not None else (None, False)


def bench(name, argv, env, files, marker=None, reps=10, cold=True):
    dts = []
    for _ in range(reps):
        if cold:
            evict(files)
        dt, ok = run_once(argv, env, marker)
        if not ok or dt is None:
            return {"name": name, "ok": False}
        dts.append(dt)
    dts.sort()
    n = len(dts)
    med = dts[n // 2] if n % 2 else (dts[n // 2 - 1] + dts[n // 2]) / 2
    return {"name": name, "ok": True, "n": n, "median": med,
            "min": dts[0], "max": dts[-1],
            "stdev": statistics.pstdev(dts) if n > 1 else 0.0}


if __name__ == "__main__":
    print("harness ok: python", sys.version.split()[0],
          "fadvise:", hasattr(os, "posix_fadvise"))
