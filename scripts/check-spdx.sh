#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Check that every tracked source file starts with an SPDX license header.
# Usage: scripts/check-spdx.sh [--fix]
# Covered: *.rs (//), *.py *.sh *.yml *.toml and Dockerfile* (#).
# Not covered: Cargo.toml/Cargo.lock (license field), generated files, binary
# fixtures, fuzz corpus, historical bench JSON, documents (CC BY 4.0, see README).
# Exit status: 0 ok, 1 a header is missing, 2 git or file access failure.
set -eu
cd "$(dirname "$0")/.."
ID="SPDX-License-Identifier: MIT OR Apache-2.0"
FIX=0
[ "${1:-}" = "--fix" ] && FIX=1
files=$(git ls-files -- '*.rs' '*.py' '*.sh' '*.yml' '*.toml' 'Dockerfile*') || {
  echo "error: git ls-files failed" >&2
  exit 2
}
if [ -z "$files" ]; then
  echo "error: no tracked source files found" >&2
  exit 2
fi
bad=0
while IFS= read -r f; do
  case "$f" in
    fuzz/corpus/*|conformance/fixtures/*|bench/*|*/Cargo.toml|Cargo.toml|fuzz/Cargo.lock) continue ;;
  esac
  case "$f" in *.rs) c="//" ;; *) c="#" ;; esac
  # The header is the first line, or the second when a shebang comes first.
  top=$(head -n 2 -- "$f") || { echo "error: cannot read $f" >&2; exit 2; }
  case "$top" in *"$c $ID"*) continue ;; esac
  if [ "$FIX" = 1 ]; then
    first=$(head -n 1 -- "$f") || { echo "error: cannot read $f" >&2; exit 2; }
    case "$first" in '#!'*) shebang=1 ;; *) shebang=0 ;; esac
    awk -v line="$c $ID" -v shebang="$shebang" \
      'BEGIN { if (shebang == 0) print line } { print } NR == 1 && shebang == 1 { print line }' \
      "$f" > "$f.spdx" || { rm -f "$f.spdx"; echo "error: cannot fix $f" >&2; exit 2; }
    cat "$f.spdx" > "$f" || { echo "error: cannot write $f" >&2; exit 2; }
    rm -f "$f.spdx"
  else
    echo "missing SPDX header: $f"
    bad=1
  fi
done <<EOF2
$files
EOF2
exit $bad
