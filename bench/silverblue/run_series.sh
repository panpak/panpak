#!/bin/bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# One series of PROCEDURE.md section 4, without the reboots, run on the machine in the graphical session's environment.
# Usage: run_series.sh <system: flatpak|panpak> <folder name of the results> <order: gimp,kdenlive,onlyoffice,qb> [extra arguments of scenario, cycle and update (panpak: --pkg-base-url, --keys, --ca)]
set -u
S=$1; NAME=$2; ORDER=$3; shift 3
export XDG_RUNTIME_DIR=/run/user/$(id -u) DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME WAYLAND_DISPLAY=wayland-0
export PATH=$HOME/.local/bin:$PATH BENCH_RESULTS=$HOME/panpak-bench-results/$NAME
cd "$(dirname "$0")"
B=./silverblue_bench.py
echo "== scenario $(date +%T)"; $B scenario --system $S --order $ORDER --warm 15 "$@" || echo "scenario FAILED"
for a in ${ORDER//,/ }; do
  echo "== cycle $a $(date +%T)"
  if [ "$S" = panpak ]; then $B cycle --system $S --app $a --trials 5 "${CYCLE_ARGS[@]}" || echo "cycle $a FAILED"; else $B cycle --system $S --app $a --trials 5 || echo "cycle $a FAILED"; fi
done
for a in kdenlive onlyoffice; do echo "== update $a $(date +%T)"; $B update --system $S --app $a --trials 3 "$@" || echo "update $a FAILED"; done
echo "== done $(date +%T)"
