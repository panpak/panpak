#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The patched Debian packages of `panpak build`, as the CI keeps them in its cache (docs/APP-HARNESS.md, "The cache of the patched packages").

`<cache>/panpak/build/patched/<id>/` holds `debs/*.deb`, `SHA256SUMS` (written by `panpak build` next to the files) and `.complete`. What comes back from a cache is
not trusted: `verify` checks every folder against its `SHA256SUMS` and removes the one that does not match, so that `panpak build` makes it again.

  patched_cache.py verify [--strict] FOLDER   check every <id> folder of FOLDER; a bad one is removed (with --strict: exit 1 when there is one)
  patched_cache.py root FOLDER                print the root hash: the hash of the lines "<id> <sha256 of its SHA256SUMS>"
  patched_cache.py compare A B                exit 0 when the two folders have the same root hash, 1 when not (both are printed)
"""
import hashlib
import os
import shutil
import sys
from pathlib import Path


def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def read_sums(path):
    """{file name: hash} from a SHA256SUMS file, or None when it is not well formed."""
    out = {}
    try:
        text = Path(path).read_text()
    except (OSError, UnicodeDecodeError):
        return None
    for line in text.splitlines():
        parts = line.split("  ", 1)
        if len(parts) != 2 or len(parts[0]) != 64 or any(c not in "0123456789abcdef" for c in parts[0]):
            return None
        name = parts[1]
        if not name.endswith(".deb") or "/" in name or name in out:
            return None
        out[name] = parts[0]
    return out or None


def problems(d):
    """What is wrong with one <id> folder (an empty list when it is right)."""
    d = Path(d)
    if d.is_symlink() or not d.is_dir():
        return ["not a plain folder"]
    bad = []
    if not (d / ".complete").is_file():
        bad.append(".complete is missing")
    sums_path = d / "SHA256SUMS"
    if sums_path.is_symlink() or not sums_path.is_file():
        return bad + ["SHA256SUMS is missing"]
    sums = read_sums(sums_path)
    if sums is None:
        return bad + ["SHA256SUMS is not well formed"]
    debs = d / "debs"
    if debs.is_symlink() or not debs.is_dir():
        return bad + ["debs is missing"]
    present = sorted(os.listdir(debs))
    if present != sorted(sums):
        bad.append(f"the files of debs ({', '.join(present) or 'none'}) are not the ones SHA256SUMS lists ({', '.join(sorted(sums))})")
        return bad
    for name, want in sorted(sums.items()):
        f = debs / name
        if f.is_symlink() or not f.is_file():
            bad.append(f"{name} is not a plain file")
        elif sha256_file(f) != want:
            bad.append(f"{name} does not match SHA256SUMS")
    return bad


def make_writable(path):
    for root, dirs, files in os.walk(path):
        for n in dirs + files:
            p = os.path.join(root, n)
            if not os.path.islink(p):
                os.chmod(p, 0o700 if os.path.isdir(p) else 0o600)
    os.chmod(path, 0o700)


def verify(folder, strict=False):
    """Check every folder of `folder`; remove the bad ones. Returns (how many are right, how many were bad)."""
    folder = Path(folder)
    if not folder.is_dir():
        print(f"patched packages: nothing in {folder}")
        return 0, 0
    good = bad = 0
    for child in sorted(folder.iterdir()):
        why = problems(child)
        if not why:
            good += 1
            print(f"patched packages {child.name}: verified against SHA256SUMS")
            continue
        bad += 1
        print(f"::warning::patched packages {child.name}: {'; '.join(why)}" + ("" if strict else ": removed, `panpak build` makes them again"))
        if child.is_symlink() or not child.is_dir():
            child.unlink()
        else:
            make_writable(child)
            shutil.rmtree(child)
    return good, bad


def root_hash(folder):
    folder = Path(folder)
    lines = []
    if folder.is_dir():
        for child in sorted(folder.iterdir()):
            if problems(child):
                raise SystemExit(f"{child}: {'; '.join(problems(child))}")
            lines.append(f"{child.name} {sha256_file(child / 'SHA256SUMS')}\n")
    return hashlib.sha256("".join(lines).encode()).hexdigest() if lines else None


def main(argv):
    if len(argv) >= 3 and argv[1] == "verify":
        strict = "--strict" in argv[2:]
        args = [a for a in argv[2:] if a != "--strict"]
        if len(args) != 1:
            return usage()
        _, bad = verify(args[0], strict)
        return 1 if strict and bad else 0
    if len(argv) == 3 and argv[1] == "root":
        r = root_hash(argv[2])
        print(r if r else "none")
        return 0 if r else 1
    if len(argv) == 4 and argv[1] == "compare":
        a, b = root_hash(argv[2]), root_hash(argv[3])
        print(f"root hash of {argv[2]}: {a}\nroot hash of {argv[3]}: {b}")
        return 0 if a is not None and a == b else 1
    return usage()


def usage():
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
