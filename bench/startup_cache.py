#!/usr/bin/env python3
"""10 warm repetitions, high-resolution wall clock (no Popen.wait polling).

Usage: python3 bench/startup_cache.py before-stub after-stub app.panpak --help
Installed package/pin required. Runs no GUI or destructive operations.
"""
import json
import os
import statistics
import subprocess
import sys
import time


def sample(command, env):
    values = []
    for _ in range(11):
        start = time.perf_counter_ns()
        p = subprocess.run(command, env=env, capture_output=True, timeout=60)
        elapsed = (time.perf_counter_ns() - start) / 1e6
        assert p.returncode == 0, p.stderr.decode(errors='replace')
        values.append(elapsed)
    values = values[1:]  # discard warmup
    return {'n': len(values), 'median_ms': statistics.median(values),
            'stdev_ms': statistics.pstdev(values), 'min_ms': min(values),
            'max_ms': max(values), 'samples_ms': values}


if __name__ == '__main__':
    before, after, package, *args = sys.argv[1:]
    env = dict(os.environ, QT_QPA_PLATFORM='offscreen')
    result = {'before': sample([before, 'run', package, *args], env),
              'after': sample([after, 'run', package, *args], env),
              'cache_disabled': sample([after, 'run', package, *args],
                                       dict(env, PANPAK_NO_ABI_CACHE='1'))}
    print(json.dumps(result, indent=2))
