#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The software bill of materials (PANPAK-FORMAT.md 7.3): `pack` makes `.panpak/sbom.cdx.json`, the manifest signs its hash and says how much it covers, the same
tree makes the same bytes, `panpak sbom` shows it from a package, from an installed app and from a URL (where only the parts of the package that hold it are
downloaded), and an author cannot write the table. Real binaries (build the release first), an isolated store, a local server with Range support.
"""
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN  # noqa: E402

ID = 'org.example.sbomlab'


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    else:
        assert p.returncode != 0, p.stdout + p.stderr
    return p


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-sbom-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), XDG_CONFIG_HOME=str(tmp / 'config'),
                   XDG_RUNTIME_DIR=str(runtime), LC_ALL='C', PANPAK_NO_BACKGROUND='1')
        env.pop('WAYLAND_DISPLAY', None)
        key = tmp / 'author.key'
        run([BIN / 'panpak', 'keygen', '-o', key], env)

        # A tree with two real shared libraries (an ELF file with a SONAME is what the SBOM lists) and enough other bytes to tell a partial read from a whole one.
        tree = tmp / 'tree'
        (tree / 'bin').mkdir(parents=True)
        (tree / 'lib').mkdir()
        (tree / 'bin/hello').write_text('#!/bin/sh\nexit 0\n')
        (tree / 'bin/hello').chmod(0o755)
        (tmp / 'x.c').write_text('int sbom_answer(void) { return 42; }\n')
        for name in ('libsbomone.so.1', 'libsbomtwo.so.3'):
            run(['gcc', '-shared', '-fPIC', f'-Wl,-soname,{name}', '-o', tree / 'lib' / name, tmp / 'x.c'], env)
        for i in range(8):
            (tree / f'payload{i}').write_bytes(hashlib.sha256(bytes([i])).digest() * 40000)
        (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Sbom Lab"\nversion="1"\nentry="bin/hello"\n')

        def pack(out, previous=None):
            args = [BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', key, '-o', out]
            run(args, env)

        # 1. The manifest signs the hash and the coverage; the file is in the package.
        pack(tmp / 'one.panpak')
        manifest = tmp / 'manifest.toml'
        run([BIN / 'panpak', 'dump-manifest', tmp / 'one.panpak', manifest], env)
        text = manifest.read_text()
        assert '[sbom]' in text and 'coverage = "best-effort"' in text, text
        # `sbom` is not a critical section: no key of it is in the list that an older reader refuses on.
        assert '"sbom.' not in text, text

        shown = run([BIN / 'panpak', 'sbom', tmp / 'one.panpak'], env)
        doc = json.loads(shown.stdout)
        assert doc['bomFormat'] == 'CycloneDX' and doc['specVersion'] == '1.6', doc
        libs = [c for c in doc['components'] if c['type'] == 'library']
        assert sorted(c['name'] for c in libs) == ['libsbomone.so.1', 'libsbomtwo.so.3'], libs
        assert all(c['hashes'][0]['alg'] == 'BLAKE3' and len(c['hashes'][0]['content']) == 64 for c in libs), libs
        assert 'serialNumber' not in doc and 'timestamp' not in doc['metadata'], 'a time or a random number would make two builds differ'
        assert any(p['name'] == 'panpak:coverage' and p['value'] == 'best-effort' for p in doc['metadata']['properties']), doc['metadata']

        # 2. The same tree makes the same bytes.
        pack(tmp / 'two.panpak')
        assert run([BIN / 'panpak', 'sbom', tmp / 'two.panpak'], env).stdout == shown.stdout, 'the SBOM of the same tree is not the same bytes'

        # Two builds of the same files, made at other times from another folder, give the same file; the package is made of the same bytes too (the tree is addressed by content).
        copy = tmp / 'copy'
        subprocess.run(['cp', '-a', str(tree), str(copy)], check=True)
        time.sleep(1.1)
        for f in copy.rglob('*'):
            if f.is_file() and not f.is_symlink():
                f.touch()
        run([BIN / 'panpak', 'pack', copy, '--stub', BIN / 'panpak-stub', '--secret-file', key, '-o', tmp / 'three.panpak'], env)
        assert run([BIN / 'panpak', 'sbom', tmp / 'three.panpak'], env).stdout == shown.stdout, 'two builds of the same files give another SBOM'

        # The export completes it for a scanner: the identity, a serial number and the time come from the signed manifest, and the file of the package has none of them.
        export_env = dict(env, SOURCE_DATE_EPOCH='1700000000')
        full = run([BIN / 'panpak', 'sbom', '--export', tmp / 'one.panpak'], export_env)
        done = json.loads(full.stdout)
        assert done['metadata']['component']['name'] == 'Sbom Lab' and done['metadata']['component']['version'] == '1', done['metadata']
        assert done['metadata']['timestamp'] == '2023-11-14T22:13:20Z' and done['serialNumber'].startswith('urn:uuid:'), done
        assert done['components'] == doc['components'] and done['specVersion'] == '1.6'
        assert run([BIN / 'panpak', 'sbom', '--export', tmp / 'one.panpak'], export_env).stdout == full.stdout, 'the export is not reproducible with SOURCE_DATE_EPOCH'
        assert 'serialNumber' not in doc and 'component' not in doc['metadata'], 'the file of the package carries an identity'

        # Names that come from files that nobody here wrote (a SONAME, a file name) go into the document: a real JSON parser must get them back as they are.
        hostile = tmp / 'hostile'
        (hostile / 'bin').mkdir(parents=True)
        (hostile / 'lib').mkdir()
        (hostile / 'bin/hello').write_text('#!/bin/sh\nexit 0\n')
        (hostile / 'bin/hello').chmod(0o755)
        (hostile / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Hostile"\nversion="1"\nentry="bin/hello"\n')
        names = ['libq"uote.so.1', 'libback\\slash.so.1', 'libuni\u00e9\u65e5\u672c\U0001F600.so.1', 'lib},{"injected":true.so.1', 'lib<script>.so.1']
        made = {}
        for i, soname in enumerate(names):
            # The file name is the SONAME where the file system and `pack` take it; the SONAME is always the one in the ELF file.
            fname = f'lib{i}-' + ''.join('_' if c in '/\t\n' else c for c in soname[3:])
            made[soname] = fname
            run(['gcc', '-shared', '-fPIC', '-Xlinker', '-soname', '-Xlinker', soname, '-o', hostile / 'lib' / fname, tmp / 'x.c'], env)
        # A SONAME with a control character or a slash would make a package that its own reader refuses: `pack` says so and writes nothing.
        for bad in ('libtab\there.so.1', 'libline\nbreak.so.1', 'lib</script>.so.1'):
            run(['gcc', '-shared', '-fPIC', '-Xlinker', '-soname', '-Xlinker', bad, '-o', hostile / 'lib' / 'bad.so', tmp / 'x.c'], env)
            refused = run([BIN / 'panpak', 'pack', hostile, '--stub', BIN / 'panpak-stub', '--secret-file', key, '-o', tmp / 'refused.panpak'], env, ok=False)
            assert 'control character or a slash' in refused.stderr and not (tmp / 'refused.panpak').exists(), refused.stderr
        (hostile / 'lib' / 'bad.so').unlink()
        run([BIN / 'panpak', 'pack', hostile, '--stub', BIN / 'panpak-stub', '--secret-file', key, '-o', tmp / 'hostile.panpak'], env)
        got = json.loads(run([BIN / 'panpak', 'sbom', tmp / 'hostile.panpak'], env).stdout)
        found = {c['name']: next(p['value'] for p in c['properties'] if p['name'] == 'panpak:path') for c in got['components']}
        assert set(found) == set(names), (sorted(found), names)
        assert all(found[n] == 'lib/' + made[n] for n in names), found
        full = json.loads(run([BIN / 'panpak', 'sbom', '--export', tmp / 'hostile.panpak'], env).stdout)
        assert full['components'] == got['components']

        # 3. `inspect` says that there is one without reading the tree; the author cannot write the table.
        inspected = run([BIN / 'panpak', 'inspect', tmp / 'one.panpak'], env).stdout
        assert 'SBOM: best-effort coverage' in inspected, inspected
        assert json.loads(run([BIN / 'panpak', 'inspect', '--json', tmp / 'one.panpak'], env).stdout)['sbom_coverage'] == 'best-effort'
        (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Sbom Lab"\nversion="1"\nentry="bin/hello"\n[sbom]\nhash="{"0" * 64}"\ncoverage="build"\n')
        refused = run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', key, '-o', tmp / 'bad.panpak'], env, ok=False)
        assert 'written by `panpak pack`' in refused.stderr and 'PANPAK-E0008' in refused.stderr, refused.stderr
        (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Sbom Lab"\nversion="1"\nentry="bin/hello"\n')

        # 4. From an installed app: the file of the tree, checked against the hash that the manifest signs.
        run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', tmp / 'one.panpak'], env)
        assert run([BIN / 'panpak', 'sbom', ID], env).stdout == shown.stdout
        # A tree that was changed is not the signed one.
        root = (tmp / 'data/panpak/apps' / ID / 'current').resolve().read_bytes()[-32:].hex()
        victim = tmp / 'data/panpak/trees' / root / '.panpak/sbom.cdx.json'
        victim.parent.chmod(0o755)
        victim.chmod(0o644)
        victim.write_bytes(victim.read_bytes().replace(b'libsbomone', b'libevilone'))
        tampered = run([BIN / 'panpak', 'sbom', ID], env, ok=False)
        assert 'does not match the hash that the manifest signs' in tampered.stderr, tampered.stderr

        # 5. From a URL: the footer, the manifest, the signature, the index and the chunks of the file; not the rest.
        sock = socket.socket()
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
        sock.close()
        (tmp / 'remote.panpak').write_bytes((tmp / 'one.panpak').read_bytes())
        env['PANPAK_RANGE_LOG'] = str(tmp / 'ranges.jsonl')
        server = subprocess.Popen([sys.executable, ROOT / 'tests/range_server.py', tmp, str(port), 'normal'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(0.4)
            (tmp / 'ranges.jsonl').unlink(missing_ok=True)
            remote = run([BIN / 'panpak', 'sbom', '--allow-insecure-http', f'http://127.0.0.1:{port}/remote.panpak'], env)
            assert remote.stdout == shown.stdout, 'the SBOM that came over HTTP is not the one of the package'
            assert 'is not checked against any you trust' in remote.stderr, remote.stderr
            assert json.loads(run([BIN / 'panpak', 'sbom', '--export', '--allow-insecure-http', f'http://127.0.0.1:{port}/remote.panpak'], export_env).stdout)['metadata']['component']['name'] == 'Sbom Lab'
            got = sum(json.loads(l)['bytes'] for l in (tmp / 'ranges.jsonl').read_text().splitlines())
            size = (tmp / 'remote.panpak').stat().st_size
            # The package holds the stub and 1.2 MB of payload; what came is a small part of it.
            assert got < size // 4, f'{got} of {size} bytes came: more than the parts that hold the SBOM'
            print(json.dumps({'package_bytes': size, 'downloaded_bytes': got}))
            assert run([BIN / 'panpak', 'sbom', f'http://127.0.0.1:{port}/remote.panpak'], env, ok=False).returncode != 0, 'HTTP without the flag'
        finally:
            server.terminate()
            server.wait(timeout=5)
    print('sbom PASS')


if __name__ == '__main__':
    main()
