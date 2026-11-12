#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Test scripts/ci-gate.sh with the job results of the cases it must tell apart.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
fail=0
ok=success
sk=skipped
# PB=<result> in front of a case sets the result of the private-bus job (default: the one of build-test).
# case <name> <want-exit> <event> <monday> <heavy> <repro> <monday> <changes> <build-test> <clippy> <fuzz> <repro> <licenses> <distro> <dco> <secrets>
run() {
  local name="$1" want="$2" got=0
  shift 2
  EVENT="$1" MONDAY_RUN="$2" HEAVY="$3" REPRO="$4" R_MONDAY="$5" R_CHANGES="$6" R_BUILD_TEST="$7" R_PRIVATE_BUS="${PB:-$7}" R_CLIPPY="$8" \
    R_FUZZ_SMOKE="$9" R_REPRO_JOB="${10}" R_LICENSES="${11}" R_DISTRO="${12}" R_DCO="${13}" R_SECRETS="${14}" \
    "$here/scripts/ci-gate.sh" >/dev/null 2>&1 || got=$?
  if [ "$got" = "$want" ]; then echo "ok   $name"; else echo "FAIL $name: exit $got, expected $want"; fail=1; fi
}
#   event         mon heavy repro | MON CHG  BLD  CLP  FUZ  REP  LIC  DIS  DCO  SEC
run "PR with code, all green"      0 pull_request "" true true  $sk $ok $ok $ok $ok $ok $ok $ok $ok $ok
run "PR docs only, heavy skipped"  0 pull_request "" false false $sk $ok $sk $ok $sk $sk $ok $sk $ok $ok
run "PR hashes only"               0 pull_request "" false true  $sk $ok $sk $ok $sk $ok $ok $sk $ok $ok
run "PR with code, fuzz skipped"   1 pull_request "" true true  $sk $ok $ok $ok $sk $ok $ok $ok $ok $ok
PB=failure run "PR with code, private bus failed" 1 pull_request "" true true  $sk $ok $ok $ok $ok $ok $ok $ok $ok $ok
PB=skipped run "PR with code, private bus skipped" 1 pull_request "" true true  $sk $ok $ok $ok $ok $ok $ok $ok $ok $ok
run "PR with code, distro skipped" 1 pull_request "" true true  $sk $ok $ok $ok $ok $ok $ok $sk $ok $ok
run "PR with code, build failed"   1 pull_request "" true true  $sk $ok failure $ok $ok $ok $ok $sk $ok $ok
run "PR docs only, secrets failed" 1 pull_request "" false false $sk $ok $sk $ok $sk $sk $ok $sk $ok failure
run "PR docs only, clippy skipped" 1 pull_request "" false false $sk $ok $sk $sk $sk $sk $ok $sk $ok $ok
run "PR docs only, build ran"      1 pull_request "" false false $sk $ok $ok $ok $sk $sk $ok $sk $ok $ok
run "PR, classification failed"    1 pull_request "" "" "" $sk failure $sk $ok $sk $sk $ok $sk $ok $ok
run "PR cancelled"                 1 pull_request "" true true  $sk $ok cancelled $ok $ok $ok $ok $ok $ok $ok
run "push to main, light only"     0 push "" "" "" $sk $sk $sk $ok $sk $sk $ok $sk $ok $ok
run "push to main, DCO skipped"    1 push "" "" "" $sk $sk $sk $ok $sk $sk $ok $sk $sk $ok
run "push to main, build ran"      1 push "" "" "" $sk $sk $ok $ok $sk $sk $ok $sk $ok $ok
run "first Monday, all green"      0 schedule true true true $ok $ok $ok $ok $ok $ok $ok $ok $sk $ok
run "other day 1-7, all skipped"   0 schedule false "" "" $ok $sk $sk $sk $sk $sk $sk $sk $sk $sk
run "other day 1-7, secrets ran"   1 schedule false "" "" $ok $sk $sk $sk $sk $sk $sk $sk $sk $ok
run "first Monday, repro skipped"  1 schedule true true true $ok $ok $ok $ok $ok $sk $ok $ok $sk $ok
run "manual run, all green"        0 workflow_dispatch "" true true $sk $ok $ok $ok $ok $ok $ok $ok $sk $ok
if EVENT=push R_MONDAY=skipped "$here/scripts/ci-gate.sh" >/dev/null 2>&1; then
  echo "FAIL missing result accepted"; fail=1
else
  echo "ok   missing result refused"
fi
exit "$fail"
