#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Check that every non-merge commit in <base>..<head> has a Signed-off-by line.
# Usage: scripts/check-dco.sh <base-rev> [<head-rev>]
set -eu
base="${1:?usage: check-dco.sh <base-rev> [<head-rev>]}"
head="${2:-HEAD}"
bad=0
for c in $(git rev-list --no-merges "$base..$head"); do
  if ! git log -1 --format=%B "$c" | grep -qE '^Signed-off-by: .+ <[^<>@ ]+@[^<>@ ]+>$'; then
    echo "missing Signed-off-by: $(git log -1 --format='%h %s' "$c")"
    bad=1
  fi
done
if [ "$bad" = 1 ]; then
  echo "Add the sign-off with 'git commit -s' or 'git rebase --signoff $base' (see CONTRIBUTING.md)."
fi
exit $bad
