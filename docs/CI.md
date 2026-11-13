<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# The CI in three levels

The workflow is `.github/workflows/ci.yml`; the weekly app harness is `.github/workflows/apps-harness.yml`. The decision of who runs when is made by the
`changes` job (`scripts/ci-changes.sh`) and checked at the end by the `gate` job (`scripts/ci-gate.sh`), which is the one to require in the branch rules.

## Levels

| Level | When | Jobs | Wall time |
|---|---|---|---|
| Fast | every pull request that changes more than documentation (`*.md`, `docs/**`) | changes, DCO, secrets, licenses, clippy, build-test, private-bus; repro too when `REPRODUCIBLE-HASHES.md` changes | about 4 minutes |
| Full | every push to main, a manual run, the weekly run, a pull request with the label `full-ci` | the fast level, and fuzz-smoke (19 targets, 30 s each), repro (two builds, same bytes), distro (the runtime in four containers) | about 4 minutes (the jobs run in parallel), 32 job-minutes |
| Weekly | Monday 03:17 UTC | the full level with 300 s for each fuzz target; the workflow apps-harness builds and starts Text Editor | |

Why the split: a pull request used 63 job-minutes on a cold cache and 34 on a warm one, and 18 of them were fuzz jobs that run the same corpus on code that mostly
did not change. The fast level is 8 job-minutes. A push to main runs the full level, so main always has the fuzz, the reproducibility and the distro check, and it saves the caches that a
new pull request can restore (before, main ran only the light jobs, and the two pull requests that started cold took 8 minutes against 4 when warm; the effect of the change on a
new pull request is to be checked on the next one). When a pull
request is ready, put the label `full-ci` on it and wait for the gate: that is the check before the merge.

Docs-only pull requests run only the light jobs. Every job has a time limit. A new push to a pull request cancels the run in progress. All actions are pinned to a
full commit with the version in a comment (`tests/ci-workflow-test.py` checks it, and that every job has a time limit).

## Times, before and after

Measured on the runs of 2026-11-08 to 2026-11-13 (seven runs, GitHub API: start and end of each job). The pull request run of this change: fast level 246 s, full
level (label) 225 s.

| Job | Level | Before (median, max) | After |
|---|---|---|---|
| changes, dco, secrets, licenses | fast | 4 to 22 s | the same |
| clippy | fast | 36 s (49) | 31 to 36 s |
| build-test (build, tests, conformance, view tests) | fast | 246 s (460, cold cache) | 195 s |
| private-bus (new: the private bus against a systemd user session) | fast | 164 s (186) | 157 to 228 s |
| repro | full (fast when the hashes change) | 212 s (381) | 188 s |
| distro (4 containers) | full | 6 to 14 s | 10 s |
| fuzz-smoke (19 targets) | full | 76 to 136 s (474, cold cache) | 80 s |
| gate | always | 5 s | 5 s |
| Pull request, wall time | | 250 s warm, 477 s cold | 246 s (fast), 225 s (full) |
| Pull request, job-minutes | | 34 warm, 63 cold | 8.4 (fast), 32 (full) |

The wall time was already under ten minutes: the jobs run in parallel. What changes is the cost of a run (a quarter of it for the fast level), and that main now
fills the caches.
