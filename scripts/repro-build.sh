#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Double-build Panpak in two different directories and compare the hashes.
# Usage: scripts/repro-build.sh [--no-remap] [--update]
# With --update, when the two builds are identical, the generated part of REPRODUCIBLE-HASHES.md (the build date, the sizes and the hashes) is
# rewritten from them; nobody edits it by hand. The CI runs this and fails if the file changes: the reference is what the script makes.
# With --remap (default) absolute paths are rewritten to fixed prefixes;
# without it, this project binaries stay identical anyway (no path
# of the sources ends up in the release binary), but remap stays recommended.
#
# Pinned toolchain: rust-toolchain.toml (channel 1.99.1) + Cargo.lock
# (build --locked). C compiler and linker: Zig, exactly the version pinned in
# tools/requirements-zig.txt (wheel from PyPI, installed with
# `pip install --require-hashes -r tools/requirements-zig.txt`); see
# scripts/zig-env.sh for how Zig is found. The script refuses to run with any
# other Zig version. Exit status: 0 identical, 1 different or failed.
set -eu
. "$(dirname "$0")/zig-env.sh"
zig_check || exit 1
REMAP=1
UPDATE=0
for a in "$@"; do
  case "$a" in
    --no-remap) REMAP=0 ;;
    --update) UPDATE=1 ;;
    *) echo "usage: scripts/repro-build.sh [--no-remap] [--update]" >&2; exit 2 ;;
  esac
done

SRC="$(cd "$(dirname "$0")/.." && pwd)"
# Where cargo and rustup live is not part of the result: both homes are remapped below wherever they are (CARGO_HOME and RUSTUP_HOME, else the
# defaults under $HOME).
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export CARGO_HOME RUSTUP_HOME
export PATH="$CARGO_HOME/bin:$HOME/.local/bin:$PATH"
# The date of the build (`panpak --build-epoch`) is SOURCE_DATE_EPOCH, from the first of these that gives one:
#  1. the environment;
#  2. git: the date of the last commit that touched the code, by the one rule of scripts/code-epoch.sh that panpak/build.rs uses too (needs the whole
#     history: the CI checks out with fetch-depth 0);
#  3. the date written in REPRODUCIBLE-HASHES.md, for a build without git (a container, a tarball, a shallow checkout).
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
  SOURCE_DATE_EPOCH="$(sh "$SRC/scripts/code-epoch.sh")"
fi
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
  SOURCE_DATE_EPOCH="$(sed -n 's/^Build date: `SOURCE_DATE_EPOCH=\([0-9][0-9]*\)`.*/\1/p' "$SRC/REPRODUCIBLE-HASHES.md" | head -n 1)"
fi
case "$SOURCE_DATE_EPOCH" in ''|*[!0-9]*) echo "error: no build date: no SOURCE_DATE_EPOCH, no git history and none in REPRODUCIBLE-HASHES.md" >&2; exit 1 ;; esac
export SOURCE_DATE_EPOCH
echo "build date (SOURCE_DATE_EPOCH): $SOURCE_DATE_EPOCH"
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
# RUSTFLAGS replaces the `rustflags` of .cargo/config.toml: repeat its flag.
LINKFLAGS="-C link-self-contained=no"
mkdir -p "$work/a" "$work/b"
# Identical sources, different paths (no target/, no .git).
# Through a tarball, not a pipe: a failing first tar must stop the script.
tar --exclude=target --exclude=.git -cf "$work/src.tar" -C "$SRC" .
tar -xf "$work/src.tar" -C "$work/a"
tar -xf "$work/src.tar" -C "$work/b"
rm -f "$work/src.tar"

for d in a b; do
  # Zig caches inside each build directory (never shared between a and b).
  export ZIG_GLOBAL_CACHE_DIR="$work/$d/target/zig-cache/global" ZIG_LOCAL_CACHE_DIR="$work/$d/target/zig-cache/local"
  # Source AND registry remap: absolute paths end up in the strings
  # of dependency panics; without it, builds with the homes of cargo and rustup in different places diverge.
  if [ "$REMAP" = 1 ]; then
    export RUSTFLAGS="$LINKFLAGS --remap-path-prefix=$work/$d=/panpak-src --remap-path-prefix=$CARGO_HOME=/cargo-home --remap-path-prefix=$RUSTUP_HOME=/rustup-home"
  else
    export RUSTFLAGS="$LINKFLAGS"
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
result=0
if cmp -s "$work/a/target/x86_64-unknown-linux-musl/release/panpak-stub" \
          "$work/b/target/x86_64-unknown-linux-musl/release/panpak-stub" \
 && cmp -s "$work/a/target/x86_64-unknown-linux-musl/release/panpak" \
          "$work/b/target/x86_64-unknown-linux-musl/release/panpak"; then
  echo "IDENTICAL"
else
  echo "DIFFERENT (see the OB6 summary for the cause)"
  result=1
fi
if [ "$UPDATE" = 1 ] && [ "$result" = 0 ]; then
  rel="$work/a/target/x86_64-unknown-linux-musl/release"
  commas() { echo "$1" | sed ':a;s/\B[0-9]\{3\}\>/,&/;ta'; }
  block="$work/block.txt"
  {
    echo '<!-- BEGIN GENERATED by scripts/repro-build.sh --update: do not edit by hand -->'
    echo "Build date: \`SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH\` (the date of the last commit that touched the code, by scripts/code-epoch.sh; \`panpak --build-epoch\` reports it)."
    echo
    echo "Sizes: \`panpak-stub\` $(commas "$(wc -c < "$rel/panpak-stub")") bytes, \`panpak\` $(commas "$(wc -c < "$rel/panpak")") bytes."
    echo
    echo '```'
    echo "$(sha256sum < "$rel/panpak-stub" | cut -d' ' -f1)  panpak-stub"
    echo "$(sha256sum < "$rel/panpak" | cut -d' ' -f1)  panpak"
    echo '```'
    echo '<!-- END GENERATED -->'
  } > "$block"
  ref="$SRC/REPRODUCIBLE-HASHES.md"
  awk -v blockfile="$block" '
    /^<!-- BEGIN GENERATED/ { while ((getline l < blockfile) > 0) print l; skip = 1; next }
    /^<!-- END GENERATED -->/ { skip = 0; next }
    !skip { print }
  ' "$ref" > "$work/ref.new"
  cat "$work/ref.new" > "$ref"
  echo "REPRODUCIBLE-HASHES.md written"
fi
echo "workdir retained at: $work"
echo "Previous work dirs are never deleted; each uses ~1 GB. Clean up: rm -rf ~/.cache/panpak-repro*"
exit "$result"
