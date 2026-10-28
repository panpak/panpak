#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Fail if a tracked file is larger than 1 MiB and not listed in
# scripts/large-files-allowed.txt (one path per line, '#' starts a comment).
# Keeps third-party binaries and other bulky blobs out of the repository.
# Usage: scripts/check-large-files.sh
set -eu
cd "$(dirname "$0")/.."
LIMIT=1048576
ALLOW=scripts/large-files-allowed.txt
allowed=$(grep -v '^#' "$ALLOW" | grep -v '^$' || true)
bad=$(git ls-files -z | xargs -0 stat -c '%s %n' |
  awk -v l="$LIMIT" '$1 > l { print }' |
  while IFS= read -r line; do
    path=${line#* }
    printf '%s\n' "$allowed" | grep -qxF -- "$path" || printf '%s\n' "$line"
  done)
if [ -n "$bad" ]; then
  echo "tracked files over $LIMIT bytes and not listed in $ALLOW:"
  printf '%s\n' "$bad"
  exit 1
fi
echo "large files: ok"
