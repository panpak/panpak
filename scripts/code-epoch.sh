#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# The date (seconds since 1970) of the last commit that touched the code: the date `panpak --build-epoch` reports for a build with no
# SOURCE_DATE_EPOCH. The one rule, used by panpak/build.rs and by scripts/repro-build.sh. A commit of documents or of the reference hashes does not
# move it. Prints nothing (and exits 0) when there is no git, no history, or a shallow checkout, where the date would be wrong.
# `code-epoch.sh REV` gives the date as of that commit instead of the checked out one; `code-epoch.sh --paths` prints the paths that count as code, one
# per line (scripts/check-repro-date.sh asks whether a change touches them).
cd "$(dirname "$0")/.." || exit 0
PATHS="panpak panpak-core panpak-stub panpak-icon Cargo.toml Cargo.lock rust-toolchain.toml .cargo scripts/zig-cc"
if [ "${1:-}" = --paths ]; then
  for p in $PATHS; do echo "$p"; done
  exit 0
fi
command -v git >/dev/null 2>&1 || exit 0
[ "$(git rev-parse --is-shallow-repository 2>/dev/null)" = false ] || exit 0
# shellcheck disable=SC2086
t="$(git log -1 --format=%ct "${1:-HEAD}" -- $PATHS 2>/dev/null)" || exit 0
case "$t" in ''|*[!0-9]*) ;; *) echo "$t" ;; esac
exit 0
