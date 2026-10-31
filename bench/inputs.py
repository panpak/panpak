#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Inputs of the public benchmarks: official AppImages, each with a pinned SHA-256.

Usage: python3 bench/inputs.py fetch|verify|list

The files are never stored in the repository. `fetch` downloads what is missing
into `$PANPAK_BENCH_DIR/public` (default `~/panpak-bench/public`) and checks the
SHA-256 before the file is put in place; a file that does not match is deleted
and the command fails. `verify` checks the files that are already there.

Where the hash comes from: for the GitHub assets (except `ail-2.2.0`) it is the digest
that GitHub publishes for the release asset, checked against the download. The
AppImageLauncher 2.2.0 and PrusaSlicer 2.8.1 releases have no published digest: their hash
is that of the file as downloaded from the release URL on the day the benchmarks were first run.
"""
import hashlib
import os
from pathlib import Path
import sys
import urllib.request

GH = 'https://github.com'

# app: short name used in the CSV files; version: as the project calls it.
INPUTS = [
    dict(key='ail-2.2.0', app='ail', version='2.2.0',
         file='appimagelauncher-lite-2.2.0-travis995-0f91801-x86_64.AppImage',
         url=f'{GH}/TheAssassin/AppImageLauncher/releases/download/v2.2.0/appimagelauncher-lite-2.2.0-travis995-0f91801-x86_64.AppImage',
         sha256='accb47520537bf91993696b4b3568c681925dac69e7764948e3c7070dd4db481', size=42373160),
    dict(key='ail-3.0.0-beta-3', app='ail', version='3.0.0-beta-3',
         file='appimagelauncher-lite-3.0.0-beta-2-gha287-x86_64.AppImage',
         url=f'{GH}/TheAssassin/AppImageLauncher/releases/download/v3.0.0-beta-3/appimagelauncher-lite-3.0.0-beta-2-gha287-x86_64.AppImage',
         sha256='1f12b95b0bf6dd1dd251b7da55b656012bd51967cb36c3405849822667557be8', size=36542968),
    dict(key='qon-26.10.8', app='qownnotes', version='26.10.8',
         file='QOwnNotes-26.10.8-x86_64.AppImage',
         url=f'{GH}/pbek/QOwnNotes/releases/download/v26.10.8/QOwnNotes-x86_64.AppImage',
         sha256='c52b09c43101fec06664e3b701c99c6415c2b74aae5957453b0078bb06857d6c', size=62290424),
    dict(key='qon-26.10.9', app='qownnotes', version='26.10.9',
         file='QOwnNotes-26.10.9-x86_64.AppImage',
         url=f'{GH}/pbek/QOwnNotes/releases/download/v26.10.9/QOwnNotes-x86_64.AppImage',
         sha256='56c407e3a8c614afc4a0d6af1a511a2a7bec09326533dafbd2ec42d16c940be5', size=62347768),
    dict(key='sn-2.27.0', app='simplenote', version='2.27.0',
         file='Simplenote-linux-2.27.0-x86_64.AppImage',
         url=f'{GH}/Automattic/simplenote-electron/releases/download/v2.27.0/Simplenote-linux-2.27.0-x86_64.AppImage',
         sha256='1d7dee1959c253335e2d6833d2006d7e1ede7e708208c8cd2f144f11dce7b0eb', size=147197121),
    dict(key='sn-2.27.1', app='simplenote', version='2.27.1',
         file='Simplenote-linux-2.27.1-x86_64.AppImage',
         url=f'{GH}/Automattic/simplenote-electron/releases/download/v2.27.1/Simplenote-linux-2.27.1-x86_64.AppImage',
         sha256='91544f51e3c2553edaf8ea37ab0d4083451e636b2d6a74dc3e654a8e333d3123', size=147197267),
    dict(key='prusaslicer-2.8.1', app='prusaslicer', version='2.8.1',
         file='PrusaSlicer-2.8.1+linux-x64-newer-distros-GTK3-202409181416.AppImage',
         url=f'{GH}/prusa3d/PrusaSlicer/releases/download/version_2.8.1/PrusaSlicer-2.8.1%2Blinux-x64-newer-distros-GTK3-202409181416.AppImage',
         sha256='565f2f4bd4dbb05904a459d54db1916b6932124709c1d17b5aacfe9f5f2f1b03', size=98731200),
]
BY_KEY = {i['key']: i for i in INPUTS}


def inputs_dir():
    return Path(os.environ.get('PANPAK_BENCH_DIR', str(Path.home() / 'panpak-bench'))) / 'public'


def path_of(key):
    return inputs_dir() / BY_KEY[key]['file']


def sha256_of(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def fetch_one(item):
    dest = inputs_dir() / item['file']
    if dest.exists() and sha256_of(dest) == item['sha256']:
        dest.chmod(0o755)
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    part = dest.with_name(dest.name + '.part')
    print('download', item['url'], flush=True)
    h = hashlib.sha256()
    with urllib.request.urlopen(item['url'], timeout=60) as r, open(part, 'wb') as out:
        for block in iter(lambda: r.read(1 << 20), b''):
            h.update(block)
            out.write(block)
    if h.hexdigest() != item['sha256']:
        part.unlink()
        raise SystemExit(f'{item["file"]}: SHA-256 {h.hexdigest()} does not match the pinned {item["sha256"]}')
    part.chmod(0o755)
    part.rename(dest)
    return dest


def fetch(keys=None):
    return {i['key']: fetch_one(i) for i in INPUTS if keys is None or i['key'] in keys}


def verify():
    ok = True
    for i in INPUTS:
        p = inputs_dir() / i['file']
        if not p.exists():
            print('missing ', i['key'])
            ok = False
        elif sha256_of(p) != i['sha256']:
            print('MISMATCH', i['key'])
            ok = False
        else:
            print('ok      ', i['key'], i['sha256'])
    return ok


if __name__ == '__main__':
    cmd = sys.argv[1] if len(sys.argv) > 1 else 'list'
    if cmd == 'fetch':
        fetch()
        sys.exit(0 if verify() else 1)
    elif cmd == 'verify':
        sys.exit(0 if verify() else 1)
    else:
        for i in INPUTS:
            print(i['key'], i['sha256'], i['url'])
