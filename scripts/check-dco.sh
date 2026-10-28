#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Check that every non-merge commit in <base>..<head> has a Signed-off-by line.
# Commits authored by Dependabot (name and email must both match) are exempt.
# Usage: scripts/check-dco.sh <base-rev> [<head-rev>]
# Exit status: 0 all commits pass (or there is nothing to check), 1 a sign-off
# is missing, 2 usage error or git failure (fails closed: nothing was checked).
set -euo pipefail
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "usage: check-dco.sh <base-rev> [<head-rev>]" >&2
  exit 2
fi
base="$1"
head="${2:-HEAD}"
dependabot='dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>'

for rev in "$base" "$head"; do
  if ! git rev-parse --verify --quiet "$rev^{commit}" >/dev/null; then
    echo "error: '$rev' is not a commit" >&2
    exit 2
  fi
done
commits=$(git rev-list --no-merges "$base..$head") || {
  echo "error: git rev-list failed for $base..$head" >&2
  exit 2
}
if [ -z "$commits" ]; then
  echo "no commits to check in $base..$head"
  exit 0
fi

bad=0
exempt=
while IFS= read -r c; do
  author=$(git log -1 --format='%an <%ae>' "$c") || { echo "error: cannot read commit $c" >&2; exit 2; }
  if [ "$author" = "$dependabot" ]; then
    exempt="$exempt $(git log -1 --format=%h "$c")"
    continue
  fi
  msg=$(git log -1 --format=%B "$c") || { echo "error: cannot read commit $c" >&2; exit 2; }
  if ! grep -qE '^Signed-off-by: .+ <[^<>@ ]+@[^<>@ ]+>$' <<<"$msg"; then
    echo "missing Signed-off-by: $(git log -1 --format='%h %s' "$c")"
    bad=1
  fi
done <<<"$commits"
if [ -n "$exempt" ]; then
  echo "exempt from sign-off (authored by Dependabot):$exempt"
fi
if [ "$bad" = 1 ]; then
  echo "Add the sign-off with 'git commit -s' or 'git rebase --signoff $base' (see CONTRIBUTING.md)."
fi
exit "$bad"
