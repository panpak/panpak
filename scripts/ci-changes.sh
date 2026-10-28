#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Classify the files changed in <base>...<head> for the CI workflow.
# A file is documentation if it ends in .md or lives under docs/.
# Prints two lines for $GITHUB_OUTPUT:
#   heavy=true|false   some changed file is not documentation (fuzz and distro run)
#   repro=true|false   heavy, or REPRODUCIBLE-HASHES.md changed (repro runs)
# An empty change list counts as heavy (fail closed: nothing is skipped).
# Usage: scripts/ci-changes.sh <base-rev> <head-rev>
# Exit status: 0 done, 2 usage error or git failure.
set -euo pipefail
if [ "$#" -ne 2 ]; then
  echo "usage: ci-changes.sh <base-rev> <head-rev>" >&2
  exit 2
fi
base="$1"
head="$2"
for rev in "$base" "$head"; do
  if ! git rev-parse --verify --quiet "$rev^{commit}" >/dev/null; then
    echo "error: '$rev' is not a commit" >&2
    exit 2
  fi
done
# --no-renames lists both the old and the new path of a rename.
files=$(git diff --name-only --no-renames "$base...$head") || {
  echo "error: git diff failed for $base...$head" >&2
  exit 2
}
heavy=false
hashes=false
if [ -z "$files" ]; then
  heavy=true
fi
while IFS= read -r f; do
  [ -n "$f" ] || continue
  case "$f" in
    REPRODUCIBLE-HASHES.md) hashes=true ;;
    *.md | docs/*) ;;
    *) heavy=true ;;
  esac
done <<<"$files"
repro=$heavy
if [ "$hashes" = true ]; then
  repro=true
fi
echo "heavy=$heavy"
echo "repro=$repro"
