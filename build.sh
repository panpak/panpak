#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Build Panpak: the programs `panpak` and `panpak-stub` in target/x86_64-unknown-linux-musl/release/.
#
#   ./build.sh [extra options for cargo build]
#
# What it does, in order: checks that the tools it needs are there (and says which are missing); prepares Zig, the C compiler and linker of the
# build, from the wheel pinned in tools/requirements-zig.txt (a Python environment in target/zig-venv, installed with the hashes checked); adds the Rust
# target x86_64-unknown-linux-musl; builds with `cargo build --locked --release`. Nothing is installed outside this folder, except what rustup keeps (the
# Rust toolchain of rust-toolchain.toml, and the musl target) and the cargo registry. Zig can also be given: PANPAK_ZIG=/path/to/zig (the version in
# tools/requirements-zig.txt is required).
set -euo pipefail
cd "$(dirname "$0")"
TARGET=x86_64-unknown-linux-musl
VENV=target/zig-venv

fail() {
  echo "build.sh: $*" >&2
  exit 1
}

# 1. The system and the tools.
[ "$(uname -s)" = Linux ] || fail "Panpak is built for Linux only (this is $(uname -s))."
[ "$(uname -m)" = x86_64 ] || fail "only x86-64 is supported for now (this is $(uname -m))."
missing=()
command -v python3 >/dev/null 2>&1 || missing+=("python3 (to install Zig)")
command -v rustup >/dev/null 2>&1 || missing+=("rustup (https://rustup.rs: it installs the Rust toolchain that rust-toolchain.toml names; a Rust from the distribution cannot add the musl target)")
command -v cargo >/dev/null 2>&1 || missing+=("cargo (comes with rustup)")
if [ -z "${PANPAK_ZIG:-}" ] && command -v python3 >/dev/null 2>&1; then
  python3 -c 'import venv, ensurepip' >/dev/null 2>&1 || missing+=("the Python modules venv and ensurepip (Debian and Ubuntu: apt install python3-venv)")
fi
if [ "${#missing[@]}" -gt 0 ]; then
  echo "build.sh: these are missing:" >&2
  printf '  - %s\n' "${missing[@]}" >&2
  exit 1
fi

# 2. Zig.
if [ -z "${PANPAK_ZIG:-}" ]; then
  if [ ! -x "$VENV/bin/python" ] || ! "$VENV/bin/python" -c 'import ziglang' >/dev/null 2>&1; then
    echo "build.sh: preparing Zig in $VENV (the wheel of tools/requirements-zig.txt, hashes checked; needs network access to PyPI)"
    rm -rf "$VENV"
    python3 -m venv "$VENV" || fail "cannot make a Python environment in $VENV."
    "$VENV/bin/pip" install --quiet --disable-pip-version-check --require-hashes -r tools/requirements-zig.txt ||
      fail "cannot install Zig from PyPI (no network access, or the hashes do not match): see the message above."
  fi
  PANPAK_ZIG="$("$VENV/bin/python" -c 'import os, ziglang; print(os.path.join(os.path.dirname(ziglang.__file__), "zig"))')"
fi
export PANPAK_ZIG
[ -x "$PANPAK_ZIG" ] || fail "Zig not found at $PANPAK_ZIG."

# 3. The Rust target (the toolchain itself is installed by rustup on first use, from rust-toolchain.toml).
echo "build.sh: adding the Rust target $TARGET (the first time it also installs the toolchain of rust-toolchain.toml)"
rustup target add "$TARGET" >/dev/null 2>&1 || rustup target add "$TARGET" || fail "cannot add the Rust target $TARGET (no network access?)."

# 4. The build.
echo "build.sh: building (the first time takes a few minutes)"
cargo build --locked --release --target "$TARGET" -p panpak -p panpak-stub "$@"
bin="$PWD/target/$TARGET/release"
echo
echo "built: $bin/panpak and $bin/panpak-stub"
echo "put them on the PATH to use them:  export PATH=\"$bin:\$PATH\""
echo "(the runtime that comes with panpak is installed by its first 'build' or 'install')"
