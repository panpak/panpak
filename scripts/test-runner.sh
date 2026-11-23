#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# The `runner` of cargo for the test binaries (.cargo/config.toml): a test starts without the desktop of the person at the keyboard. No display, no Wayland socket,
# a session bus address that leads nowhere and a runtime folder of its own, so a window, a notification or a clipboard write of a test cannot reach that desktop, even by
# mistake. The runner of the CI (tests/with_timeout.py) puts a canary bus in the environment; then it is left as it is, so the canary sees what connects.
# Only test binaries (the ones in a `deps` folder) are changed: `cargo run` runs the program as it is.
case "$1" in
*/deps/*)
	if [ "${PANPAK_CANARY_BUS:-}" != 1 ]; then
		rt=$(mktemp -d "${TMPDIR:-/tmp}/panpak-test-run.XXXXXX") || exit 1
		chmod 700 "$rt"
		env -u DISPLAY -u WAYLAND_DISPLAY -u WAYLAND_SOCKET -u XAUTHORITY \
			DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent/panpak-test-no-bus XDG_RUNTIME_DIR="$rt" "$@"
		status=$?
		rmdir "$rt" 2>/dev/null
		exit $status
	fi
	exec env -u DISPLAY -u WAYLAND_DISPLAY -u WAYLAND_SOCKET -u XAUTHORITY "$@"
	;;
esac
exec "$@"
