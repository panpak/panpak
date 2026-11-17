#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Test scripts/check-repro-date.sh in a throw-away repository.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
git init -q -b main .
git config user.email test@example.invalid
git config user.name test
mkdir -p scripts panpak docs
cp "$here/scripts/code-epoch.sh" "$here/scripts/check-repro-date.sh" scripts/
echo a >panpak/lib.rs
echo a >docs/x.md
ref() { printf '<!-- BEGIN -->\nBuild date: `SOURCE_DATE_EPOCH=%s` (x).\n<!-- END -->\n' "$1" >REPRODUCIBLE-HASHES.md; }
commit() { GIT_COMMITTER_DATE="$1" GIT_AUTHOR_DATE="$1" git commit -q -am "$2"; }
ref 1000
git add -A
GIT_COMMITTER_DATE="@1000 +0000" GIT_AUTHOR_DATE="@1000 +0000" git commit -q -m base
base=$(git rev-parse HEAD)
fail=0
expect() { # name, want status
  if scripts/check-repro-date.sh "$base" HEAD >/tmp/check-repro.out 2>&1; then got=0; else got=1; fi
  if [ "$got" = "$2" ]; then echo "ok   $1"; else echo "FAIL $1: status $got"; cat /tmp/check-repro.out; fail=1; fi
}
echo b >docs/x.md; commit "@2000 +0000" docs
expect "documents only: nothing to check" 0
git reset -q --hard "$base"
echo b >panpak/lib.rs; commit "@2000 +0000" code
expect "code changed, reference still the old date" 1
grep -q "scripts/repro-build.sh --update" /tmp/check-repro.out || { echo "FAIL: the message does not name the command"; fail=1; }
ref 2000; commit "@3000 +0000" reference
expect "code changed, reference with the date of the code commit" 0
echo c >panpak/lib.rs; commit "@4000 +0000" more-code
expect "another code commit after the reference" 1
ref 4000; commit "@5000 +0000" reference2
expect "reference follows the last code commit" 0
rm -f /tmp/check-repro.out
exit $fail
