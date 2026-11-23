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
from mkpanpak import critical_paths, build, encode_index, sig_block, sig_entry, sig_message, sign_ed25519, zstd_enc

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


def format_table(critical):
    lst = ", ".join(json.dumps(c) for c in critical)
    return (
        f'[format]\nmin_panpak = 1\ncritical = [{lst}]\ncompression = "zstd"\n'
        'chunking = "fastcdc:16384:65536:262144"\npacked_by = "conformance generator"\n'
    )


FORMAT = format_table([])


RECOVERY = "blake3:" + blake3(Ed25519PrivateKey.from_private_bytes(hashlib.sha256(b'conformance recovery key').digest()).public_key().public_bytes_raw()).hex()


def trust_table(key, recovery=None, extra=''):
    return f'[trust]\nroot = ["{key}"]\nthreshold = 1\nrecovery = "{recovery or RECOVERY}"\n{extra}'


TRUST = trust_table(PUBHEX)
RELEASE = '[release]\nrevision = 1\nfloor = 0\nsecurity = 0\nchannel = "stable"\n'


def manifest(key=PUBHEX, extra="", version="1.0", entry="bin/hello", iid="org.example.conf", name="Conf", fmt=None, release=RELEASE, trust_extra="", trust_text=None):
    """A manifest as `panpak pack` writes it. `fmt` is the `[format]` text; by default it is made from what the manifest uses (the list
    of critical fields is derived, never written by hand), and a test of a refusal gives its own."""
    stub_hash = blake3(STUB).hex()
    parts = (
        f'[app]\nid = {tstr(iid)}\nname = {tstr(name)}\nversion = {tstr(version)}\nentry = {tstr(entry)}\n'
        f"{extra}{release}",
        f'[runtime]\nstub_hash = "{stub_hash}"\n'
        + (trust_table(key, extra=trust_extra) if trust_text is None else trust_text),
    )
    if fmt is None:
        fmt = format_table(critical_paths("".join(parts).encode()))
    return (parts[0] + fmt + parts[1]).encode()


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


def assemble(stub, index_raw, man_toml, sign_key=SECRET, blobs=(), block=None):
    """Like mkpanpak.build but without signing/verifying anything: trust blindly."""
    from mkpanpak import MAGIC

    comp_index = zstd_enc(index_raw)
    root = blake3(index_raw)
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    sig = Ed25519PrivateKey.from_private_bytes(sign_key).sign(sig_message(1, 0, root, man_toml))
    sig = sig_block(sig) if block is None else block(sig)
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

    # The package of Appendix A of the specification: built here from its inputs, and its signature block and footer must be
    # the ones the specification prints (a check that the text and the code do not drift apart).
    APPENDIX_A_SIZE = 987  # the total size that PANPAK-FORMAT.md prints for it
    _stub = b"PANPAKTEST-VECTOR-v1\0"
    _hi = b"hi\n"
    _hic = zstd_enc(_hi)
    _vidx = encode_index([(blake3(_hi), len(_stub), len(_hic), len(_hi))], [
        {"path": "bin", "mode": 0o755, "kind": "dir"},
        {"path": "bin/hi", "mode": 0o755, "kind": "file", "size": 3, "chunks": [0]}])
    _vman = (f'[app]\nid = "org.example.tv"\nname = "TV"\nversion = "1.0"\nentry = "bin/hi"\n'
             + format_table(["release.channel", "release.floor", "release.revision", "release.security", "runtime.stub_hash", "trust.recovery", "trust.root", "trust.threshold"]) +
             RELEASE +
             f'[runtime]\nstub_hash = "{blake3(_stub).hex()}"\n'
             + trust_table(PUBHEX)).encode()
    _vec = assemble(_stub, _vidx, _vman, blobs=[_hic])
    if "--print-appendix" in sys.argv:
        # The bullets of Appendix A of PANPAK-FORMAT.md, from the bytes that this script makes (paste them there).
        _ft = _vec[-80:]
        _io, _il, _mo, _ml = struct.unpack_from("<QQQQ", _ft, 16)
        _me = _mo + _ml
        _be = len(_vec) - 80
        print(f"- total size: {len(_vec)} bytes")
        print(f"- chunk 0 `[{len(_stub)}, {len(_stub) + len(_hic)})`: `{_hic.hex()}`")
        print(f"- compressed index `[{_io}, {_io + _il})` ({_il} bytes): `{_vec[_io:_io + _il].hex()}`")
        print(f"- raw index ({len(_vidx)} bytes): `{_vidx.hex()}`")
        print(f"- root_hash: `{blake3(_vidx).hex()}`")
        print(f"- manifest TOML `[{_mo}, {_me})` ({_ml} bytes):\n  `" + _vman.decode().replace("\n", "\\n") + "`")
        print(f"- signature block `[{_me}, {_be})` ({_be - _me} bytes): `{_vec[_me:_be].hex()}`")
        print(f"- footer `[{_be}, {len(_vec)})` (80 bytes): `{_ft.hex()}` (index_len={_il}, manifest_off={_mo}, manifest_len={_ml})")
        sys.exit(0)
    _spec = open(os.path.join(HERE, "..", "PANPAK-FORMAT.md")).read()
    assert APPENDIX_A_SIZE == len(_vec) and _vec[-80:].hex() in _spec and _vec[len(_vec) - 80 - 72 : len(_vec) - 80].hex() in _spec, "Appendix A of PANPAK-FORMAT.md does not match the code"
    put("valid-appendix-a.panpak", _vec, True)

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
        # the signature is the data of the first entry of the block: 8 bytes after the end of the manifest
        import struct as _st
        fsz = len(b)
        man_len = _st.unpack_from("<Q", b, fsz - 40)[0]
        man_off = _st.unpack_from("<Q", b, fsz - 48)[0]
        b[man_off + man_len + 8 + 7] ^= 1

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
    _sig_r = sig_block(_EP.from_private_bytes(SECRET).sign(sig_message(1, 0, bytes(_root_bad), _man_r)))
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
    # ---- the facts about a file (section 4): flags, SONAME, extension area ----
    def with_file(**kw):
        data = b"xy"
        comp = zstd_enc(data)
        cc = [(blake3(data), len(STUB), len(comp), len(data))]
        ee = [{"path": "f", "mode": 0o755, "kind": "file", "size": len(data), "chunks": [0], **kw}]
        return assemble(STUB, encode_index(cc, ee), manifest(), blobs=[comp])

    put("valid-file-elf-soname.panpak", with_file(flags=1, soname="libf.so.1"), True)
    put("valid-file-script-flag.panpak", with_file(flags=2), True)
    put("valid-file-ext-noncritical.panpak", with_file(ext=struct.pack("<HH", 0x0001, 3) + b"abc"), True)
    put("valid-file-patch.panpak", with_file(patch={"base": bytes(range(32)), "off": 5, "len": 100, "hash": bytes(32)}), True)
    put("valid-file-elf-soname-patch.panpak", with_file(flags=1, soname="libf.so.1", patch={"base": bytes(32), "off": 0, "len": 1, "hash": bytes(32)}), True)
    put("invalid-file-patch-length-zero.panpak", with_file(patch={"base": bytes(32), "off": 0, "len": 0, "hash": bytes(32)}), False, ["patch"])
    put("invalid-file-patch-length-huge.panpak", with_file(patch={"base": bytes(32), "off": 0, "len": (1 << 30) + 1, "hash": bytes(32)}), False, ["patch"])
    put("invalid-file-flag-unknown.panpak", with_file(flags=16), False, ["flags", "update panpak"])
    put("invalid-file-soname-not-elf.panpak", with_file(flags=0, soname="libf.so.1"), False, ["SONAME"])
    put("invalid-file-elf-and-script.panpak", with_file(flags=3), False, ["ELF", "script"])
    put("invalid-file-ext-critical.panpak", with_file(ext=struct.pack("<HH", 0x8001, 0)), False, ["critical", "update panpak"])
    put("invalid-file-ext-cut.panpak", with_file(ext=struct.pack("<HH", 0x0001, 9) + b"abc"), False, ["extension"])
    # trailing extra byte in the index
    idx_extra = encode_index(
        [(blake3(b"xy"), len(STUB), len(zstd_enc(b"xy")), 2)],
        [{"path": "f", "mode": 0o644, "kind": "file", "size": 2, "chunks": [0]}],
    ) + b"\x00"
    put("invalid-trailing.panpak", assemble(STUB, idx_extra, manifest()), False, ["trailing"])

    # ---- the signature block ----
    # A good signature of the old message (manifest || root, no label): the key signed, but not this object.
    _old_sig = Ed25519PrivateKey.from_private_bytes(SECRET).sign(man + blake3(idx))
    put("invalid-sig-unlabeled.panpak", assemble(STUB, idx, man, blobs=blobs, block=lambda s: sig_block(_old_sig)), False, ["signature"])
    # Unknown entries: ignored when not critical, a refusal when critical (before or after the signature).
    put("valid-sig-block-unknown-noncritical.panpak",
        assemble(STUB, idx, man, blobs=blobs, block=lambda s: sig_entry(9, 0, b"a proof of some kind") + sig_block(s, sig_entry(10, 0, b""))), True)
    put("invalid-sig-block-unknown-critical.panpak",
        assemble(STUB, idx, man, blobs=blobs, block=lambda s: sig_block(s, sig_entry(9, 1, b"x"))), False, ["critical"])
    put("invalid-sig-block-two-signatures.panpak", assemble(STUB, idx, man, blobs=blobs, block=lambda s: sig_block(s) + sig_block(s)), False, ["repeated"])
    put("invalid-sig-block-no-signature.panpak", assemble(STUB, idx, man, blobs=blobs, block=lambda s: sig_entry(9, 0, bytes(64))), False, ["no package signature"])
    put("invalid-sig-block-trailing-byte.panpak", assemble(STUB, idx, man, blobs=blobs, block=lambda s: sig_block(s) + b"\0"), False, ["truncated"])
    put("invalid-sig-block-undefined-flags.panpak", assemble(STUB, idx, man, blobs=blobs, block=lambda s: sig_block(s, sig_entry(9, 2, b""))), False, ["flags"])
    put("invalid-sig-block-too-large.panpak", assemble(STUB, idx, man, blobs=blobs, block=lambda s: sig_block(s, sig_entry(9, 0, bytes(64 * 1024)))), False, ["not adjacent", "too large"])
    put("invalid-sig-bare-64-bytes.panpak", assemble(STUB, idx, man, blobs=blobs, block=lambda s: s), False, ["not adjacent"])

    # manifest without [runtime].stub_hash (previous format), regularly signed
    man_old = b'[app]\nid = "org.example.conf"\nname = "Conf"\nversion = "1.0"\nentry = "bin/hello"\n' + format_table(["release.channel", "release.floor", "release.revision", "release.security", "trust.recovery", "trust.root", "trust.threshold"]).encode() + RELEASE.encode() + TRUST.encode()
    put("invalid-no-stubhash.panpak", assemble(STUB, encode_index(*std_index()[:2]), man_old), False, ["stub_hash", "previous format"])

    # ---- [format] (PANPAK-FORMAT.md section 3.1.9): correctly signed, refused for what they say ----
    def fmt(min_panpak=1, critical="[]", compression="zstd", extra=""):
        return (f'[format]\nmin_panpak = {min_panpak}\ncritical = {critical}\ncompression = "{compression}"\n'
                f'chunking = "fastcdc:16384:65536:262144"\npacked_by = "conformance generator"\n{extra}')

    def bad_format(name, man, *errs):
        put(name, assemble(STUB, encode_index(*std_index()[:2]), man, blobs=std_index()[2]), False, list(errs))

    bad_format("invalid-no-format.panpak", manifest(fmt=""), "format")
    bad_format("invalid-format-newer-level.panpak", manifest(fmt=fmt(min_panpak=2)), "update panpak")
    bad_format("invalid-format-level-zero.panpak", manifest(fmt=fmt(min_panpak=0)), "update panpak")
    bad_format("invalid-format-compression.panpak", manifest(fmt=fmt(compression="brotli")), "update panpak")
    bad_format("invalid-format-critical-unknown.panpak", manifest(fmt=fmt(critical='["permissions.deny_udp"]')), "update panpak")
    bad_format("invalid-format-critical-unknown-section.panpak", manifest(fmt=fmt(critical='["sandbox"]')), "update panpak")
    # The manifest was changed after pack and signed with the old list: a field that restricts the app that the list does not name.
    stale = '[permissions]\nnetwork = true\nx11 = true\n'
    bad_format("invalid-format-critical-stale.panpak", manifest(extra=stale, fmt=fmt(critical='["release.channel", "release.floor", "release.revision", "release.security", "runtime.stub_hash", "trust.recovery", "trust.root", "trust.threshold"]')), "critical")
    bad_format("invalid-format-critical-incomplete.panpak", manifest(extra=stale, fmt=fmt(critical='["permissions.network", "release.channel", "release.floor", "release.revision", "release.security", "runtime.stub_hash", "trust.recovery", "trust.root", "trust.threshold"]')), "critical")
    put("valid-format-critical-complete.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(extra=stale), blobs=std_index()[2]), True)
    bad_format("invalid-format-unknown-key.panpak", manifest(fmt=fmt(extra='future = 1\n')), "format")
    bad_format("invalid-format-critical-malformed.panpak", manifest(fmt=fmt(critical='["Permissions.X"]')), "critical")
    put("valid-format-critical-known.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(
        extra='[permissions]\nnetwork = true\n', fmt=fmt(critical='["permissions.network", "release.channel", "release.floor", "release.revision", "release.security", "runtime.stub_hash", "trust.recovery", "trust.root", "trust.threshold"]')), blobs=std_index()[2]), True)
    # An unknown key outside [format] that is not named in `critical` is read as before (it is ignored, with a warning).
    put("valid-unknown-key-not-critical.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(
        extra='[desktop]\nfuture_thing = 1\n'), blobs=std_index()[2]), True)

    # ---- [release] (PANPAK-FORMAT.md section 3.1.10) ----
    def bad_release(name, rel, *errs):
        bad_format(name, manifest(release=rel), *errs)

    bad_release("invalid-no-release.panpak", "", "release")
    bad_release("invalid-release-revision-zero.panpak", '[release]\nrevision = 0\n', "revision")
    bad_release("invalid-release-floor-above-revision.panpak", '[release]\nrevision = 3\nfloor = 4\n', "floor")
    bad_release("invalid-release-security-above-revision.panpak", '[release]\nrevision = 3\nsecurity = 4\n', "security")
    bad_release("invalid-release-channel.panpak", '[release]\nrevision = 3\nchannel = "Beta"\n', "channel")
    bad_release("invalid-release-history-hash.panpak", '[release]\nrevision = 3\nhistory = "abc"\n', "history")
    bad_release("invalid-release-unknown-key.panpak", '[release]\nrevision = 3\nfuture = 1\n', "release")
    bad_release("invalid-release-missing-revision.panpak", '[release]\nfloor = 0\n', "release")
    put("valid-release-full.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(
        release='[release]\nrevision = 7\nfloor = 5\nsecurity = 6\nchannel = "beta-2"\nhistory = "' + "ab" * 32 + '"\n'), blobs=std_index()[2]), True)

    # ---- [trust] (PANPAK-FORMAT.md section 3.2) ----
    def bad_trust(name, man, *errs):
        bad_format(name, man, *errs)

    other = "ed25519:" + "11" * 32
    step = lambda by="root", frm=PUBHEX, to=other, rec=RECOVERY: f'[[trust.chain]]\nby = "{by}"\nfrom = "{frm}"\nto = "{to}"\nrecovery = "{rec}"\nsig = "{"ab" * 64}"\n'
    bad_trust("invalid-no-trust.panpak", manifest(trust_text=""), "trust")
    bad_trust("invalid-trust-two-roots.panpak", manifest(trust_text=f'[trust]\nroot = ["{PUBHEX}", "{other}"]\nthreshold = 1\nrecovery = "{RECOVERY}"\n'), "update panpak")
    bad_trust("invalid-trust-threshold-two.panpak", manifest(trust_text=f'[trust]\nroot = ["{PUBHEX}"]\nthreshold = 2\nrecovery = "{RECOVERY}"\n'), "update panpak")
    bad_trust("invalid-trust-bad-root.panpak", manifest(trust_text=f'[trust]\nroot = ["ed25519:zz"]\nthreshold = 1\nrecovery = "{RECOVERY}"\n'), "key")
    bad_trust("invalid-trust-bad-recovery.panpak", manifest(trust_text=f'[trust]\nroot = ["{PUBHEX}"]\nthreshold = 1\nrecovery = "sha256:00"\n'), "recovery")
    bad_trust("invalid-trust-domain-mismatch.panpak", manifest(trust_extra='domain = "example.com"\n'), "domain")
    bad_trust("invalid-trust-domain-one-label.panpak", manifest(trust_extra='domain = "org"\n'), "domain")
    bad_trust("invalid-trust-unknown-key.panpak", manifest(trust_extra='future = 1\n'), "trust")
    bad_trust("invalid-trust-update-key.panpak", manifest(extra=f'[update]\nkey = "{PUBHEX}"\n'), "update")
    bad_trust("invalid-trust-chain-too-long.panpak", manifest(trust_extra=step() * 9), "longer than 8")
    bad_trust("invalid-trust-chain-unknown-by.panpak", manifest(trust_extra=step(by="threshold")), "update panpak")
    bad_trust("invalid-trust-chain-bad-step.panpak", manifest(trust_extra=step(to="ed25519:00")), "malformed")
    put("valid-trust-domain.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(trust_extra='domain = "example.org"\n'), blobs=std_index()[2]), True)
    put("valid-trust-chain.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(trust_extra=step() + step(by="recovery")), blobs=std_index()[2]), True)

    # ---- permissions.devices, a list of categories (section 3.1.5) ----
    def bad_devices(name, value, err):
        put(name, assemble(STUB, encode_index(*std_index()[:2]), manifest(extra=f"[permissions]\ndevices = {value}\n"), blobs=std_index()[2]), False, [err])

    put("valid-devices-list.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(extra='[permissions]\ndevices = ["gamepad", "camera"]\n'), blobs=std_index()[2]), True)
    bad_devices("invalid-devices-boolean.panpak", "true", "devices")
    bad_devices("invalid-devices-unknown.panpak", '["joystick"]', "update panpak")
    bad_devices("invalid-devices-reserved.panpak", '["usb"]', "update panpak")
    bad_devices("invalid-devices-empty.panpak", "[]", "devices")
    bad_devices("invalid-devices-repeat.panpak", '["gamepad", "gamepad"]', "devices")

    # ---- the [host] table (section 3.1.11) ----
    def bad_host(name, man, err):
        put(name, assemble(STUB, encode_index(*std_index()[:2]), man, blobs=std_index()[2]), False, [err])

    full_host = '[host]\narch = "x86_64"\nglibc = "2.28"\nkernel = "5.15"\nlandlock = 3\ndisplay = ["x11", "wayland"]\nportals = ["document"]\nbus = "private"\n'
    put("valid-host.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(extra=full_host), blobs=std_index()[2]), True)
    bad_host("invalid-host-display-unknown.panpak", manifest(extra='[host]\ndisplay = ["mir"]\n'), "update panpak")
    bad_host("invalid-host-arch-unknown.panpak", manifest(extra='[host]\narch = "riscv64"\n'), "update panpak")
    bad_host("invalid-host-bus-unknown.panpak", manifest(extra='[host]\nbus = "shared"\n'), "update panpak")
    bad_host("invalid-host-kernel-form.panpak", manifest(extra='[host]\nkernel = "five"\n'), "host.kernel")
    bad_host("invalid-host-landlock-range.panpak", manifest(extra='[host]\nlandlock = 12\n'), "host.landlock")
    bad_host("invalid-host-display-repeat.panpak", manifest(extra='[host]\ndisplay = ["x11", "x11"]\n'), "host.display")
    bad_host("invalid-host-unknown-key.panpak", manifest(extra='[host]\nfuture = 1\n'), "host")
    bad_host("invalid-host-critical-missing.panpak", manifest(extra='[host]\nkernel = "5.15"\n', fmt=fmt(critical='["release.channel", "release.floor", "release.revision", "release.security", "runtime.stub_hash", "trust.recovery", "trust.root", "trust.threshold"]')), "critical")

    # ---- the [sbom] table (section 3.1.12): not critical, so a reader that does not use it still reads the package ----
    h64 = "ab" * 32
    put("valid-sbom.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(extra=f'[sbom]\nhash = "{h64}"\ncoverage = "best-effort"\n'), blobs=std_index()[2]), True)
    put("valid-sbom-unknown-coverage.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(extra=f'[sbom]\nhash = "{h64}"\ncoverage = "full-audit"\n'), blobs=std_index()[2]), True)
    bad_host("invalid-sbom-hash-form.panpak", manifest(extra='[sbom]\nhash = "ABCD"\ncoverage = "build"\n'), "sbom.hash")
    bad_host("invalid-sbom-coverage-form.panpak", manifest(extra=f'[sbom]\nhash = "{h64}"\ncoverage = "Best Effort"\n'), "sbom.coverage")
    bad_host("invalid-sbom-coverage-missing.panpak", manifest(extra=f'[sbom]\nhash = "{h64}"\n'), "sbom")

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
    # user folders and URL schemes (PANPAK-FORMAT.md section 3.1.4)
    put("valid-xdg-dirs-desktop.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(extra=(
        '[permissions]\n[[permissions.xdg_dirs]]\nname = "music"\naccess = "rw"\n'
        '[[permissions.xdg_dirs]]\nname = "documents"\naccess = "ro"\n'
        '[desktop]\nschemes = ["example-app", "x-example.v2+test"]\n')),
        blobs=std_index()[2]), True)
    for fname, extra, field in [
        ("xdg-name", '[[permissions.xdg_dirs]]\nname = "desktop"\naccess = "ro"\n', "xdg_dirs"),
        ("xdg-access", '[[permissions.xdg_dirs]]\nname = "music"\naccess = "wo"\n', "xdg_dirs"),
        ("xdg-twice", '[[permissions.xdg_dirs]]\nname = "music"\naccess = "ro"\n[[permissions.xdg_dirs]]\nname = "music"\naccess = "rw"\n', "xdg_dirs"),
        ("scheme-reserved", '[desktop]\nschemes = ["https"]\n', "desktop.schemes"),
        ("scheme-file", '[desktop]\nschemes = ["file"]\n', "desktop.schemes"),
        ("scheme-upper", '[desktop]\nschemes = ["MyApp"]\n', "desktop.schemes"),
        ("scheme-digit-first", '[desktop]\nschemes = ["1app"]\n', "desktop.schemes"),
        ("scheme-slash", '[desktop]\nschemes = ["a/b"]\n', "desktop.schemes"),
        ("scheme-twice", '[desktop]\nschemes = ["app", "app"]\n', "desktop.schemes"),
        ("scheme-long", '[desktop]\nschemes = ["' + "a" * 33 + '"]\n', "desktop.schemes"),
        ("scheme-many", '[desktop]\nschemes = [' + ", ".join(f'"a{i}"' for i in range(9)) + ']\n', "desktop.schemes"),
    ]:
        bad_manifest(f"invalid-{fname}.panpak", manifest(extra=extra), field, code="PANPAK-E0007")
    # notifications, tray, extra bus names and the window class (PANPAK-FORMAT.md section 3.1.5)
    put("valid-bus-fields.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(extra=(
        '[permissions]\nnotifications = true\ntray = true\ndbus_own = ["org.example.app.Second", "org.kde.conf"]\n'
        '[desktop]\nwm_class = "Example-App"\n')),
        blobs=std_index()[2]), True)
    for fname, extra, field in [
        ("dbus-own-freedesktop", '[permissions]\ndbus_own = ["org.freedesktop.Notifications"]\n', "dbus_own"),
        ("dbus-own-portal", '[permissions]\ndbus_own = ["org.freedesktop.portal.Desktop"]\n', "dbus_own"),
        ("dbus-own-gnome-shell", '[permissions]\ndbus_own = ["org.gnome.Shell.Screenshot"]\n', "dbus_own"),
        ("dbus-own-dconf", '[permissions]\ndbus_own = ["ca.desrt.dconf"]\n', "dbus_own"),
        ("dbus-own-a11y", '[permissions]\ndbus_own = ["org.a11y.Bus"]\n', "dbus_own"),
        ("dbus-own-kde-service", '[permissions]\ndbus_own = ["org.kde.StatusNotifierWatcher"]\n', "dbus_own"),
        ("dbus-own-kde-other", '[permissions]\ndbus_own = ["org.kde.plasmashell"]\n', "dbus_own"),
        ("dbus-own-wildcard", '[permissions]\ndbus_own = ["org.example.app.*"]\n', "dbus_own"),
        ("dbus-own-short", '[permissions]\ndbus_own = ["org.example"]\n', "dbus_own"),
        ("dbus-own-twice", '[permissions]\ndbus_own = ["org.example.a.B", "org.example.a.B"]\n', "dbus_own"),
        ("dbus-own-many", '[permissions]\ndbus_own = [' + ", ".join(f'"org.example.a.n{i}"' for i in range(9)) + ']\n', "dbus_own"),
        ("wm-class-edge-space", '[desktop]\nwm_class = " App"\n', "wm_class"),
        ("wm-class-semicolon", '[desktop]\nwm_class = "App;X"\n', "wm_class"),
        ("wm-class-empty", '[desktop]\nwm_class = ""\n', "wm_class"),
        ("wm-class-long", '[desktop]\nwm_class = "' + "a" * 101 + '"\n', "wm_class"),
    ]:
        bad_manifest(f"invalid-{fname}.panpak", manifest(extra=extra), field, code="PANPAK-E0007")
    # A value of the wrong type is a TOML type error in Rust (no field code), and an invalid field in the verifier.
    for fname, extra, field in [
        ("dbus-own-type", '[permissions]\ndbus_own = "org.example.app.X"\n', "dbus_own"),
        ("notifications-type", '[permissions]\nnotifications = "yes"\n', "notifications"),
    ]:
        put(f"invalid-{fname}.panpak", assemble(STUB, encode_index(*std_index()[:2]), manifest(extra=extra), blobs=std_index()[2]), False, [field])
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
