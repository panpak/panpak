#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Conformance runner: independent verify.py over every fixture, against expected.json.
Usage: run.py [--rust]   (with --rust re-checks the valid ones with `panpak doctor`)
Exit 0 if everything checks out, 1 otherwise (lists discrepancies = ambiguities to resolve).
"""

import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
FIX = os.path.join(HERE, "fixtures")
EXPECTED = json.load(open(os.path.join(FIX, "expected.json")))


def check_one(name, exp):
    p = os.path.join(FIX, name)
    r = subprocess.run([sys.executable, os.path.join(HERE, "verify.py"), p, "--json"], capture_output=True, text=True)
    try:
        got = json.loads(r.stdout.strip().splitlines()[-1])
    except Exception:
        return f"{name}: non-JSON output: {r.stdout[:200]!r} {r.stderr[:200]!r}"
    if exp["ok"]:
        if not got.get("ok"):
            return f"{name}: expected VALID, got error: {got.get('error')}"
    else:
        if got.get("ok"):
            return f"{name}: expected INVALID, but accepted as valid"
        err = got.get("error", "")
        wants = exp.get("err", [])
        if wants and not any(w.lower() in err.lower() for w in wants):
            return f"{name}: unexpected error: {err!r} (expected one of {wants})"
    return None


def main():
    fails = []
    for name in sorted(EXPECTED):
        exp = EXPECTED[name]
        msg = check_one(name, exp)
        status = "EXPECTED-FAIL" if not exp["ok"] else "OK"
        if msg:
            print(f"FAIL {msg}")
            fails.append(name)
        else:
            print(f"pass {name} ({status})")
    # Cross-check with Rust (panpak doctor): valid fixtures are accepted, invalid
    # ones rejected (with the expected PANPAK code when the fixture names one).
    if "--rust" in sys.argv:
        root = os.path.dirname(HERE)
        panpak = os.path.join(root, "target/x86_64-unknown-linux-musl/release/panpak")
        for name, exp in sorted(EXPECTED.items()):
            if not exp["ok"]:
                r = subprocess.run([panpak, "doctor", os.path.join(FIX, name)], capture_output=True, text=True)
                if "INVALID package" not in r.stdout:
                    print(f"FAIL {name}: Rust accepted an invalid package:\n{r.stdout[-500:]}")
                    fails.append(name + "#rust")
                elif exp.get("code") and exp["code"] not in r.stdout:
                    print(f"FAIL {name}: Rust did not report {exp['code']}:\n{r.stdout[-500:]}")
                    fails.append(name + "#rust")
                else:
                    print(f"pass {name} (Rust agrees: invalid)")
                continue
            r = subprocess.run([panpak, "doctor", os.path.join(FIX, name)], capture_output=True, text=True)
            if "verified package" not in r.stdout or "INVALID package" in r.stdout:
                print(f"FAIL {name}: Rust did not accept it:\n{r.stdout[-500:]}")
                fails.append(name + "#rust")
            else:
                print(f"pass {name} (Rust agrees: valid)")
    print(f"\n{len(EXPECTED) - len(fails)}/{len(EXPECTED)} conformant")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
