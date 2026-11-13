#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Classify the files changed in <base>...<head> for the CI workflow.
# A file is documentation if it ends in .md or lives under docs/ (this includes
# docs/bench-data/, the measured data and charts). Everything else is code for
# this purpose, fail closed: the Rust crates, bench/, tests/, fuzz/, scripts/,
# conformance/, tools/, the workflows (so that a change to the CI runs the jobs
# it changes) and any folder added later.
# The third argument says whether the full level was asked for (a push to main, a manual or weekly
# run, or a pull request with the label full-ci): true or false.
# Prints lines for $GITHUB_OUTPUT:
#   heavy=true|false   some changed file is not documentation (build-test and private-bus run)
#   fuzz=true|false    heavy, and the full level (fuzz-smoke runs)
#   distro=true|false  heavy, and the full level (distro runs)
#   repro=true|false   (heavy and the full level), or REPRODUCIBLE-HASHES.md changed (repro runs)
# An empty change list counts as heavy (fail closed: nothing is skipped). With an empty <base-rev>
# and <head-rev> (not a pull request) there is nothing to compare: heavy is true.
# Usage: scripts/ci-changes.sh <base-rev> <head-rev> [full: true|false]
# Exit status: 0 done, 2 usage error or git failure.
set -euo pipefail
if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: ci-changes.sh <base-rev> <head-rev> [full]" >&2
  exit 2
fi
base="$1"
head="$2"
full="${3:-false}"
if [ "$full" != true ] && [ "$full" != false ]; then
  echo "error: full must be true or false" >&2
  exit 2
fi
if [ -z "$base" ] && [ -z "$head" ]; then
  printf 'heavy=true\nfuzz=%s\ndistro=%s\nrepro=%s\n' "$full" "$full" "$full"
  exit 0
fi
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
    # Listed only to say that they count: the default below gives the same answer.
    bench/* | tests/* | fuzz/* | scripts/* | conformance/* | tools/* | .github/*) heavy=true ;;
    *) heavy=true ;;
  esac
done <<<"$files"
fuzz=false
distro=false
repro=false
if [ "$heavy" = true ] && [ "$full" = true ]; then
  fuzz=true
  distro=true
  repro=true
fi
if [ "$hashes" = true ]; then
  repro=true
fi
echo "heavy=$heavy"
echo "fuzz=$fuzz"
echo "distro=$distro"
echo "repro=$repro"
