#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The work at the end of the last copy of an app (PANPAK-FORMAT.md 6.7), and downloads over HTTPS with a test authority, with real binaries.

The server speaks TLS with a certificate that a test authority signed; the settings name that authority (`network.ca_file`). Nothing here uses `--allow-insecure-http`.

1. HTTPS: a staged update and a repair work with the authority in the settings; with the built-in list (no `ca_file`) the certificate is refused; with another
   authority in `ca_file` it is refused too (the file replaces the list); a `ca_file` that is not there stops the download and says so.
2. The update staged while a copy runs is applied when the last copy ends, without another start.
3. A file that the verification found damaged and could not repair (the job could not trust the server) is named at the next start, the start downloads nothing
   and does not wait (the app runs), and the repair is made when the last copy ends.
"""
import datetime
import ipaddress
import os
from pathlib import Path
import random
import socket
import subprocess
import sys
import tempfile
import time

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN, PUBLIC, resign  # noqa: E402

from cryptography import x509  # noqa: E402
from cryptography.hazmat.primitives import hashes, serialization  # noqa: E402
from cryptography.hazmat.primitives.asymmetric import ec  # noqa: E402
from cryptography.x509.oid import NameOID  # noqa: E402

ID = 'org.example.incremental'


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok is True:
        assert p.returncode == 0, p.stdout + p.stderr
    elif ok is False:
        assert p.returncode != 0, p.stdout + p.stderr
    return p


def make_ca(tmp, name):
    key = ec.generate_private_key(ec.SECP256R1())
    subject = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, f'panpak test authority {name}')])
    now = datetime.datetime.now(datetime.timezone.utc)
    cert = (x509.CertificateBuilder().subject_name(subject).issuer_name(subject).public_key(key.public_key()).serial_number(x509.random_serial_number())
            .not_valid_before(now - datetime.timedelta(days=1)).not_valid_after(now + datetime.timedelta(days=2))
            .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
            .add_extension(x509.KeyUsage(digital_signature=True, key_cert_sign=True, crl_sign=True, content_commitment=False, key_encipherment=False, data_encipherment=False,
                                         key_agreement=False, encipher_only=False, decipher_only=False), critical=True)
            .sign(key, hashes.SHA256()))
    pem = tmp / f'ca-{name}.pem'
    pem.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    return key, cert, pem


def make_server_cert(tmp, ca_key, ca_cert):
    key = ec.generate_private_key(ec.SECP256R1())
    now = datetime.datetime.now(datetime.timezone.utc)
    cert = (x509.CertificateBuilder().subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, '127.0.0.1')])).issuer_name(ca_cert.subject)
            .public_key(key.public_key()).serial_number(x509.random_serial_number())
            .not_valid_before(now - datetime.timedelta(days=1)).not_valid_after(now + datetime.timedelta(days=2))
            .add_extension(x509.SubjectAlternativeName([x509.IPAddress(ipaddress.ip_address('127.0.0.1'))]), critical=False)
            .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
            .add_extension(x509.ExtendedKeyUsage([x509.oid.ExtendedKeyUsageOID.SERVER_AUTH]), critical=False)
            .sign(ca_key, hashes.SHA256()))
    (tmp / 'server.pem').write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    (tmp / 'server.key').write_bytes(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))


def pack(tmp, env, version):
    tree = tmp / f'tree{version}'
    tree.mkdir()
    (tree / 'hello').write_text(f'#!/bin/sh\necho "version {version}"\nsleep 3\n')
    (tree / 'hello').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Test"\nversion="{version}"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
    rng = random.Random(3)
    for i in range(10):
        d = bytearray(rng.randbytes(150_000))
        if version == '2' and i in (2, 8):
            d[100:110] = b'version two'
        (tree / f'payload{i}').write_bytes(bytes(d))
    out = tmp / f'packed{version}.panpak'
    run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', out], env)
    return out.read_bytes()


def wait_for(cond, seconds=20):
    t = time.time()
    while time.time() - t < seconds:
        if cond():
            return True
        time.sleep(0.2)
    return False


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-exit-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), XDG_CONFIG_HOME=str(tmp / 'config'),
                   XDG_RUNTIME_DIR=str(runtime), LC_ALL='C')
        env.pop('WAYLAND_DISPLAY', None)
        env.pop('PANPAK_NO_BACKGROUND', None)
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        ca_key, ca_cert, ca_pem = make_ca(tmp, 'good')
        _, _, other_pem = make_ca(tmp, 'other')
        make_server_cert(tmp, ca_key, ca_cert)
        sock = socket.socket()
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
        sock.close()
        v1 = resign(pack(tmp, env, '1'), '1.0', port, 'https')
        v2 = resign(pack(tmp, env, '2'), '1.1', port, 'https')
        serve = tmp / 'serve'
        serve.mkdir()
        (serve / 'remote.panpak').write_bytes(v1)
        (tmp / 'v1.panpak').write_bytes(v1)
        store = tmp / 'data/panpak'
        appdir = store / 'apps' / ID
        stub = store / 'bin/panpak-stub'
        panpak = BIN / 'panpak'
        log = tmp / 'requests.log'
        server = subprocess.Popen([sys.executable, str(ROOT / 'tests/range_server.py'), str(serve), str(port), 'normal', f'{tmp / "server.pem"}:{tmp / "server.key"}'],
                                  env=dict(env, PANPAK_RANGE_LOG=str(log)), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        envn = dict(env, PANPAK_NO_BACKGROUND='1')
        try:
            time.sleep(0.8)
            run([panpak, 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', tmp / 'v1.panpak'], envn)
            (appdir / 'update-state').write_text(f'next_check={int(time.time()) + 10**6}\n')   # the daily job is not due: only the exit work is looked at

            # 1. HTTPS and the settings
            (serve / 'remote.panpak').write_bytes(v2)
            p = run([panpak, 'update', '--stage', ID], envn, ok=False)
            assert 'certificate' in (p.stdout + p.stderr).lower() or 'unknown' in (p.stdout + p.stderr).lower() or 'invalid' in (p.stdout + p.stderr).lower(), 'the built-in list refuses the test authority: ' + p.stdout + p.stderr
            run([panpak, 'config', 'set', 'network.ca_file', other_pem], envn)
            p = run([panpak, 'update', '--stage', ID], envn, ok=False)
            assert not (appdir / 'pending').exists(), 'another authority in ca_file: refused'
            run([panpak, 'config', 'set', 'network.ca_file', tmp / 'nowhere.pem'], envn)
            p = run([panpak, 'update', '--stage', ID], envn, ok=False)
            assert 'network.ca_file' in p.stdout + p.stderr and 'nowhere.pem' in p.stdout + p.stderr, p.stdout + p.stderr
            run([panpak, 'config', 'set', 'network.ca_file', ca_pem], envn)
            print('https: the built-in list and another authority refuse the test certificate; a missing ca_file says so PASS')

            # 2. a copy runs while the update is staged; it is applied when the last copy ends
            (serve / 'remote.panpak').write_bytes(v1)
            copy = subprocess.Popen([stub, 'run', appdir / 'current'], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            time.sleep(1.0)
            (serve / 'remote.panpak').write_bytes(v2)
            run([panpak, 'update', '--stage', ID], envn)
            assert (appdir / 'pending').is_symlink(), 'staged over https'
            cur1 = (appdir / 'current').readlink()
            out, err = copy.communicate(timeout=30)
            assert out.startswith('version 1'), out + err
            assert wait_for(lambda: not (appdir / 'pending').exists() and (appdir / 'current').readlink() != cur1), 'the update is applied when the last copy ends'
            assert (appdir / 'previous').readlink() == cur1
            print('the update staged while a copy runs is applied when the last copy ends, with no other start PASS')

            # 3. damage that the job could not repair: named at the start, nothing downloaded, repaired at the end
            tree = store / 'trees' / v2[-32:].hex()
            victim = tree / 'payload5'
            good = victim.read_bytes()
            os.chmod(tree, 0o755)
            os.chmod(victim, 0o644)
            bad = bytearray(good)
            bad[1000] ^= 0xFF
            victim.write_bytes(bytes(bad))
            os.chmod(victim, 0o444)
            os.chmod(tree, 0o555)
            run([panpak, 'config', 'set', 'network.ca_file', other_pem], envn)          # the job cannot trust the server: it finds the damage and cannot repair it
            run([panpak, 'config', 'set', 'background.verify_mb', '100'], envn)
            p = run([panpak, 'background', '--force', ID], envn)
            assert 'could not be repaired' in p.stderr, p.stderr
            assert 'damaged=1' in (appdir / 'update-state').read_text()
            run([panpak, 'config', 'set', 'network.ca_file', ca_pem], envn)
            requests = len(log.read_text().splitlines()) if log.exists() else 0
            t0 = time.time()
            copy = subprocess.Popen([stub, 'run', appdir / 'current'], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            line = copy.stdout.readline()
            first_output = time.time() - t0
            assert line.startswith('version 2'), line
            assert first_output < 3, f'the app opened without waiting for a repair: {first_output:.1f} s'
            time.sleep(0.5)
            assert (len(log.read_text().splitlines()) if log.exists() else 0) == requests, 'the start downloaded nothing'
            assert victim.read_bytes() != good, 'the start did not repair'
            out, err = copy.communicate(timeout=30)
            assert 'do not match the signed index' in err and f'panpak repair {ID}' in err, 'the start names the damage and the command: ' + err
            assert wait_for(lambda: victim.read_bytes() == good and 'damaged=0' in (appdir / 'update-state').read_text()), 'repaired when the last copy ended'
            print('damage the job could not repair: named at the start (with the command), nothing downloaded, the app opens, repaired at the end PASS')
        finally:
            server.terminate()
            server.wait()
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
