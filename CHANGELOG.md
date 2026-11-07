# Changelog

## Unreleased

- **Icon caches.** `panpak build` makes `icon-theme.cache` for every icon theme folder of the package (`gtk-update-icon-cache`), and `pack` warns about a theme folder with icons and no cache. Qt looked each icon up file by file without it: qBittorrent from source made 49,000 failed lookups at the start (0.415 s; now 0.21 s).
- **One time for every entry of a tree in the store** (2000-01-01T00:00:00Z): the store made each entry with the time of its creation, which made a cache file older than its folder, and Qt ignores such a cache (PANPAK-FORMAT section 6).
- **Downloads choose the address like Happy Eyeballs** (IPv6 and IPv4 in turn, 250 ms apart, 4 s per address) for `build` and `update`; an unreachable IPv6 address no longer stalls a download.

- **`panpak init` and `panpak build`: packages from source.** A recipe (`panpak.toml` with `[source]`, `[build]`, `[collect]`) and `panpak.lock` (the build image: its digest and the date of the package snapshot,
  changed only by `panpak build --update-lock`). `build` makes the image (a Debian root file system unpacked from the registry by digest, plus the recipe's packages from the snapshot) without root, fetches the source at a pinned commit or hash,
  builds in bubblewrap with no network, collects the libraries (read from the ELF files, never with `ldd`), the plugins of GTK 4 or Qt 6 and the data, makes the icons, writes the manifest and packs. A recipe states the memory it needs
  (`build.memory`, `jobs`): the build checks the memory that is available, lowers the number of jobs if it must, runs in a systemd scope with a memory cap, and measures the peak. A linked Qt module whose plugin is missing stops the build.
  Dependencies taken from the image are declared (`[[build.image_deps]]`) and shown at install. See docs/SOURCE-BUILD.md.
- **`[env]` in the manifest** (variables that point the app at its own tree, from a fixed list, relative to `${PANPAK_ROOT}`) replaces the shell script as the entry; `pack` warns about a script entry. New `[provenance]` table.
- **Faster start.** The stub is hashed in pieces of 64 KiB (1.4 ms instead of 5 ms), and the answer of `gsettings` is collected after the Wayland, D-Bus and shared-folder work. With the pty always available the fixed cost is 25.3 ms (24.6 ms before).

- **Apps can open a pty (terminals, IDEs).** The isolated view now has a devpts of its own and a minimal `/dev` of bubblewrap (null, zero, full, random, urandom, tty,
  the GPU folder `dri`) instead of the host's `/dev`; Landlock grants `ptmx` and `pts` read, write and the device ioctls. Before, `open("/dev/ptmx")` failed with `EACCES` and
  no terminal emulator could start a shell. The ptys an app sees are its own, never the user's terminals; `TIOCSTI` stays refused. No permission is needed. `tests/pty.py`.
- **The Wayland client libraries come from the host.** `libwayland-client`, `libwayland-egl` and `libwayland-cursor` join the graphics stack that the system always wins (a bundled copy next to the host's
  Mesa made EGL fail: Ghostty built from source had no OpenGL context).
- **`pack` writes `glibc_min`** (the highest `GLIBC_x.y` the files ask for) and `install` and `update` refuse a package that needs a newer glibc than the system has, with both versions (`PANPAK-E2005`).
- **`pack` warns about a Qt tree without the plugins Qt loads at run time:** the `xdgdesktopportal` platform theme (dark mode, portal dialogs) and, with `libQt6Svg`, the SVG image format and icon engine plugins (tray icons).

- **A lock file left by an app that stops on a signal is removed when the last copy ends.** qBittorrent (a Qt `QLockFile`) stopped with `SIGTERM` left its `lockfile`, and the next start did not
  start because process ids start again at 1 in the view. The runtime that stays now searches for the stale lock files of the private folders when the last copy ends, before it removes the mark "in use" (cost at
  exit, 0.4 to 3.7 ms on the 15 test apps; none at start). A copy that is not the last touches nothing. `tests/stale_lock_exit.py`.

- **Desktop integration (step 7b).** Apps get their icon in the menu: `[desktop] icons` lists up to 6 PNG files of the package; `install`
  and `update` check them (PNG only, a short list of chunks, 16 to 1024 pixels, at most 256 KiB, image data of exactly the promised size;
  SVG is not accepted) and write them as `~/.local/share/icons/hicolor/<S>x<S>/apps/panpak.<id>.png`; `remove` deletes them. `panpak
  convert` takes the icons from the AppImage (`.DirIcon`, the root file, the `hicolor` sizes) and has `--icon <file.png>`; an app with no usable
  PNG gets the generic icon and a warning. New crate `panpak-icon`, fuzz target `fuzz_icon`.
- **Icons are cleaned by `pack` and `convert`.** Real PNG files carry text, profile, gamma and other chunks that `install` does not accept. `pack` and `convert` now
  keep only `IHDR`, `PLTE`, `tRNS`, `IDAT` and `IEND` (byte for byte; the pixels and the transparency are not touched), print the chunks they removed from each icon, and
  `install` stays strict. `pack` puts the cleaned icons in the package as generated entries `.panpak/icons/<side>.png` (the signed manifest points there); `convert` reads the icons
  (links followed only inside the tree) and hands them to `pack` in memory: it writes no icon file. `.panpak/` is the folder that only `pack` writes.
- **`convert` no longer writes through links of the AppImage.** The unpacked tree is a private `mkdtemp` folder, and every write to it (files, folders, links, the wrapper
  scripts and `panpak.toml`) goes through folder descriptors that do not follow links. Before, an AppImage with `bin -> /somewhere` or `panpak.toml -> /some/file` could make
  `convert` write outside its folder.
- **The user's `hicolor` icon theme is not in the view of the app.** The menu icons that `install` writes there made Qt apps start 60 to 110 ms later (no `icon-theme.cache`: 40,000 failed lookups). The app still reads the
  other theme folders of `~/.local/share/icons` and `~/.icons`.
- **A stable `panpak` and a stable stub.** The store has `bin/panpak-stub` (a link to the best runtime) and `bin/panpak`. Launchers run the stub
  through the link, so a new runtime reaches every app at its next start, and the menu action "Uninstall" runs `bin/panpak`: no launcher or
  entry names the folder from which `panpak` was run any more. `install-runtime` writes both with a rename; `install` copies `panpak` when there is none and warns when the one in
  the store is older. `panpak --version` prints the version.
- **The menu entry is thin and versioned.** The entry is made from a fixed template (the keys it may have are tested) and carries
  `X-Panpak-Desktop-Version`. When an app starts and its entry or launcher is older, the runtime rewrites both (temporary file, `fsync`, `rename`); when
  there is nothing to do it costs 0.04 ms.
- **`panpak remove` and `panpak list` say more.** `removed <id> <version>: freed <size>`; a second `remove` (or an unknown id) says "not installed:
  nothing to remove" with `PANPAK-E2004`; `list` of an empty store says how to install.
- **The menu entry and GNOME Shell.** Tested on GNOME Shell 50: a new or changed entry (and its icon) shows about 5 seconds after the file changes, with nothing but the
  `rename` panpak already does; an app that was already started from the Shell in the session keeps its old entry until the next login. `install` and `update` say so when
  they replace an entry with a different one.
- **The desktop's own scope is recognised for every launcher.** The shortcut that skips the scope of the app follows the portal's rule for scopes
  (`app-gnome-panpak.<id>-<n>.scope`, `app-panpak.<id>-<n>.scope`), with the systemd escape; the unit is the one `sd_pid_get_user_unit` finds, so a process in a sub-cgroup counts.
  A service (`app-...@<n>.service`) does not get the shortcut: a portal older than 1.21 may not read services, so the runtime makes its own scope, as from a terminal.
  `tests/dbus_filter.py` runs the three shapes against the real portal.
- **`install` and `update` warn about unknown fields.** A signed package that has manifest keys this panpak does not know is still installed (they are ignored), with a
  warning that says to update panpak (`panpak install-runtime`).
- **A key that this panpak does not know** in a `panpak.toml` given to `pack` or `convert` is still refused, now with `PANPAK-E0008` and a message that says it is a
  misspelling or a field of a newer format, and how to get a newer panpak (`panpak install-runtime`).

- **Hardening of the app process.** Every app now starts under a seccomp filter (a deny list, installed last, with
  `no_new_privs`; it returns errors, it does not kill): `io_uring`, `bpf`, `userfaultfd`, `perf_event_open`, the key ring,
  `ptrace` and `process_vm_*`, the mount calls, `setns`, new namespaces through `clone` and `unshare` (user namespaces too), `clone3` (`ENOSYS`),
  terminal input injection, odd `personality` values and the non-native ABIs are refused; the table with the reasons is
  PANPAK-FORMAT.md §9.8. New error code `PANPAK-E3002` (no seccomp). **Programs that need a user namespace of their own (Chromium and
  Electron sandboxes, WebKitGTK, `bwrap`, `flatpak`) cannot run in the view.** The device ioctl right (Landlock V5) is kept only on `/dev/tty` and `/dev/dri`.
  `/var/cache/fontconfig` is read-only in the view: the first start with an empty private font cache went from 446 ms to 48 ms with `fc-match`.
- **Wayland security context.** With a compositor that offers `wp_security_context_v1` (KWin, Sway and the other wlroots compositors),
  the app gets a socket of its own that the compositor knows as sandboxed (engine `io.github.panpak`, app id `panpak.<id>`). GNOME's compositor does not offer it:
  the app keeps the normal socket and `panpak doctor` says so. The client is part of `panpak-core` (no new dependencies) with the fuzz target `fuzz_wayland`.
- **The open-address channel is read by a receiver with no rights.** The resident stub is now two pieces (PANPAK-FORMAT.md §9.7): a receiver process
  (Landlock with no rule, a seccomp allow list, limits) reads what the app sends and passes validated requests as frames to the actor, which checks them again.
  Fuzz target `fuzz_receiver`.
- **The `audio` permission says that it includes the microphone** in the text of `install` and `update`; it was already so, and it is a known limit (KL-30).
- **Faster start.** The fixed cost of a start (measured against running the program directly) fell from 40 ms to 25 ms without the filtered session bus and from 56 ms
  to 38 ms with it (26 ms when the desktop started the app in a scope that already names it, as the GNOME menu does: then no scope is made): the `gsettings` queries, the systemd scope and the proxy overlap the work of the start, the inner stub waits for the proxy's socket, and the version of the proxy is remembered between starts. The app and the proxy are always in a scope that gives the app its identity.
  `PANPAK_DEBUG` prints where the milliseconds go. `tests/startup_budget.py` measures it and runs in the CI as an advisory step.

- **`panpak list` and `panpak remove`.** `list [--json]` shows the installed apps (id, name, version, disk space of the
  app and of its data, status, including incomplete installs). `remove [--purge-data] [--yes] <id>` takes away the launcher,
  the desktop entries panpak wrote, the packages, the trees and the shared `/tmp` of the app; the settings and data stay
  unless `--purge-data` is given and confirmed. It refuses while a copy is running (`PANPAK-E2002`), never follows a link,
  moves the app folder aside with one rename and is finished by the next command if it is interrupted
  (PANPAK-FORMAT.md §9.6, THREAT-MODEL.md §2.4). The desktop entry gets the action "Uninstall" (`panpak remove --ask <id>`: a dialog with zenity or kdialog, a notification at the end). A start waits for a removal in progress and ends with `PANPAK-E2004` when the app is gone. New error codes `PANPAK-E2002` to `E2004`; the fuzz targets `fuzz_dbus`,
  `fuzz_dbus_names` and the new `fuzz_journal` run in the CI smoke job.

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
