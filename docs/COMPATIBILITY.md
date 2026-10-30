# Compatibility tests with real AppImages

Status: step 4 of the plan, second round. Build under test: branch `fix-batch` (fixes for defects 1, 2 and 10 applied; defect 3 open), first round on `app-tests`. X11 on Xvfb only
(no headless Wayland compositor was available), isolated HOME, one app at a time. Converted
packages are local and are not redistributed. Times are the first window that appeared (it can be
a splash or a first-run dialog); "first" is the first start after install, "later" the median of 5.

## Discovery note: GTK 4

No app with an AppImage published by its author was found for GTK 4. Looked at: Czkawka (raw
binaries only), Gaphor, Foliate, Rnote, Lutris, Anki, Zrythm, Pinta, Ghostty (no AppImage);
the AM list only has unofficial or very small GTK 4 apps. GTK 4 is left for the "Flathub without
Flatpak" phase. Inkscape's site was unreachable, so it was not added.

## Results

| App | Type | Result | Window first / later | Notes |
|---|---|---|---|---|
| MuseScore Studio 4.7.6 | Qt 6 | works | 0.46 s / 0.48 s | needs `--entry bin/mscore4portable`; absolute symlink in the image had to be removed by hand; has an internal AppImage updater; no sound server in the lab (ALSA error) |
| PCSX2 2.8.2 | Qt 6, Vulkan/GL | works (setup wizard) | 1.25 s / 0.32 s | no game or BIOS tried |
| qBittorrent 5.3.0 | Qt 6, tray | works | 1.36 s / 0.42 s | legal notice pre-accepted in config; stale `lockfile` after a kill (defect 3); a second copy given a `magnet:` address handed it over and exited 0 |
| Krita 6.0.4 | Qt 6 | works | 6.06 s / 1.1 s | |
| Jitsi Meet 2026.10.0 | Electron | works | 2.38 s / 0.69 s | second copy given `jitsi-meet:` exited 0, first stayed; needs `allow_missing` for `libdbus-glib-1.so.2`, `libffmpeg.so`, `libindicator3.so.7`; no meeting, camera or microphone tried |
| Obsidian 1.14.5 | Electron | works | 2.56 s / 0.77 s | same `allow_missing` family; second copy given an `obsidian:` address exited 1 |
| Zulip 5.13.2 | Electron, tray | works | 1.84 s / 0.80 s | same `allow_missing` family; no account |
| Neovim 0.12.6 | terminal | works | 0.54 s / 0.19 s (`--version`) | writes a file in the `documents` folder |
| Nextcloud 35.0.1 | Qt 6 + KF6, tray | converts now; does not start | | 39 `allow_missing` libraries, the image expects KDE Frameworks 6 of the host (`KZip`, `libKF6*`); D-Bus: `RequestName`, `Activate` |
| PrusaSlicer 2.8.1 | GTK 3 | works | 1.37 s / 0.33 s | convert now passes; 105 MB / 249 MB |
| Cura 5.13.0 | Qt 6 | converts now; does not start | | PyInstaller bundle: `GLIBC_2.38 not found` from the `libc.so.6` of the farm (defect 12) |
| FreeCAD 1.1.4 | Qt 6 | converts now; install refuses | | `abi.needs` holds a name the install validation rejects (defect 13) |
| GIMP 3.2.6 | GTK 3 | works (with `--entry usr/bin/gimp-3.2`) | 4.26 s / 0.64 s | convert now passes (`libLLVM`); 39 `allow_missing`; 278 MB / 824 MB |
| balenaEtcher 2.1.3 (prerelease) | Electron | converts now; aborts | | Chromium: `The SUID sandbox helper binary was found, but is not configured correctly`; needs `--no-sandbox` (defect 14); root case not reached |
| VSCodium 1.143 | Electron | not converted | | "anylinux" AppImage is DwarFS, convert reads squashfs only (defect 6) |
| LMMS 1.2.2 | Qt 5 | fails | | image does not carry Qt 5 or `libFLAC.so.8`; host has none |
| OpenShot 4.0.1 | Qt/PyQt | fails | | image expects host libs not present (Qt5Designer, ImageMagick 6, OpenEXR 2.3) |
| Shotcut 26.9.27 | Qt 6 | fails | | many Qt 6 libs and `libCuteLogger.so` reported missing (defect 5) |
| OrcaSlicer 2.4.2 | GTK 3, GL | fails | | needs `libGLU.so.1`, not installed on the host |
| Audacity 4.0.1 | Qt 6 | works (with `--entry bin/audacity4portable`) | 1.52 s / 0.70 s | 29 `allow_missing`; 87 MB / 210 MB |
| Kdenlive 24.12.3 | Qt 6 (KDE) | works | 3.78 s / 1.34 s | 39 `allow_missing`; 224 MB / 630 MB |
| QOwnNotes 26.10.9 | Qt 6 | works | 1.42 s / 0.39 s | 34 `allow_missing`; 68 MB / 171 MB (failed in round 1 with the cap of 8 libraries) |
| Simplenote 2.27.1 | Electron | works (with `--entry simplenote`) | 2.25 s / 1.47 s | default entry `AppRun` fails (defect 7); 156 MB / 473 MB |
| Joplin 3.7.21 | Electron | starts, no window in Xvfb | | process and a window class `Joplin` appear but no viewable window within 90 s; same in round 1 |
| Bitwarden 2026.10.2 | Electron | works (with `--entry bitwarden`) | 2.12 s / 0.76 s | default entry `AppRun` fails (defect 7); `allow_missing` for 6 libraries incl. `libc.musl-x86_64.so.1`; a second copy given a `bitwarden:` address exited 0, first copy stayed; no login tried |

Not done: usage tests beyond start (no GUI input), AppImage updater notes (only MuseScore bundles
`appimageupdatetool`), no Wayland compositor. Electron apps print `Failed to connect to the bus` when
the session bus is not allowed and run anyway; only Etcher stops, on the Chromium sandbox.
Start cost of the new view, 30 alternating runs of `nvim --version` through the launcher: 33 ms median
with the new view and 36 ms with the old one (both with `ldconfig` in `PATH`), so the narrower root costs
nothing measurable. With `/sbin` missing from `PATH` the old build took 135 ms (inventory scan on every
start, defect 2) against 35 ms now.

## Sizes (package / installed tree)

MuseScore 219 MB / 942 MB; PCSX2 70 MB / 186 MB; qBittorrent 127 MB / 305 MB; Krita 435 MB / 1100 MB;
Jitsi 140 MB / 336 MB; Obsidian 149 MB / 360 MB; Zulip 135 MB / 308 MB; Neovim 16 MB / 36 MB; PrusaSlicer 105 MB / 249 MB;
GIMP 278 MB / 824 MB; Audacity 87 MB / 210 MB; Kdenlive 224 MB / 630 MB; QOwnNotes 68 MB / 171 MB;
Simplenote 156 MB / 473 MB; Joplin 224 MB / 641 MB; Cura 356 MB / 943 MB; Etcher 165 MB / 407 MB.

## Socket probe (view)

Round 1: sockets of Docker, containerd, podman, snapd and libvirt below `/run` and `/var/run` were not
visible, and a listening socket made in `/var/tmp` was reachable from the view (Landlock ABI 8 does not
restrict path sockets). Round 2 (`fix-batch`): the root of the view is a new tmpfs with only `/usr`, `/etc`,
`/sys`, `/dev`, `/proc` and the top-level links, so that socket is no longer reachable
(`tests/view.py`), and a scan of the whole visible tree finds no socket in a fake host with sockets in `/var`,
`/opt`, `/srv`, `/mnt`, `/media`, `/root`, `/boot`, `/snap` and the Nix daemon folder
(`panpak-stub/src/view.rs`, test `no_socket_of_the_host_is_reachable_in_the_view`).

## Signatures

- Nextcloud: valid, key 2880 6A87 8AE4 23A2 8372 792E D758 99B9 A724 937A (nextcloud.com).
- qBittorrent: valid, key D8F3 DA77 AAC6 7410 5359 9C13 6E4A 2D02 5B7C C9A2 (project README).
- Krita: valid, key E9FB 29E7 4ADE ACC5 E303 5B8A B69E B4CF 7468 332F (file linked from the release announcement).
- Cura: signed by C1A1B91069C4AF59; no official page with the key was found, not verified.
- MuseScore, Shotcut, OpenShot, FreeCAD, Etcher: sha256 from the author's file matched. Others: only a hash computed here.

## Defects (priority, state, plan task)

Round 1, fixed in `fix-batch` (one commit each):

1. High, convert: `.dynstr` over 1 MiB refused. Fixed: the limit follows the file and the section (cap 1 GiB). Plan: done.
2. High, symbol check: loader symbols reported undefined. Cause: no `ldconfig` in `PATH` (`/sbin` is often missing) and the fallback scan did not list the loader. Fixed. Plan: done.
10. Critical, view: sockets outside `/run`, `/tmp` and home reachable. Fixed: new root with system folders only. Plan: done.

Round 1, open:

3. Critical, view/PID namespace: process ids repeat in every view (the app is always pid 2), so lock files that store a pid (`QLockFile`, Chromium `SingletonLock`, qBittorrent `lockfile`, Electron `LOCK`) are never stale after an unclean exit. Open: `ns_last_pid` is not usable (see below). Plan: step 5 (view), awaiting a decision.
4. Medium, convert: absolute symlinks accepted by convert and refused by the stub at first start. Plan: Task C (converter).
5. Medium, convert/resolution: libraries in directories that only `AppRun` adds to `LD_LIBRARY_PATH`, or `$ORIGIN` neighbours (`libffmpeg.so`), reported missing. Plan: Task C.
6. Medium, convert: DwarFS AppImages unsupported; entry taken from `usr/bin` only, symlinks not followed. Plan: Task C.
7. Medium, convert: `AppRun` as entry loses `APPDIR` (Bitwarden, Simplenote). Plan: Task C.
8. Low, convert: default name and version (`1.0`). Plan: Task C.
9. Low, `doctor` reports only the first unresolved library. Plan: Task C.
11. Medium, same cause as 3 (Electron `LOCK`).
15. Medium, convert/install: AppImages that rely on system libraries that are absent (LMMS, OpenShot, Shotcut, OrcaSlicer, Nextcloud with KF6) must be told clearly at convert and at install, not crash at start. Plan: Task C.

New in round 2, open (not fixed):

12. Medium, resolution: Cura (PyInstaller) gets `libc.so.6` from the farm and fails with `GLIBC_2.38 not found`: a libc of the bundle is chosen next to libraries that need a newer one. Plan: abi resolution (step 4 follow-up).
13. Medium, convert: FreeCAD gives `abi.needs` a name that the install validation refuses (`PANPAK-E0007`); convert and install disagree. Plan: Task C.
14. Medium, Electron: Etcher aborts on the Chromium SUID sandbox (`chrome-sandbox` found, not setuid; user namespaces cannot be nested in the view). Other Electron apps ran without `--no-sandbox`. Needs a launch option or a documented flag. Plan: Task C (launch arguments).
16. Low, harness-visible: the loop that allows missing libraries one by one needs 29 to 39 rounds for Audacity, Kdenlive, QOwnNotes, GIMP, Nextcloud, Cura. Same as 9.

## D-Bus summary for step 5

Apps logged, 18: MuseScore, PCSX2, qBittorrent, Krita, Jitsi, Obsidian, Zulip, Bitwarden, Neovim, Audacity,
Kdenlive, QOwnNotes, Simplenote, Joplin, PrusaSlicer, GIMP, Nextcloud, Cura and Etcher (Cura, Etcher and
Neovim reached no bus). In round 1 the Electron apps did not reach the proxy because the harness did not
set `DBUS_SESSION_BUS_ADDRESS` (a test error, fixed); now they do.

| Name or interface | Apps | Rule proposal |
|---|---|---|
| `org.freedesktop.portal.Desktop` (Settings `Read`/`ReadAll`, Properties) | MuseScore, Krita, Kdenlive, Audacity, Simplenote (5 call it); 8 watch the name | allow `Settings` |
| `org.a11y.Bus` (`GetAddress`, `Properties.Get`) | MuseScore, Audacity, PrusaSlicer, GIMP, Bitwarden, Jitsi, Obsidian, Zulip (8) | allow, or deny quietly |
| `org.gtk.vfs.Daemon` (`ListMountableInfo`) and volume monitors | MuseScore, Audacity, PrusaSlicer, GIMP, Bitwarden, Jitsi, Obsidian, Zulip (8); GIMP also the monitors | deny (GVfs would show host mounts) |
| `org.freedesktop.DBus.StartServiceByName` | 8 | needed by the above, deny |
| `org.freedesktop.systemd1` (name watched) | Bitwarden, Jitsi, Obsidian, Zulip (4) | deny |
| `org.freedesktop.IBus` (name watched) | Bitwarden, Jitsi, Zulip, Simplenote (4) | allow only with an input-method permission |
| `org.kde.StatusNotifierWatcher` | qBittorrent, Zulip, Bitwarden (3) | allow with a tray permission |
| `com.canonical.AppMenu.Registrar` | MuseScore, Audacity, Kdenlive, qBittorrent (4) | deny |
| `org.freedesktop.secrets` | Bitwarden, Obsidian (2) | allow only with a secrets permission |
| `org.freedesktop.Notifications` | qBittorrent (1) | allow with a notifications permission |
| `org.gnome.SessionManager`, `org.freedesktop.PowerManagement` | qBittorrent, GIMP (SessionManager) | deny (inhibit) |
| own name: `RequestName` and `Activate` on its own id | GIMP (`org.gimp.GIMP.UI`), Nextcloud | allow own name only, tied to the id |
| `org.freedesktop.portal.Documents` | GIMP | allow |
| nothing | Neovim, Cura, Etcher | none |

Joplin (no window in Xvfb) asked only for the portal name, `systemd1` and GVfs.

## Defect 3: evaluation of a random first process id

Writing `/proc/sys/kernel/ns_last_pid` in the PID namespace of the view needs `CAP_SYS_ADMIN` or
`CAP_CHECKPOINT_RESTORE` in the owning user namespace. Tested on this machine (Ubuntu, kernel 7.0,
bubblewrap 0.11.1) with `bwrap --unshare-user --unshare-pid --cap-add CAP_CHECKPOINT_RESTORE` and also with
`CAP_SYS_ADMIN`: the capability is present in the process (`CapEff` set) and the write fails with `EPERM`,
because the AppArmor profile `unpriv_bwrap` (`/etc/apparmor.d/bwrap-userns-restrict`) strips the capabilities
of everything bubblewrap starts (documented there: "its children do not have capabilities"). The method
therefore cannot work on Ubuntu 24.04 and later, nor on any system with
`apparmor_restrict_unprivileged_userns`. Also: the process that runs the app is the inner stub itself, which
is pid 2, so it would have to fork the app and wait for it. Alternatives are in the report of this task.
