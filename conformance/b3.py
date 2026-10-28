# SPDX-License-Identifier: MIT OR Apache-2.0
"""Pure-Python BLAKE3 (only for the conformance verifier).
Implemented from the algorithm description (not from Panpak's Rust code).
Validate with _selftest() against the Rust implementation before use.
"""

IV = [
    0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A,
    0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19,
]

MSG_PERMUTATION = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8]

CHUNK_START = 1 << 0
CHUNK_END = 1 << 1
PARENT = 1 << 2
ROOT = 1 << 3


def _rotr(x, n):
    return ((x >> n) | (x << (32 - n))) & 0xFFFFFFFF


def _g(s, a, b, c, d, mx, my):
    s[a] = (s[a] + s[b] + mx) & 0xFFFFFFFF
    s[d] = _rotr(s[d] ^ s[a], 16)
    s[c] = (s[c] + s[d]) & 0xFFFFFFFF
    s[b] = _rotr(s[b] ^ s[c], 12)
    s[a] = (s[a] + s[b] + my) & 0xFFFFFFFF
    s[d] = _rotr(s[d] ^ s[a], 8)
    s[c] = (s[c] + s[d]) & 0xFFFFFFFF
    s[b] = _rotr(s[b] ^ s[c], 7)


def _compress(chaining_value, block_words, counter, block_len, flags):
    assert len(chaining_value) == 8
    assert len(block_words) == 16
    s = list(chaining_value[:8]) + list(IV[:4]) + [
        counter & 0xFFFFFFFF,
        (counter >> 32) & 0xFFFFFFFF,
        block_len,
        flags,
    ]
    m = list(block_words)
    for _ in range(7):
        _g(s, 0, 4, 8, 12, m[0], m[1])
        _g(s, 1, 5, 9, 13, m[2], m[3])
        _g(s, 2, 6, 10, 14, m[4], m[5])
        _g(s, 3, 7, 11, 15, m[6], m[7])
        _g(s, 0, 5, 10, 15, m[8], m[9])
        _g(s, 1, 6, 11, 12, m[10], m[11])
        _g(s, 2, 7, 8, 13, m[12], m[13])
        _g(s, 3, 4, 9, 14, m[14], m[15])
        m = [m[i] for i in MSG_PERMUTATION]
    return [(s[i] ^ s[i + 8]) & 0xFFFFFFFF for i in range(8)] + [
        (s[i + 8] ^ chaining_value[i]) & 0xFFFFFFFF for i in range(8)
    ]


def _words(block: bytes):
    import struct
    assert len(block) == 64
    return list(struct.unpack("<16I", block))


def _chunk_cv(data: bytes, chunk_index: int):
    """Chaining value of a chunk (<=1024 bytes), no ROOT flag."""
    import struct
    assert 0 < len(data) <= 1024
    cv = IV
    nblocks = (len(data) + 63) // 64
    for n in range(nblocks):
        blk = data[n * 64 : (n + 1) * 64]
        words = _words(blk + b"\x00" * (64 - len(blk)))
        flags = 0
        if n == 0:
            flags |= CHUNK_START
        if n == nblocks - 1:
            flags |= CHUNK_END
        out = _compress(cv, words, chunk_index, len(blk), flags)
        cv = out[:8]
    return cv


def _parent(left_cv, right_cv, is_root: bool):
    # A root that is also an internal node has PARENT|ROOT flags.
    flags = PARENT | (ROOT if is_root else 0)
    out = _compress(IV, left_cv + right_cv, 0, 64, flags)
    return out[:8]


def _split(n: int) -> int:
    p = 1
    while p * 2 < n:
        p *= 2
    return p


def _subtree(cvs, is_root: bool):
    n = len(cvs)
    assert n >= 1
    if n == 1:
        return cvs[0]
    s = _split(n)
    left = _subtree(cvs[:s], False)
    right = _subtree(cvs[s:], False)
    return _parent(left, right, is_root)


def blake3(data: bytes) -> bytes:
    """32-byte BLAKE3 hash."""
    import struct
    if len(data) == 0:
        out = _compress(IV, [0] * 16, 0, 0, CHUNK_START | CHUNK_END | ROOT)
        return struct.pack("<8I", *out[:8])
    chunks = [data[i : i + 1024] for i in range(0, len(data), 1024)]
    if len(chunks) == 1 and len(chunks[0]) <= 64:
        # Shortcut for single-block chunks: ROOT straight on the only block.
        words = _words(chunks[0] + b"\x00" * (64 - len(chunks[0])))
        out = _compress(IV, words, 0, len(chunks[0]), CHUNK_START | CHUNK_END | ROOT)
        return struct.pack("<8I", *out[:8])
    cvs = [_chunk_cv(c, i) for i, c in enumerate(chunks)]
    if len(cvs) == 1:
        # Single multi-block chunk: ROOT on the last block.
        c = chunks[0]
        nblocks = (len(c) + 63) // 64
        cv = IV
        for n in range(nblocks):
            blk = c[n * 64 : (n + 1) * 64]
            words = _words(blk + b"\x00" * (64 - len(blk)))
            flags = (CHUNK_START if n == 0 else 0) | (CHUNK_END | ROOT if n == nblocks - 1 else 0)
            out = _compress(cv, words, 0, len(blk), flags)
            cv = out[:8]
        return struct.pack("<8I", *cv)
    root = _subtree(cvs, True)
    return struct.pack("<8I", *root)


def _selftest():
    # Known BLAKE3("") vector: af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262
    assert blake3(b"").hex() == "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262", blake3(b"").hex()
    print("selftest base OK")


if __name__ == "__main__":
    _selftest()
