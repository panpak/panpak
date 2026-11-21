#!/bin/bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# One series of PROCEDURE.md section 4, without the reboots, run on the machine in the environment of the graphical session.
# Usage: run_series.sh flatpak <folder name of the results> <order: gimp,kdenlive,onlyoffice,qb>
#        run_series.sh panpak  <folder name of the results> <order> <base url of the packages> <converter key ed25519:...> <ca.pem>
set -u
S=$1; NAME=$2; ORDER=$3; BASE=${4:-}; KEY=${5:-}; CA=${6:-}
declare -A ID=([gimp]=org.gimp.GIMP [kdenlive]=org.kde.kdenlive [onlyoffice]=org.onlyoffice.desktopeditors [qb]=org.qbittorrent.qBittorrent)
export XDG_RUNTIME_DIR=/run/user/$(id -u) DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus
export PATH=$HOME/.local/bin:$PATH BENCH_RESULTS=$HOME/panpak-bench-results/$NAME
cd "$(dirname "$0")"
B=./silverblue_bench.py
if [ "$S" = panpak ]; then
  [ -n "$BASE" ] && [ -n "$KEY" ] && [ -n "$CA" ] || { echo "panpak needs the base url, the key and the authority"; exit 2; }
  echo "== prerequisites $(date +%T)"; $B prerequisites --system panpak --trials 5 || echo "prerequisites FAILED"
  COMMON=(--pkg-base-url "$BASE" --keys "*=$KEY" --ca "$CA")
else
  COMMON=()
fi
echo "== scenario $(date +%T)"; $B scenario --system $S --order $ORDER --warm 15 "${COMMON[@]}" || echo "scenario FAILED"
for a in ${ORDER//,/ }; do
  echo "== cycle $a $(date +%T)"
  if [ "$S" = panpak ]; then $B cycle --system $S --app $a --trials 5 --pkg-url "$BASE/${ID[$a]}.panpak" --keys "*=$KEY" --ca "$CA" || echo "cycle $a FAILED"
  else $B cycle --system $S --app $a --trials 5 || echo "cycle $a FAILED"; fi
done
for a in kdenlive onlyoffice; do echo "== update $a $(date +%T)"; $B update --system $S --app $a --trials 3 "${COMMON[@]}" || echo "update $a FAILED"; done
echo "== done $(date +%T)"
