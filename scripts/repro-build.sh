#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Double-build Panpak in two different directories and compare the hashes.
# Usage: scripts/repro-build.sh [--no-remap]
# With --remap (default) absolute paths are rewritten to fixed prefixes;
# without it, this project binaries stay identical anyway (no path
# of the sources ends up in the release binary), but remap stays recommended.
#
# Pinned toolchain: rust-toolchain.toml (channel 1.99.1) + Cargo.lock
# (build --locked). C cross-compiler: musl.cc x86_64-linux-musl-cross
# (build 2021-11-23), registered SHA-256:
#   c5d410d9f82a4f24c549fe5d24f988f85b2679b452413a9f7e5f7b956f2fe7ea
# musl.cc publishes no checksums: the value above is self-detected to
# catch silent changes in future downloads. Alternative with
# verified signatures: musl-tools from the Ubuntu archive (apt, Release.gpg).
set -eu
REMAP=1
if [ "${1:-}" = "--no-remap" ]; then REMAP=0; fi

SRC="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1700000000}"
export CARGO_INCREMENTAL=0
# No --offline: --locked pins versions (Cargo.lock), the network is only needed
# at the first fetch and does not change the result.

# Working directory: PANPAK_REPRO_DIR if set, else a per-user cache dir
# (not /tmp, which is world-writable). An existing directory is never
# deleted: an explicit PANPAK_REPRO_DIR is refused, the default gets a
# timestamped name instead.
if [ -n "${PANPAK_REPRO_DIR:-}" ]; then
  work="$PANPAK_REPRO_DIR"
  if [ -e "$work" ]; then
    echo "error: $work already exists; refusing to delete it." >&2
    echo "Remove it yourself or choose another PANPAK_REPRO_DIR." >&2
    exit 1
  fi
else
  work="${XDG_CACHE_HOME:-$HOME/.cache}/panpak-repro"
  if [ -e "$work" ]; then
    work="$work-$(date +%Y%m%d-%H%M%S)-$$"
  fi
fi
mkdir -p "$work/a" "$work/b"
# Identical sources, different paths (no target/, no .git).
tar --exclude=target --exclude=.git -cf - -C "$SRC" . | tar -xf - -C "$work/a"
tar --exclude=target --exclude=.git -cf - -C "$work/a" . | tar -xf - -C "$work/b"

for d in a b; do
  # Source AND registry remap: absolute paths end up in the strings
  # of dependency panics; without it, builds in different $HOMEs diverge.
  if [ "$REMAP" = 1 ]; then
    export RUSTFLAGS="--remap-path-prefix=$work/$d=/panpak-src --remap-path-prefix=$HOME/.cargo=/cargo-home"
  else
    unset RUSTFLAGS
  fi
  (cd "$work/$d" && cargo build --locked --release \
    --target x86_64-unknown-linux-musl) > "$work/$d.log" 2>&1 \
    || { echo "BUILD FAILED in $d (see $work/$d.log)"; exit 1; }
done

echo "== sha256 =="
sha256sum "$work/a/target/x86_64-unknown-linux-musl/release/panpak-stub" \
           "$work/b/target/x86_64-unknown-linux-musl/release/panpak-stub"
sha256sum "$work/a/target/x86_64-unknown-linux-musl/release/panpak" \
           "$work/b/target/x86_64-unknown-linux-musl/release/panpak"
if cmp -s "$work/a/target/x86_64-unknown-linux-musl/release/panpak-stub" \
          "$work/b/target/x86_64-unknown-linux-musl/release/panpak-stub" \
 && cmp -s "$work/a/target/x86_64-unknown-linux-musl/release/panpak" \
          "$work/b/target/x86_64-unknown-linux-musl/release/panpak"; then
  echo "IDENTICAL"
else
  echo "DIFFERENT (see the OB6 summary for the cause)"
fi
echo "workdir retained at: $work"
echo "Previous work dirs are never deleted; each uses ~1 GB. Clean up: rm -rf ~/.cache/panpak-repro*"
