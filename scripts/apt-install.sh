#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Install packages on a CI runner with a time limit and a second mirror.
#
# Usage: scripts/apt-install.sh [--no-update] <package>...
#
# The package mirror of the runner image (azure.archive.ubuntu.com) is the largest variable of the CI: the same step took 23 s in most runs and 518 to 716 s in
# others. The first attempt has APT_LIMIT seconds (180 by default); when it does not finish or fails, the same mirror is replaced by archive.ubuntu.com and the
# second attempt has the same limit. A slow mirror that works must not turn into a failure (both mirrors were slow on the same day, and a step that used to end in
# 400 s failed): the third attempt, on the second mirror, has APT_LAST_LIMIT seconds (900 by default), and the downloads of the attempts before are kept by apt and
# go on from where they stopped. Nothing else changes: the packages are the same, from the same suite, checked by apt against the same signing keys.
# Test hooks: APT_GET (the command, `sudo apt-get` by default), APT_SOURCES (the files that name the mirror), APT_SUDO (`sudo` by default).
set -u
limit="${APT_LIMIT:-180}"
last_limit="${APT_LAST_LIMIT:-900}"
apt_get="${APT_GET:-sudo apt-get}"
sudo_cmd="${APT_SUDO-sudo}"
sources="${APT_SOURCES:-/etc/apt/sources.list /etc/apt/sources.list.d/ubuntu.sources}"
update=1
if [ "${1:-}" = "--no-update" ]; then update=0; shift; fi
[ "$#" -gt 0 ] || { echo "usage: apt-install.sh [--no-update] <package>..." >&2; exit 2; }

attempt() {
  # Only the part that waits for the network has a limit: the update and the download. Stopping `apt-get install` while dpkg works on the files leaves dpkg running with
  # its lock, and every later attempt then fails with "Could not get lock /var/lib/dpkg/lock" (the CI of main failed that way on 2026-11-23: the install was at its last
  # triggers when the 180 s ended). The installation itself works on files that are on the disk and is not stopped; it waits for the lock of dpkg for a while.
  timeout --kill-after=10 "${LIMIT:-$limit}" bash -c '
    set -e
    upd=$1; apt=$2; shift 2
    if [ "$upd" = 1 ]; then $apt update -qq; fi
    $apt install -y -qq --download-only "$@"
  ' _ "$update" "$apt_get" "$@" || return $?
  $apt_get -o DPkg::Lock::Timeout=300 install -y -qq "$@"
}

attempt "$@" && exit 0
status=$?
echo "apt-install: the first attempt did not finish in ${limit} s or failed (status $status); trying archive.ubuntu.com" >&2
for f in $sources; do
  [ -f "$f" ] && $sudo_cmd sed -i 's#//[a-z0-9.-]*archive\.ubuntu\.com#//archive.ubuntu.com#g' "$f"
done
$sudo_cmd dpkg --configure -a >/dev/null 2>&1 || true
# The lists were made from the first mirror: the next attempts always update.
update=1
attempt "$@" && exit 0
status=$?
echo "apt-install: the second attempt did not finish in ${limit} s or failed (status $status); one more, with ${last_limit} s" >&2
$sudo_cmd dpkg --configure -a >/dev/null 2>&1 || true
LIMIT="$last_limit" attempt "$@" && exit 0
status=$?
echo "apt-install: the last attempt did not finish in ${last_limit} s or failed (status $status): the packages could not be installed" >&2
exit 1
