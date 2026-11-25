#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Fail if a tracked file names the private work folder of a maintainer. The repository must not depend on a folder that exists on one disk only.
# This script is left out of the search, because it has to name the folder to look for it.
# Usage: scripts/check-private-paths.sh
# Exit status: 0 ok, 1 a tracked file names it, 2 git failure.
set -eu
cd "$(dirname "$0")/.."
name="panpak-dogfood"
if hits=$(git grep -n -I -e "$name" -- . ':!scripts/check-private-paths.sh'); then
  echo "error: a tracked file names a private folder ($name); the repository must not depend on it:" >&2
  printf '%s\n' "$hits" | cut -c1-200 >&2
  exit 1
else
  rc=$?
  [ "$rc" = 1 ] || { echo "error: git grep failed" >&2; exit 2; }
fi
echo "ok: no tracked file names $name"
