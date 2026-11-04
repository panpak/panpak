#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Generate the fuzz seed corpus for fuzz_elf, fuzz_squash, fuzz_dbus and fuzz_dbus_names.

Every seed is written by this script from scratch (hand-built ELF headers) or
built from small text files created here (squashfs, via mksquashfs). No
third-party binary is copied into the repository.

Usage: python3 fuzz/gen-seeds.py          (run from anywhere)
The squashfs seeds need mksquashfs (squashfs-tools); without it they are
skipped. Output is deterministic for a given mksquashfs version.
"""
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

CORPUS = Path(__file__).resolve().parent / "corpus"
BASE = 0x400000
INTERP = b"/lib64/ld-linux-x86-64.so.2\0"
# exit(0) on x86-64: xor edi,edi; mov eax,60; syscall
CODE = bytes.fromhex("31ff" "b83c000000" "0f05")

PT_LOAD, PT_DYNAMIC, PT_INTERP = 1, 2, 3


def ehdr64(phnum, phoff=64, entry=BASE + 0x1000, etype=2, cls=2, data=1):
    ident = b"\x7fELF" + bytes([cls, data, 1, 0]) + b"\0" * 8
    return ident + struct.pack("<HHIQQQIHHHHHH", etype, 62, 1, entry, phoff, 0,
                               0, 64, 56, phnum, 64, 0, 0)


def phdr64(typ, off, vaddr, size, flags=5, align=0x1000):
    return struct.pack("<IIQQQQQQ", typ, flags, off, vaddr, vaddr, size, size, align)


def pad(b, n):
    return b + b"\0" * (n - len(b))


def build(kind):
    """Return a small ELF64 image; kind selects the program headers."""
    if kind == "static":
        n = 1
    elif kind == "interp":
        n = 2
    else:
        n = 3
    hdr_end = 64 + 56 * n
    # Layout: headers, interp, dynamic, dynstr, code (page 0x1000).
    interp_off = hdr_end
    dyn_off = interp_off + len(INTERP)
    dyn_off = (dyn_off + 7) & ~7
    strtab = b"\0libc.so.6\0libm.so.6\0"
    dyn = b"".join(struct.pack("<qQ", t, v) for t, v in (
        (1, 1), (1, 11),                    # DT_NEEDED libc.so.6, libm.so.6
        (5, BASE + dyn_off + 5 * 16),       # DT_STRTAB (right after the table)
        (10, len(strtab)), (0, 0)))         # DT_STRSZ, DT_NULL
    str_off = dyn_off + len(dyn)
    code_off = 0x1000
    total = code_off + len(CODE)
    phs = []
    if kind in ("interp", "dynamic"):
        phs.append(phdr64(PT_INTERP, interp_off, BASE + interp_off, len(INTERP), 4, 1))
    phs.append(phdr64(PT_LOAD, 0, BASE, total, 5))
    if kind == "dynamic":
        phs.append(phdr64(PT_DYNAMIC, dyn_off, BASE + dyn_off, len(dyn), 6, 8))
    img = ehdr64(len(phs)) + b"".join(phs)
    img = pad(img, interp_off) + INTERP
    img = pad(img, dyn_off) + dyn + strtab
    img = pad(img, code_off) + CODE
    return img


def elf_seeds():
    out = {}
    out["min"] = (ehdr64(1, entry=BASE + 0x78) +
                  phdr64(PT_LOAD, 0, BASE, 0x78 + len(CODE)) + CODE)
    out["static"] = build("static")
    out["interp"] = build("interp")
    dyn = build("dynamic")
    out["dynamic"] = dyn
    out["truncated-dynamic"] = dyn[:0x98 + len(INTERP) + 24]
    out["truncated-phdrs"] = dyn[:64 + 56 + 10]
    out["truncated-ehdr"] = dyn[:40]
    out["header-only"] = ehdr64(0)

    bad = bytearray(dyn)
    bad[0x38:0x3A] = struct.pack("<H", 0xFFFF)            # e_phnum too large
    out["bad-phnum"] = bytes(bad)
    bad = bytearray(dyn)
    bad[0x20:0x28] = struct.pack("<Q", 0xFFFFFFFFFFFFFF00)  # e_phoff overflow
    out["bad-phoff"] = bytes(bad)
    bad = bytearray(dyn)
    bad[4] = 3                                             # unknown class
    out["bad-class"] = bytes(bad)
    bad = bytearray(dyn)
    bad[5] = 2                                             # big endian
    out["bad-endian"] = bytes(bad)
    bad = bytearray(dyn)
    # DT_STRTAB pointing outside every PT_LOAD
    dyn_off = ((64 + 56 * 3 + len(INTERP)) + 7) & ~7
    bad[dyn_off + 2 * 16 + 8:dyn_off + 2 * 16 + 16] = struct.pack("<Q", 0x7fff0000)
    out["bad-dynstr"] = bytes(bad)
    bad = bytearray(dyn)
    bad[dyn_off + 3 * 16 + 8:dyn_off + 3 * 16 + 16] = struct.pack("<Q", 1 << 40)
    out["huge-strsz"] = bytes(bad)
    # DT_STRSZ just past the end of the file, and a table size above the old fixed limit of 1 MiB.
    for name, size in (("strsz-past-file", len(dyn) + 1), ("strsz-over-1mib", (1 << 20) + 1), ("strsz-zero", 0)):
        bad = bytearray(dyn)
        bad[dyn_off + 3 * 16 + 8:dyn_off + 3 * 16 + 16] = struct.pack("<Q", size)
        out[name] = bytes(bad)
    # No DT_NULL terminator: all entries are DT_NEEDED, the table runs off the file.
    bad = bytearray(dyn)
    for i in range(4, 5):
        bad[dyn_off + i * 16:dyn_off + i * 16 + 8] = struct.pack("<q", 1)
    out["dynamic-unterminated"] = bytes(bad)

    # 32-bit little-endian: header plus PT_LOAD only, then a dynamic variant cut short.
    e32 = (b"\x7fELF" + bytes([1, 1, 1, 0]) + b"\0" * 8 +
           struct.pack("<HHIIIIIHHHHHH", 2, 3, 1, 0x8049000, 52, 0, 0, 52, 32, 2, 40, 0, 0))
    ph = struct.pack("<IIIIIIII", PT_LOAD, 0, 0x8048000, 0x8048000, 0x200, 0x200, 5, 0x1000)
    ph2 = struct.pack("<IIIIIIII", PT_DYNAMIC, 0x100, 0x8048100, 0x8048100, 0x20, 0x20, 6, 4)
    out["elf32"] = pad(e32 + ph + ph2, 0x100) + struct.pack("<ii", 0, 0)
    out["elf32-truncated"] = (e32 + ph + ph2)[:70]
    out["magic-only"] = b"\x7fELF"
    out["not-elf"] = b"#!/bin/sh\nexit 0\n"
    return out


def squash_seeds():
    mk = shutil.which("mksquashfs")
    if not mk:
        print("mksquashfs not found: squashfs seeds skipped", file=sys.stderr)
        return {}
    out = {}
    with tempfile.TemporaryDirectory(prefix="panpak-seeds-") as tmp:
        tree = Path(tmp) / "tree"
        (tree / "dir" / "sub").mkdir(parents=True)
        (tree / "hello.txt").write_text("hello\n")
        (tree / "dir" / "notes.txt").write_text("line\n" * 400)
        (tree / "dir" / "sub" / "empty.txt").write_text("")
        (tree / "link").symlink_to("hello.txt")
        variants = {
            "gzip": ["-comp", "gzip"],
            "zstd": ["-comp", "zstd"],
            "plain": ["-noI", "-noD", "-noF", "-noX"],
        }
        for name, opts in variants.items():
            dst = Path(tmp) / (name + ".sqfs")
            subprocess.run([mk, str(tree), str(dst), "-quiet", "-noappend", "-no-xattrs",
                            "-all-root", "-mkfs-time", "0", "-all-time", "0",
                            "-processors", "1", *opts],
                           check=True, stdout=subprocess.DEVNULL)
            out[name] = dst.read_bytes()
    gz = out["gzip"]
    out["gzip-truncated"] = gz[: len(gz) // 2]
    out["gzip-header-only"] = gz[:96]
    out["gzip-at-offset"] = b"\x00" * 4096 + gz
    bad = bytearray(gz)
    bad[0x0C:0x10] = struct.pack("<I", 0x7FFFFFFF)         # block_size field
    out["gzip-bad-block-size"] = bytes(bad)
    out["magic-only"] = b"hsqs"
    return out


def _al(b, n):
    return b + b"\0" * (-len(b) % n)


def _str(s):
    d = s.encode()
    return struct.pack("<I", len(d)) + d + b"\0"


def dbus_message(kind, serial, fields, bodysig="", body=b"", flags=0):
    """A little-endian message. `fields` is a list of (code, signature, payload-after-alignment)."""
    arr = b""
    for code, sig, payload in fields:
        arr = _al(arr, 8)
        item = bytes([code, len(sig)]) + sig.encode() + b"\0"
        # alignment is relative to the start of the message: the array starts at offset 16
        pos = 16 + len(arr) + len(item)
        item += b"\0" * (-pos % (1 if sig == "g" else 4)) + payload
        arr += item
    if bodysig:
        arr = _al(arr, 8)
        sg = bytes([8, 1]) + b"g\0" + bytes([len(bodysig)]) + bodysig.encode() + b"\0"
        arr += sg
    head = b"l" + bytes([kind, flags, 1]) + struct.pack("<II", len(body), serial) + struct.pack("<I", len(arr))
    return _al(head + arr, 8) + body


def dbus_seeds():
    out = {}
    path = lambda p: (1, "o", _str(p))
    iface = lambda i: (2, "s", _str(i))
    member = lambda m: (3, "s", _str(m))
    dest = lambda d: (6, "s", _str(d))
    out["hello"] = bytes([0]) + dbus_message(1, 1, [path("/org/freedesktop/DBus"), iface("org.freedesktop.DBus"), member("Hello"), dest("org.freedesktop.DBus")])
    out["return-string"] = bytes([0]) + dbus_message(2, 2, [(5, "u", struct.pack("<I", 1))], "s", _str(":1.42"))
    out["error"] = bytes([0]) + dbus_message(3, 3, [(4, "s", _str("org.freedesktop.DBus.Error.UnknownMethod")), (5, "u", struct.pack("<I", 1))], "s", _str("no such method"))
    out["signal"] = bytes([0]) + dbus_message(4, 4, [path("/org/freedesktop/DBus"), iface("org.freedesktop.DBus"), member("NameAcquired")], "s", _str(":1.42"))
    out["call-with-fd"] = bytes([4]) + dbus_message(1, 5, [path("/org/x"), iface("org.x.I"), member("Do"), (9, "u", struct.pack("<I", 1))], "hs", struct.pack("<I", 0) + _str("hi"))
    ent = _str("PIDs") + bytes([2]) + b"au\0"
    ent = _al(ent, 4) + struct.pack("<I", 4) + struct.pack("<I", 1234)
    arr = struct.pack("<I", len(ent)) + b"\0" * 4 + ent
    out["a-sv-body"] = bytes([0]) + dbus_message(1, 6, [path("/org/x"), member("Props")], "a{sv}", arr)
    return out


def dbus_names_seeds():
    names = ["org.example.app.Second", "org.freedesktop.Notifications", "org.kde.kdenlive", "org.kde.StatusNotifierWatcher",
             "org.example.*", "a.b", "ca.desrt.dconf", "org.example.app\norg.example.app\nApp", "org.gtk.vfs.Daemon", "x.y.z" + "a" * 260]
    return {f"{i}": n.encode() for i, n in enumerate(names)}


def write(target, seeds, prefix):
    d = CORPUS / target
    d.mkdir(parents=True, exist_ok=True)
    for name, data in sorted(seeds.items()):
        (d / f"{prefix}-{name}").write_bytes(data)
    print(f"{target}: {len(seeds)} seeds")


def main():
    write("fuzz_elf", elf_seeds(), "seed")
    write("fuzz_squash", squash_seeds(), "seed")
    write("fuzz_dbus", dbus_seeds(), "seed")
    write("fuzz_dbus_names", dbus_names_seeds(), "seed")


if __name__ == "__main__":
    main()
