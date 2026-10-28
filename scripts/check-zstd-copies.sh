#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Fails if panpak-core or panpak-stub has any zstd other than 0.14.x in its
# dependency tree (the zstd 0.13 copy that backhand pulls into the panpak CLI is
# tolerated, see deny.toml). The stub must have exactly one copy of zstd.
set -euo pipefail
cd "$(dirname "$0")/.."

status=0
for pkg in ${@:-panpak-core panpak-stub}; do
  # Normal and build dependencies only; one "name vX.Y.Z" per line.
  copies=$(cargo tree --locked -p "$pkg" -e normal,build --prefix none \
    | grep -E '^zstd v' | sed 's/ (\*)$//' | sort -u)
  if [ -z "$copies" ]; then
    echo "FAIL: $pkg: no zstd found in its dependency tree (script out of date?)"
    status=1
    continue
  fi
  if [ "$(printf '%s\n' "$copies" | wc -l)" -ne 1 ] \
     || ! printf '%s\n' "$copies" | grep -qE '^zstd v0\.14\.[0-9]+$'; then
    echo "FAIL: $pkg must have exactly one zstd, 0.14.x; found:"
    printf '  %s\n' "$copies"
    status=1
  else
    echo "ok: $pkg -> $copies"
  fi
done
exit $status
