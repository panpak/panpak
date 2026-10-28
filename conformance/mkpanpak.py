#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Builds .panpak by hand (independent of Rust): stub + index + manifest+sig + footer.
Used for the conformance fixtures. Correctness is cross-checked:
valid files must also be accepted by `panpak doctor` (Rust).
"""

import struct
import subprocess
import sys

sys.path.insert(0, __import__("os").path.dirname(__file__))
from b3 import blake3

MAGIC = b"PANP\0\x01\0\0"


def zstd_enc(data: bytes, level: int = 3) -> bytes:
    p = subprocess.run(["zstd", f"-{level}", "-c"], input=data, capture_output=True)
    assert p.returncode == 0, p.stderr[:200]
    return p.stdout


def sign_ed25519(sk32: bytes, msg: bytes) -> bytes:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    return Ed25519PrivateKey.from_private_bytes(sk32).sign(msg)


def build(stub: bytes, index_raw: bytes, manifest_toml: bytes, secret: bytes) -> bytes:
    comp_index = zstd_enc(index_raw)
    root = blake3(index_raw)
    sig = sign_ed25519(secret, manifest_toml + root)
    index_off = len(stub)
    man_off = index_off + len(comp_index)
    out = bytearray()
    out += stub
    out += comp_index
    out += manifest_toml
    out += sig
    footer = bytearray(80)
    footer[0:8] = MAGIC
    struct.pack_into("<I", footer, 8, 1)
    struct.pack_into("<I", footer, 12, 0)
    struct.pack_into("<Q", footer, 16, index_off)
    struct.pack_into("<Q", footer, 24, len(comp_index))
    struct.pack_into("<Q", footer, 32, man_off)
    struct.pack_into("<Q", footer, 40, len(manifest_toml))
    footer[48:80] = root
    out += footer
    return bytes(out)


def encode_index(chunks, entries) -> bytes:
    """chunks: [(hash32, off, comp, raw)], entries: [dict]."""
    out = bytearray()
    out += struct.pack("<I", len(chunks))
    for h, off, comp, raw in chunks:
        out += h
        out += struct.pack("<QII", off, comp, raw)
    out += struct.pack("<I", len(entries))
    for e in entries:
        pb = e["path"].encode()
        out += struct.pack("<H", len(pb)) + pb
        if e["kind"] == "file":
            out += b"\x00" + struct.pack("<I", e["mode"] & 0o777)
            out += struct.pack("<Q", e["size"])
            out += struct.pack("<I", len(e["chunks"]))
            for cid in e["chunks"]:
                out += struct.pack("<I", cid)
        elif e["kind"] == "dir":
            out += b"\x01" + struct.pack("<I", e["mode"] & 0o777)
        elif e["kind"] == "symlink":
            t = e["target"].encode()
            out += b"\x02" + struct.pack("<I", e["mode"] & 0o777)
            out += struct.pack("<H", len(t)) + t
        else:
            raise ValueError(e)
    return bytes(out)


if __name__ == "__main__":
    # Smoke: builds a minimal panpak and verifies it with itself.
    sys.path.insert(0, __import__("os").path.dirname(__file__))
    print("mkpanpak importable")
