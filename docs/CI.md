<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# The CI in three levels

The workflow is `.github/workflows/ci.yml`; the weekly app harness is `.github/workflows/apps-harness.yml`. The decision of who runs when is made by the
`changes` job (`scripts/ci-changes.sh`) and checked at the end by the `gate` job (`scripts/ci-gate.sh`), which is the one to require in the branch rules.

## Levels

| Level | When | Jobs | Wall time |
|---|---|---|---|
| Fast | every pull request that changes more than documentation (`*.md`, `docs/**`) | changes, DCO, secrets, licenses, clippy, build-test (with `tests/app_dirs.py`, 5 s), private-bus; repro too when `REPRODUCIBLE-HASHES.md` changes; `changes` also checks, with no build, that a change of code carries the build date of its last code commit in `REPRODUCIBLE-HASHES.md` (`scripts/check-repro-date.sh`) | about 4 minutes |
| Full | every push to main, a manual run, the weekly run, a pull request with the label `full-ci` | the fast level, `tests/view.py` (the isolated view end to end, 2.5 minutes, a step of build-test), and fuzz-smoke (19 targets, 30 s each), repro (two builds, same bytes), distro (the runtime in four containers) | about 4 minutes (the jobs run in parallel), 32 job-minutes |
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

## build-test in two jobs

`build-test` was one job of 707 s at the full level (531 to 633 s at the fast level), and everything else ended in under four minutes, so it set the length of every run. It is two jobs that run at the same time, with the same set-up (checkout, caches, toolchain, Zig, packages, bubblewrap): `rust-tests` (the unit and integration tests of the workspace, the release build, the test points with the failpoints build, the blob visibility, the lock files of a signal and the icons of an update) and `build-test` (the release build, the conformance, the scripts of `tests/`, `view.py` at the full level, the artifact for the distro matrix). Each does its own release build (about 50 s), which is the price of running beside the other. `gate` needs both (`scripts/ci-gate.sh`, tested by `tests/ci-gate-test.sh`); the rule of `main` requires only `gate`, so there is nothing to change in the settings of the repository. No test was removed, skipped or moved to another level: every step of the old job is in one of the two.

## The advisory job and the caches of the fixed downloads

- `sway` (the Wayland security context against a real Sway) is a job of its own, advisory (`continue-on-error`, not in `needs` of `gate`), with its own release build, so it runs beside the others and nothing waits for it. It was a step of `build-test`, where it took 7 to 284 s, most of it `apt`.
- In `build-test` the Rust toolchain is cached with a key that holds what it is, `rustup-<toolchain>-minimal-rustfmt-musl`. A cache is never trusted for what it holds: the step still runs `rustup toolchain install` (a no-op when the cache is right). Measured: the toolchain took 10 to 21 s, with the cache 3 s (restore 2 s, step 1 s). The Zig wheel was cached too and taken out again: with the cache the two steps took 7 s against 6 to 8 s without it, so there was nothing to save.

## Time limits, the package mirror, and the stores of the tests

- **Every step has a time limit** (`timeout-minutes`), about three times the longest time seen in 25 successful runs (two minutes at least for a script, one more minute than the script's own limit so that the script's message comes first). A job already had a limit of 5 to 40 minutes; a step that hangs now ends in minutes.
- **Every script of `tests/` that a step runs goes through `tests/with_timeout.py <seconds> <script>`**: after the limit the whole process group is stopped and the log says `TIMEOUT: <the command> was still running after N s` (exit status 124), so the log names the test and not only the job. It is tested by `tests/with_timeout_test.py`. A command that moves to a session of its own escapes the group; the limit of the step is the second net. The steps of `cargo test` have the limit of the step only (the log names the step).
- **`scripts/apt-install.sh`** installs the packages of every job with 180 s for the first attempt and, if it does not finish or fails, replaces the mirror of the runner image (`azure.archive.ubuntu.com`) by `archive.ubuntu.com` and tries again with 180 s. The mirror took 23 s in most runs and 518 to 716 s in others (`private-bus`, 2026-11-18). It is tested by `tests/apt_install_test.py` with a fake `apt-get`. A cache of the `.deb` files made by hand was not done.
- **`PANPAK_TEST_LINK_STORE`** (a debug build only) makes `install` put a hard link of the debug `panpak` in the store instead of a copy of 100 MB. Five more test files set it (`install_paths`, `ext_alias`, `convert_icons`, `pack_checks`, `permissions_cli`): locally 115 s became 36 s. `desktop.rs` must not set it: its tests write into the stored `panpak` in place, which with a link is the debug binary itself (found when a first try overwrote it).
- **A shared release build** (an artifact made by one job and used by the others) was measured on paper and not done: the other jobs would wait for the job that builds (about 75 s with its set-up) to save a build of 37 to 54 s, so each of them would end later, not sooner. `distro` uses the artifact of `build-test` because it has nothing else to do while it waits.

## Every test runs in a level

Every file of `tests/` runs in at least one level of the CI, or is listed in `tests/ci-coverage.txt` with its kind (`helper`: not a test; `manual`: needs a VM, a desktop session or a real app) and the reason. `tests/ci-workflow-test.py` fails for a file that is in neither place, and for a listed file that is also in a workflow.
