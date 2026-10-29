#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Generate conformance/fixtures: valid + invalid with expected outcome.
Usage: gen.py  (writes to conformance/fixtures/ + expected.json)

TEST KEY: the signing key of every fixture is derived deterministically as
SHA-256 of the fixed seed below. Its secret half is therefore public by
construction: it is a TEST key, only for signing these fixtures, and must
never be used for anything else. No secret file is tracked, and the fixtures
are reproducible byte for byte (Ed25519 signatures are deterministic).
"""

import hashlib
import json
import os
import struct
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from b3 import blake3
from mkpanpak import build, encode_index, sign_ed25519, zstd_enc

FIX = os.path.join(HERE, "fixtures")
os.makedirs(FIX, exist_ok=True)

TEST_KEY_SEED = b"panpak conformance test key v1 - PUBLIC TEST KEY, NEVER USE FOR ANYTHING ELSE"
SECRET = hashlib.sha256(TEST_KEY_SEED).digest()

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

PUB = Ed25519PrivateKey.from_private_bytes(SECRET).public_key()
PUBHEX = "ed25519:" + PUB.public_bytes_raw().hex()

STUB = b"PANPAKTEST-STUB-v1\x00"


def tstr(s):
    """TOML basic string (JSON string syntax is a subset: \\n, \\uXXXX, \\", \\\\)."""
    return json.dumps(s)


def manifest(key=PUBHEX, extra="", version="1.0", entry="bin/hello", iid="org.example.conf", name="Conf"):
    stub_hash = blake3(STUB).hex()
    return (
        f'[app]\nid = {tstr(iid)}\nname = {tstr(name)}\nversion = {tstr(version)}\nentry = {tstr(entry)}\n'
        f"{extra}"
        f'[runtime]\nstub_hash = "{stub_hash}"\n'
        f'[update]\nkey = "{key}"\n'
    ).encode()


def std_index(data: bytes = b"conformance-content"):
    comp = zstd_enc(data)
    chunks = [(blake3(data), len(STUB), len(comp), len(data))]
    entries = [
        {"path": "bin", "mode": 0o755, "kind": "dir"},
        {"path": "bin/hello", "mode": 0o755, "kind": "file", "size": len(data), "chunks": [0]},
        {"path": "latest", "mode": 0o777, "kind": "symlink", "target": "bin/hello"},
    ]
    return chunks, entries, [comp]


EXPECTED = {}


def put(name, blob, ok, err=()):
    assert isinstance(err, (list, tuple))
    with open(os.path.join(FIX, name), "wb") as f:
        f.write(blob)
    EXPECTED[name] = {"ok": ok, "err": list(err)}


def assemble(stub, index_raw, man_toml, sign_key=SECRET, blobs=()):
    """Like mkpanpak.build but without signing/verifying anything: trust blindly."""
    from mkpanpak import MAGIC

    comp_index = zstd_enc(index_raw)
    root = blake3(index_raw)
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    sig = Ed25519PrivateKey.from_private_bytes(sign_key).sign(man_toml + root)
    index_off = len(stub) + sum(len(b) for b in blobs)
    man_off = index_off + len(comp_index)
    out = bytearray(stub + b"".join(blobs) + comp_index + man_toml + sig)
    footer = bytearray(80)
    footer[0:8] = MAGIC
    struct.pack_into("<I", footer, 8, 1)
    struct.pack_into("<I", footer, 12, 0)
    struct.pack_into("<Q", footer, 16, index_off)
    struct.pack_into("<Q", footer, 24, len(comp_index))
    struct.pack_into("<Q", footer, 32, man_off)
    struct.pack_into("<Q", footer, 40, len(man_toml))
    footer[48:80] = root
    out += footer
    return bytes(out)


def main():
    # ---- valid ----
    chunks, entries, blobs = std_index()
    idx = encode_index(chunks, entries)
    man = manifest()
    put("valid-min.panpak", assemble(STUB, idx, man, blobs=blobs), True)

    big = bytes(i % 251 for i in range(5000))
    c0, c1 = big[:3000], big[3000:]
    b0, b1 = zstd_enc(c0), zstd_enc(c1)
    off0, off1 = len(STUB), len(STUB) + len(b0)
    chunks2 = [(blake3(c0), off0, len(b0), len(c0)), (blake3(c1), off1, len(b1), len(c1))]
    entries2 = [
        {"path": "d", "mode": 0o755, "kind": "dir"},
        {"path": "d/big", "mode": 0o644, "kind": "file", "size": len(big), "chunks": [0, 1]},
    ]
    idx2 = encode_index(chunks2, entries2)
    put("valid-multi.panpak", assemble(STUB, idx2, manifest(), blobs=[b0, b1]), True)

    # tree with only a dir (zero chunks)
    idx3 = encode_index([], [{"path": "empty", "mode": 0o755, "kind": "dir"}])
    put("valid-empty-tree.panpak", assemble(STUB, idx3, manifest()), True)

    # ---- invalid: mechanical mutations of valid-min ----
    base = open(os.path.join(FIX, "valid-min.panpak"), "rb").read()

    def mut(name, fn, *errs):
        b = bytearray(base)
        fn(b)
        put(name, bytes(b), False, list(errs))

    # footer magic (not byte 0 of the file: that is the stub, covered by stub_hash)
    mut("invalid-magic.panpak", lambda b: b.__setitem__(len(b) - 80, ord("X")), "wrong magic")
    mut("invalid-version.panpak", lambda b: struct.pack_into("<I", b, len(b) - 72, 2), "unsupported version")
    mut("invalid-flags.panpak", lambda b: struct.pack_into("<I", b, len(b) - 68, 1), "nonzero flags")
    mut("invalid-truncated.panpak", lambda b: b.__delitem__(slice(len(b) - 100, None)), "truncated", "too short", "magic", "consistent")
    # layout: shift man_off by +1 in the footer
    def gap_footer(b):
        moff = struct.unpack_from("<Q", b, len(b) - 48)[0]
        struct.pack_into("<Q", b, len(b) - 48, moff + 1)

    mut("invalid-layout.panpak", gap_footer, "consistent")
    # signature: flip one byte of the SIGNATURE (not of the TOML: that would give a TOML error)
    def sig_tamper(b):
        # signature = last 64+80 bytes...: man_off+man_len .. man_off+man_len+64
        import struct as _st
        fsz = len(b)
        man_len = _st.unpack_from("<Q", b, fsz - 40)[0]
        man_off = _st.unpack_from("<Q", b, fsz - 48)[0]
        b[man_off + man_len + 7] ^= 1

    mut("invalid-sig.panpak", sig_tamper, "signature")
    # stub: flip in the prefix
    mut("invalid-stub.panpak", lambda b: b.__setitem__(3, b[3] ^ 0xFF), "stub does not match")
    # root hash NOT matching the index, but with a VALID signature over
    # (manifest || wrong_root): isolates exactly the root check.
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey as _EP
    _d = b"conformance-content"
    _cb = zstd_enc(_d)
    _idx_r = encode_index([(blake3(_d), len(STUB), len(_cb), len(_d))],
        [{"path": "f", "mode": 0o644, "kind": "file", "size": len(_d), "chunks": [0]}])
    _man_r = manifest()
    _root_ok = blake3(_idx_r)
    _root_bad = bytearray(_root_ok)
    _root_bad[0] ^= 1
    _sig_r = _EP.from_private_bytes(SECRET).sign(_man_r + bytes(_root_bad))
    _comp_r = zstd_enc(_idx_r)
    _ioff_r = len(STUB) + len(_cb)
    _moff_r = _ioff_r + len(_comp_r)
    _out = bytearray(STUB + _cb + _comp_r + _man_r + _sig_r)
    _foot = bytearray(80)
    _foot[0:8] = b"PANP\0\x01\0\0"
    struct.pack_into("<I", _foot, 8, 1)
    struct.pack_into("<I", _foot, 12, 0)
    struct.pack_into("<Q", _foot, 16, _ioff_r)
    struct.pack_into("<Q", _foot, 24, len(_comp_r))
    struct.pack_into("<Q", _foot, 32, _moff_r)
    struct.pack_into("<Q", _foot, 40, len(_man_r))
    _foot[48:80] = bytes(_root_bad)
    put("invalid-root.panpak", bytes(_out + _foot), False, ["root hash"])
    # chunk: flip inside the first compressed blob (after the stub, dynamic len)
    def chunk_tamper(b):
        b[len(STUB) + 2] ^= 0xFF

    mut("invalid-chunk.panpak", chunk_tamper, "chunk", "zstd")

    # ---- invalid: hand-built index (correctly signed) ----
    def bad_index(entries_extra):
        data = b"xy"
        comp = zstd_enc(data)
        cc = [(blake3(data), len(STUB), len(comp), len(data))]
        ee = [{"path": "f", "mode": 0o644, "kind": "file", "size": len(data), "chunks": [0]}]
        idx = encode_index(cc, ee + entries_extra)
        man = manifest()
        return assemble(STUB, idx, man, blobs=[comp]), [comp]

    # path with ..
    put("invalid-dotdot.panpak", bad_index([{"path": "a/../../x", "mode": 0o644, "kind": "dir"}])[0], False, ["unsafe"])
    # path with "." segment (canonical alias: would bypass the duplicate check)
    put("invalid-dotseg.panpak", bad_index([{"path": "a/./b", "mode": 0o644, "kind": "dir"}])[0], False, ["unsafe"])
    # absolute path
    put("invalid-abspath.panpak", bad_index([{"path": "/etc/x", "mode": 0o644, "kind": "dir"}])[0], False, ["unsafe"])
    # duplicate path
    put("invalid-dup.panpak", bad_index([{"path": "f", "mode": 0o644, "kind": "dir"}])[0], False, ["duplicate"])
    # chunk_id past the table
    put(
        "invalid-chunkid.panpak",
        bad_index([{"path": "g", "mode": 0o644, "kind": "file", "size": 2, "chunks": [7]}])[0],
        False,
        ["beyond table", "size"],
    )
    # trailing extra byte in the index
    idx_extra = encode_index(
        [(blake3(b"xy"), len(STUB), len(zstd_enc(b"xy")), 2)],
        [{"path": "f", "mode": 0o644, "kind": "file", "size": 2, "chunks": [0]}],
    ) + b"\x00"
    put("invalid-trailing.panpak", assemble(STUB, idx_extra, manifest()), False, ["trailing"])

    # manifest without [runtime].stub_hash (previous format), regularly signed
    man_old = b'[app]\nid = "org.example.conf"\nname = "Conf"\nversion = "1.0"\nentry = "bin/hello"\n[update]\nkey = "' + PUBHEX.encode() + b'"\n'
    put("invalid-no-stubhash.panpak", assemble(STUB, encode_index(*std_index()[:2]), man_old), False, ["stub_hash", "previous format"])

    # manifest_len that does not add up (footer man_len +1)
    good = bytearray(open(os.path.join(FIX, "valid-min.panpak"), "rb").read())
    ml = struct.unpack_from("<Q", good, len(good) - 40)[0]
    struct.pack_into("<Q", good, len(good) - 40, ml + 1)
    put("invalid-manlen.panpak", bytes(good), False, ["consistent", "truncated", "signature"])

    # ---- invalid manifest fields (correctly signed): spec 3.1.1 ----
    def bad_manifest(name, man, *errs, code):
        put(name, assemble(STUB, encode_index(*std_index()[:2]), man, blobs=std_index()[2]), False, list(errs))
        EXPECTED[name]["code"] = code

    for fname, iid in [
        ("invalid-id-dotdot", "../../escape.app"),
        ("invalid-id-slash", "org/example.app"),
        ("invalid-id-newline", "org.example.app\nExec=/bin/sh"),
        ("invalid-id-nul", "org.example.\u0000app"),
        ("invalid-id-leading-dot", ".org.example.app"),
        ("invalid-id-one-label", "app"),
        ("invalid-id-long", ".".join(["a" * 60] * 3)),
        ("invalid-id-long-label", "org." + "a" * 64),
    ]:
        bad_manifest(fname + ".panpak", manifest(iid=iid), "app.id", code="PANPAK-E0006")
    bad_manifest("invalid-id-digit-segment.panpak", manifest(iid="org.example.1app"), "app.id", code="PANPAK-E0006")
    bad_manifest("invalid-id-hyphen-first.panpak", manifest(iid="-app.example"), "app.id", code="PANPAK-E0006")
    bad_manifest("invalid-id-hyphen-segment.panpak", manifest(iid="org.-app.example"), "app.id", code="PANPAK-E0006")
    # a valid id with an underscore in a segment (D-Bus and Flatpak names allow it)
    put("valid-id-underscore.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(iid="org.example.my_app"), blobs=std_index()[2]), True)
    bad_manifest("invalid-name-newline.panpak", manifest(name="Evil\nExec=/bin/sh"), "app.name", code="PANPAK-E0007")
    bad_manifest("invalid-version-path.panpak", manifest(version="../1"), "app.version", code="PANPAK-E0007")
    bad_manifest("invalid-entry-dotdot.panpak", manifest(entry="../../bin/sh"), "app.entry", code="PANPAK-E0007")
    bad_manifest("invalid-entry-abs.panpak", manifest(entry="/bin/sh"), "app.entry", code="PANPAK-E0007")
    bad_manifest("invalid-abi-name.panpak", manifest(extra='[abi]\nneeds = ["../../x"]\n'), "abi.needs", code="PANPAK-E0007")
    # launch fields and home paths (PANPAK-FORMAT.md section 3.1.2)
    put("valid-launch-home.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(extra=(
        '[permissions]\naudio = true\nnetwork = true\nnetwork_allow_tcp = [2658]\nnetwork_bind_tcp = [2658]\n'
        '[[permissions.home_paths]]\npath = ".config/example-app"\naccess = "rw"\n'
        '[[permissions.home_paths]]\npath = "Documents/Notes"\naccess = "ro"\n'
        '[launch]\nargs = ["--profile", "${XDG_CONFIG_HOME}/example"]\n[launch.env]\nEXAMPLE_DIR = "${XDG_DATA_HOME}/x:${TMPDIR}"\n')),
        blobs=std_index()[2]), True)
    bad_manifest("invalid-launch-env-ld-preload.panpak", manifest(extra='[launch.env]\nLD_PRELOAD = "/tmp/x.so"\n'), "launch.env", code="PANPAK-E0007")
    bad_manifest("invalid-launch-placeholder.panpak", manifest(extra='[launch]\nargs = ["${HOME}/x"]\n'), "launch.args", code="PANPAK-E0007")
    for fname, path in [
        ("dotdot", "a/../b"), ("autostart", ".config/autostart"), ("local-bin", ".local/bin"), ("config-whole", ".config"),
    ]:
        bad_manifest(f"invalid-home-path-{fname}.panpak",
                     manifest(extra=f'[[permissions.home_paths]]\npath = "{path}"\naccess = "rw"\n'), "home_paths", code="PANPAK-E0007")
    # index path with a control character
    _d = b"xy"
    _c = zstd_enc(_d)
    _idx = encode_index([(blake3(_d), len(STUB), len(_c), len(_d))], [{"path": "a\nb", "mode": 0o644, "kind": "file", "size": 2, "chunks": [0]}])
    put("invalid-index-control.panpak", assemble(STUB, _idx, manifest(), blobs=[_c]), False, ["control character"])

    # Negative fixture: a magic other than the one defined by the format is rejected.
    EXPECTED["unknown-magic.bin"] = {"ok": False, "err": ["magic"]}

    with open(os.path.join(FIX, "expected.json"), "w") as f:
        json.dump(EXPECTED, f, indent=1, sort_keys=True)
    print(f"fixtures: {len(EXPECTED)} files, expected failures: {sum(1 for v in EXPECTED.values() if not v['ok'])}")


if __name__ == "__main__":
    main()
