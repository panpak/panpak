# SPDX-License-Identifier: MIT OR Apache-2.0
# Sourced by scripts/zig-cc, scripts/zig-ar and scripts/repro-build.sh.
# Defines zig_run (runs the Zig found) and zig_check (fails unless it is
# exactly the version pinned in tools/requirements-zig.txt).
#
# Zig is found via $PANPAK_ZIG (path to the zig executable), else `zig` on
# PATH, else the binary inside the `ziglang` Python wheel of `python3`.
# Its caches live inside the build tree: $ZIG_GLOBAL_CACHE_DIR and
# $ZIG_LOCAL_CACHE_DIR if set, else <repo>/target/zig-cache.
PANPAK_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PANPAK_ZIG_PINNED="$(sed -n 's/^ziglang==\([0-9][0-9.]*\).*/\1/p' "$PANPAK_ROOT/tools/requirements-zig.txt")"
: "${ZIG_GLOBAL_CACHE_DIR:=$PANPAK_ROOT/target/zig-cache/global}"
: "${ZIG_LOCAL_CACHE_DIR:=$PANPAK_ROOT/target/zig-cache/local}"
export ZIG_GLOBAL_CACHE_DIR ZIG_LOCAL_CACHE_DIR

if [ -z "${PANPAK_ZIG:-}" ]; then
  if command -v zig >/dev/null 2>&1; then
    PANPAK_ZIG="$(command -v zig)"
  else
    PANPAK_ZIG="$(python3 -c 'import os, ziglang; print(os.path.join(os.path.dirname(ziglang.__file__), "zig"))' 2>/dev/null || true)"
  fi
fi
export PANPAK_ZIG

zig_run() {
  if [ ! -x "${PANPAK_ZIG:-}" ]; then
    echo "error: zig not found; install it with: pip install --require-hashes -r tools/requirements-zig.txt" >&2
    return 127
  fi
  "$PANPAK_ZIG" "$@"
}

zig_check() {
  have="$(zig_run version 2>/dev/null || true)"
  if [ "$have" != "$PANPAK_ZIG_PINNED" ]; then
    echo "error: Zig ${PANPAK_ZIG_PINNED} is required (tools/requirements-zig.txt), found '${have:-none}'." >&2
    echo "Install it with: pip install --require-hashes -r tools/requirements-zig.txt" >&2
    return 1
  fi
}
