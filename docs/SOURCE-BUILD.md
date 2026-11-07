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

### 1.1.1 Ghostty: the route used at the end, and its dependencies

Final route: **Ghostty 1.3.1 with Zig 0.15.2, without `-fsys`**: every dependency is the Zig package that Ghostty pins (hash-checked by Zig), built from source
and linked statically into the `ghostty` executable (FreeType, HarfBuzz 11.0.0, Fontconfig 2.14.2, Oniguruma, libpng, zlib, glslang, SPIRV-Cross, highway, simdutf and
the others of `build.zig.zon.txt`). The `-fsys=glslang -fsys=spirv-cross` route was tried only on 1.4.0 (F16) and did not take effect there; it is not used in the packages.
What stays shared is the GTK stack and one more library: the executable needs `libgtk-4`, `libgio`, `libgobject`, `libglib`, `libadwaita-1`, `libX11`,
`libwayland-client` and `libgtk4-layer-shell`. All are collected from the build image (Debian) into the package (`libwayland-client` comes from the host since the
correction of F19). `libgtk4-layer-shell` is Debian's **1.0.4**; Ghostty pins 1.1.0 in its Zig packages but this build did not produce or install that copy (the
executable links the system one by `pkg-config`, with `RUNPATH` to the system folder), so the one in the package is the Debian one. It worked in every test; the quick-terminal
(the layer-shell feature) was not tried. When 1.4.0 comes back in 13b, this version question comes with it.

### 1.2 Results

| App | Build | Start | Wayland, no X11 | Use tests | Problems |
|---|---|---|---|---|---|
| Text Editor 48.3 (GTK 4, libadwaita) | ok, 7 s | window in 0.52 s (median of 5, first excluded) | yes: no `x11` permission, no `DISPLAY`, window in Sway | light and dark follow the portal live (mean brightness 0.97 light, 0.13 dark, dark again after switching back); opens a file given on the command line; saves with the GTK file dialog into the granted `documents` folder; settings persist (keyfile backend, F15); symbolic icons render (SVG pixbuf loader collected) | F11 (D-Bus name), F12 (a11y bus), F15 (GSettings), F28 (no `glibc_min`). Only a close button was seen: the GNOME default layout, and the headless run has no GSettings to say otherwise; minimize and maximize were not tested |
| qBittorrent 5.2.4 (Qt 6, CMake) | ok, 225 s (`-j4`) | window in 0.31 s | yes (platform plugin `wayland`, no `xcb` plugin collected) | window and menus; legal notice pre-accepted as in the AppImage tests; tray: the app calls `RegisterStatusNotifierItem` on the watcher through the filtered bus (seen with `dbus-monitor`; a fake watcher cannot draw it); notifications: the `Notify` rules are granted but no notification was triggered; dark at start follows the portal | F25 (SVG plugins), F26 (platform theme plugin), F27 (no live switch). The "listen on local socket failed, Name error" line is old: the AppImage logs of step 7 have it too |
| Ghostty 1.3.1 (GTK 4, Zig) | ok, 187 s (`-j3`, with the package download) | window in 0.41 s, OpenGL 4.5 | yes | Phase 1: window, rendering, no shell (F21). **After the correction of F21 and F19**: a shell with colors (`TERM=xterm-ghostty`, 256 colors, red, green and blue text rendered), paste from the clipboard (`Ctrl+Shift+V`), select all and copy (`Ctrl+Shift+A`, `Ctrl+Shift+C`; the clipboard held the terminal text), no `prefer_host` needed for OpenGL 4.5. **Custom shader** (`custom-shader` with a GLSL file that tints the output red): the shader is loaded and applied (mean color of the window 187/23/29 against 41/46/55 without it), so rendering is right with a custom shader and not only a window that opens. Opening a link: **not verified** (the injected Ctrl+click did not reach Ghostty; no `xdg-open` call and no portal call were seen, F29). Single instance with a native Ghostty: F22 and F23 | F16 to F24, F29 |

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

Status after phase 2: F19, F21, F25, F26 and F28 are corrected (section 5); F22 and F23 have a design in section 4 and wait for a decision; F11
waits for that decision. Rows keep the text of phase 1.

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
| F29 | M, to verify | Opening a link from Ghostty (GTK `show_uri`): the filtered bus has no `org.freedesktop.portal.OpenURI` rule (`panpak dbus-rules` lists Settings, Inhibit, Request and Session) and the opener of section 9.3 only serves programs that run `xdg-open`. A GTK app that opens a link through the portal probably cannot. The test could not click the link (the virtual pointer events did not reach the app), so this is **not verified**; try it by hand: Ctrl+click on a URL in the Panpak Ghostty, with a native `xdg-open` that you can see |
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

**Decision (accepted for the beta): A.** The floor is glibc 2.39. It covers Ubuntu 24.04 and later, Debian 13, Fedora 40 and later, Bluefin and the rolling
distributions. **Debian 12, Ubuntu 22.04 and RHEL 9 and its clones stay outside.** `pack` now writes `glibc_min` from the files (F28), and `install` and
`update` refuse with `PANPAK-E2005` and both versions on a system with an older glibc, so that no one finds out from a loader error. Going lower later is option B: an older image (Debian 12 or Ubuntu 22.04) with GLib, GTK, libadwaita, GtkSourceView and Qt compiled on top of it, at an estimated cost (not measured) of hours of build per toolkit version and a recipe to keep up to date for each; or option C. It is a recipe choice, the format does not change. Not done now.

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

### 3.2 The recipe (`panpak.toml`), `panpak.lock` and the commands

Decided with the owner (phase 3), and built (section 6). A recipe is a `panpak.toml` with the tables `[app]`, `[source]`, `[build]`, `[collect]`, `[desktop]`, `[permissions]`, `[launch]`,
`[env]`, `[license]`; `panpak build` makes from it the manifest of the package (adds `[abi]`, `[runtime]`, `[update]`, `[provenance]`).

```toml
[app]                        # as in a manifest
id = "org.gnome.TextEditor"  # the application id (F11)
name = "Text Editor"
version = "48.3"
entry = "bin/gnome-text-editor"   # the program itself, relative to the installed tree (/usr is the root): never a shell script

[source]                     # exactly one of git (with commit) or url (with sha256)
git = "https://gitlab.gnome.org/GNOME/gnome-text-editor.git"
commit = "6f40f0bf2d443471ecc0267e8e87d202b6d1ff6c"   # the full id: a tag can move, so it is refused
# epoch = 1790000000         # SOURCE_DATE_EPOCH; default: the date of the commit (git) or the newest file of the tarball (url)

[build]
# image = "debian-13"        # default; the NAME of the image. Which one exactly is fixed in panpak.lock
system = "meson"             # meson | cmake | zig | custom
configure = ["--prefix=/usr", "--libdir=lib", "--buildtype=release"]
packages = ["meson", "ninja-build", "libgtk-4-dev"]   # of the image, from its snapshot (the base tools are always there)
memory = "1G"                # what the build was measured to need at `jobs` jobs; optional
jobs = 6                     # the most jobs it was tested with; default: the processors
# memory_per_job = "0.5G"    # default: memory / jobs
# network = "fetch"          # the first run may fetch what the build system downloads (Zig packages); default none
# writable_source = true     # the build writes into its source (a generated header): it works on a copy
# zig = { version = "0.15.2", sha256 = "<sha256 of the archive>" }     # for system = "zig": the compiler is pinned here
# steps = [["make", "-j2"], ["make", "install"]]                      # for system = "custom": argument lists, never a shell string

[[build.image_deps]]         # a dependency taken from the image instead of built from the pinned source
name = "gtk4-layer-shell"
package = "libgtk4-layer-shell-dev"   # must be in build.packages
why = "linked from the system by pkg-config"   # required

[collect]
toolkit = "gtk4"             # gtk4 | qt6 | none
# prefer_host = []   allow_missing = []   data = ["share/gtksourceview-5"]   exclude = ["include/*"]

[desktop]                    # sizes default to [256, 128, 64, 48, 32]
icon_svg = "share/icons/hicolor/scalable/apps/org.gnome.TextEditor.svg"   # or icons_png = [...] for PNG files of the tree
# wm_class = "..."   schemes = [...]

[env]                        # optional: added to what the toolkit needs (PANPAK-FORMAT.md section 3.1.6)
# GSETTINGS_BACKEND = "keyfile"

[permissions]  /  [launch]   # as in a manifest

[license]
spdx = "GPL-3.0-or-later"
files = ["COPYING"]          # copied into share/licenses/<id>/
bundled = "auto"             # the copyright files of the image packages that the collected libraries came from
```

`panpak.lock`, next to the recipe, written by `panpak init` or the first `panpak build` and changed only by `panpak build --update-lock`:

```toml
[image]
name = "debian-13"
manifest_digest = "sha256:7378260c0a098d27ac206225db5e7d92b8c7c84ea7afccc86eeb513756bbd392"
snapshot = "20261107T000000Z"
```

**Commands.**

| Command | What it does |
|---|---|
| `panpak init [folder]` | Looks at the folder (`meson.build`, `CMakeLists.txt`, `build.zig`, a desktop entry, a D-Bus service file, an SVG icon, the git remote and the commit that is checked out) and writes a recipe with the holes marked `TODO`, and `panpak.lock`. Runs nothing of the project; does not overwrite |
| `panpak build [folder] --secret-file <f> [--stub <stub>] [-o <out>] [--update-lock]` | The lock; the memory plan; the image (made once, kept in the cache); the source (git commit or hashed tarball); the build in the image; the collection; the manifest; `pack`. Prints the peak memory and, if the recipe does not say or says too little, the `build.memory` to write |
| `panpak pack` | As before, plus: refuses an `[env]` path that is not in the tree, writes `glibc_min`, warns about a shell script as the entry and about a Qt tree without its plugins |

**Defaults.** `build.image` is `debian-13`; `desktop.sizes` is `[256, 128, 64, 48, 32]`; `build.jobs` is the number of processors (lowered by the memory); `build.network` is `none`.

**The lock and the image name.** The recipe has the name; the lock has the digest of the image in the registry and the date of the package snapshot. Two builds of one recipe use one image until someone runs
`--update-lock`. A lock for another name than the recipe's is an error that says so. The image of a recipe is kept in the cache under a key made of the lock and the packages.

**Memory.** `build.memory` is what the recipe was measured to need at `build.jobs` jobs. Before it starts, `build` reads `MemAvailable`: with enough, it runs `jobs` jobs; with less, it lowers the number
of jobs (the part of the need that does not depend on the jobs is `memory - memory_per_job x (jobs - 1)`) and says so; if not even one job fits it stops with `PANPAK-E6005`, before downloading anything. The build runs in a
systemd user scope with `MemoryMax` of 1.25 times what it needs and `MemorySwapMax` of the swap that exists (at most 6 GB), so that when memory ends the kernel closes the build and not the desktop; the tool samples `memory.peak` of the scope.
At the end it prints the peak and, if there is no `build.memory` or the peak is above it, the value to write (the peak plus a tenth). The peak is **not** written into the package: it changes from run to run, and the same inputs must give the same bytes.
Without `systemd-run --user` the build runs without a cap, with a warning.

**Dependencies taken from the image.** `[[build.image_deps]]` declares them one by one, with the reason; the package must be in `build.packages`. The build records the version found, and the list goes into the signed
`[provenance]` of the package and is shown by `install`. The three recipes below have one (Ghostty: `gtk4-layer-shell`); the `-fsys` route of Ghostty 1.4.0 (13b) would add `glslang` and `spirv-cross`.

**`[env]` instead of a script.** The entry is the program. What the toolkits need (`GSETTINGS_SCHEMA_DIR`, `QT_PLUGIN_PATH`, `GDK_PIXBUF_MODULE_FILE`, `XDG_DATA_DIRS`, `GSETTINGS_BACKEND`, `QT_QPA_PLATFORM`) is data in the manifest, set by the stub;
the allowed names, their kinds and why are in PANPAK-FORMAT.md section 3.1.6. Values are `${PANPAK_ROOT}/relative/path` (checked by `pack`: no absolute path, nothing outside the tree, the path must exist) or a word from a short list.

**Warnings and refusals.** `pack` and `build` say, besides the above: broad permissions (`network = true` without ports, `x11 = true`, any `home_paths`); a Qt tree without its theme or SVG plugins (a stop in `build`); `libwayland-*` is never collected (the host's copy
is used); a terminal-like app needs nothing (the pty is always there). Open until F22 is decided: a `dbus_own` equal to the id of an app that is single-instance gives the copy a way to the native instance; the recipe of Ghostty
turns the single instance off and gives the window a class of its own.

### 3.2.1 The three recipes in the final format

As built by `panpak build` (these are the files used in section 6).

```toml
# gnome-text-editor/panpak.toml
[app]
id = "org.gnome.TextEditor"
name = "Text Editor"
version = "48.3"
entry = "bin/gnome-text-editor"

[source]
git = "https://gitlab.gnome.org/GNOME/gnome-text-editor.git"
commit = "6f40f0bf2d443471ecc0267e8e87d202b6d1ff6c"

[build]
system = "meson"
configure = ["--prefix=/usr", "--libdir=lib", "--buildtype=release"]
packages = [
  "meson", "ninja-build", "libgtk-4-dev", "libadwaita-1-dev", "libgtksourceview-5-dev", "libspelling-1-dev", "libicu-dev",
  "libenchant-2-dev", "libeditorconfig-dev", "blueprint-compiler", "itstool", "appstream", "sassc", "libxml2-utils",
  "gsettings-desktop-schemas", "adwaita-icon-theme", "shared-mime-info", "librsvg2-common",
]
memory = "1G"
jobs = 6

[collect]
toolkit = "gtk4"
data = ["share/gtksourceview-5"]

[desktop]
icon_svg = "share/icons/hicolor/scalable/apps/org.gnome.TextEditor.svg"

[permissions]
dbus_own = ["org.gnome.TextEditor"]

[[permissions.xdg_dirs]]
name = "documents"
access = "rw"

[license]
spdx = "GPL-3.0-or-later"
files = ["COPYING"]
bundled = "auto"
```

```toml
# qbittorrent/panpak.toml
[app]
id = "org.qbittorrent.qBittorrent"
name = "qBittorrent"
version = "5.2.4"
entry = "bin/qbittorrent"

[source]
git = "https://github.com/qbittorrent/qBittorrent.git"
commit = "a1e4649cb8ae581925acc161dbab5cbc6584c542"

[build]
system = "cmake"
configure = [
  "-DCMAKE_BUILD_TYPE=Release", "-DCMAKE_INSTALL_PREFIX=/usr", "-DCMAKE_INSTALL_LIBDIR=lib",
  "-DGUI=ON", "-DWEBUI=ON", "-DSYSTEMD=OFF", "-DSTACKTRACE=OFF", "-DTESTING=OFF",
]
packages = [
  "cmake", "ninja-build", "qt6-base-dev", "qt6-base-private-dev", "qt6-tools-dev", "qt6-tools-dev-tools", "qt6-l10n-tools",
  "libqt6svg6-dev", "qt6-svg-plugins", "qt6-xdgdesktopportal-platformtheme", "qt6-wayland",
  "libtorrent-rasterbar-dev", "libboost-dev", "libssl-dev", "zlib1g-dev",
]
writable_source = true
memory = "3G"
jobs = 4
memory_per_job = "0.6G"

[collect]
toolkit = "qt6"

[desktop]
icon_svg = "share/icons/hicolor/scalable/apps/qbittorrent.svg"

[permissions]
network = true
network_allow_tcp = [80, 443]
network_bind_tcp = [6881]
notifications = true
tray = true
dbus_own = ["org.qbittorrent.qBittorrent"]

[[permissions.xdg_dirs]]
name = "download"
access = "rw"

[license]
spdx = "GPL-3.0-or-later"
files = ["COPYING"]
bundled = "auto"
```

```toml
# ghostty/panpak.toml
[app]
id = "com.mitchellh.ghostty"
name = "Ghostty"
version = "1.3.1"
entry = "bin/ghostty"

[source]
url = "https://release.files.ghostty.org/1.3.1/ghostty-1.3.1.tar.gz"
sha256 = "3349d25600ffbda281197a18314f7d18791969cffe9474f0ff16a45a9ebfccdb"
epoch = 1790000000

[build]
system = "zig"
zig = { version = "0.15.2", sha256 = "02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239" }
configure = ["--prefix", "/usr", "-Doptimize=ReleaseFast", "-Dcpu=baseline"]
packages = [
  "libgtk-4-dev", "libadwaita-1-dev", "libgtk4-layer-shell-dev", "blueprint-compiler", "libxml2-utils",
  "gsettings-desktop-schemas", "adwaita-icon-theme", "shared-mime-info", "librsvg2-common",
]
network = "fetch"
memory = "3G"
jobs = 3
memory_per_job = "1G"

[[build.image_deps]]
name = "gtk4-layer-shell"
package = "libgtk4-layer-shell-dev"
why = "linked from the system by pkg-config; Ghostty pins 1.1.0 and the image has 1.0.4"

[collect]
toolkit = "gtk4"
exclude = [
  "include/*", "lib/libghostty-vt*", "share/vim/*", "share/fish/*", "share/bash-completion/*", "share/zsh/*", "share/nvim/*",
  "share/bat/*", "share/kio/*", "share/nautilus-python/*", "share/systemd/*", "share/pkgconfig/*",
]

[desktop]
icons_png = [
  "share/icons/hicolor/512x512/apps/com.mitchellh.ghostty.png", "share/icons/hicolor/256x256/apps/com.mitchellh.ghostty.png",
  "share/icons/hicolor/128x128/apps/com.mitchellh.ghostty.png", "share/icons/hicolor/32x32/apps/com.mitchellh.ghostty.png",
  "share/icons/hicolor/16x16/apps/com.mitchellh.ghostty.png",
]
wm_class = "com.mitchellh.ghostty.panpak"

[launch]
args = ["--gtk-single-instance=false", "--class=com.mitchellh.ghostty.panpak"]

[[permissions.xdg_dirs]]
name = "documents"
access = "rw"

[license]
spdx = "MIT"
files = ["LICENSE"]
bundled = "auto"
```

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

## 4. D-Bus names shared with a native app (F22 and F23): design

Design only; no code. The decision is the owner's. Everything was tested on a private session bus (a `dbus-daemon` of its own with the real
`xdg-desktop-portal`, in a runtime folder of its own), with fake "native" programs that own a name and log every call they get, and a headless
Sway. The user's session bus was never used. The scripts are `f22.py`, `fake_native.py` and the probe package in the work folder.

### 4.1 What the filtered bus allows today

The runtime gives the proxy `--own=<name>` and `--talk=<name>` for the application id and for each name of `dbus_own`
(`panpak dbus-rules` prints exactly these lines). In `xdg-dbus-proxy` the policy levels add up (its manual): **OWN implies TALK**, and TALK means "you can send any
method calls and signals to the name" and call `StartServiceByName` on it. So the proxy has no setting "own it, but do not talk to a foreign owner".

| Test | What was done | Result |
|---|---|---|
| A. A native process owns the name (a fake password manager, `org.keepassxc.KeePassXC`) | A package with `dbus_own` of that name calls three methods on the name: `org.freedesktop.DBus.Peer.Ping`, an invented `com.example.Secret.GetSecret` on an invented path, and `org.gtk.Application.Activate` | All three are delivered and answered; the native program logged all three, from the proxy's connection. The package also tried to own the name: it lost (the name stays with the native process), but kept the right to call it. **Any interface and any path reach the native process** |
| B. The native app is not running; a package owns its name | A package with `dbus_own = ["org.keepassxc.KeePassXC"]` owns it; a client outside the sandbox (what a browser extension or another app would do) calls `org.keepassxc.KeePassXC.GetEntry` | The package owns the name (`GetNameOwner` shows the proxy's connection) and **receives the call** (`Introspect` and `GetEntry` in its log) and answers it. The caller got the package's reply: **a package can take the place of a native app on the session bus, and receive what is meant for it** |
| C. The native app is not running but has an activation file | A service file for `org.example.NativeApp` on the bus; a package with that name in `dbus_own` calls it | The bus starts the native program **outside the sandbox** (its log shows it started and received the three calls from the package) |
| D. Which names `dbus_own` refuses | `pack` with names of services and apps | `org.freedesktop.secrets` and `org.gnome.keyring.*` are refused (`PANPAK-E0007`, the list of section 3.1.5). `org.keepassxc.KeePassXC`, `com.bitwarden.desktop` and `org.mozilla.firefox` are accepted: the list protects the desktop's own services, not other apps |
| E. What the user is told | `panpak install` of the package of B | One line: "the app can own org.keepassxc.KeePassXC on your session bus, besides its own id". It does not say that the app can also call whoever owns it, and the application id itself (`--own` and `--talk` for `app.id`) is never listed, so a package whose **id** is `org.keepassxc.KeePassXC` has the same powers with no line at all |
| F. Ghostty with the native one running (phase 1) | The Panpak copy starts while a native `ghostty` owns `com.mitchellh.ghostty` (both orders) | The copy becomes a remote instance and calls `org.gtk.Actions.DescribeAll` and `org.gtk.Application.Activate`; the native process opens a window outside the sandbox (F22). In the other order the native command opens its window inside the copy (F23) |

Answers: (1) yes, with `--own` on a name that a process outside the sandbox owns, the app can call that process, every interface, and start it if it is activatable.
(2) Yes: a package with the id or a `dbus_own` of a sensitive app owns the name when that app is closed and receives its messages. Nothing in the install text warns about it.
What it cannot do: own the services of the desktop (`org.freedesktop.*`, the keyring, the shell). What the test does not show: whether a given real app (a real KeePassXC, a real browser
integration) accepts or leaks anything through those calls; the test shows that the messages arrive.

### 4.2 Flatpak in the same case

From the documentation (read for this document, nothing was run: Flatpak is not installed here): the default session bus policy of a Flatpak app "only allows the application to
own its own namespace named by `$FLATPAK_ID`" (subnames and the MPRIS name of that id included), the page "does not address" what happens when a host app has the same id, and it says
nothing about verifying the id. `xdg-dbus-proxy` is the same program that Panpak uses, so with the same policy lines the same case behaves the same way: a Flatpak app that finds
its own id owned by a host process is a remote instance of it, and can call it. What Flatpak adds is on the **distribution** side: Flathub requires that the application id
be a reverse-DNS name of a domain the author controls ("the author or the developer or the project must have control over the domain", with a token file for verification) or the
code-hosting prefix (`io.github.`, `io.gitlab.`, ...), and `org.kde.` only for projects on KDE Invent. That narrows who can publish a package with a given id, so the squatting of B
needs a stranger to get a Flathub id they do not own. It is not a rule of the bus.

### 4.3 Options

| Option | What it is | Pros | Cons |
|---|---|---|---|
| 1. Check at start | The outer stub asks the real bus who owns each name it would grant (`GetNameOwner`, then the owner's credentials and unit with the code of `conn.rs`). If the owner is not in this app's own scope (not one of our copies), the stub leaves out `--own` and `--talk` for that name and says so. Also: the install text lists the application id too and says "and call whoever owns it"; `install` prints a note when the id or a name is owned now or has a `.desktop` file or a service file on the system | Small (a function of the outer stub plus text); closes F22 and test A and C for names that are taken at the start; cheap at start (one call, about the cost of the scope call that is already made) | A race: a native app that starts after the check, and F23 (the native `ghostty` command is routed into the copy that holds the name) stay; test B is not closed when the native app is simply closed (the package may own the name then); and an app that is refused its own name dies with F11 (a GApplication cannot register) unless the recipe turns the single instance off (Ghostty has `--gtk-single-instance=false`; many apps have no such switch) |
| 2. A proxy of our own that owns but does not call foreign owners | Replace `xdg-dbus-proxy` by a filtering proxy that knows who is the owner of a name and lets an app talk to a name only when the owner is the app itself | The exact rule; closes A, B (with a prefix or id check), C, and F | Large and risky: an authenticated D-Bus server side (names, matches, file descriptors, replies, broadcasts) of our own, in a part of the system where `xdg-dbus-proxy` is the well-tested one; `conn.rs` and `wire.rs` (about 1,000 lines) are a client, not a daemon. No estimate is given: it needs a prototype before a date |
| 3. A private session bus for each app | The app does not see the user's bus: it gets a bus of its own (one per app and shared by its copies, in `$XDG_RUNTIME_DIR/panpak/<id>/`) where it owns its id and any names it wants. Only what it asked for on the real bus (portals, notifications, tray, the settings) is bridged to it | Closes A, B, C, F and F23 completely, in both directions; F11 disappears (the app can always own its id on its own bus); single instance between our copies keeps working; no list of allowed names to keep | A bus daemon per app (the host's `dbus-daemon`, which not every system has; `dbus-broker` systems may not) or our own, and a bridge that forwards calls and replies and the signals to the real bus without turning into option 2; tray items and MPRIS are objects that the host calls back by name, so the bridge has to carry that too; an extra process and some milliseconds at start, to be measured. The most work, and the right shape |
| 4. Names only for verified ids (step 9) | The own id and `dbus_own` on the real bus are granted only to a package whose id the store has verified (domain control, as Flathub does), or a prefix | Stops strangers from taking the name of a known app (B) at the place where it is cheap: the store | Not the bus: F22 and F23 stay for a verified publisher whose app is also installed natively (the same publisher, so a smaller problem); useless for local and unsigned packages; a GApplication name cannot be given a prefix (F11) |

### 4.4 Effect on F11 and on the dock (F24)

- **F11** (a GApplication that dies when it cannot own its name): option 1 makes it more visible, because a refused name means a dead app, so it needs the recipe to switch the single
  instance off where the app allows it. Option 3 removes it. Options 2 and 4 do not change it (2 grants the name, 4 does not touch it).
- **Dock (F24)**: the window class that the app sets (for a GTK app its application id) decides the grouping, and the desktop looks for `<class>.desktop`: the native entry matches before
  `panpak.<id>.desktop` does. None of the options change that. The fix is in the recipe: a class of its own where the app has the option (Ghostty has `--class`, which also changes its
  application id) with the same value in `desktop.wm_class`; where it has not, the copy shows up under the native app's icon. To check by hand on the real desktop (not done here, it cannot be done headless).

### 4.5 Recommendation

Two steps. **Now, option 1**, because it is small and closes the call-through (F22, A and C) for the case that is certain, a native instance that is running: check the owner at
start, leave the name out and say so, list the application id and the "can call" meaning in the install text, and print a note at install when the id exists natively.
For recipes: set the single instance off where the app has the switch, and a class of its own. **Target, option 3**, the private bus per app, because it is the only
one that removes F22, F23, the squatting (B) and F11 together, and it is the shape that does not depend on a list of names; start with a prototype that measures the cost at start and
shows that the bridge for portals, notifications and the tray does not need a filter of its own. Option 4 is worth doing at step 9 for its own reasons (who may publish which id) and is not
a substitute. Option 2 only if the prototype of option 3 shows that the bridge is, in fact, option 2. The decision is the owner's.

## 5. Phase 2: corrections in the product

Corrections that the owner cleared, on branch `source-1`. F11, F22 and F23 are not touched (they wait for section 4). Each has a test that failed before and passes now.

| Finding | What changed | Test and result |
|---|---|---|
| F21, no pty | The view's `/dev` is bubblewrap's own minimal one (`--dev /dev`, which has a devpts of its own and `ptmx`) with the GPU folder put back (`--dev-bind-try /dev/dri`), instead of the host's `/dev`. Landlock gives `/dev/ptmx` and `/dev/pts` read, write and the device ioctls (also in the whole-`/dev` fallback). `TIOCSTI` stays refused. PANPAK-FORMAT section 9 and THREAT-MODEL describe it | `tests/pty.py` (in the CI): `openpty`; `/dev/pts` in the view has another `st_dev` than the host's, so the user's terminals are not visible; an interactive bash answers, has a controlling terminal, sees the window size and a second change of it; Ctrl+Z stops a job, `jobs` lists it, `fg` and Ctrl+C end it; `less` uses the alternate screen and leaves it; `TIOCSTI` on the pty gives EPERM. With the old binaries the same test stops at "out of pty devices". Ghostty 1.3.1 now runs a shell with colors, paste, select-all and copy, and a custom shader (section 1.2) |
| F19, `libwayland` | `libwayland-client`, `libwayland-egl` and `libwayland-cursor` are in the list of libraries that the system always wins (`SYSTEM_WINS`), with the bundle as the fallback when the host has none. Section 7 of the format says so | Unit test of the choice table; Ghostty repacked **without** the `prefer_host` line of phase 1: OpenGL 4.5 and no EGL error |
| F25 and F26, Qt plugins | `pack` prints a warning when a tree has `libQt5Core` or `libQt6Core` and no `platformthemes/libqxdgdesktopportal`, and when it has `libQt6Svg` and not both `imageformats/libqsvg` and `iconengines/libqsvgicon`. It is a warning, not a refusal: converted AppImages may lack them for reasons of their own. The real collector comes later | Unit tests and `panpak/tests/pack_checks.rs`; by hand: the qBittorrent tree with the plugins removed gives both warnings, and the complete tree gives none. qBittorrent with the plugins: the tray registration call reaches the watcher on the bus (seen with `dbus-monitor`) and the window starts dark when the portal says dark. A live switch to light is not followed (F27) |
| F28, `glibc_min` | `pack` writes `abi.glibc_min` from the symbol versions of the files (`.gnu.version_r`, only the glibc libraries; a declared value can only raise it; a tree with its own glibc gets none). `install` and `update` refuse a package that needs more than the system has (`PANPAK-E2005`, both versions in the message) before anything is written. The field is validated (`2.39`-like) | Unit tests; `panpak/tests/pack_checks.rs`: the file's version is written, a script gets none, a declared `99.1` is kept and refused at install with the right text, a declared `2.1` is not kept below the file's. The three source packages say `glibc_min = "2.39"` |

The icon rule you cleared stays as it is: the `bKGD` chunk is removed with one note per icon.

**Pty: a permission or always?** Always. A pty is not an authority: the view has a devpts of its own, so the ptys of the app are the app's, not the user's terminals; `TIOCSTI` and
`TIOCLINUX` are refused; a shell on the pty is inside the same view, Landlock and seccomp as the app. And it is not only terminals: IDEs, editors with a terminal, git front ends and
Electron apps with `node-pty` open ptys, so a permission would be asked of half of the apps and would carry no information. The price, measured: bubblewrap's own `/dev` costs **3.0 ms** more
at every start than the old bind of the host's `/dev` (60 interleaved runs of a bare `bwrap`: median 5.60 ms against 8.57 ms), and the fixed cost of the launcher in
`tests/startup_budget.py` is **29.6 ms** (budget 30 ms, 24.5 ms at step 7: the rest of the difference is not looked into). If the 3 ms matters, the alternative is `terminal = true` in the manifest and the old `/dev` for the apps that do not
ask; the price is that every app that opens a pty without declaring it fails with "out of pty devices".

**Harness.** AppImage group, cold (every conversion again, because the binaries changed), 15 apps: all start, all with icon; 1,838 s (conversions 1,796 s, installs 5 s, starts 34 s), peak
memory of the scope 7.3 GB. AppImage group, full mode (median of 5 starts, first left out, conversions from the cache): all 15 start with icon, 545 s, peak 5.6 GB; window medians against step 7b: audacity 0.697 s (0.714), qBittorrent 0.269 (0.284), MuseScore 0.269 (0.280), Krita 0.875 (0.910), Kdenlive 1.341 (1.363), QOwnNotes 0.379 (0.397), so no regression from the new `/dev`. Source group: fast 11 s, full 33 s (window medians: Text Editor 0.52 s, qBittorrent 0.41 s, Ghostty 0.41 s; qBittorrent was 0.31 s before the
change of `/dev`, one run each, not looked into). Workspace tests: all pass (peak 2.7 GB); clippy clean.

## 6. Phase 3: decisions, the cost of start, and `panpak build`

### 6.1 Decisions of the owner

- **F22 and F23: no change in the product now.** The fix will be option 3 of section 4 (a private session bus for each app, with a bridge only for the portals, the notifications and the tray), in a step of its own right after 13a. The stop-gap
  of option 1 would be thrown away when the private bus comes, and nothing is published today. It was not started.
- **Ghostty, for now, in the recipe only:** `--gtk-single-instance=false` and a window class of its own (`--class=com.mitchellh.ghostty.panpak`, the same as `desktop.wm_class`). Tested in the lab (a native `/usr/bin/ghostty` running as a second program on a
  private bus): the Panpak copy stays alive, opens a window of its own (its own process, class `com.mitchellh.ghostty.panpak`), the native instance keeps its one window, and there is no method call from the copy to `com.mitchellh.ghostty` on the bus (before: `DescribeAll` and
  `Activate`). The recipe does not give the copy `dbus_own`. What this does not do: it protects only this app, and only while the recipe says so; any other single-instance app is still open until the private bus.
  Without a session bus (`DBUS_SESSION_BUS_ADDRESS=disabled:`, a test with no bus) the window class that Sway sees is `GTK Application`: the class reaches the window only when the app has a bus.
- **glibc floor 2.39 (Debian 13): accepted for the beta**, with how to go lower later written in 3.1.
- **Schema approved with changes**, built in this phase: `panpak.lock`, `[env]` instead of a script, defaults, `build.packages`, memory measured and checked, `[[build.image_deps]]` and `[provenance]`, our own ELF reader for `collect`, GTK and Qt as tables of data.

### 6.2 The cost of start (target: 26 ms or less, with the pty always there)

The fixed cost of `tests/startup_budget.py` (80 interleaved runs per build, the main of 15fff8a built next to the branch):

| | main | branch before | branch now |
|---|---|---|---|
| fixed cost, three series (ms) | 25.4, 24.7, 24.5 | 27.9, 27.9, 26.2 (first fix: 26.3, 26.1, 26.2) | **25.5, 25.3, 25.1** (main in the same series: 25.8, 24.0, 23.9) |

So the "2.1 ms not explained" of the first measure (24.5 to 29.6) was in good part the state of the machine: interleaved against a main built here, the pty cost 2.4 ms, not 5.

What was measured and tried:

- **The nested user namespace.** `bwrap --dev` is the only unprivileged way to a devpts of its own, and it makes the view in two user namespaces (it needs to be uid 0 to mount the devpts, then enters a second one where the app has its own uid again): `/proc/self/uid_map` is
  `1000 0 1` instead of `1000 1000 1`. The app still has its own uid and no capabilities; `tests/view.py` now expects this form. Passing `--uid`/`--gid` does not avoid it.
- **Where the 3 ms of `--dev` go** (150 interleaved runs of a bare `bwrap`): no `/dev` 4.28 ms; the old bind of the host's `/dev` 4.52; `--dev` 6.37; `--dev` with the GPU folder 6.57; an empty tmpfs with six node binds and no pty 6.41. So the cost is the tmpfs and the six node binds, plus the second
  namespace: the devpts itself costs next to nothing, and there is no cheaper way to a private devpts with bubblewrap. Not done: keeping the host `/dev` and adding only `ptmx` (it shares the host's devpts, with the user's terminals: refused in phase 2); giving the inner stub `CAP_SYS_ADMIN` to mount a devpts itself (it would have capabilities
  for a moment in front of the app, to save about 2 ms: not worth the risk).
- **What was taken out instead.** Profile of the stages (`PANPAK_DEBUG`): checking the stub (BLAKE3 of 3.2 MB) took 5 ms. The hash is 0.56 ms; the rest was allocating and faulting in a buffer of the size of the stub and one `read`. Hashing in pieces of 64 KiB takes 1.4 ms with the same hash.
  The answer of `gsettings` (two processes started at the beginning, about 6 ms) was then the longest wait, so it is collected after the Wayland, D-Bus and shared-folder work (as late as the environment of the app allows) and the wait overlaps it. Total: 25.3 ms with the pty, 0.7 ms above main.
- **Chosen:** keep `--dev` (the pty always available), and the two changes above.

**qBittorrent from source, 0.31 s to 0.41 s** (median of 5 starts, first left out, 4 interleaved series each): the **same tree packed with the stub of main and with the stub of the branch gives 0.414 s and 0.415 s**, so it is not the runtime. The same tree with the SVG plugins taken out (no `libqsvg`, no `libqsvgicon`) gives 0.311 s, and with only the portal theme taken out 0.415 s.
The 0.1 s is qBittorrent drawing its SVG icons, which it could not do when the plugins were missing (and the tray icon was empty, F25). It is the cost of working correctly, not a regression.

### 6.3 F29: opening a link from the shell of Ghostty

`xdg-open https://example.org` typed in the shell of the sandboxed Ghostty (lab with a fake `xdg-open` on the outer PATH that logs its arguments): **it goes through the opener channel**: the shim in the view (`$XDG_RUNTIME_DIR/panpak/bin/xdg-open`) hands the address to the outer stub, which ran the outer program, and the fake got `https://example.org` (exit 0).
`xdg-open file:///etc/passwd` is refused in the view ("scheme file is not allowed (only http, https and mailto)", exit 1). The click on a link in the terminal (GTK `show_uri`) is still **not verified**: the injected Ctrl+click did not reach Ghostty. The filter has no rule for the `OpenURI` portal, so a GTK app that opens links
that way probably cannot; nothing was changed.

### 6.4 `panpak build` on the three recipes

All three were built with `panpak build` from the recipes of 3.2.1 (no script, no wrapper) in the cache `XDG_CACHE_HOME`, every build in a scope with a memory cap, one at a time. The first build of a recipe includes making the image.

| App | First build (image, source, build) | Warm build | Peak memory of the build | Package | Libraries | `glibc_min` |
|---|---|---|---|---|---|---|
| Text Editor | 62 s | 11 s | 0.5 GB | 52.7 MB | 106 (101 MB) | 2.39 |
| qBittorrent | 283 s | 235 s | 2.8 GB | 60.5 MB | 83 (104 MB) | 2.39 |
| Ghostty | 211 s | 172 s | 2.1 to 2.7 GB | 56.9 MB | 101 (66 MB) | 2.39 |

- **Same as part A.** Text Editor: the 221 installed files are identical to part A. qBittorrent: the 31 installed files are identical. The libraries are the same except `libwayland-client` and `libwayland-egl`, which the host provides since phase 2.
  Ghostty: 540 of 542 installed files are identical; **the `ghostty` executable differs from part A** (31,519,440 bytes in both, 721,298 bytes different), and so does `libghostty-vt.so`, which is not packaged (it differed between two builds of part A as well). The executable is the same in every `panpak build`
  (two builds, and a third with the source mounted at another path), and it does not change with two more packages in the image (part A's image had `glslang-dev` and `libspirv-cross-c-shared-dev`). **The cause was not found.** What is left untested: the path of the Zig package cache, and the modification times
  of the source files (part A unpacked the tarball; the build copies the files). Epoch, flags, Zig and the source hash are the same.
- **Reproducible.** Each recipe was built twice and the two packages compared: **the three packages are identical byte for byte** (Text Editor `5e79681e…`, qBittorrent `4035600387…`, Ghostty `b2ed7346…`).
- **Behavior is the same as part A.** Text Editor (portal color scheme live, icons through the pixbuf loader without a wrapper), qBittorrent (tray registration reaches the watcher; dark at start, no live switch), Ghostty (shell, colors, copy and paste, shader 187/23/29): same results.

What the build found while it was being written (all fixed): the peak memory in `[provenance]` made two builds differ, so it is shown and not written; `ureq` tries only the first address it resolves and stalled on an unreachable IPv6 one (IPv4 only, 15 s to connect); the reader of ELF files did not look at `RUNPATH`
(libproxy's backend in a folder next to it; `ldd` did); Zig wants `-j3`, not `-j 3`; the Qt table pointed at the root of the plugins and not at each category (the check "a linked module needs its plugin" caught it); `desktop-file-utils` had to be a base tool of the image.

### 6.5 Open points

- **`[env]` and old runtimes.** A package with `[env]` run by a runtime that does not read it starts without the variables (readers ignore what they do not know): the app would look broken. The remedy is the step 9 note "critical manifest fields with a minimum version"; not done.
- The `[env]` table could also carry `GDK_PIXBUF_MODULE_FILE` only because the loader modules are named by file name in the cache and listed in `abi.needs` (the farm of the runtime links them); a cache with absolute paths would need the tree at a fixed place.

## 7. Risks and open points

- **F22 and F23 are the first thing to decide** (section 4). They exist today for any app whose panpak id equals an application
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
- Not done: the dock check (F24), notifications actually delivered by qBittorrent, opening a link from Ghostty (F29: the test could not click it), the
  minimize and maximize buttons, `linuxdeploy` run side by side,
  Podman measured, Zig output with a different path or CPU count.
