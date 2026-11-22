#!/bin/bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# The measures of one app again, after the way it is stopped changed: the reinstalls and first starts, the later starts and the update (the reboots are made with `reboot-series`).
# Usage: run_app.sh flatpak <folder name of the results> <app: gimp|kdenlive|onlyoffice|qb>
#        run_app.sh panpak  <folder name of the results> <app> <base url of the packages> <converter key ed25519:...> <ca.pem>
set -u
S=$1; NAME=$2; A=$3; BASE=${4:-}; KEY=${5:-}; CA=${6:-}
declare -A ID=([gimp]=org.gimp.GIMP [kdenlive]=org.kde.kdenlive [onlyoffice]=org.onlyoffice.desktopeditors [qb]=org.qbittorrent.qBittorrent)
export XDG_RUNTIME_DIR=/run/user/$(id -u) DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus
export PATH=$HOME/.local/bin:$PATH BENCH_RESULTS=$HOME/panpak-bench-results/$NAME
cd "$(dirname "$0")"
B=./silverblue_bench.py
if [ "$S" = panpak ]; then
  PK=(--pkg-url "$BASE/${ID[$A]}.panpak" --keys "*=$KEY" --ca "$CA"); PB=(--pkg-base-url "$BASE" --keys "*=$KEY" --ca "$CA")
else
  PK=(); PB=()
fi
echo "== cycle $A $(date +%T)"; $B cycle --system $S --app $A --trials 3 "${PK[@]}" || { echo "cycle $A FAILED"; exit 1; }
echo "== warm $A $(date +%T)"; $B start --system $S --app $A --trials 10 --kind warm || { echo "warm $A FAILED"; exit 1; }
if [ "$A" = kdenlive ] || [ "$A" = onlyoffice ]; then echo "== update $A $(date +%T)"; $B update --system $S --app $A --trials 3 "${PB[@]}" || { echo "update $A FAILED"; exit 1; }; fi
echo "== done $(date +%T)"
