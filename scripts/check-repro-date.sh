#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Usage: scripts/check-repro-date.sh BASE HEAD
# The quick check of a pull request, with no build: when the change touches the code (the paths of scripts/code-epoch.sh), the build date in
# REPRODUCIBLE-HASHES.md at HEAD must be the one scripts/code-epoch.sh gives for HEAD. Otherwise the reference is the one of an older build and the
# `repro` job fails on main after the merge. The double build itself runs on main, with the label `full-ci`, and when the reference changes.
# Needs the whole history (a shallow checkout gives no date and fails).
set -eu
base="$1"
head="$2"
here=$(cd "$(dirname "$0")/.." && pwd)
cd "$here"
paths=$(sh scripts/code-epoch.sh --paths | tr '\n' ' ')
# shellcheck disable=SC2086
if git diff --quiet "$base" "$head" -- $paths; then
  echo "no code changed: the reference date is not checked"
  exit 0
fi
want=$(sh scripts/code-epoch.sh "$head")
if [ -z "$want" ]; then
  echo "::error::no build date for $head: the history is not complete (fetch-depth 0)"
  exit 1
fi
have=$(git show "$head:REPRODUCIBLE-HASHES.md" | sed -n 's/^Build date: `SOURCE_DATE_EPOCH=\([0-9][0-9]*\)`.*/\1/p' | head -n 1)
if [ "$have" != "$want" ]; then
  echo "::error::this change touches the code, so its last commit has the date $want, and REPRODUCIBLE-HASHES.md says ${have:-nothing}. Run scripts/repro-build.sh --update after the last code commit and commit the file"
  exit 1
fi
echo "ok: the build date of the reference is $want"
