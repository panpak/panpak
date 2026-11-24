#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Test that scripts/repro-build.sh removes the folder it made, and only that one: when it ends (identical, different, failed), when it is
# interrupted, and never a folder that existed before; PANPAK_REPRO_KEEP=1 keeps it. A fake Zig and a fake cargo stand in for the real ones, so no build is made.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
pinned=$(sed -n 's/^ziglang==\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p' "$here/tools/requirements-zig.txt")
[ -n "$pinned" ] || { echo "FAIL: no pinned Zig version"; exit 1; }
mkdir -p "$tmp/cargo/bin" "$tmp/home" "$tmp/cache"
printf '#!/bin/sh\necho %s\n' "$pinned" >"$tmp/zig"
# The fake cargo makes the two binaries in the folder it runs in. FAKE_CARGO says what else it does: ok (nothing), differ (the second build differs),
# fail (exit 1), wait (say it started, then wait to be stopped).
cat >"$tmp/cargo/bin/cargo" <<'EOF'
#!/bin/sh
rel=target/x86_64-unknown-linux-musl/release
mkdir -p "$rel"
case "${FAKE_CARGO:-ok}" in
  fail) echo "error: fake failure"; exit 1 ;;
  differ) echo "$PWD" >"$rel/panpak"; echo stub >"$rel/panpak-stub" ;;
  wait)
    echo $$ >"$FAKE_CARGO_PIDFILE"
    while [ ! -e "$FAKE_CARGO_STOP" ]; do sleep 0.1; done
    exit 1 ;;
  *) echo binary >"$rel/panpak"; echo stub >"$rel/panpak-stub" ;;
esac
EOF
chmod +x "$tmp/zig" "$tmp/cargo/bin/cargo"
fail=0
ok() { echo "ok   $1"; }
bad() { echo "FAIL $1"; fail=1; }
# run NAME STATUS [VAR=value...]: run the script with the fakes, the folder in $tmp/work, the output in $tmp/out.
run() {
  local want=$1; shift
  set +e
  env HOME="$tmp/home" XDG_CACHE_HOME="$tmp/cache" CARGO_HOME="$tmp/cargo" RUSTUP_HOME="$tmp/rustup" PANPAK_ZIG="$tmp/zig" SOURCE_DATE_EPOCH=1000 \
    PANPAK_REPRO_DIR="$tmp/work" "$@" sh "$here/scripts/repro-build.sh" >"$tmp/out" 2>&1
  got=$?
  set -e
  [ "$got" = "$want" ] || { echo "     status $got, wanted $want"; cat "$tmp/out"; return 1; }
}

run 0 FAKE_CARGO=ok && [ ! -e "$tmp/work" ] && grep -q "^removed: $tmp/work" "$tmp/out" && ok "identical builds: the folder is removed" || bad "identical builds: the folder is removed"
run 0 FAKE_CARGO=ok PANPAK_REPRO_KEEP=1 && [ -d "$tmp/work/a/target" ] && grep -q "^kept: $tmp/work" "$tmp/out" && ok "PANPAK_REPRO_KEEP=1 keeps the folder" || bad "PANPAK_REPRO_KEEP=1 keeps the folder"
# The folder exists now: the script must refuse it and leave it as it is, whatever it holds.
echo precious >"$tmp/work/mine.txt"
run 1 FAKE_CARGO=ok && [ "$(cat "$tmp/work/mine.txt")" = precious ] && [ -d "$tmp/work/a/target" ] && grep -q "already exists" "$tmp/out" && ok "an existing folder is refused and left alone" || bad "an existing folder is refused and left alone"
# Nothing removes it but its owner; the next cases start clean.
rm -rf "$tmp/work"
# `differ` writes the path into the binary, so the two builds differ.
run 1 FAKE_CARGO=differ && [ ! -e "$tmp/work" ] && grep -q "^DIFFERENT" "$tmp/out" && grep -q "PANPAK_REPRO_KEEP=1" "$tmp/out" && ok "different builds: the folder is removed, the message says how to keep it" || bad "different builds: the folder is removed, the message says how to keep it"
run 1 FAKE_CARGO=fail && [ ! -e "$tmp/work" ] && grep -q "^BUILD FAILED" "$tmp/out" && grep -q "fake failure" "$tmp/out" && ok "a failed build: the folder is removed, the end of the log is shown" || bad "a failed build: the folder is removed, the end of the log is shown"
# The default folder, when one already exists: a new name is used, and the old one is not touched.
mkdir -p "$tmp/cache/panpak-repro"
echo precious >"$tmp/cache/panpak-repro/mine.txt"
set +e
env -u PANPAK_REPRO_DIR HOME="$tmp/home" XDG_CACHE_HOME="$tmp/cache" CARGO_HOME="$tmp/cargo" RUSTUP_HOME="$tmp/rustup" PANPAK_ZIG="$tmp/zig" SOURCE_DATE_EPOCH=1000 FAKE_CARGO=ok \
  sh "$here/scripts/repro-build.sh" >"$tmp/out" 2>&1
got=$?
set -e
[ "$got" = 0 ] && [ "$(cat "$tmp/cache/panpak-repro/mine.txt")" = precious ] && [ "$(ls "$tmp/cache" | wc -l)" = 1 ] && ok "the default folder that existed is not touched, the new one is removed" || { bad "the default folder that existed is not touched, the new one is removed"; cat "$tmp/out"; }
# Interrupted while a build runs: the script is stopped with TERM, the build (the fake cargo, by its pid) ends, and the folder is gone.
for sig in TERM INT HUP; do
  rm -f "$tmp/pid" "$tmp/stop"
  env HOME="$tmp/home" XDG_CACHE_HOME="$tmp/cache" CARGO_HOME="$tmp/cargo" RUSTUP_HOME="$tmp/rustup" PANPAK_ZIG="$tmp/zig" SOURCE_DATE_EPOCH=1000 \
    PANPAK_REPRO_DIR="$tmp/work" FAKE_CARGO=wait FAKE_CARGO_PIDFILE="$tmp/pid" FAKE_CARGO_STOP="$tmp/stop" sh "$here/scripts/repro-build.sh" >"$tmp/out" 2>&1 &
  script=$!
  for _ in $(seq 1 100); do [ -s "$tmp/pid" ] && break; sleep 0.1; done
  [ -s "$tmp/pid" ] || { bad "interrupted ($sig): the fake build did not start"; kill "$script" 2>/dev/null || true; continue; }
  kill -"$sig" "$script"
  touch "$tmp/stop"
  set +e
  wait "$script"
  set -e
  [ ! -e "$tmp/work" ] && ok "interrupted ($sig): the folder is removed" || bad "interrupted ($sig): the folder is removed"
done
exit $fail
