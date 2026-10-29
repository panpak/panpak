# Changelog

## Unreleased

- **Private folders for each app.** The stub gives every app its own
  config, data, state, tmp and cache folders (mode 0700, outside the store) and
  points `XDG_*`, `TMPDIR` and `SQLITE_TMPDIR` at them. Writes go only there,
  to `/tmp` and to the `home_paths` of the manifest. The app can read the
  user's theme, font and icon settings and nothing else of the real config.
  `~/.local/share/<id>` is no longer used and is not migrated: move the files
  by hand with `panpak data path <id>`. New commands `panpak data path|clear`.
- **Manifest: `[launch]` and `permissions.home_paths`.** Extra start arguments
  and environment (with placeholders for the private folders), and fixed home
  paths as a last resort. `install` shows the permissions; `update` asks before
  it adds any. A package that asks for the D-Bus socket gets a warning.
- **Local sockets.** The stub removes `SSH_AUTH_SOCK`, `GPG_AGENT_INFO` and
  `SSH_AGENT_PID` and sets `DBUS_SESSION_BUS_ADDRESS` and `DBUS_SYSTEM_BUS_ADDRESS` to `disabled:` (each unless the
  manifest lists that bus). `doctor` shows whether the kernel can isolate
  an app from local sockets, and `install` and `update` warn when it cannot.
  `install` over an installed app asks before it adds permissions.
  `--yes` no longer accepts new permissions: use a terminal answer or the new
  `--accept-new-permissions`. `gsettings` is looked for only in `/usr/bin` and
  `/bin`.
- **Socket probe** (`PANPAK_SOCKET_PROBE=<log>`), a measurement mode, never on by default:
  the stub supervises the app and logs its socket and namespace calls. It blocks nothing.
  `DBUS_SYSTEM_BUS_ADDRESS` is hidden like the session bus. See docs/SOCKET-PROBE.md.
- **Isolated view (bubblewrap).** If `/usr/bin/bwrap` works, the app runs in a view with `/` read-only,
  an empty home folder (only the app folders, `home_paths`, the readable settings, the tree and the stub
  come back), a private `/tmp` (`TMPDIR=/tmp`, which also fixes the long-path crash of Simplenote), a `/run`
  and a runtime folder that hold only the allowed sockets (Wayland, journal, resolver, the `sockets_allow` list),
  its own process list and, without X11, its own IPC namespace. Inside it the session and system buses,
  systemd of the user, gpg-agent and ssh-agent do not exist. Landlock is applied on top. Without bubblewrap
  the start is as before. New manifest key `permissions.audio` (PipeWire and PulseAudio sockets).
  `socket_isolation` now reports the active layers; `doctor`, `install` and `update` show them, and `doctor`
  says why the view is unavailable and how to fix it. `sockets_allow` also works on kernels below Landlock V9
  when the view is available. Landlock V9 gets `ResolveUnix` rules for the same sockets (not tested: this
  machine has V8).
- **Libraries: the bundle first.** When a library is in the package, the package copy is used, except for
  glibc and the graphics stack (which must match the system and the driver) and for `libstdc++` and
  `libgcc_s` (the copy with every symbol version the app needs). A library that is not in the package comes
  from the system, as before. This fixes `symbol lookup error` with a system library of the same name and a
  different API (Audacity and `libSoundTouch.so.1`). `panpak doctor` checks, for an installed app, that the
  symbols it needs are defined by the chosen libraries. The format does not change. See PANPAK-FORMAT.md §7.
- **Libraries: families and a symbol check.** GLib, GTK (with GDK, Pango, Cairo and the like), GStreamer and FFmpeg move
  whole, as Qt already did: all from the package or all from the system. After the rules, the runtime looks for symbols
  that no chosen library defines and, if it finds some, takes the library (with its family) from its other copy, until
  nothing is missing or nothing can change (at most 8 steps). If some remain, the app still starts, with a warning, and
  `panpak doctor` lists them. This fixes QOwnNotes, whose bundled GLib was older than the system libraries it also loads.
- **Resolution cache and speed.** The cache of the library resolution is now keyed by the package root and its `[abi]`
  fields, the stub, the kernel, `/etc/ld.so.cache` and the identity of every system library the decision looked at (also
  the ones not chosen) and of the loader, so an update of `libglib` or `libcrypto` makes the next start resolve again.
  A hit no longer hashes the package or runs `ldconfig`. The ELF files are read once each with `pread`, only the sections
  that matter, on several threads: the resolution of Kdenlive (449 libraries) takes about 40-140 ms instead of 1.2-1.4 s.
- **GTK3 dark theme** from `gsettings` at start (at most 200 ms).
- **Security: paths and text from the manifest.** `app.id` has one strict
  grammar (spec §3.1.1) and every manifest field that becomes a path, a file
  name, a `.desktop` line or terminal text is checked in `verify`, so a signed
  package cannot make `panpak` or the stub write outside the store (new codes
  `PANPAK-E0006`, `PANPAK-E0007`). `rollback` and `open` check the id they are
  given. Index paths and symlink targets reject control characters.
- **Security: launchers and `.desktop` files.** Written through a temporary
  file and a rename, never through a symbolic link, and only over a file that
  panpak wrote for the same app (`PANPAK-E2001`). Values are escaped as the
  Desktop Entry Specification requires and the `Exec` path is quoted.
  `panpak convert` escapes the strings it copies from an AppImage.
- **Test and bench scripts** refuse to run when HOME is not inside their work
  directory and never delete outside it; `bench/campaign.py` no longer copies
  into `~/Applications`.

## 0.1.0

First version. Format v1, runtime ABI 1.

- **Package format**: `.panpak` file (`.pan` accepted as an alias; a file is
  identified by its magic and signature, never by its extension) with a static
  executable stub, deduplicated zstd chunks, a signed index, a signed manifest
  and an 80-byte footer. Specification in `PANPAK-FORMAT.md`.
- **Verification before execution**: Ed25519 signature over manifest and root
  hash, BLAKE3 hash of the stub bound to the signature, per-chunk BLAKE3
  check before writing. Typed `PANPAK-Exxxx` error codes.
- **Sandbox**: Landlock confinement with no silent fallback, read-only
  trees, private SQLite temporary directory, optional network and device
  allowlists.
- **Trust model**: key shown at install and pinned (TOFU), signed key
  rotation, fail-closed ABI check of the host libraries.
- **CLI** (`panpak`): `keygen`, `pack`, `convert` (AppImage), `install`,
  `install-runtime`, `update`, `rollback`, `open`, `doctor`, `licenses`,
  `release`, `dump-index`, `dump-manifest`.
- **Updates**: incremental updates over HTTPS with range requests, reusing
  the chunks already present.
- **Desktop integration**: launchers and `.desktop` entries, the MIME type
  `application/x-panpak` for the packages (`panpak desktop-integration
  install|remove`); explicit file open for double click.
- **Quality**: conformance suite with an independent Python verifier, fuzz
  targets for every parser (corpus seeds generated by `fuzz/gen-seeds.py`),
  reproducible builds (`scripts/repro-build.sh`, `Dockerfile.repro`),
  SPDX headers, cargo-deny and gitleaks in CI.
