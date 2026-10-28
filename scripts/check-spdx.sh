#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Check that every tracked source file starts with an SPDX license header.
# Usage: scripts/check-spdx.sh [--fix]
# Covered: *.rs (//), *.py *.sh *.yml *.toml and Dockerfile* (#).
# Not covered: Cargo.toml/Cargo.lock (license field), generated files, binary
# fixtures, fuzz corpus, historical bench JSON, documents (CC BY 4.0, see README).
set -eu
cd "$(dirname "$0")/.."
ID="SPDX-License-Identifier: MIT OR Apache-2.0"
FIX=0
[ "${1:-}" = "--fix" ] && FIX=1
bad=0
for f in $(git ls-files -- '*.rs' '*.py' '*.sh' '*.yml' '*.toml' 'Dockerfile*'); do
  case "$f" in
    fuzz/corpus/*|conformance/fixtures/*|bench/*|*/Cargo.toml|Cargo.toml|fuzz/Cargo.lock) continue ;;
  esac
  case "$f" in *.rs) c="//" ;; *) c="#" ;; esac
  # The header is the first line, or the second when a shebang comes first.
  if head -n 2 "$f" | grep -qF "$c $ID"; then continue; fi
  if [ "$FIX" = 1 ]; then
    if head -n 1 "$f" | grep -q '^#!'; then
      { head -n 1 "$f"; echo "$c $ID"; tail -n +2 "$f"; } > "$f.spdx" && cat "$f.spdx" > "$f" && rm "$f.spdx"
    else
      { echo "$c $ID"; cat "$f"; } > "$f.spdx" && cat "$f.spdx" > "$f" && rm "$f.spdx"
    fi
  else
    echo "missing SPDX header: $f"
    bad=1
  fi
done
exit $bad
