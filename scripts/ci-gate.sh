#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Final check of the CI workflow: every job that had to run succeeded, and every
# job that did not have to run was skipped. A job that was skipped by mistake (a
# wrong condition, a failed dependency) fails the gate instead of passing as
# "skipped".
# Input, all in the environment:
#   EVENT                      pull_request, push, schedule or workflow_dispatch
#   MONDAY_RUN                 output of the monday job: "true", "false", or empty
#   HEAVY, REPRO               outputs of the changes job: "true", "false", or empty
#   R_<JOB>                    result of each job: success, failure, cancelled, skipped
#                              (JOB is MONDAY, CHANGES, BUILD_TEST, CLIPPY, FUZZ_SMOKE,
#                              REPRO_JOB, LICENSES, DISTRO, DCO, SECRETS)
# Exit status: 0 all as expected, 1 some job is not, 2 input missing.
set -euo pipefail
: "${EVENT:?}"
status=0
rows=""

# want <job> <expected>: expected is "success" or "skipped".
want() {
  local job="$1" expected="$2" var got
  var="R_$1"
  got="${!var:-}"
  if [ -z "$got" ]; then
    echo "error: $var is not set" >&2
    exit 2
  fi
  if [ "$got" = "$expected" ]; then
    rows+="ok    $job: $got"$'\n'
  else
    rows+="FAIL  $job: $got (expected $expected)"$'\n'
    status=1
  fi
}

# Scheduled runs continue only on the first Monday of the month.
all=true
if [ "$EVENT" = schedule ] && [ "${MONDAY_RUN:-}" != true ]; then
  all=false
fi
if [ "$EVENT" = schedule ]; then
  want MONDAY success
else
  want MONDAY skipped
fi

# Fast checks: always, except the whole run is off.
light() { if [ "$all" = true ]; then want "$1" success; else want "$1" skipped; fi; }
light CLIPPY
light LICENSES
light SECRETS
if [ "$all" = true ] && { [ "$EVENT" = pull_request ] || [ "$EVENT" = push ]; }; then
  want DCO success
else
  want DCO skipped
fi

# Heavy checks: not on a push to main; on the other events as the classification says.
if [ "$all" = true ] && [ "$EVENT" != push ]; then
  want CHANGES success
  heavy() { if [ "${!2:-}" = true ]; then want "$1" success; else want "$1" skipped; fi; }
  heavy BUILD_TEST HEAVY
  heavy FUZZ_SMOKE HEAVY
  heavy DISTRO HEAVY
  heavy REPRO_JOB REPRO
else
  for j in CHANGES BUILD_TEST FUZZ_SMOKE DISTRO REPRO_JOB; do want "$j" skipped; done
fi

printf '%s' "$rows"
exit "$status"
