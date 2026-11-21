#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Serve the packages of the series over HTTPS with Range requests, with a certificate of a test authority made here.

Usage: python3 serve_https.py <folder with the .panpak files> [--port 8443] [--address 192.168.122.1] [--state DIR]

The certificate is for the addresses given (`--address`, repeatable; the default is 192.168.122.1, the host as the virtual machines of libvirt see it, and 127.0.0.1) and is valid for 30 days. The authority is
`<state>/ca.pem`: copy that file (only that one, never the key) to the virtual machine and give it to the script with `--ca`. The packages are served as the files of `<folder>`; a request with `Range` gets a partial answer
(`panpak update` fetches only the chunks that the installed version does not have). Nothing is written to the folder. It stops with Ctrl+C.
"""
import argparse
import datetime
import ipaddress
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]


def make(state, addresses):
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import NameOID

    now = datetime.datetime.now(datetime.timezone.utc)
    ca_key = ec.generate_private_key(ec.SECP256R1())
    ca_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'panpak bench test authority')])
    ca = (x509.CertificateBuilder().subject_name(ca_name).issuer_name(ca_name).public_key(ca_key.public_key()).serial_number(x509.random_serial_number())
          .not_valid_before(now - datetime.timedelta(days=1)).not_valid_after(now + datetime.timedelta(days=30))
          .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
          .add_extension(x509.KeyUsage(False, False, False, False, False, True, True, False, False), critical=True).sign(ca_key, hashes.SHA256()))
    key = ec.generate_private_key(ec.SECP256R1())
    leaf = (x509.CertificateBuilder().subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, addresses[0])])).issuer_name(ca_name).public_key(key.public_key())
            .serial_number(x509.random_serial_number()).not_valid_before(now - datetime.timedelta(days=1)).not_valid_after(now + datetime.timedelta(days=30))
            .add_extension(x509.SubjectAlternativeName([x509.IPAddress(ipaddress.ip_address(a)) for a in addresses]), critical=False)
            .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True).sign(ca_key, hashes.SHA256()))
    state.mkdir(parents=True, exist_ok=True)
    (state / 'ca.pem').write_bytes(ca.public_bytes(serialization.Encoding.PEM))
    (state / 'server.pem').write_bytes(leaf.public_bytes(serialization.Encoding.PEM))
    (state / 'server.key').write_bytes(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
    (state / 'server.key').chmod(0o600)


def need_keep(state, addresses):
    """True if the files of `state` can be used again: they exist, are valid for at least 7 more days, and the certificate names every address asked for."""
    try:
        from cryptography import x509
        leaf = x509.load_pem_x509_certificate((state / 'server.pem').read_bytes())
        ca = x509.load_pem_x509_certificate((state / 'ca.pem').read_bytes())
        if not (state / 'server.key').exists():
            return False
        limit = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=7)
        if leaf.not_valid_after_utc < limit or ca.not_valid_after_utc < limit:
            return False
        names = {str(i) for i in leaf.extensions.get_extension_for_class(x509.SubjectAlternativeName).value.get_values_for_type(x509.IPAddress)}
        return set(addresses) <= names
    except Exception:
        return False


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('folder')
    ap.add_argument('--port', type=int, default=8443)
    ap.add_argument('--address', action='append', help='an address that the certificate is valid for (default: 192.168.122.1 and 127.0.0.1)')
    ap.add_argument('--renew', action='store_true', help='make a new authority and certificate even if the ones in the state folder are still good (the machines then need the new ca.pem)')
    ap.add_argument('--state', default=str(Path.home() / '.cache/panpak-bench-server'))
    a = ap.parse_args()
    state = Path(a.state)
    addresses = a.address or ['192.168.122.1', '127.0.0.1']
    # Never on every interface: only on addresses of the host that the machines see.
    for x in addresses:
        if ipaddress.ip_address(x).is_unspecified:
            sys.exit(f'refused: {x} would listen on every interface; give the address of the libvirt network (192.168.122.1)')
    # The authority is kept between runs (the machines hold a copy of ca.pem): it is made again only if it is missing, expires within a week, or `--renew` is given.
    if a.renew or not need_keep(state, addresses):
        make(state, addresses)
    print(f'authority: {state / "ca.pem"} (copy this file to the machine, with --ca); serving {a.folder} on port {a.port}, bound to {next((x for x in addresses if not x.startswith('127.')), '127.0.0.1')}, certificate for {", ".join(addresses)}', flush=True)
    import os
    # Bound to the first address that is not the loopback (the one of the virtual machines), not to every interface of the host.
    bind = next((x for x in addresses if not x.startswith('127.')), '127.0.0.1')
    env = dict(os.environ, RANGE_SERVER_BIND=bind)
    sys.exit(subprocess.call([sys.executable, str(ROOT / 'tests/range_server.py'), a.folder, str(a.port), 'normal', f'{state / "server.pem"}:{state / "server.key"}'], env=env))


if __name__ == '__main__':
    main()
