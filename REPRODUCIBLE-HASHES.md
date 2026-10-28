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

Sizes: `panpak-stub` 2,142,720 bytes, `panpak` 6,098,088 bytes.

```
5edad69304a6403de3c0984a6328cb198f92e2d6d5b9eec3b779afe112adc202  panpak-stub
327c16032f5acbdbfbecf438aa6c7c69c91e97a9a4cb6e5fd04946d374b7120f  panpak
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
