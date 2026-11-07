# Building from source (step 13a): practical test and design

Status: phase 1 (practical test and design). No product code changed. Everything below was measured on one
machine (Ubuntu 26.04, 16 threads, 14 GB RAM) unless a line says "estimate" or "not measured". The scripts used
(image, builds, collect prototype, harness) live outside this repository, in a local work folder; the design
sections say what would move into the product.

Panpak is a distribution method, and the main path is the developer who builds from source. Until now the
harness had only converted AppImages. This step builds three apps from official sources, packs them with
today's tools, runs them on Wayland without X11, and records what the product and the future `collect` must do.

## 1. Part A: the practical test

### 1.1 Setup

| Item | Value |
|---|---|
| Build image | Debian 13 (trixie) amd64, base `debian:trixie-slim` pinned by manifest digest `sha256:7378260c0a098d27ac206225db5e7d92b8c7c84ea7afccc86eeb513756bbd392`, packages from a dated snapshot (`snapshot.debian.org`, `debian` and `debian-security`, 2026-11-07). glibc 2.41, GTK 4.18.6, libadwaita 1.7.6, GtkSourceView 5.16, Qt 6.8.2, gcc 14.2 |
| How it runs | Rootless: the rootfs is unpacked from the registry with `curl` and `tar` (about 60 lines of Python), commands run in a user namespace made by bubblewrap with one uid. No container engine |
| Display | A headless Sway in its own runtime folder (never the desktop session). Screenshots with `grim`, keys with a small virtual-keyboard client (`zwp_virtual_keyboard_v1`), windows read with `swaymsg`. For D-Bus, the lab of `tests/dbus_lab.py` (private session bus, real xdg-desktop-portal, fake backend) |
| Signing | A local ed25519 key (`panpak keygen`) |

Pinned sources (official only; nothing is redistributed, packages and images stay local):

| App | Source | Pin |
|---|---|---|
| GNOME Text Editor 48.3 | git tag `48.3` of the GNOME gitlab | commit `6f40f0bf2d443471ecc0267e8e87d202b6d1ff6c` |
| qBittorrent 5.2.4 | git tag `release-5.2.4` | commit `a1e4649cb8ae581925acc161dbab5cbc6584c542` |
| Ghostty 1.3.1 | release tarball `ghostty-1.3.1.tar.gz` | SHA-256 `3349d25600ffbda281197a18314f7d18791969cffe9474f0ff16a45a9ebfccdb`, minisign signature verified with the key of the Ghostty packaging guide; built with Zig 0.15.2 (SHA-256 `02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239`) |

Version choices, with the reason (each is a finding, see 3):

- Text Editor 51.0 and 49 or later need GTK 4.19.4 or later, which no stable distribution has. 48.3 needs GTK 4.17.3
  and libadwaita 1.6, which trixie has.
- Ghostty 1.4.0 (needs Zig 0.17.0) was tried first and could not be built, see F16.

### 1.2 Results

| App | Build | Start | Wayland, no X11 | Use tests | Problems |
|---|---|---|---|---|---|
| Text Editor 48.3 (GTK 4, libadwaita) | ok, 7 s | window in 0.52 s (median of 5, first excluded) | yes: no `x11` permission, no `DISPLAY`, window in Sway | light and dark follow the portal live (mean brightness 0.97 light, 0.13 dark, dark again after switching back); opens a file given on the command line; saves with the GTK file dialog into the granted `documents` folder; settings persist (keyfile backend, F15); symbolic icons render (SVG pixbuf loader collected) | F11 (D-Bus name), F12 (a11y bus), F15 (GSettings), F28 (no `glibc_min`). Only a close button was seen: the GNOME default layout, and the headless run has no GSettings to say otherwise; minimize and maximize were not tested |
| qBittorrent 5.2.4 (Qt 6, CMake) | ok, 225 s (`-j4`) | window in 0.31 s | yes (platform plugin `wayland`, no `xcb` plugin collected) | window and menus; legal notice pre-accepted as in the AppImage tests; tray: the app calls `RegisterStatusNotifierItem` on the watcher through the filtered bus (seen with `dbus-monitor`; a fake watcher cannot draw it); notifications: the `Notify` rules are granted but no notification was triggered; dark at start follows the portal | F25 (SVG plugins), F26 (platform theme plugin), F27 (no live switch). The "listen on local socket failed, Name error" line is old: the AppImage logs of step 7 have it too |
| Ghostty 1.3.1 (GTK 4, Zig) | ok, 187 s (`-j3`, with the package download) | window in 0.41 s, OpenGL 4.5 | yes | window, rendering. **No shell**: `openpty` fails (F21), so colors, copy and paste, and opening a link could not be tried. Single instance with a native Ghostty: F22 and F23 | F16 to F24 |

Harness (group `source` of the fast harness, outside the repository): fast mode (one start per app) 10 s for the three
apps; full mode (first start, then the median of 5) 31 s. Both run in a scope with a memory cap; the peak was 0.9 GB.

Timings, sizes and memory peaks:

| App | Build time | Peak memory of the build | Package | Installed tree |
|---|---|---|---|---|
| Text Editor | 7 s (`ninja -j6`) | 0.55 GB | 52.3 MB | 132 MB (108 libraries) |
| qBittorrent | 225 s (`-j4`) | 3.0 GB | 60.2 MB | 121 MB (86 libraries) |
| Ghostty | 187 s with fetch, 175 s without network (`-j3`) | 3.7 GB with fetch, 2.4 GB without | 56.5 MB | 126 MB (103 libraries) |
| Image from scratch | 107 s | 2.1 GB (mostly page cache) | | 1.7 GB on disk (base 84 MB, 29.9 MB compressed) |

Disk used by the work folder at the end of the step: 3.6 GB (image 1.7 GB, packages 219 MB, output trees 434 MB,
Zig package cache 476 MB, Zig 0.15.2 380 MB). Before cleaning it was 9.7 GB: the integration runs (2.9 GB, each
with its own 100 MB copy of `panpak`), a second image, the failed Ghostty 1.4.0 build, and build directories.

## 2. Findings

Priority: H = security or blocks an app class, M = wrong or silent behaviour, L = information. "Not corrected" means
the product was not touched, as the task requires.

| Id | Prio | Finding |
|---|---|---|
| F22 | **H, security** | The sandboxed copy of an app whose application id equals a natively installed one talks to the native instance. Lab test (private bus, headless Sway, the native `/usr/bin/ghostty` as a second program on that private bus; never the user's bus): the Panpak copy (package id = application id, `dbus_own = ["com.mitchellh.ghostty"]`) finds the name owned, becomes a remote instance and, through the filtered bus, calls `org.gtk.Actions.DescribeAll` and `org.gtk.Application.Activate` on the owner. The copy exits 0 and the **native process, outside the sandbox, opens a second window**. Same with and without `--gtk-single-instance=true`. Cause: the proxy rules `--own=<name>` and `--talk=<name>` cannot tell the app's own name from the same name owned by a foreign process. Impact: for a terminal, windows with the user's shell outside the sandbox; for any app, calls to its actions. Not corrected |
| F23 | H | Reverse direction: with the copy running first (it owns `com.mitchellh.ghostty` on the bus), the user's native `ghostty` command exits 0 and its window opens **inside the sandboxed copy**. Native launches are hijacked while the copy runs. Not corrected |
| F21 | H | No pty in the view. `/dev/ptmx` and `/dev/pts` exist in the view but Landlock has no rule for them: `open("/dev/ptmx")` fails with EACCES, `openpty()` with "out of pty devices". Ghostty ends with `error.OpenptyFailed`. No terminal emulator, embedded terminal or program that spawns a pty can work. Needs a permission (for example `pty`: `/dev/ptmx` and `/dev/pts` with `IoctlDev`; TIOCSTI and TIOCLINUX are already refused by seccomp), decided with the threat model. Not corrected |
| F19 | H | A bundled `libwayland-client.so.0` or `libwayland-egl.so.1` next to the host Mesa breaks EGL: Ghostty dies with "Failed to create EGL display" while the native copy works. `[abi] prefer_host = ["libwayland-*"]` fixes it (OpenGL 4.5). PANPAK-FORMAT section 7 lists the graphics stack that the host must win; `libwayland-*` is not in it. Text Editor does not show it because GTK falls back to the cairo renderer silently. Not corrected |
| F26 | M | A Qt app ignored the portal color scheme (always light) because `qt6/plugins/platformthemes/libqxdgdesktopportal.so` was not collected: Debian ships it in its own package (`qt6-xdgdesktopportal-platformtheme`). The runtime sets `QT_QPA_PLATFORMTHEME=xdgdesktopportal` with the filtered bus; with the plugin absent Qt falls back silently, and the Qt file dialog through the portal is lost too. With the plugin, dark at start works. Neither `pack` nor the runtime says anything. Not corrected |
| F25 | M | The tray icon of qBittorrent was not drawn ("No Icon set"): the image had `libqt6svg6-dev` but not `qt6-svg-plugins`, so `imageformats/libqsvg.so` and `iconengines/libqsvgicon.so` did not exist. The `-dev` package does not bring the plugin package |
| F11 | M | The default name an app may own on the bus is the panpak id. A GApplication owns its application id (`org.gnome.TextEditor`): with another panpak id, a filtered bus gives "Failed to register: ServiceUnknown". Rule for source builds: panpak id = application id; the names come from `share/dbus-1/services/*.service`. This rule is what makes F22 reachable |
| F28 | M | `pack` writes no `glibc_min` (the field is empty in the three manifests). The tools can compute it from the version needs of the ELF files (see 4.1); on an older host the failure is a loader error at start |
| F15 | M | The dconf GSettings backend needs `ca.desrt.dconf` on the bus (not allowed). `GSETTINGS_BACKEND=keyfile` in the wrapper keeps settings in the private config folder (verified: the keyfile is written) |
| F24 | M, to verify | Dock and menu grouping. The copy's window class is `com.mitchellh.ghostty` (with D-Bus) or `GTK Application` (without), and the entry is `panpak.com.mitchellh.ghostty.desktop`. With the real class GNOME groups it with the native launcher and shows the native icon and name. Not verifiable headless. Steps for the user: install the package, start native Ghostty, then the Panpak one, and look at the dock (one icon or two, which icon, which name). Run it with the native one stopped too |
| F27 | L | With the portal theme plugin, qBittorrent starts dark when the portal says dark; a live switch to light within 2 s is not followed (Qt 6.8 Fusion). GTK follows live (F13) |
| F12 | L | GTK a11y bus: "Unable to acquire the address of the accessibility bus" on the filtered bus. Harmless; `GTK_A11Y=none` or a filter rule would silence it |
| F16 | build | Ghostty 1.4.0 with Zig 0.17.0: compiling the bundled glslang `SPIRV/doc.cpp` (the clang of Zig) reached 10 GB of RSS in 12 s and was killed by the kernel inside the capped scope. With `-j6` and a 7 GB cap the build thrashed (the cap reached 1.36 million times, 800 MB/s of disk reads). `-fsys=glslang -fsys=spirv-cross` did not avoid the bundled copy. Ghostty 1.3.1 with Zig 0.15.2 builds in 3.7 GB |
| F17 | build | `zig build --system <cache>/p` (the offline route of the Ghostty packaging guide) panics with "reached unreachable code" under Zig 0.17.0, on the host too. A normal online first build fills the Zig cache; the next build needs no network (verified) |
| F5 F6 F8 F9 | build | qBittorrent writes `src/base/version.h` into the source tree at configure time (the source must be a writable copy); it needs `qt6-base-private-dev`; one-uid user namespace plus a chown shim is enough for `apt` (see 3.1); the base image already had security updates newer than the plain archive snapshot, so both snapshots are needed |
| F10 | collect | The names of Qt platform plugins change with the Qt version (6.8 on Debian: `libqwayland-egl.so` and `libqwayland-generic.so`; newer: `libqwayland.so`). Collect takes a category by pattern, never a name |
| F1 F2 F3 F4 | info | `rsvg-convert` PNGs carry a `bKGD` chunk, which `pack` removes with a message per icon; no `Documents` folder in an isolated home gives a "not granted" note; "transport disabled" for the session bus when no bus permission is asked |

### 2.1 Product question from the 7c: "write an executable, run it at once" in the product

Question: does the product (not the tests) write an executable and start it at once while other threads may fork,
so that users could get `ETXTBSY` too?

Searched: every `Command::new` and `exec` of the three crates, and every place that writes a file with the execute
bit. Result: one place has the pattern. `panpak install-runtime <release.tar.gz>` (`panpak/src/main.rs`, the function
that unpacks the release into `runtimes/.stage`) writes `panpak-stub` with `entry.unpack(...)` and then runs
`<stub> --abi-version` through `stub_abi` (`Command::new(stub).arg("--abi-version")`). The write is finished and
closed before the run, and the CLI has no other thread that forks, so the race cannot happen today. It would if a
thread that forks were added to that command. The other writes of executables are the launcher script (written
atomically by rename, and started later by the desktop), the stub copy in the store (`install_file_atomically`, a
copy then a rename, which also avoids "Text file busy" on a running stub), and the files of a converted tree (never
started by the product). Nothing was corrected.

## 3. Part B: the design

### 3.1 Build environment

**Base and glibc.** The conflict: a base with an old glibc runs on more systems, but GTK 4.18, libadwaita and Qt 6.8
need a recent distribution. With the bundle-first resolution (PANPAK-FORMAT section 7) the glibc comes from the
host, so what counts is the highest `GLIBC_x.y` that the packaged files ask for. Measured with a small script
(`DT_VERNEED` of every ELF file of each tree):

| Tree | Highest GLIBC | Where | Highest without `libsystemd.so.0` | Highest GLIBCXX |
|---|---|---|---|---|
| Text Editor | 2.39 | `exp10` in the bundled `libsystemd.so.0` | 2.38 (`libcom_err`) | 3.4.30 |
| qBittorrent | 2.39 | same | 2.38 (`libexec/qbittorrent`) | 3.4.32 |
| Ghostty | 2.39 | same | 2.38 (`libexec/ghostty`) | 3.4.29 |

The 2.38 comes from `__isoc23_strtol` and its family, which gcc 14 with the 2.41 headers selects. So the floor is set by
the image, not by the app.

| Option | glibc floor | Hosts covered | Cost |
|---|---|---|---|
| A. Debian 13 with distribution toolkits (used here) | 2.39 | Ubuntu 24.04 and later, Debian 13, Fedora 40 and later, rolling distributions. Not Debian 12 (2.36), Ubuntu 22.04 (2.35), RHEL 9 and clones (2.34) | one image of 1.7 GB, 107 s to make |
| B. Older base (Debian 12 or Ubuntu 22.04) with GLib, GTK, libadwaita, GtkSourceView and Qt compiled in the image | 2.34 to 2.36 | adds the three older systems | Estimate, not measured: hours for Qt 6 and a long list of GNOME libraries to keep in step; a recipe per toolkit version. Ghostty's GTK and libadwaita floors are high anyway |
| C. Debian 13 with the host glibc symbols pinned (link against old symbol versions) | about 2.34 for most symbols | same as B | not tried: it needs a compat layer for each of `__isoc23_*`, `exp10`, `pidfd_*`; fragile |

Proposal for 13a: **A**, and let `pack` write `glibc_min` from the files (F28), so that an older host gets a clear
message. B is a later option for the apps whose users run enterprise distributions; it is a recipe choice, the format does not change.

**Rootless, with the fewest outside dependencies.** Measured on this host:

| | Podman rootless | bubblewrap sandbox (what was built) |
|---|---|---|
| Outside dependencies | `podman`, `crun` or `runc`, `conmon`, `uidmap`, a network helper, `/etc/subuid`. Not installed here, so **not measured** | `bwrap` (already a runtime dependency of Panpak), `curl`, `tar`, Python |
| Several uids | yes, through the setuid `newuidmap` and the AppArmor profile that the distribution ships for it | no: `unshare --map-auto` works for `id` but its mount step is refused (AppArmor restricts unprivileged user namespaces here; only binaries with a profile, such as `bwrap`, may). One uid plus a small `LD_PRELOAD` shim that makes `chown` succeed is enough for `apt` and `dpkg` (files stay owned by the one uid) |
| Image source | registries, layers, digests built in | a 60-line puller by manifest digest; the layer digest is verified |
| Network off for the build | `--network none` | `--unshare-net` (used for Text Editor and qBittorrent; Ghostty after the first run) |
| Risk | a different tool on every machine | a maintainer script that needs a real second uid would fail (none did) |

Docker is on the machine but its group is root-equivalent; it was not used.

Proposal: the bubblewrap sandbox for 13a (the same engine as the runtime, nothing more to install). Podman can be
a second backend in 13b if CI wants it: the recipe is a list of packages and a build script, not a Containerfile.

**Reproducibility.** Image fixed by digest, packages by snapshot date, `SOURCE_DATE_EPOCH` set from the tag commit,
build with no network after the sources are in (for Ghostty, after the first run that fills the Zig cache; the Zig
packages are checked by their own hashes). Measured:

| Test | Result |
|---|---|
| Text Editor, two builds in one image | 221 files, identical |
| Text Editor, a third build in an image made again from scratch (same digest, same snapshot; `dpkg -l` identical) | identical; the packed `.panpak` is byte for byte the same (SHA-256 starts `2be43fbb`) |
| qBittorrent, rebuilt (`-j4` against the first `-j6`, other image) | 31 files, identical |
| Ghostty, rebuilt with no network | 314 of 315 files identical, including the `ghostty` executable; `libghostty-vt.so`, which is not packaged, differs. The cause was not looked into |

Limits: same build path (`/work/...`) in every build; a different path would be a test of its own. Not tried:
different CPU count, which for Meson and CMake does not change the output, and for Zig is not known.

**Weight and time.** In the table of 1.2. In short: image 1.7 GB, 107 s; builds from 7 s to 4 minutes; memory peak
3.0 to 3.7 GB for the C++ and Zig builds, so a CI runner needs at least 8 GB for these, and a developer machine
should not run two at once (the machine froze once when several builds ran together; since then every build runs in a
scope with a memory cap).

### 3.2 `panpak.toml` for the developer

Today `panpak.toml` describes the package. For a source build it would grow three tables. Minimal schema:

```toml
[app]                       # as today
id = "org.gnome.TextEditor" # the application id (F11)
name = "Text Editor"
version = "48.3"
entry = "bin/gnome-text-editor"

[source]                    # what is built
git = "https://gitlab.gnome.org/GNOME/gnome-text-editor.git"
commit = "6f40f0bf2d443471ecc0267e8e87d202b6d1ff6c"   # required; a tag alone is refused
# or: url = "https://.../editor-1.0.tar.gz", sha256 = "<64 hex>"

[build]
image = "debian-13"                  # a named recipe image, pinned by the tool to a digest and a snapshot date
system = "meson"                     # meson | cmake | cargo | zig | custom
configure = ["--prefix=/usr", "--libdir=lib", "--buildtype=release"]
steps = []                           # only for system = "custom"; each step is a list, no shell
packages = ["libgtk-4-dev"]          # extra packages of the image, from the snapshot
network = "sources"                  # sources | none; the build step itself never has the network

[collect]                            # what goes with the app, see 3.3
toolkit = "gtk4"                     # gtk4 | gtk3 | qt6 | none
prefer_host = ["libwayland-*"]
data = ["share/gtksourceview-5", "share/icons/Adwaita"]

[desktop]                            # as today, plus the source of the icons
icon_svg = "share/icons/hicolor/scalable/apps/org.gnome.TextEditor.svg"
sizes = [256, 128, 64, 48, 32]
schemes = []

[permissions]                        # as today
dbus_own = ["org.gnome.TextEditor"]

[license]                            # data for the license screen and for `panpak licenses`
spdx = "GPL-3.0-or-later"
files = ["COPYING"]
bundled = "auto"                     # license files of the collected libraries, from the image's package data

[update]
key = "ed25519:..."
```

Commands:

| Command | What it does | Clear errors and warnings |
|---|---|---|
| `panpak init` | Looks at the folder (`meson.build`, `CMakeLists.txt`, `Cargo.toml`, `build.zig`, a `.desktop` file, `share/dbus-1/services/*.service`, an icon) and writes a first `panpak.toml` with comments. Does not run anything of the project | does not overwrite an existing file; says which fields it could not guess (`id`, `commit`) and leaves them empty so that `build` refuses |
| `panpak build` | Prepares the image (by digest), fetches the source at the pinned commit or hash (the only step with the network), builds in the sandbox with no network, installs into a staging folder, runs `collect`. Output: a tree, ready for `pack`, and a build record (inputs and digests) | `PANPAK-E6001` source without a commit or hash; `E6002` the fetched source does not match; `E6003` the build step failed (the last 40 lines are shown); `E6004` an image or snapshot that is not pinned; `E6005` the build needs more memory than the cap (the cap and the peak are shown) |
| `panpak pack` | As today, plus checks on the tree | see the warnings below |

Warnings and refusals (new, all with the usual error-code style):

- Broad permissions: `network = true` without `network_allow_tcp`; `x11 = true` for an app that has a Wayland plugin;
  any `home_paths`; `dbus_own` for a name that is another app's well-known name; `xdg_dirs` with `rw` that the
  app's own code does not need (a hint, not a rule).
- **Application id equal to a name that a native install may own** (F22): until the runtime can tell its own name
  from a foreign owner, `pack` warns when `dbus_own` holds the application id of an app that is single-instance.
  This is a stop-gap; the fix belongs in the runtime.
- Missing pieces that fail silently today: a Qt app without the platform theme plugin (F26) or the SVG plugins
  (F25); a GTK app without the pixbuf SVG loader; a tree with `libwayland-*` and no `prefer_host` (F19); an entry
  that is not an executable; no `glibc_min`.
- A terminal-like app (it opens `/dev/ptmx`): refused until a `pty` permission exists (F21).

### 3.3 `panpak collect`: what is needed

What these three apps need, measured:

| Part | Text Editor (GTK 4) | qBittorrent (Qt 6) | Ghostty (GTK 4) |
|---|---|---|---|
| ELF closure (`ldd`, minus glibc family and graphics stack) | 108 libraries, 107 MB before `strip` | 86 libraries, 110 MB | 103 libraries, 70 MB |
| Plugins loaded with `dlopen` | gdk-pixbuf SVG loader (`libpixbufloader_svg.so`) and the others of the image; a loader cache with a placeholder root, filled at start | `platforms` (Wayland: `libqwayland-egl`, `libqwayland-generic`), `wayland-shell-integration`, `wayland-decoration-client`, `wayland-graphics-integration-client`, `platformthemes/libqxdgdesktopportal`, `imageformats` (with `libqsvg`), `iconengines/libqsvgicon`, `tls`, `networkinformation` | same as Text Editor |
| Data folders | compiled GSettings schemas (`glib-compile-schemas` of the schemas of the toolkits and of the app), Adwaita and hicolor icons, shared MIME database, GtkSourceView data, locale | the app's `share` (icons, translations) | the app's resources (`share/ghostty`: shell integration, themes, terminfo), locale, icons |
| Environment set by the wrapper | `XDG_DATA_DIRS`, `GSETTINGS_SCHEMA_DIR`, `GSETTINGS_BACKEND=keyfile`, `GIO_MODULE_DIR`, `GDK_PIXBUF_MODULE_FILE` | `QT_PLUGIN_PATH`, `QT_QPA_PLATFORM=wayland`, `XDG_DATA_DIRS` | as Text Editor |
| Not collected, on purpose | the dconf GIO module (needs a service the sandbox does not reach) | the X11 platform plugin | |

The runtime refuses to take `GIO_MODULE_DIR`, `GTK_PATH`, `QT_PLUGIN_PATH`, `XDG_DATA_DIRS` from the manifest, so
the entry has to be a small wrapper that sets them from its own location. That is what the prototype generates
(`bin/<app>`, a shell script; the real executable is in `libexec/`). For Ghostty the resources are found next to
the executable (`libexec/../share/ghostty`), which the layout keeps.

Comparison with linuxdeploy (from its documentation and plugins; **not run here**): it also walks `ldd`, copies
GTK and Qt plugins and data (the GTK and Qt plugins, with hooks that set the environment), and sets `RPATH` with
`patchelf`; it decides what to leave out with the AppImage excludelist and assumes the AppImage layout and the
AppRun. Panpak already has what that needs on the other side: the runtime resolves libraries by SONAME and builds a
farm, so no `RPATH` or `patchelf` is needed; the "host wins" list is in the format; and the checks that were missing
in this test (F19, F25, F26) are exactly what a collect that knows the runtime can do and a general tool cannot.

Decision: write our own `collect` (ELF closure with the section 7 rules, plus a GTK adapter and a Qt adapter, each a
list of plugin categories, data folders and environment lines), and copy from linuxdeploy's plugins the knowledge
of what to take, not the code. The adapters stay small data tables. A `none` toolkit (for example a Rust CLI) is the
ELF closure only. Checks `collect` must make and report: every Qt module linked has its plugin package in the image;
the platform theme plugin is present; the Wayland integration plugins are present; no `libwayland-*` in the tree
unless the host copy is wrong; undefined symbols of the closure (the same check as `panpak doctor`); GSettings
schemas compile; the highest `GLIBC_` version, written to `glibc_min`.

### 3.4 Icons

The build image has `librsvg2-bin`; `rsvg-convert` renders the SVG of the app at 256, 128, 64, 48 and 32 pixels
(Ghostty already ships PNG up to 1024, which are used as they are). `pack` then applies the rules it already has:
at most 6 files, the size read from the PNG header, cleaned by one deterministic function (the `bKGD` chunk of
`rsvg-convert` output is removed, with one message per file). Proposal: sizes 512 (only from a source that is at
least that large), 256, 128, 64, 48, 32; symbolic icons are not part of the desktop entry. Rendering in the build image
keeps the user's machine free of an SVG renderer and keeps the output the same for everyone.

### 3.5 Signing

For 13a: the local key made by `panpak keygen`, in a file outside the repository, used by `panpak pack --secret-file`.
How it will be replaced by the CI key of step 9 without changing the format: the format already carries the key in
`[update]` and has a rotation (`rotation_old`, `rotation_sig`, `pack --rotate-from-file`): a package signed by
the CI key can carry a rotation signed by the old one when users already trust it; for apps that were never
distributed, the package is simply signed again. The certification of the CI key (step 9) is a separate statement
about a key, so it sits next to the package (an optional field or file that readers that do not know it ignore, as
section 3.1 of the format already allows), not inside the signed bytes that exist today. No change to the footer,
the index or the verification rules is needed.

### 3.6 Recipes

For now in a local folder `recipes/<app>/` next to the work folder: for each app the build script, the tree
script and the permissions fragment that were used (the three recipes of this test are there), and `common/` with
the image maker, the sandbox runner, the puller, the memory-capped runner and the collect prototype. In 13b they move to a separate public
repository (proposal: `panpak/recipes`), one folder per app with a `panpak.toml` of section 3.2 and nothing else,
so that a review of a recipe is a review of one file.

### 3.7 What changes for step 8 (shared store)

Measured: the chunks of the packages (the format's FastCDC chunks, compared by hash with `panpak dump-index`):

| Pair | Shared chunks | Shared, compressed | Share of each |
|---|---|---|---|
| Text Editor and Ghostty (both GTK 4, same image) | 2,701 | 32.5 MB | 66% of Text Editor, 61% of Ghostty |
| Text Editor and qBittorrent | 866 | 29.7 MB | 61% and 52% |
| Ghostty and qBittorrent | 457 | 15.3 MB | 29% and 27% |

Stored once, the two GTK 4 apps take 69.5 MB compressed instead of 102.0 MB (32% less). The shared part is the GTK,
GLib, Pango, Cairo, ICU, Adwaita and data. The gain only exists when the libraries are byte for byte the same,
which is true because both were built in one pinned image; an AppImage conversion would rarely share. Consequences
for step 8: the store works on chunks of the content (not of the package), and the garbage collection must count
references per chunk; and apps built in one pinned image should be encouraged (the image digest is a natural key
for "these libraries are the same").

## 4. Risks and open points

- **F22 and F23 are the first thing to decide.** They exist today for any app whose panpak id equals an application
  id that another instance may own, and the design of this document (id = application id, F11) makes that the
  normal case.
- F21 (no pty) removes a whole class of apps; F19, F25, F26 produce apps that start but look or work wrong with no
  message.
- Memory: a Zig or C++ build can need 3.7 GB and one translation unit can run away (F16). Recipes must state the
  memory and the job count they were tested with.
- Decision for Ghostty: it stays on 1.3.1 with Zig 0.15.2 for all of 13a. Ghostty 1.4.0 with Zig 0.17 is taken up again in
  13b, by the `-fsys=glslang -fsys=spirv-cross` route and not by raising the memory (the one attempt made here still
  compiled the bundled `doc.cpp`, so the flags or the packages of the image need a closer look first).
- Toolkit freshness: the three apps pinned to versions the image could build; newer upstream releases need newer
  toolkits than a stable distribution has (Text Editor 49 and later, Ghostty 1.4), which pushes toward option B of 3.1.
- Not done in this phase: the dock check (F24), notifications actually delivered by qBittorrent, copy and paste and
  link opening in Ghostty (blocked by F21), the minimize and maximize buttons, `linuxdeploy` run side by side,
  Podman measured, Zig output with a different path or CPU count.
