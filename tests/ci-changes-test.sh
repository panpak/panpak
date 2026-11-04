#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Test scripts/ci-changes.sh in a throw-away repository.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
git init -q -b main .
git config user.email test@example.invalid
git config user.name test
mkdir docs src
echo a >README.md
echo a >src/lib.rs
echo a >REPRODUCIBLE-HASHES.md
git add -A
git commit -q -m base
base=$(git rev-parse HEAD)

fail=0
# expect <name> <heavy> <repro>: the last commit is compared with $base.
expect() {
  local out
  out=$("$here/scripts/ci-changes.sh" "$base" HEAD)
  if [ "$out" != $'heavy='"$2"$'\nrepro='"$3" ]; then
    echo "FAIL $1: got $(tr '\n' ' ' <<<"$out")"
    fail=1
  else
    echo "ok   $1"
  fi
}
reset() { git reset -q --hard "$base"; }

reset; mkdir -p docs; echo b >README.md; echo b >docs/x.txt; git add -A; git commit -q -m t
expect "docs only" false false
reset; echo b >REPRODUCIBLE-HASHES.md; git add -A; git commit -q -m t
expect "reference hashes only" false true
reset; echo b >src/lib.rs; git add -A; git commit -q -m t
expect "code" true true
reset; echo b >README.md; echo b >src/lib.rs; git add -A; git commit -q -m t
expect "docs and code" true true
reset; git mv src/lib.rs src/lib.md; git add -A; git commit -q -m t
expect "code renamed to .md" true true
reset; git commit -q --allow-empty -m t
expect "empty change list" true true
reset; mkdir -p .github/workflows; echo b >.github/workflows/ci.yml; git add -A; git commit -q -m t
expect "workflow" true true
reset; mkdir -p docs; echo b >docs/deep.md; mkdir -p src/notes; echo b >src/notes/n.md; git add -A; git commit -q -m t
expect "markdown in a source directory" false false

for f in bench/make_charts.py tests/view.py fuzz/fuzz_targets/x.rs scripts/repro-build.sh conformance/run.py tools/requirements-zig.txt newdir/file.txt; do
  reset; mkdir -p "$(dirname "$f")"; echo b >"$f"; git add -A; git commit -q -m t
  expect "$f" true true
done
reset; mkdir -p docs/bench-data; echo b >docs/bench-data/delta.svg; echo b >docs/bench-data/a.csv; git add -A; git commit -q -m t
expect "docs/bench-data only" false false
reset; mkdir -p docs/bench-data bench; echo b >docs/bench-data/a.csv; echo b >bench/inputs.py; git add -A; git commit -q -m t
expect "bench data and bench code" true true

# The CI case: a pull request that changes only a workflow. The job checks the repository out
# with full history (fetch-depth 0) in a fresh clone and passes the base and head commits of the
# pull request, which are not the checked-out merge commit.
reset; mkdir -p .github/workflows; echo b >.github/workflows/ci.yml; git add -A; git commit -q -m t
head=$(git rev-parse HEAD)
git checkout -q -b other "$base"; echo c >README.md; git commit -q -am other; git checkout -q main
git merge -q --no-ff -m "merge of the pull request" "$head" 2>/dev/null || true
ci="$tmp-ci"
git clone -q --no-local "$tmp" "$ci"
out=$(cd "$ci" && "$here/scripts/ci-changes.sh" "$base" "$head")
rm -rf "$ci"
if [ "$out" = $'heavy=true\nrepro=true' ]; then echo "ok   pull request with a changed workflow, fresh full clone"; else echo "FAIL pull request with a changed workflow: got $(tr '\n' ' ' <<<"$out")"; fail=1; fi
reset

if "$here/scripts/ci-changes.sh" nosuchrev HEAD 2>/dev/null; then
  echo "FAIL unknown revision accepted"
  fail=1
else
  echo "ok   unknown revision refused"
fi
exit "$fail"
