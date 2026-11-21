#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A controlled BitTorrent peer for the qBittorrent operation of the series: it serves one test file, always the same bytes, and nothing else.

Usage: python3 seed_peer.py [--address 192.168.122.1] [--port 6891]      serve (until Ctrl+C) and print the magnet link
       python3 seed_peer.py --print-magnet [--address A] [--port P]      only print the magnet link
       python3 seed_peer.py --self-test                                  start on 127.0.0.1 and download the file with a client of this script

The file is 8 MiB made from a fixed seed, so the info hash is the same on every run and on every machine. The magnet link names this peer (`x.pe`), so no tracker and no DHT are needed and nothing
is announced to the internet: the peer listens on the one address given (the libvirt network, 192.168.122.1) and never on every interface. It speaks the basic peer protocol (BEP 3) with the extension protocol
and the metadata exchange (BEP 9, 10), which is what a client needs to start from a magnet link. It serves pieces and nothing more: no upload to other addresses beyond those that connect to it, no state on disk.
"""
import argparse
import asyncio
import hashlib
import ipaddress
import struct
import sys

NAME = 'panpak-bench-testfile.bin'
SIZE = 8 * 1024 * 1024
PIECE = 256 * 1024
BLOCK = 16 * 1024
MAX_MESSAGE = 1 << 20


def content():
    out, n = [], 0
    while n < SIZE:
        b = hashlib.sha256(b'panpak bench test file %d' % len(out)).digest() * 1024  # 32 KiB per round
        out.append(b)
        n += len(b)
    return b''.join(out)[:SIZE]


DATA = content()


def bencode(x):
    if isinstance(x, int):
        return b'i%de' % x
    if isinstance(x, bytes):
        return b'%d:%s' % (len(x), x)
    if isinstance(x, str):
        return bencode(x.encode())
    if isinstance(x, list):
        return b'l' + b''.join(bencode(i) for i in x) + b'e'
    if isinstance(x, dict):
        return b'd' + b''.join(bencode(k) + bencode(v) for k, v in sorted((k.encode() if isinstance(k, str) else k, v) for k, v in x.items())) + b'e'
    raise TypeError(type(x))


def bdecode(b, i=0):
    """Returns (value, next index). Only what the messages of this protocol use."""
    c = b[i:i + 1]
    if c == b'i':
        j = b.index(b'e', i)
        return int(b[i + 1:j]), j + 1
    if c == b'l':
        i += 1
        out = []
        while b[i:i + 1] != b'e':
            v, i = bdecode(b, i)
            out.append(v)
        return out, i + 1
    if c == b'd':
        i += 1
        out = {}
        while b[i:i + 1] != b'e':
            k, i = bdecode(b, i)
            v, i = bdecode(b, i)
            out[k] = v
        return out, i + 1
    j = b.index(b':', i)
    n = int(b[i:j])
    return b[j + 1:j + 1 + n], j + 1 + n


PIECES = b''.join(hashlib.sha1(DATA[i:i + PIECE]).digest() for i in range(0, SIZE, PIECE))
INFO = bencode({'length': SIZE, 'name': NAME, 'piece length': PIECE, 'pieces': PIECES})
INFOHASH = hashlib.sha1(INFO).digest()


def magnet(address, port):
    return f'magnet:?xt=urn:btih:{INFOHASH.hex()}&dn={NAME}&x.pe={address}:{port}'


async def reply(w, mid, payload=b''):
    w.write(struct.pack('>IB', len(payload) + 1, mid) + payload)
    await w.drain()


async def handle(r, w):
    peer = w.get_extra_info('peername')
    try:
        hs = await asyncio.wait_for(r.readexactly(68), 15)
        if hs[:20] != b'\x13BitTorrent protocol' or hs[28:48] != INFOHASH:
            return
        w.write(b'\x13BitTorrent protocol' + b'\0\0\0\0\0\x10\0\0' + INFOHASH + b'-PB0001-' + b'seedpeer0001'[:12])
        await w.drain()
        # extension handshake (BEP 10), our id for ut_metadata is 1; then every piece is ours
        await reply(w, 20, b'\0' + bencode({'m': {'ut_metadata': 1}, 'metadata_size': len(INFO)}))
        bits = bytearray((len(PIECES) // 20 + 7) // 8)
        for i in range(len(PIECES) // 20):
            bits[i // 8] |= 0x80 >> (i % 8)
        await reply(w, 5, bytes(bits))
        their_meta = 0
        while True:
            head = await asyncio.wait_for(r.readexactly(4), 120)
            n = struct.unpack('>I', head)[0]
            if n == 0:
                continue
            if n > MAX_MESSAGE:
                return
            msg = await asyncio.wait_for(r.readexactly(n), 60)
            mid, body = msg[0], msg[1:]
            if mid == 2:  # interested
                await reply(w, 1)  # unchoke
            elif mid == 6 and len(body) == 12:  # request
                idx, begin, ln = struct.unpack('>III', body)
                off = idx * PIECE + begin
                if ln <= BLOCK and begin + ln <= PIECE and off + ln <= SIZE:
                    await reply(w, 7, struct.pack('>II', idx, begin) + DATA[off:off + ln])
            elif mid == 20 and body:
                if body[0] == 0:
                    d, _ = bdecode(body, 1)
                    their_meta = d.get(b'm', {}).get(b'ut_metadata', 0)
                elif body[0] == 1 and their_meta:
                    d, _ = bdecode(body, 1)
                    if d.get(b'msg_type') == 0:
                        p = d.get(b'piece', -1)
                        chunk = INFO[p * BLOCK:(p + 1) * BLOCK]
                        if p >= 0 and chunk:
                            await reply(w, 20, bytes([their_meta]) + bencode({'msg_type': 1, 'piece': p, 'total_size': len(INFO)}) + chunk)
                        else:
                            await reply(w, 20, bytes([their_meta]) + bencode({'msg_type': 2, 'piece': p}))
    except (asyncio.IncompleteReadError, asyncio.TimeoutError, ConnectionError, ValueError, KeyError, struct.error):
        pass
    finally:
        w.close()
        print(f'closed {peer}', flush=True)


async def serve(address, port):
    server = await asyncio.start_server(handle, address, port)
    print(f'peer on {address}:{port}, info hash {INFOHASH.hex()}', flush=True)
    async with server:
        await server.serve_forever()


async def leech(address, port):
    """The client of the self-test: metadata by BEP 9, then every piece, each checked against the hash."""
    r, w = await asyncio.open_connection(address, port)
    w.write(b'\x13BitTorrent protocol' + b'\0\0\0\0\0\x10\0\0' + INFOHASH + b'-XX0001-123456789012')
    await w.drain()
    hs = await r.readexactly(68)
    assert hs[28:48] == INFOHASH
    await reply(w, 20, b'\0' + bencode({'m': {'ut_metadata': 3}}))

    async def msg():
        while True:
            n = struct.unpack('>I', await r.readexactly(4))[0]
            if n:
                m = await r.readexactly(n)
                return m[0], m[1:]
    meta, size = b'', None
    while size is None:
        mid, body = await msg()
        if mid == 20 and body[0] == 0:
            size = bdecode(body, 1)[0][b'metadata_size']
    for p in range((size + BLOCK - 1) // BLOCK):
        await reply(w, 20, bytes([1]) + bencode({'msg_type': 0, 'piece': p}))
        while True:
            mid, body = await msg()
            if mid == 20 and body[0] == 3:
                d, end = bdecode(body, 1)
                assert d[b'msg_type'] == 1
                meta += body[end:]
                break
    assert hashlib.sha1(meta).digest() == INFOHASH, 'the metadata does not match the info hash'
    info, _ = bdecode(meta)
    assert info[b'length'] == SIZE
    await reply(w, 2)
    while (await msg())[0] != 1:
        pass
    got = bytearray()
    for idx in range(SIZE // PIECE):
        for begin in range(0, PIECE, BLOCK):
            await reply(w, 6, struct.pack('>III', idx, begin, BLOCK))
            while True:
                mid, body = await msg()
                if mid == 7:
                    got += body[8:]
                    break
    w.close()
    assert bytes(got) == DATA, 'the downloaded bytes differ'
    return len(got)


def check_address(a):
    if ipaddress.ip_address(a).is_unspecified:
        sys.exit(f'refused: {a} would listen on every interface; give the address of the libvirt network (192.168.122.1)')


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--address', default='192.168.122.1')
    ap.add_argument('--port', type=int, default=6891)
    ap.add_argument('--print-magnet', action='store_true')
    ap.add_argument('--self-test', action='store_true')
    a = ap.parse_args()
    check_address(a.address)
    if a.print_magnet:
        print(magnet(a.address, a.port))
        return

    if a.self_test:
        async def run():
            server = await asyncio.start_server(handle, '127.0.0.1', 0)
            port = server.sockets[0].getsockname()[1]
            async with server:
                n = await asyncio.wait_for(leech('127.0.0.1', port), 60)
            print(f'seed_peer self-test PASS: metadata and {n} bytes, every piece checked')
        asyncio.run(run())
        return
    print(magnet(a.address, a.port), flush=True)
    try:
        asyncio.run(serve(a.address, a.port))
    except KeyboardInterrupt:
        pass


if __name__ == '__main__':
    main()
