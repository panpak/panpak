#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Fail if a tracked file is larger than 1 MiB and not listed in
# scripts/large-files-allowed.txt (one path per line, '#' starts a comment).
# Keeps third-party binaries and other bulky blobs out of the repository.
# Usage: scripts/check-large-files.sh
# Exit status: 0 ok, 1 a file is too large, 2 git, stat or allow-list failure.
set -eu
cd "$(dirname "$0")/.."
LIMIT=1048576
ALLOW=scripts/large-files-allowed.txt
if [ ! -r "$ALLOW" ]; then
  echo "error: cannot read $ALLOW" >&2
  exit 2
fi
tmp=$(mktemp -d) || { echo "error: cannot create a temporary directory" >&2; exit 2; }
trap 'rm -rf "$tmp"' EXIT
fail() { echo "error: $1" >&2; exit 2; }

git ls-files -z > "$tmp/files" || fail "git ls-files failed"
[ -s "$tmp/files" ] || fail "no tracked files found"
xargs -0 stat -c '%s %n' < "$tmp/files" > "$tmp/sizes" || fail "stat failed on a tracked file"
awk -v l="$LIMIT" -v allow="$ALLOW" '
  FILENAME == allow { if ($0 !~ /^#/ && $0 != "") ok[$0] = 1; next }
  $1 > l { p = substr($0, index($0, " ") + 1); if (!(p in ok)) print }
' "$ALLOW" "$tmp/sizes" > "$tmp/bad" || fail "awk failed"
if [ -s "$tmp/bad" ]; then
  echo "tracked files over $LIMIT bytes and not listed in $ALLOW:"
  cat "$tmp/bad"
  exit 1
fi
echo "large files: ok"
