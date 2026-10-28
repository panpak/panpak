#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Check that every non-merge commit in <base>..<head> has a Signed-off-by line.
# Commits authored by Dependabot (name and email must both match) are exempt.
# Usage: scripts/check-dco.sh <base-rev> [<head-rev>]
set -eu
base="${1:?usage: check-dco.sh <base-rev> [<head-rev>]}"
head="${2:-HEAD}"
dependabot='dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>'
bad=0
exempt=
for c in $(git rev-list --no-merges "$base..$head"); do
  if [ "$(git log -1 --format='%an <%ae>' "$c")" = "$dependabot" ]; then
    exempt="$exempt $(git log -1 --format=%h "$c")"
    continue
  fi
  if ! git log -1 --format=%B "$c" | grep -qE '^Signed-off-by: .+ <[^<>@ ]+@[^<>@ ]+>$'; then
    echo "missing Signed-off-by: $(git log -1 --format='%h %s' "$c")"
    bad=1
  fi
done
if [ -n "$exempt" ]; then
  echo "exempt from sign-off (authored by Dependabot):$exempt"
fi
if [ "$bad" = 1 ]; then
  echo "Add the sign-off with 'git commit -s' or 'git rebase --signoff $base' (see CONTRIBUTING.md)."
fi
exit $bad
