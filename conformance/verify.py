#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Independent Panpak v1 format verifier.

Written ONLY from the spec (PANPAK-FORMAT.md), without reading the Rust code.
Checks: footer, adjacent layout, Ed25519 signature (manifest||root_hash),
root hash, index, chunks (hash+before use), stub_hash, safe paths.

Usage: verify.py <file.panpak> [--json]
Exit 0 if valid (prints VALID), 1 if invalid (reason on stderr / JSON).
"""

import json
import struct
import subprocess
import sys

sys.path.insert(0, __import__("os").path.dirname(__file__))
from b3 import blake3

MAGIC = b"PANP\0\x01\0\0"
FOOTER_LEN = 80
SIG_LEN = 64
MAX_MANIFEST = 1 << 20
MAX_INDEX_COMP = 1 << 28
MAX_INDEX_RAW = 1 << 28
MAX_CHUNK_RAW = 1 << 20
MAX_CHUNK_COMP = 1 << 21


class Fail(Exception):
    pass


def u16(b, o):
    if o + 2 > len(b):
        raise Fail("truncated index (u16)")
    return struct.unpack_from("<H", b, o)[0]


def u32(b, o):
    if o + 4 > len(b):
        raise Fail("truncated index (u32)")
    return struct.unpack_from("<I", b, o)[0]


def u64(b, o):
    if o + 8 > len(b):
        raise Fail("truncated index (u64)")
    return struct.unpack_from("<Q", b, o)[0]


def zstd_decompress(data: bytes, what: str) -> bytes:
    p = subprocess.run(["zstd", "-d", "-c"], input=data, capture_output=True)
    if p.returncode != 0:
        raise Fail(f"{what}: zstd decompression failed")
    return p.stdout


def ed25519_verify(vk_hex: str, msg: bytes, sig: bytes):
    # Key in strict "ed25519:" + 64 lowercase-hex format.
    if not vk_hex.startswith("ed25519:"):
        raise Fail("key without ed25519: prefix")
    hx = vk_hex[8:]
    if len(hx) != 64 or any(c not in "0123456789abcdef" for c in hx):
        raise Fail("key must be 64 lowercase [0-9a-f] characters")
    vk = bytes.fromhex(hx)
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

    try:
        Ed25519PublicKey.from_public_bytes(vk).verify(sig, msg)
    except InvalidSignature:
        raise Fail("invalid Ed25519 signature")
    except Exception as e:
        raise Fail(f"malformed key/signature: {e}")


def parse_index(raw: bytes):
    """Return (chunks, entries). chunks: list[dict]; entries: list[dict]."""
    p = 0
    n = u32(raw, p)
    p += 4
    if n > (len(raw) // 48) + 1:
        raise Fail(f"excessive n_chunks: {n}")
    chunks = []
    for i in range(n):
        if p + 48 > len(raw):  # [u8;32] + u64 + u32 + u32
            raise Fail(f"chunk {i} truncated")
        h, off, comp, rlen = raw[p : p + 32], u64(raw, p + 32), u32(raw, p + 40), u32(raw, p + 44)
        if not (1 <= comp <= MAX_CHUNK_COMP):
            raise Fail(f"chunk {i}: comp_len out of range ({comp})")
        if not (1 <= rlen <= MAX_CHUNK_RAW):
            raise Fail(f"chunk {i}: raw_len out of range ({rlen})")
        chunks.append({"hash": h, "off": off, "comp": comp, "raw": rlen})
        p += 48
    n = u32(raw, p)
    p += 4
    entries = []
    seen = set()
    for _ in range(n):
        ln = u16(raw, p)
        p += 2
        if not (1 <= ln <= 4096):
            raise Fail(f"path_len out of range: {ln}")
        if p + ln > len(raw):
            raise Fail("truncated path")
        try:
            path = raw[p : p + ln].decode("utf-8")
        except UnicodeDecodeError:
            raise Fail("non-UTF-8 path")
        p += ln
        parts = path.split("/")
        if any(c in ("", ".", "..") for c in parts):
            raise Fail(f"unsafe path (empty, . or ..): {path!r}")
        if path in seen:
            raise Fail(f"duplicate path: {path!r}")
        seen.add(path)
        kind = raw[p] if p < len(raw) else (_ for _ in ()).throw(Fail("truncated kind"))
        p += 1
        mode = u32(raw, p) & 0o777
        p += 4
        if kind == 0:
            size, k = u64(raw, p), u32(raw, p + 8)
            p += 12
            ids = []
            total = 0
            for _ in range(k):
                cid = u32(raw, p)
                p += 4
                if cid >= len(chunks):
                    raise Fail(f"chunk_id {cid} beyond table ({len(chunks)})")
                total += chunks[cid]["raw"]
                ids.append(cid)
            if total != size:
                raise Fail(f"size {size} does not match chunks ({total})")
            entries.append({"path": path, "mode": mode, "kind": "file", "size": size, "chunks": ids})
        elif kind == 1:
            entries.append({"path": path, "mode": mode, "kind": "dir"})
        elif kind == 2:
            ln = u16(raw, p)
            p += 2
            if not (1 <= ln <= 4096):
                raise Fail(f"symlink target out of range: {ln}")
            if p + ln > len(raw):
                raise Fail("truncated symlink target")
            try:
                tgt = raw[p : p + ln].decode("utf-8")
            except UnicodeDecodeError:
                raise Fail("non-UTF-8 symlink target")
            p += ln
            if not tgt:
                raise Fail("empty symlink target")
            entries.append({"path": path, "mode": mode, "kind": "symlink", "target": tgt})
        else:
            raise Fail(f"unknown kind: {kind}")
    if p != len(raw):
        raise Fail(f"trailing bytes after index ({len(raw) - p})")
    return chunks, entries


def check(data: bytes):
    size = len(data)
    if size <= FOOTER_LEN:
        raise Fail("file too short")
    foot = data[size - FOOTER_LEN :]
    if foot[0:8] != MAGIC:
        raise Fail("wrong magic")
    if u32(foot, 8) != 1:
        raise Fail(f"unsupported version: {u32(foot, 8)}")
    if u32(foot, 12) != 0:
        raise Fail(f"nonzero flags: {u32(foot, 12)}")
    index_off, index_len = u64(foot, 16), u64(foot, 24)
    man_off, man_len = u64(foot, 32), u64(foot, 40)
    root = foot[48:80]
    # Layout coherence (adjacency, no holes nor extras).
    if index_off + index_len != man_off:
        raise Fail("inconsistent layout: index not adjacent to manifest")
    if man_off + man_len + SIG_LEN != size - FOOTER_LEN:
        raise Fail("inconsistent layout: manifest+signature not adjacent to footer")
    if man_len > MAX_MANIFEST:
        raise Fail("manifest too large")
    if index_len > MAX_INDEX_COMP:
        raise Fail("compressed index too large")
    # Manifest + signature.
    toml_b = data[man_off : man_off + man_len]
    sig = data[man_off + man_len : man_off + man_len + SIG_LEN]
    if len(toml_b) != man_len or len(sig) != SIG_LEN:
        raise Fail("manifest/signature outside file bounds")
    try:
        import tomllib

        man = tomllib.loads(toml_b.decode("utf-8"))
    except Exception as e:
        raise Fail(f"manifest not TOML/UTF-8: {e}")
    try:
        key = man["update"]["key"]
        app = man["app"]
        entry, app_id = app["entry"], app["id"]
    except KeyError as e:
        raise Fail(f"manifest: missing field {e}")
    # Verify signature over manifest_toml || root_hash (raw).
    ed25519_verify(key, toml_b + root, sig)
    # stub_hash REQUIRED, verified over bytes [0:first chunk].
    try:
        want_stub = man["runtime"]["stub_hash"]
    except KeyError:
        raise Fail("manifest without [runtime].stub_hash")
    if not (isinstance(want_stub, str) and len(want_stub) == 64 and all(c in "0123456789abcdef" for c in want_stub)):
        raise Fail("malformed stub_hash")
    # Index: decompress with cap, verify root, parse.
    comp_index = data[index_off : index_off + index_len]
    if len(comp_index) != index_len:
        raise Fail("index outside file bounds")
    raw_index = zstd_decompress(comp_index, "index")
    if len(raw_index) > MAX_INDEX_RAW:
        raise Fail("decompressed index exceeds limit")
    if blake3(raw_index) != root:
        raise Fail("root hash mismatch")
    chunks, entries = parse_index(raw_index)
    # Stub position: first chunk, or index_off if no chunks.
    stub_end = min([c["off"] for c in chunks], default=index_off)
    if blake3(data[0:stub_end]).hex() != want_stub:
        raise Fail("stub does not match signed hash")
    # Every chunk: decompress and verify BEFORE use (here: verify only).
    for i, c in enumerate(chunks):
        comp = data[c["off"] : c["off"] + c["comp"]]
        if len(comp) != c["comp"]:
            raise Fail(f"chunk {i} outside file bounds")
        raw = zstd_decompress(comp, f"chunk {i}")
        if len(raw) != c["raw"] or blake3(raw) != c["hash"]:
            raise Fail(f"chunk {i} corrupt")
    if not entry or entry.startswith("/") or ".." in entry.split("/"):
        raise Fail(f"invalid app.entry: {entry!r}")
    return {"app": app_id, "entry": entry, "chunks": len(chunks), "entries": len(entries)}


def main():
    args = [a for a in sys.argv[1:] if a != "--json"]
    as_json = "--json" in sys.argv
    if not args:
        print("usage: verify.py <file.panpak> [--json]", file=sys.stderr)
        return 2
    try:
        info = check(open(args[0], "rb").read())
    except Fail as e:
        if as_json:
            print(json.dumps({"ok": False, "error": str(e)}))
        else:
            print(f"INVALID: {e}", file=sys.stderr)
        return 1
    if as_json:
        print(json.dumps({"ok": True, "info": info}))
    else:
        print(f"VALID: {info['app']} ({info['entries']} entries, {info['chunks']} chunks)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
