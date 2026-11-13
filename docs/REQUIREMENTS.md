<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Requirements

What a system needs to run Panpak apps, what each part gives, and what happens without it. `panpak doctor` checks a
system and says which part is missing and how to fix it; `panpak doctor <package>` checks one package against the system.
The tests and the distributions behind these statements are in [COMPATIBILITY.md](COMPATIBILITY.md) and
[../distro-matrix.md](../distro-matrix.md); the sandbox rules in [../THREAT-MODEL.md](../THREAT-MODEL.md).

## To run apps

- The runtime (`panpak-stub`) comes with `panpak`: the release tarball and `target/<triple>/release` hold both files. `panpak build` and `panpak install` put the one
  beside the running `panpak` in `~/.local/share/panpak/runtimes/` when there is none or the installed one has an older ABI, and say so in one line. Nothing is
  installed as root.

- **Architecture:** x86-64 Linux. The runtime is a static musl binary built for `x86_64-unknown-linux-musl`; other
  architectures are not supported yet.
- **Kernel, with Landlock enabled** (`landlock` among the active security modules). Linux 5.13 is the minimum (Landlock 1,
  file rules): below it the runtime refuses to start an app. Later versions add rules, and the runtime uses what the kernel has:
  - Linux 6.7 (Landlock 4): TCP port rules. Without them an app can only be started with `--network-unfiltered`, and an app that
    asks for network is refused.
  - Linux 6.12 (Landlock 6): scopes that confine abstract Unix sockets and signals.
  - Linux 7.1 (Landlock 9): rules for Unix sockets by path.

  The table with the effect of each version is in [../THREAT-MODEL.md](../THREAT-MODEL.md).
- **glibc:** an app uses the glibc of the system unless its package carries its own. A package built from source with the current build
  image (`debian-13`) needs glibc 2.39 or later: `pack` writes the highest `GLIBC_x.y` the files ask for into the manifest as `glibc_min`, and
  `install` and `update` refuse a package that needs a newer glibc than the system has, with both versions. An AppImage converted
  with `panpak convert` needs what the AppImage was built against (Cura needs 2.38 in the compatibility tests). Hosts with another libc
  (Alpine) run only packages that carry glibc and its loader, or static programs.
- **bubblewrap** (`/usr/bin/bwrap`, optional) with unprivileged user namespaces. It provides the isolated view. Without it the app runs with
  Landlock alone: below Landlock 9 it can reach the session bus and the agents through their sockets, it sees the real `/proc`, `/sys`, `/run` and
  `/tmp`, it has no process and IPC namespaces of its own, links it opens go to the system `xdg-open` from inside the sandbox, and an app with no
  network permission can still send UDP. On Ubuntu 24.04 and later an AppArmor profile for `bwrap` is needed (the distribution ships one).
- **The private D-Bus bus of each app** (step 13p) needs all of these, and `doctor` says which one is missing:
  - the isolated view (bubblewrap);
  - `xdg-dbus-proxy` 0.1.5 or newer at `/usr/bin/xdg-dbus-proxy` or `/run/current-system/sw/bin/xdg-dbus-proxy` (a dependency of Flatpak, so
    usually present where Flatpak is);
  - a systemd user manager reachable on `$XDG_RUNTIME_DIR/bus`, which gives the app its own scope (the identity the portals read), and the
    session bus socket.

  The router that serves the bus is a process of the runtime itself: there is nothing more to install, and it needs no privilege. For the portals to
  know the app as `panpak.<id>`, `xdg-desktop-portal` must be there and the menu entry `panpak.<id>.desktop` that `install` writes must exist (portal
  1.21 and later check it). Without the bus the app has no session bus: no portals, notifications, tray icon or media keys, and its single-instance
  name is not shared between copies. Accessibility works when the host has the AT-SPI bus; if not, the app is not visible to screen readers.
- **systemd** is therefore needed for the bus. Without the bus, the runtime does not need systemd: if it exists, the journal socket and the
  systemd-resolved socket are made available to the app (the resolver only if the app asks for network), and `XDG_RUNTIME_DIR` is used for the sockets
  and the shared `/tmp` of an app's copies.
- **Also used:** `xdg-open` to open links (the app is told if it fails; on a system whose only browser is a Flatpak, `XDG_DATA_DIRS` must
  list the Flatpak export folders, which the runtime adds when the variable is unset), a Wayland or X11 session for graphical apps, and for `convert` an AppImage
  with a SquashFS image (DwarFS images are not supported).

## To build Panpak

- `build.sh` (run as ./build.sh) does everything below and says what is missing. It needs `rustup` (the Rust toolchain of `rust-toolchain.toml`, 1.99.1, and the target
  `x86_64-unknown-linux-musl` are installed by it), `python3` with `venv` (Debian and Ubuntu: `python3-venv`) and network access (PyPI for Zig,
  `static.rust-lang.org` for the toolchain, crates.io). It prepares Zig 0.17.0, the C compiler and linker, from the pinned wheel in `target/zig-venv`
  (`pip install --require-hashes -r ../tools/requirements-zig.txt`). `scripts/repro-build.sh` builds twice and compares the bytes; the
  reference hashes are in [../REPRODUCIBLE-HASHES.md](../REPRODUCIBLE-HASHES.md).

## To build an app from source (`panpak build`)

- bubblewrap with user namespaces (the build runs in a sandbox with no network), network access to the registry and to the Debian snapshot service
  for the first build of an image, and the memory the recipe declares (`build.memory`; the build checks it and runs in a systemd scope with a cap when
  it can). No root, no Docker, no daemon.
- The build image `debian-13` is made from the pinned base image and the dated package snapshot of `panpak.lock`. GTK 4 in it is rebuilt once per
  image version with a one-line patch (defect 54, [PRIVATE-BUS.md](PRIVATE-BUS.md)); the patch is recorded in the lock and in the manifest.
  See [SOURCE-BUILD.md](SOURCE-BUILD.md).

## To run the tests

- The tests that use bubblewrap, `xdg-dbus-proxy`, a systemd user manager, `xdg-desktop-portal` and `python3-gi` skip with a message when one is
  missing. The app harness also needs `xwininfo` and `Xvfb` for X11 apps and `sway` 1.9 or newer for Wayland apps ([APP-HARNESS.md](APP-HARNESS.md)).
