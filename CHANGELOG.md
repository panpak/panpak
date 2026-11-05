# Changelog

## Unreleased

- **A filtered session bus for apps in the isolated view.** With `xdg-dbus-proxy` (0.1.5 or newer), a systemd user
  manager and the view, an app gets a session bus that reaches only: the appearance and inhibit portals, its
  own `app.id` (single instance across copies), and, if the manifest asks, notifications, a tray icon and up to 8
  declared names (`[permissions] notifications`, `tray`, `dbus_own`). Every other interface and name is refused.
  The outer stub moves itself into a systemd scope `app-panpak.<id>-<n>.scope` so that the portal knows the app as
  `panpak.<id>`, and the folder `doc/by-app/panpak.<id>` of the document portal is in the view. Without the parts the
  app has no bus, as before; `panpak doctor` reports it. See PANPAK-FORMAT.md §3.1.5 and §9.5, THREAT-MODEL.md §3 and KL-5, KL-23 to KL-27.
- **The desktop entry is `panpak.<id>.desktop`** (it was `<id>.desktop`) and carries `StartupWMClass` when the manifest has
  `[desktop] wm_class`. `install` and `update` remove the entry of the old name when panpak wrote it. After an update,
  the launcher of an app is found under the new name in menus and the dock.
- **Quiet start.** The lines about `allow_missing`, libraries taken from another copy and undefined symbols are no longer printed
  at every start; they appear with `PANPAK_DEBUG` set. `install` lists the excluded libraries once and `doctor <package>` checks the symbols.
- `install-runtime` replaces the runtime by a rename (open apps are not disturbed) and takes the flags before or after the file.
- In the isolated view, writable folders get the Landlock `Refer` right, so a rename between folders works (Qt local sockets).
- New command `panpak dbus-rules <package>` prints the arguments the stub gives the proxy.
- A small D-Bus client for the outer stub in `panpak-core` (no new dependencies), with the fuzz targets `fuzz_dbus` and `fuzz_dbus_names`.
- **No network means no network in the isolated view.** An app whose manifest asks for no network (`network` not
  true, no TCP port) runs in a network namespace of its own with only the loopback interface, so it can no longer
  send UDP datagrams or look names up; before, Landlock stopped TCP only and UDP went out. The resolver socket
  of systemd is in the view only for apps that ask for network. Without bubblewrap nothing changes: UDP is still
  open (see THREAT-MODEL.md).
- **Fix: Landlock V9 rules were dropped on the musl build.** The rules for the sockets every app may reach
  (journal, Wayland, resolver, audio) and for `sockets_allow` were left out without a message, because
  `landlock::PathFd` opens files with the standard library, which drops `O_PATH` on musl. On a V9 kernel the
  journal socket was refused. Every rule now opens its path with `O_PATH`; a rule left out for any reason
  other than a missing path prints a warning, the fallbacks for `/dev` say why they were used, and
  `panpak doctor` lists rule paths that cannot be opened.
- **Fix: sockets outside `/run`, `/tmp` and the home folder.** The root of the isolated view is a new tmpfs with
  only `/usr`, `/etc`, `/sys`, `/dev`, `/proc`, the Nix and Guix stores and the top-level links, so a socket
  in `/var`, `/opt`, `/srv` or the folder of a container manager is not in the view.
- **Fix: stale lock files in the view.** The app is always process 2 in its own PID namespace, so lock files
  with a process id (Chromium `SingletonLock`, Qt `QLockFile`, Firefox `lock`) never expired. The runtime now
  removes the ones it recognises by content, in the app's private folders only, when no copy is alive and the
  mark `userdata/<id>.in-use` shows that the last run did not end normally. New command
  `panpak data unlock <id>`.
- **Fix: `panpak convert`.** The manifest is validated with the rules of `install` before it is signed;
  a `DT_NEEDED` entry with a path is not a library name; the string table limit follows the file instead of a
  fixed 1 MiB; the loader symbols are no longer reported undefined when `ldconfig` is not in `PATH`.
- **Fix: processes started after a fork.** Between `fork` and `exec` the opener and the socket probe call
  only async-signal-safe functions, so a start made while other threads allocate cannot hang.
- **Documentation.** `SECURITY.md` uses GitHub's private vulnerability reporting; `THREAT-MODEL.md` has one
  list of known limitations; `docs/BENCHMARKS.md` and the scripts in `bench/` give reproducible measurements.

- **Fix: first starts at the same time.** Two starts that overlapped on a store without the tree could end with
  `PANPAK-E2000: No such file or directory`, because they built in the same `.partial` folder. The tree is now
  built under an `flock`, in a folder apart, and published with one rename; the others wait and then use it.
  The problem was in the first version of the stub, so it exists in every earlier release.

- **User folders: `[[permissions.xdg_dirs]]`.** An app can ask for the music, documents,
  pictures, videos or download folder of the user, `ro` or `rw`. `install` and `update` find the
  folder in `~/.config/user-dirs.dirs` with a strict parser, refuse unsafe places, show it
  (`music: ~/Music (rw)`) and pin it; at start the folder is granted only if it still matches. The app gets a
  private `user-dirs.dirs`. In the isolated view the home folder can be listed.
- **Opening links from the isolated view.** `xdg-open` (and GIO, Qt and Electron through it) in the view
  asks the outer stub over a private socket, which passes on `http`, `https` and `mailto` addresses only
  (with only the plain `mailto` fields), at most 5 per second. The stub now stays running while the app runs.
- **URL schemes: `[desktop] schemes`.** The `.desktop` file declares `x-scheme-handler/<scheme>` and runs the
  launcher with `%U`.
- **Copies of one app** share a `/tmp` and a `/dev/shm` of their own in the isolated view; a start with no
  copy alive empties them. Two copies still do not see each other's System V IPC without `x11`.

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
  when the view is available. Landlock V9 gets `ResolveUnix` rules for the same sockets (tested on Fedora 44,
  kernel 7.2.7; see the fix below).
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
  Panpak wrote for the same app (`PANPAK-E2001`). Values are escaped as the
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
