#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# A release binary must not hold the failpoints of the test builds (panpak_core::failpoint!): the names of the points, the message that a point
# prints, the name of the environment variable that arms one, and the one that points the check of a domain at a local server.
# Usage: scripts/check-no-failpoints.sh <binary>...            fails (1) if one of them holds any
#        scripts/check-no-failpoints.sh --expect <binary>...   fails (1) if one of them holds none: proves that the check can see them
set -eu
mode=absent
if [ "${1:-}" = "--expect" ]; then
  mode=present
  shift
fi
[ "$#" -ge 1 ] || { echo "usage: $0 [--expect] <binary>..." >&2; exit 2; }
status=0
for f in "$@"; do
  [ -f "$f" ] || { echo "error: no such file: $f" >&2; exit 2; }
  n=0
  # The words are split so that this script does not hold them as one string.
  for w in "FAIL""POINT" "fail""point " "install.record_""written" "prepare.tree_""renamed" "rollback.between_""links" "PANPAK_TEST_""RESOLVE"; do
    if grep -aqF -- "$w" "$f"; then
      n=$((n + 1))
    fi
  done
  if [ "$mode" = absent ] && [ "$n" -gt 0 ]; then
    echo "FAIL  $f holds $n failpoint marker(s)"
    status=1
  elif [ "$mode" = present ] && [ "$n" -eq 0 ]; then
    echo "FAIL  $f holds no failpoint marker, so this check could not see one"
    status=1
  else
    echo "ok    $f ($n marker(s), expected $mode)"
  fi
done
exit $status
