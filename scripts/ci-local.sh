#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# The checks of the CI, run here, in one command, before every push:
#
#   scripts/ci-local.sh [--quick] [BASE]
#
# BASE is the branch the pull request goes to (default main). Run it on a clean tree, with everything committed: the checks read the commits.
# The order is the one of the CI: DCO, the static checks (SPDX header, large files, links of the documents, copies of zstd, the rules of the workflow
# itself), the date of the reproducible reference, formatting, clippy with `-D warnings` (musl), cargo-deny, gitleaks, then the tests (`cargo test` on musl through
# the runner with the canary, and every `tests/*.py` that the workflow runs), and last the double build of the reproducible hashes. `--quick` stops after clippy,
# cargo-deny and gitleaks.
#
# What it does NOT repeat (needs what a machine of the CI has, and is left to the CI): the `private-bus` job (a systemd user session and packages installed
# with apt), the `sway` job (a headless Sway), the `distro` matrix (containers of other distributions), `fuzz-smoke` (a nightly toolchain), the weekly app
# harness, the kernel of the runner (the tests that need a newer Landlock than the host has skip here and fail there), the fresh checkout with the whole
# history and no cache, and the fetch of the RustSec database when there is no network. A failure of the infrastructure (apt locked, a download that times
# out) is written in tests/UNSTABLE.md, and the job is run again.
#
# Temporary files: the tests run with their own TMPDIR (a new folder under $PANPAK_CI_TMP_BASE, default /tmp; not under $HOME, which the view of the sandbox hides), and after every step the script lists what the step left
# there and what it left in /tmp outside it. A test that leaves a folder fails the run, unless the name matches a line of tests/ci-tmp-allowed.txt (a reason on the same line).
# The folder of the run is removed with rmdir only when it is empty; when something was left it is kept and named, to be looked at and cleaned by hand.
#
# Priority limits: every heavy step runs under `nice -n 19` and `ionice -c 3`; PANPAK_LOWRUN=<wrapper> puts a wrapper in front instead (a systemd scope with a
# memory cap), for example ~/panpak-dogfood/tools/lowrun.sh. Never run it from a graphical terminal's tests by hand with another environment: the runners take the
# desktop out of the environment of the tests (CONTRIBUTING.md).
set -u
cd "$(dirname "$0")/.."
quick=0
base=main
for a in "$@"; do
	case "$a" in
	--quick) quick=1 ;;
	-h | --help) sed -n '2,/^set -u/p' "$0" | sed '$d'; exit 0 ;;
	*) base="$a" ;;
	esac
done

# The Zig that builds for musl: the one of the pinned wheel (scripts/zig-env.sh finds it); a venv of it on PATH is allowed, but not its python3 (it would hide the system one).
if [ -n "${PANPAK_ZIG_VENV:-}" ]; then
	zigdir=$("$PANPAK_ZIG_VENV/bin/python" -c 'import os, ziglang; print(os.path.dirname(ziglang.__file__))' 2>/dev/null) && export PATH="$zigdir:$PATH"
elif [ -d "$HOME/panpak-dogfood/zigvenv" ]; then
	zigdir=$("$HOME/panpak-dogfood/zigvenv/bin/python" -c 'import os, ziglang; print(os.path.dirname(ziglang.__file__))' 2>/dev/null) && export PATH="$zigdir:$PATH"
fi

lowrun() {
	if [ -n "${PANPAK_LOWRUN:-}" ]; then
		"$PANPAK_LOWRUN" "$@"
	else
		nice -n 19 ionice -c 3 "$@" 2>/dev/null || nice -n 19 "$@"
	fi
}

failed=()
skipped=()
timings=()
leftovers=()
started=$(date +%s)
# Not under $HOME: the view of the sandbox puts an empty tmpfs on it, so the tests could not see their own files.
ci_tmp_base="${PANPAK_CI_TMP_BASE:-/tmp}"
mkdir -p "$ci_tmp_base"
ci_tmp=$(mktemp -d "$ci_tmp_base/ci.XXXXXX")
export TMPDIR="$ci_tmp"
trap 'rmdir "$ci_tmp" 2>/dev/null' EXIT
allowed="tests/ci-tmp-allowed.txt"
# The names in the run's TMPDIR and the names in /tmp (outside it) that are not the ones of other users of the machine: one per line, sorted.
tmp_listing() {
	ls -A "$ci_tmp" 2>/dev/null | sed 's|^|T:|'
	ls -A /tmp 2>/dev/null | grep -E '^(panpak|\.panpak)' | sed 's|^|/tmp:|'
}
is_allowed() { # is_allowed NAME: a line of the allow-list (a glob, then a reason) matches it
	[ -f "$allowed" ] || return 1
	local pat
	while read -r pat _; do
		case "$pat" in '' | '#'*) continue ;; esac
		# shellcheck disable=SC2254
		case "$1" in $pat) return 0 ;; esac
	done <"$allowed"
	return 1
}
step() { # step "name" command...
	local name="$1"
	shift
	printf '\n=== %s\n' "$name"
	local t0 t1 before after n
	before=$(tmp_listing | LC_ALL=C sort)
	t0=$(date +%s)
	if "$@"; then
		printf 'ok   %s\n' "$name"
	else
		printf 'FAIL %s\n' "$name"
		failed+=("$name")
	fi
	t1=$(date +%s)
	timings+=("$((t1 - t0)) s  $name")
	after=$(tmp_listing | LC_ALL=C sort)
	while read -r n; do
		[ -n "$n" ] || continue
		is_allowed "${n#*:}" && continue
		printf 'LEFT %s (after the step "%s")\n' "$n" "$name"
		leftovers+=("$n  <- $name")
	done < <(LC_ALL=C comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$after"))
}
skip() {
	printf '\n=== %s\nskip %s\n' "$1" "$2"
	skipped+=("$1: $2")
}

if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
	echo "The tree has changes that are not committed: the checks read the commits (the date of the reference, the DCO). Commit first." >&2
	exit 2
fi
head=$(git rev-parse HEAD)

step "DCO (every commit of $base..HEAD is signed off)" ./scripts/check-dco.sh "$base" "$head"
step "SPDX headers" ./scripts/check-spdx.sh
step "large files" ./scripts/check-large-files.sh
step "copies of zstd" ./scripts/check-zstd-copies.sh
step "the rules of the workflow, and every test in a level of the CI" python3 -I tests/ci-workflow-test.py
step "the date of REPRODUCIBLE-HASHES.md is the one of the last code commit" ./scripts/check-repro-date.sh "$base" "$head"
if cargo fmt --version >/dev/null 2>&1; then
	step "formatting (cargo fmt --check)" cargo fmt --all --check
else
	skip "formatting" "rustfmt is not installed here (rustup component add rustfmt); the CI does not check it either"
fi
step "clippy, zero warnings (musl)" lowrun cargo clippy --locked --all-targets --target x86_64-unknown-linux-musl -- -D warnings
if command -v cargo-deny >/dev/null 2>&1; then
	step "cargo-deny (licenses, bans, sources, advisories)" cargo deny check
else
	skip "cargo-deny" "not installed"
fi
if command -v gitleaks >/dev/null 2>&1; then
	step "gitleaks" gitleaks detect --no-banner --config .gitleaks.toml --redact
else
	skip "gitleaks" "not installed"
fi

if [ "$quick" = 0 ]; then
	step "build for the tests that start the programs (debug, musl)" lowrun cargo build --locked --target x86_64-unknown-linux-musl -p panpak -p panpak-stub
	step "cargo test (musl, with the canary bus)" lowrun python3 tests/with_timeout.py 600 cargo test --locked --no-fail-fast --target x86_64-unknown-linux-musl
	step "release build" lowrun cargo build --locked --release --target x86_64-unknown-linux-musl
	# The scripts that the workflow runs through the runner, in the order of the file, each once. The ones that need a tool this machine has not skip by themselves
	# (the CI sets PANPAK_CI=1 and forbids that).
	for t in $(grep -oE 'python3 tests/with_timeout\.py [0-9]+ tests/[a-z_0-9]+\.py' .github/workflows/ci.yml | awk '{print $3" "$4}' | awk '!seen[$2]++' | tr ' ' ':'); do
		secs=${t%%:*}
		script=${t##*:}
		case "$script" in
		tests/failpoints.py | tests/apps/*) skip "$script" "needs the build with test points, run by the CI" ;;
		tests/privbus.py | tests/dbus_filter.py | tests/wayland_sway.py | tests/gtk_filechooser.py) skip "$script" "needs a bus, portals or a Sway of its own: the jobs private-bus and sway" ;;
		*) step "$script" lowrun python3 tests/with_timeout.py "$secs" "$script" ;;
		esac
	done
	# The unit tests of the app harness (tests/apps), which the workflow runs with `unittest discover` (a table of the harness that must be the one of the Rust code is checked there).
	for cmd in $(grep -oE "python3 -m unittest discover -s [a-z/]+ -p 'test_\*\.py'" .github/workflows/ci.yml | sort -u | tr ' ' '|'); do
		line=$(echo "$cmd" | tr '|' ' ')
		step "$line" bash -c "$line"
	done
	# The reference of the reproducible build, last: the double build starts from nothing and takes the longest. It is compared with the committed file.
	step "reproducible build: the reference is what the script makes" bash -c 'lowrun() { "$@"; }; ./scripts/repro-build.sh --update | tee /tmp/panpak-repro-local.log && grep -q IDENTICAL /tmp/panpak-repro-local.log && git diff --exit-code -- REPRODUCIBLE-HASHES.md'
fi

printf '\n--------\n'
printf 'times (seconds):\n'
printf '  %s\n' "${timings[@]}"
printf '  total %d s\n' "$(($(date +%s) - started))"
for s in "${skipped[@]:-}"; do [ -n "$s" ] && printf 'skipped: %s\n' "$s"; done
if rmdir "$ci_tmp" 2>/dev/null; then :; else
	printf 'The temporary folder of this run was kept, it is not empty: %s\n' "$ci_tmp"
fi
if [ "${#leftovers[@]}" -gt 0 ]; then
	printf 'LEFT BEHIND (%d), T: is in the folder of the run, /tmp: is in /tmp:\n' "${#leftovers[@]}"
	printf '  %s\n' "${leftovers[@]}"
	failed+=("temporary files left by the steps")
fi
if [ "${#failed[@]}" -gt 0 ]; then
	printf 'FAILED (%d):\n' "${#failed[@]}"
	printf '  %s\n' "${failed[@]}"
	echo "Do not push. If a step failed in the infrastructure (a download, apt), say so in tests/UNSTABLE.md."
	exit 1
fi
echo "all the checks that can be run here passed: push."
