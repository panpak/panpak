#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Make the two packages that `power_loss_dmlog.py record` needs (`v1.panpak` and `v2.panpak`, about 28 MB each, signed with a test key and for an
update URL on 127.0.0.1:PORT), the same ones that `power_loss_vm.py` uses.

Usage: tests/power_loss_packs.py OUT_DIR [--port 18765]   (build the release first: target/x86_64-unknown-linux-musl/release)
"""
import argparse
from pathlib import Path
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
import power_loss_vm  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('out')
    ap.add_argument('--port', type=int, default=18765)
    a = ap.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.work-', dir=out) as t:
        work = Path(t)
        (work / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (work / 'secret.key').chmod(0o600)
        for v in ('1', '2'):
            (out / f'v{v}.panpak').write_bytes(power_loss_vm.pack(work, v, v == '2', a.port))
            print(out / f'v{v}.panpak')


if __name__ == '__main__':
    main()
