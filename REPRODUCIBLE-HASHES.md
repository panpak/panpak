# Reproducible build hashes (tracked reference)

Reference SHA-256 of the release binaries built with `scripts/repro-build.sh`.
The script builds twice, in two different directories, and compares the
results. The same hashes were obtained on three environments:

- the host, Zig installed in one place;
- the host again, Zig installed in a different path and the build directory
  deeper in the tree;
- a clean container (`Dockerfile.repro`).

Toolchain (everything that can change the bytes is pinned):

- Rust 1.99.1 (`rust-toolchain.toml`), target `x86_64-unknown-linux-musl`,
  `Cargo.lock` with `--locked`, `[profile.release] strip = "symbols"`.
- Zig 0.17.0 as C compiler and linker (`tools/requirements-zig.txt`, wheel
  hash checked by `pip --require-hashes`); `scripts/repro-build.sh` refuses
  any other version.
- `SOURCE_DATE_EPOCH=1700000000` and path remapping for the sources and the
  cargo registry.

Sizes: `panpak-stub` 2,835,464 bytes, `panpak` 6,373,608 bytes.

```
438f8bab19002bd39e529e198cb13f7fcddcd799125e0aabc517f4e2a200a941  panpak-stub
f50086faa3674ef215cb1da4f10f6e60ddfd2a657674e39b89b34fb71664fef8  panpak
```

## Reproduce

On the host:

```
python3 -m venv ~/.cache/panpak-zig
~/.cache/panpak-zig/bin/pip install --require-hashes -r tools/requirements-zig.txt
export PATH="$(~/.cache/panpak-zig/bin/python -c 'import os, ziglang; print(os.path.dirname(ziglang.__file__))'):$PATH"
./scripts/repro-build.sh
```

In a container:

```
docker build -f Dockerfile.repro -t panpak-repro .
docker run --rm panpak-repro
```

Both print the hashes above and `IDENTICAL`.

## Notes

- Any change to the sources can change the hashes, even a change limited to
  comments or tests: rustc derives symbol names from a hash of the crate
  sources. The C compiler is part of the toolchain as well: another Zig
  version, or another C compiler, gives different but equally valid binaries.
- The `repro` job in `.github/workflows/ci.yml` runs the double-build and
  compares the hashes with the lines above (`diff`). If sources change
  legitimately, regenerate the double-build, verify host against container
  and update THIS file in the same commit.
