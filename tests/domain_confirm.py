#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The confirmation of the domain of an author (PANPAK-FORMAT.md 3.2.1), against a local HTTPS server with a certificate of a test authority: confirmed, not confirmed
(no file, an app that is not listed, a redirect to another host, a file that is too big, a server that does not answer, the check turned off, `--offline`), contradicted
(other keys, a key that the domain says was replaced), the first install with `--yes` and no `--trust-first-key` when the domain confirms, a refusal that `--yes` and
`--trust-first-key` do not override, the privacy sentence once, and a succession of the key at an update. Real binaries (build the release first), an isolated store.
"""
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

DOMAIN = 'example.test'
ID = 'test.example.dlab'


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    else:
        assert p.returncode != 0, p.stdout + p.stderr
    return p


def free_port():
    s = socket.socket()
    s.bind(('127.0.0.1', 0))
    port = s.getsockname()[1]
    s.close()
    return port


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-domain-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        tls_port, http_port = free_port(), free_port()
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), XDG_CONFIG_HOME=str(tmp / 'config'),
                   XDG_RUNTIME_DIR=str(runtime), LC_ALL='C', PANPAK_NO_BACKGROUND='1', PANPAK_TEST_RESOLVE=f'{DOMAIN}=127.0.0.1:{tls_port}')
        env.pop('WAYLAND_DISPLAY', None)
        env.pop('PANPAK_RECOVERY', None)

        # A test authority and a certificate for the domain.
        ca, srv = tmp / 'ca.pem', tmp / 'srv.pem'
        ec = ['-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:prime256v1', '-nodes']
        run(['openssl', 'req', '-x509', *ec, '-keyout', tmp / 'ca.key', '-out', ca, '-subj', '/CN=panpak-test-ca', '-days', '2',
             '-addext', 'basicConstraints=critical,CA:TRUE', '-addext', 'keyUsage=critical,keyCertSign'], env)
        run(['openssl', 'req', *ec, '-keyout', tmp / 'srv.key', '-out', tmp / 'srv.csr', '-subj', f'/CN={DOMAIN}'], env)
        (tmp / 'ext').write_text(f'subjectAltName=DNS:{DOMAIN}\nbasicConstraints=CA:FALSE\n')
        run(['openssl', 'x509', '-req', '-in', tmp / 'srv.csr', '-CA', ca, '-CAkey', tmp / 'ca.key', '-CAcreateserial', '-out', srv, '-days', '2', '-extfile', tmp / 'ext'], env)
        run([BIN / 'panpak', 'config', 'set', 'network.ca_file', ca], env)

        ctl = tmp / 'ctl'
        ctl.mkdir()
        (ctl / 'mode').write_text('file')
        (ctl / 'body.toml').write_text('')

        def say(mode, entries=''):
            (ctl / 'mode').write_text(mode)
            (ctl / 'body.toml').write_text(entries)
            (ctl / 'requests.log').write_text('')

        def requests():
            return (ctl / 'requests.log').read_text().split()

        def new_key(name):
            p = run([BIN / 'panpak', 'key', 'new', '-o', tmp / f'{name}.key'], env)
            return next(l.split()[-1] for l in p.stdout.splitlines() if l.startswith('key:')), (tmp / f'{name}.key.recovery').read_text().strip()

        pub_a, rec_a = new_key('a')
        pub_b, _ = new_key('b')
        pub_x, _ = new_key('x')

        def entry(app, roots, replaces=()):
            r = ', '.join(f'"{k}"' for k in roots)
            rep = f'replaces = [{", ".join(chr(34) + k + chr(34) for k in replaces)}]\n' if replaces else ''
            return f'[[app]]\nid = "{app}"\nroot = [{r}]\n{rep}'

        def pack(name, key, extra=(), domain=True, app_id=ID):
            tree = tmp / f'tree-{name}'
            tree.mkdir()
            (tree / 'hello').write_text(f'#!/bin/sh\necho "{name}"\n')
            (tree / 'hello').chmod(0o755)
            trust = f'[trust]\ndomain="{DOMAIN}"\n' if domain else ''
            (tree / 'panpak.toml').write_text(f'[app]\nid="{app_id}"\nname="Domain Lab"\nversion="{name}"\nentry="hello"\n{trust}[update]\nurl="http://127.0.0.1:{http_port}/remote.panpak"\n')
            out = tmp / f'{name}.panpak'
            run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / f'{key}.key', '-o', out, *extra], env)
            return out

        appdir = tmp / 'data/panpak/apps' / ID
        install = [BIN / 'panpak', 'install', '--runtime', BIN / 'panpak-stub']

        server = subprocess.Popen([sys.executable, ROOT / 'tests/domain_server.py', str(tls_port), str(srv), str(tmp / 'srv.key'), str(ctl)], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(0.6)
            v1 = pack('v1', 'a')

            # 1. Not confirmed: the domain does not list the app. The privacy sentence is said once. `--yes` alone is not enough, as before.
            say('file', entry('test.example.other', [pub_a]))
            p = run([*install, '--yes', v1], env, ok=False)
            assert 'PANPAK-E1003' in p.stderr, p.stderr
            assert 'Published by: EXAMPLE.TEST' in p.stdout and 'Not confirmed: example.test does not list test.example.dlab' in p.stdout, p.stdout
            assert 'asks https://example.test/.well-known/panpak.toml' in p.stdout and 'confirm_domain off' in p.stdout, p.stdout
            assert 'key: ' + pub_a in p.stdout, 'without a confirmation the key is what says who this is'
            assert requests() == ['/.well-known/panpak.toml'], requests()
            p = run([*install, '--yes', '--trust-first-key', v1], env)
            assert 'asks https://example.test' not in p.stdout, 'the privacy sentence is said once'
            assert appdir.exists()
            print('not listed: not confirmed, the key is shown, --yes alone refused, --trust-first-key goes on, the privacy sentence once PASS')
            run([BIN / 'panpak', 'remove', '--yes', ID], env)

            # 2. Confirmed: `--yes` is enough, the domain is the main text and the key is behind --show-key.
            say('file', entry(ID, [pub_a]))
            p = run([*install, '--yes', v1], env)
            assert 'Published by: EXAMPLE.TEST' in p.stdout and 'Confirmed: example.test lists this key for test.example.dlab' in p.stdout, p.stdout
            assert 'key: ed25519' not in p.stdout and '--show-key' in p.stdout, p.stdout
            run([BIN / 'panpak', 'remove', '--yes', ID], env)
            p = run([*install, '--yes', '--show-key', v1], env)
            assert 'key: ' + pub_a in p.stdout, p.stdout
            run([BIN / 'panpak', 'remove', '--yes', ID], env)
            # A wildcard entry covers the app too; without --yes and without a terminal the question cannot be asked.
            say('file', entry('test.example.*', [pub_a]))
            p = run([*install, v1], env, ok=False)
            assert 'PANPAK-E5004' in p.stderr and '--yes' in p.stderr, p.stderr
            run([*install, '--yes', v1], env)
            run([BIN / 'panpak', 'remove', '--yes', ID], env)
            print('confirmed: --yes is enough, the domain leads, the key is behind --show-key, a prefix entry covers the app PASS')

            # 3. Contradicted: other keys, or a key that the domain says was replaced. `--yes` and `--trust-first-key` do not override; nothing is left.
            for name, body in [('other keys', entry(ID, [pub_x])), ('replaced', entry(ID, [pub_x], [pub_a]))]:
                say('file', body)
                p = run([*install, '--yes', '--trust-first-key', v1], env, ok=False)
                assert 'PANPAK-E1006' in p.stderr and 'example.test says otherwise' in p.stderr and '--despite-domain-mismatch' in p.stderr, p.stderr
                assert not (appdir / 'trust').exists() and not (appdir / 'current').exists() and not (tmp / '.local/bin' / ID).exists(), name + ': something was left behind'
                p = run([*install, '--yes', '--expect-key', pub_a, v1], env, ok=False)
                assert 'PANPAK-E1006' in p.stderr, p.stderr
            # The explicit option, and the person still has to say whom they trust.
            p = run([*install, '--yes', '--despite-domain-mismatch', v1], env, ok=False)
            assert 'PANPAK-E1003' in p.stderr, p.stderr
            p = run([*install, '--yes', '--despite-domain-mismatch', '--trust-first-key', v1], env)
            assert 'Going on because of --despite-domain-mismatch' in p.stdout, p.stdout
            run([BIN / 'panpak', 'remove', '--yes', ID], env)
            print('contradicted: refused with --yes and --trust-first-key, nothing left; --despite-domain-mismatch plus a statement of trust goes on PASS')

            # 4. Not confirmed, never a refusal: a redirect to another host is not followed, a file too big, no file, a server that does not answer, bad content.
            for mode, body, want in [('redirect-other', '', 'sent the request to another address'), ('big', '', 'too large'), ('notfound', '', 'HTTP 404'),
                                     ('file', 'this is not [toml', 'is not valid')]:
                say(mode, body)
                p = run([*install, '--yes', '--trust-first-key', v1], env)
                assert 'Not confirmed' in p.stdout and want in p.stdout, (mode, p.stdout)
                if mode == 'redirect-other':
                    assert requests() == ['/.well-known/panpak.toml'], requests()
                run([BIN / 'panpak', 'remove', '--yes', ID], env)
            say('redirect-same', entry(ID, [pub_a]))
            p = run([*install, '--yes', v1], env)
            assert 'Confirmed: example.test' in p.stdout and requests() == ['/.well-known/panpak.toml', '/moved/panpak.toml'], (p.stdout, requests())
            run([BIN / 'panpak', 'remove', '--yes', ID], env)
            say('slow')
            t = time.time()
            p = run([*install, '--yes', '--trust-first-key', v1], env)
            took = time.time() - t
            assert 'Not confirmed' in p.stdout and 'could not be asked' in p.stdout and took < 15, (took, p.stdout)
            run([BIN / 'panpak', 'remove', '--yes', ID], env)
            print(f'not confirmed: another host is not followed, a file too big, no file, bad content, a server that does not answer ({took:.1f} s), a redirect inside the host is followed PASS')

            # 5. A domain that is not asked: --offline, the setting, a package that names none. The server sees nothing; the message says "not confirmed".
            say('file', entry(ID, [pub_a]))
            p = run([*install, '--yes', '--trust-first-key', '--offline', v1], env)
            assert 'Not confirmed: this command was run with --offline' in p.stdout and requests() == [], (p.stdout, requests())
            run([BIN / 'panpak', 'remove', '--yes', ID], env)
            run([BIN / 'panpak', 'config', 'set', 'trust.confirm_domain', 'off'], env)
            p = run([*install, '--yes', '--trust-first-key', v1], env)
            assert 'Not confirmed: the check is turned off' in p.stdout and requests() == [], (p.stdout, requests())
            run([BIN / 'panpak', 'remove', '--yes', ID], env)
            run([BIN / 'panpak', 'config', 'set', 'trust.confirm_domain', 'on'], env)
            nodomain = pack('nodomain', 'a', domain=False)
            p = run([*install, '--yes', '--trust-first-key', nodomain], env)
            assert 'Not confirmed: the package names no domain' in p.stdout and requests() == [], (p.stdout, requests())
            run([BIN / 'panpak', 'remove', '--yes', ID], env)
            print('--offline, the setting and a package with no domain: not confirmed, and nothing is asked PASS')

            # 6. A succession of the key at an update: asked before the body, confirmed or contradicted.
            say('file', entry(ID, [pub_a]))
            run([*install, '--yes', v1], env)
            serve = tmp / 'serve'
            serve.mkdir()
            rng = subprocess.Popen([sys.executable, ROOT / 'tests/range_server.py', serve, str(http_port), 'normal'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                time.sleep(0.4)
                v2 = pack('v2', 'b', extra=['--previous', v1, '--rotate-from-file', tmp / 'a.key', '--recovery', rec_a])
                (serve / 'remote.panpak').write_bytes(v2.read_bytes())
                update = [BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '5', ID]
                before = (appdir / 'current').readlink()
                say('file', entry(ID, [pub_a]))  # the domain still lists only the old key
                p = run(update, env, ok=False)
                assert 'PANPAK-E1006' in p.stderr and 'The update was not downloaded' in p.stderr, p.stderr
                assert (appdir / 'current').readlink() == before
                say('file', entry(ID, [pub_b], [pub_a]))
                p = run(update, env)
                assert 'Confirmed: example.test lists the new key of test.example.dlab' in p.stdout, p.stdout
                assert pub_b in (appdir / 'trust').read_text()
                print('a succession: contradicted before the body is downloaded, confirmed when the domain lists the new key PASS')
            finally:
                rng.terminate()
                rng.wait(timeout=5)
        finally:
            server.terminate()
            server.wait(timeout=5)
    print('domain confirm PASS')


if __name__ == '__main__':
    main()
