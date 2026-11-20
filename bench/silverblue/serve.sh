#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Serve the two packages to the virtual machine: serve.sh <folder with org.gnome.TextEditor.panpak and org.qbittorrent.qBittorrent.panpak> [port]
set -eu
dir=${1:?folder}
port=${2:-8000}
echo "serving $dir on port $port (the machine reaches it at http://<address of this host>:$port)"
exec python3 -m http.server "$port" --directory "$dir" --bind 0.0.0.0
