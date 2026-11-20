#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Step 9b: a real `panpak build` gives an SBOM with `coverage = "build"` that names what the recipe pins.

Usage: python3 bench/silverblue/verify_sbom_build.py [--panpak DIR] [--cache DIR] [--work DIR] RECIPE_DIR...

For each recipe (a folder with a `panpak.toml`): `panpak build` twice, then checks on both packages:
  - `[sbom].coverage` is `build`, and `panpak sbom` is a CycloneDX 1.6 document whose header says the same;
  - the source component has the commit of the recipe (or the SHA-256 of its archive);
  - the build image is named, and every dependency that the recipe takes from the image (`[[build.image_deps]]`) is a component, with a version;
  - the libraries of the tree are components with a BLAKE3 hash each;
  - the two builds give the same SBOM bytes, and the same hash in the manifest;
  - `panpak sbom --export` has the name and the version of the recipe and the id as a property.
It runs in a home of its own, with no session bus (the build is capped by the scope that you start it in: run it as
`systemd-run --user --scope -p MemoryMax=7G -p MemorySwapMax=2G -- python3 verify_sbom_build.py ...`). One recipe at a time, and nothing else heavy at the same time.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def run(cmd, env, check=True, timeout=7200):
    p = subprocess.run([str(c) for c in cmd], env=env, capture_output=True, text=True, timeout=timeout)
    if check and p.returncode != 0:
        sys.exit(f'FAIL: {" ".join(map(str, cmd))}\n{p.stdout[-2000:]}{p.stderr[-4000:]}')
    return p


def props(component):
    return {p['name']: p['value'] for p in component.get('properties', [])}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--panpak', default=str(ROOT / 'target/x86_64-unknown-linux-musl/release'), help='folder with panpak and panpak-stub')
    ap.add_argument('--cache', help='XDG_CACHE_HOME that holds the build image (it is made if it is not there; default: a folder of the work folder)')
    ap.add_argument('--work', default=str(Path.home() / '.cache/panpak-sbom-verify'))
    ap.add_argument('recipes', nargs='+')
    a = ap.parse_args()
    work = Path(a.work)
    work.mkdir(parents=True, exist_ok=True)
    home = work / 'home'
    home.mkdir(exist_ok=True)
    env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / 'config'), XDG_DATA_HOME=str(home / 'data'), XDG_CACHE_HOME=a.cache or str(work / 'cache'),
               XDG_RUNTIME_DIR=str(work / 'run'), DBUS_SESSION_BUS_ADDRESS='unix:path=/nonexistent/panpak-no-bus', LC_ALL='C', PANPAK_NO_BACKGROUND='1')
    (work / 'run').mkdir(mode=0o700, exist_ok=True)
    panpak, stub = Path(a.panpak) / 'panpak', Path(a.panpak) / 'panpak-stub'
    key = work / 'verify.key'
    if not key.exists():
        run([panpak, 'keygen', '-o', key], env)
    for recipe in map(Path, a.recipes):
        r = tomllib.loads((recipe / 'panpak.toml').read_text())
        name = recipe.name
        print(f'== {name} {r["app"]["version"]}', flush=True)
        sboms, hashes = [], []
        for i in (1, 2):
            out = work / f'{name}-{i}.panpak'
            out.unlink(missing_ok=True)
            p = run([panpak, 'build', recipe, '--secret-file', key, '--stub', stub, '-o', out], env)
            print(p.stdout.strip().splitlines()[-1] if p.stdout.strip() else f'build {i} done', flush=True)
            raw = run([panpak, 'sbom', out], env).stdout
            sboms.append(raw)
            man = work / f'{name}-{i}.manifest.toml'
            run([panpak, 'dump-manifest', out, man], env)
            hashes.append(tomllib.loads(man.read_text())['sbom'])
        doc = json.loads(sboms[0])
        sb = hashes[0]
        assert sb['coverage'] == 'build', sb
        header = [p for p in doc['metadata']['properties'] if p['name'] == 'panpak:coverage']
        assert header and header[0]['value'] == 'build', doc['metadata']
        assert doc['bomFormat'] == 'CycloneDX' and doc['specVersion'] == '1.6'
        comps = doc['components']
        by_role = {}
        for c in comps:
            by_role.setdefault(props(c).get('panpak:role', 'library-of-the-tree'), []).append(c)
        src = by_role['source'][0]
        want = r['source'].get('commit') or r['source'].get('sha256')
        assert want in json.dumps(src), f'the source component does not hold {want}: {src}'
        assert by_role.get('build-image'), 'no build image component'
        declared = [d['package'] for d in r['build'].get('image_deps', [])]
        found = {c['name']: c for c in by_role.get('build-image-dependency', [])}
        assert sorted(found) == sorted(declared), (sorted(found), sorted(declared))
        assert all(c.get('version') for c in found.values()), 'a dependency of the image has no version'
        libs = by_role['library-of-the-tree']
        assert libs, 'no library of the tree'
        assert all(c['hashes'][0]['alg'] == 'BLAKE3' and len(c['hashes'][0]['content']) == 64 for c in libs)
        assert sboms[0] == sboms[1], 'two builds of the same recipe gave different SBOM bytes'
        assert hashes[0] == hashes[1], 'two builds gave different [sbom] in the manifest'
        full = json.loads(run([panpak, 'sbom', '--export', work / f'{name}-1.panpak'], env).stdout)
        comp = full['metadata']['component']
        assert comp['name'] == r['app']['name'] and comp['version'] == r['app']['version'], comp
        assert props(comp).get('panpak:id') == r['app']['id'], comp
        print(f'{name}: coverage build, {len(comps)} components ({len(libs)} libraries, {len(found)} from the image, {len(built)} built from an archive), two builds with the same SBOM, export names {comp["name"]} {comp["version"]} PASS', flush=True)
    print('verify_sbom_build PASS')


if __name__ == '__main__':
    main()
