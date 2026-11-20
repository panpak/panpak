#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The confirmation of the domain of an author (PANPAK-FORMAT.md 3.2.1), against a local HTTPS server with a certificate of a test authority: confirmed, not confirmed
(no file, an app that is not listed, a redirect to another host, a file that is too big, a server that does not answer, the check turned off, `--offline`), contradicted
(other keys, a key that the domain says was replaced), certificates that must never confirm (another authority, expired, for another name), a domain with
characters outside ASCII shown as Punycode, the release build that does not read the test hook, the first install with `--yes` and no `--trust-first-key` when the domain confirms, a refusal that `--yes` and
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
from incremental import BIN as RELEASE  # noqa: E402

# The hook that points the check at a local server is in the test build only (the feature of the test points), like the failpoints: this test runs that build, and
# a section of it runs the release build to show that the release does not have the hook.
BIN = ROOT / 'target/failpoints/x86_64-unknown-linux-musl/release'
if not (BIN / 'panpak').exists():
    sys.exit(f'{BIN}/panpak is missing: cargo build --release --target x86_64-unknown-linux-musl --target-dir target/failpoints --features panpak/failpoints,panpak-stub/failpoints -p panpak -p panpak-stub')

DOMAIN = 'example.test'
ID = 'test.example.dlab'
IDN_ID = 'test.xn--bcher-kva.dlab'   # the app of the domain bücher.test, which is xn--bcher-kva.test


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

        # A test authority and certificates made with it: the good one, and three that must never confirm anything.
        import datetime
        from cryptography import x509
        from cryptography.hazmat.primitives import hashes, serialization
        from cryptography.hazmat.primitives.asymmetric import ec
        from cryptography.x509.oid import NameOID

        def pem_key(k, path):
            Path(path).write_bytes(k.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))

        def authority(cn):
            k = ec.generate_private_key(ec.SECP256R1())
            name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, cn)])
            now = datetime.datetime.now(datetime.timezone.utc)
            c = (x509.CertificateBuilder().subject_name(name).issuer_name(name).public_key(k.public_key()).serial_number(x509.random_serial_number())
                 .not_valid_before(now - datetime.timedelta(days=1)).not_valid_after(now + datetime.timedelta(days=2))
                 .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
                 .add_extension(x509.KeyUsage(False, False, False, False, False, True, True, False, False), critical=True)
                 .sign(k, hashes.SHA256()))
            return k, c

        def leaf(ca, names, name, valid=(-1, 2)):
            """A certificate for `names` made by the authority `ca`, valid from `valid[0]` to `valid[1]` days from now. Returns the paths of the chain and of the key."""
            ca_key, ca_cert = ca
            k = ec.generate_private_key(ec.SECP256R1())
            now = datetime.datetime.now(datetime.timezone.utc)
            c = (x509.CertificateBuilder().subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, names[0])])).issuer_name(ca_cert.subject)
                 .public_key(k.public_key()).serial_number(x509.random_serial_number())
                 .not_valid_before(now + datetime.timedelta(days=valid[0])).not_valid_after(now + datetime.timedelta(days=valid[1]))
                 .add_extension(x509.SubjectAlternativeName([x509.DNSName(n) for n in names]), critical=False)
                 .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
                 .sign(ca_key, hashes.SHA256()))
            cert, key = tmp / f'{name}.pem', tmp / f'{name}.key.pem'
            cert.write_bytes(c.public_bytes(serialization.Encoding.PEM))
            pem_key(k, key)
            return cert, key

        trusted, stranger = authority('panpak-test-ca'), authority('another-ca')
        ca = tmp / 'ca.pem'
        ca.write_bytes(trusted[1].public_bytes(serialization.Encoding.PEM))
        good = leaf(trusted, [DOMAIN, 'xn--bcher-kva.test'], 'good')
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
        pub_c, _ = new_key('c')

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

        def serve_tls(cert_key):
            srv = subprocess.Popen([sys.executable, ROOT / 'tests/domain_server.py', str(tls_port), str(cert_key[0]), str(cert_key[1]), str(ctl)], env=env,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            time.sleep(0.6)
            return srv

        server = serve_tls(good)
        try:
            v1 = pack('v1', 'a')

            # 1. Not confirmed: the domain does not list the app. The privacy sentence is said once. `--yes` alone is not enough, as before.
            say('file', entry('test.example.other', [pub_a]))
            p = run([*install, '--yes', v1], env, ok=False)
            assert 'PANPAK-E1003' in p.stderr, p.stderr
            assert '\nPublished by:\n\n    example.test\n\n' in p.stdout, p.stdout  # the domain, alone on its line, not in capitals
            assert 'Not confirmed: example.test does not list test.example.dlab' in p.stdout and 'Install it only if you trust the place where you got the file.' in p.stdout, p.stdout
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
            assert '\nPublished by:\n\n    example.test\n\n' in p.stdout and 'Confirmed: example.test lists this key for test.example.dlab' in p.stdout, p.stdout
            assert 'Install it only if' not in p.stdout, 'the warning is for what is not confirmed'
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

            # 5b. A certificate that must never confirm: another authority, an expired one, one for another name. Each ends as "not confirmed" with the reason, the
            # server never sees a request, and `--yes` alone still does not install.
            say('file', entry(ID, [pub_a]))
            for what, cert in [('another authority', leaf(stranger, [DOMAIN], 'stranger')),
                               ('an expired certificate', leaf(trusted, [DOMAIN], 'expired', valid=(-10, -1))),
                               ('a certificate for another name', leaf(trusted, ['other.test'], 'othername'))]:
                server.terminate()
                server.wait(timeout=5)
                server = serve_tls(cert)
                (ctl / 'requests.log').write_text('')
                p = run([*install, '--yes', v1], env, ok=False)
                assert 'PANPAK-E1003' in p.stderr, (what, p.stderr)
                assert 'Not confirmed: example.test could not be asked' in p.stdout and 'Install it only if you trust the place' in p.stdout, (what, p.stdout)
                assert 'Confirmed: example.test' not in p.stdout and not (appdir / 'trust').exists() and not (appdir / 'current').exists(), (what, p.stdout)
                p = run([*install, '--yes', '--trust-first-key', v1], env)
                assert 'Not confirmed' in p.stdout and 'Confirmed: example.test' not in p.stdout and requests() == [], (what, p.stdout, requests())
                run([BIN / 'panpak', 'remove', '--yes', ID], env)
            server.terminate()
            server.wait(timeout=5)
            server = serve_tls(good)
            print('certificates: another authority, an expired one and one for another name are never "confirmed", and the server sees no request PASS')

            # 5c. A domain with characters outside ASCII is the form of a browser: `pack` writes `xn--...`, `inspect` and the prompt show it, the request is made for it.
            idn_tree = tmp / 'tree-idn'
            idn_tree.mkdir()
            (idn_tree / 'hello').write_text('#!/bin/sh\necho idn\n')
            (idn_tree / 'hello').chmod(0o755)
            (idn_tree / 'panpak.toml').write_text(f'[app]\nid="{IDN_ID}"\nname="Idn Lab"\nversion="1"\nentry="hello"\n[trust]\ndomain="b\u00fccher.test"\n')
            idn = tmp / 'idn.panpak'
            run([BIN / 'panpak', 'pack', idn_tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'a.key', '-o', idn], env)
            manifest = tmp / 'idn-manifest.toml'
            run([BIN / 'panpak', 'dump-manifest', idn, manifest], env)
            assert 'domain = "xn--bcher-kva.test"' in manifest.read_text() and '\u00fc' not in manifest.read_text(), manifest.read_text()
            assert 'domain: xn--bcher-kva.test' in run([BIN / 'panpak', 'inspect', idn], env).stdout
            # The same domain written in capitals, or with the u and the diaeresis as two characters (not NFC), is the same domain: `pack` writes one form.
            for variant in ('B\u00dcCHER.TEST', 'bu\u0308cher.test', 'BU\u0308CHER.test'):
                (idn_tree / 'panpak.toml').write_text(f'[app]\nid="{IDN_ID}"\nname="Idn Lab"\nversion="1"\nentry="hello"\n[trust]\ndomain="{variant}"\n')
                other = tmp / 'idn-variant.panpak'
                run([BIN / 'panpak', 'pack', idn_tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'a.key', '-o', other], env)
                run([BIN / 'panpak', 'dump-manifest', other, manifest], env)
                assert 'domain = "xn--bcher-kva.test"' in manifest.read_text(), (variant, manifest.read_text())
            idn_env = dict(env, PANPAK_TEST_RESOLVE=f'xn--bcher-kva.test=127.0.0.1:{tls_port}')
            say('file', entry(IDN_ID, [pub_a]))
            p = run([*install, '--yes', idn], idn_env)
            assert '\nPublished by:\n\n    xn--bcher-kva.test\n\n' in p.stdout and 'Confirmed: xn--bcher-kva.test lists this key' in p.stdout, p.stdout
            assert '\u00fc' not in p.stdout, 'the name is shown only in the form that a browser shows'
            run([BIN / 'panpak', 'remove', '--yes', IDN_ID], idn_env)
            print('a domain with other characters is written and shown as xn--..., and the request is made for that name PASS')

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
                # Not confirmed (the domain has no file): the succession is valid, the update is made, and the person is told what that means.
                say('notfound')
                p = run(update, env)
                assert 'The new key is signed by the one before it, so the update is valid. The author\'s site does not confirm it, though: if anything looks strange to you, ask the author.' in p.stdout, p.stdout
                assert 'HTTP 404' in p.stdout and 'Install it only' not in p.stdout, p.stdout
                assert pub_b in (appdir / 'trust').read_text()
                # Confirmed: the next succession (b to c) with the domain listing the new key.
                v3 = pack('v3', 'c', extra=['--previous', v2, '--rotate-from-file', tmp / 'b.key', '--recovery', rec_a])
                (serve / 'remote.panpak').write_bytes(v3.read_bytes())
                say('file', entry(ID, [pub_c], [pub_b]))
                p = run(update, env)
                assert 'Confirmed: example.test lists the new key of test.example.dlab' in p.stdout and 'does not confirm it' not in p.stdout, p.stdout
                assert pub_c in (appdir / 'trust').read_text()
                print('a succession: contradicted before the body, not confirmed (valid, with the sentence), confirmed when the domain lists the new key PASS')
            finally:
                rng.terminate()
                rng.wait(timeout=5)
            # 7. The release build does not have the hook: with the variable set it connects to the real name (that is not there), never to the local server, and it
            # does not hold the name of the variable at all.
            rel_env = dict(env, PANPAK_TEST_RESOLVE=f'{DOMAIN}=127.0.0.1:{tls_port}')
            rel_pkg = tmp / 'rel-v1.panpak'
            run([RELEASE / 'panpak', 'pack', tmp / 'tree-v1', '--stub', RELEASE / 'panpak-stub', '--secret-file', tmp / 'a.key', '-o', rel_pkg], env)
            run([BIN / 'panpak', 'remove', '--yes', ID], env)
            say('file', entry(ID, [pub_a]))
            p = run([RELEASE / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', RELEASE / 'panpak-stub', rel_pkg], rel_env)
            assert 'Not confirmed: example.test could not be asked' in p.stdout and requests() == [], (p.stdout, requests())
            for exe in ('panpak', 'panpak-stub'):
                assert b'PANPAK_TEST_RESOLVE' not in (RELEASE / exe).read_bytes(), f'the release {exe} holds the name of the test hook'
            assert b'PANPAK_TEST_RESOLVE' in (BIN / 'panpak').read_bytes(), 'the test build must hold it (the check above could not see it otherwise)'
            print('the release build ignores PANPAK_TEST_RESOLVE and does not hold its name PASS')
        finally:
            server.terminate()
            server.wait(timeout=5)
    print('domain confirm PASS')


if __name__ == '__main__':
    main()
