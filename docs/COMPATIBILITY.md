# Compatibility tests with real AppImages

Status: partial run (step 4 of the plan). Build under test: branch `app-tests`, X11 on Xvfb only
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
| Jitsi Meet 2026.10.0 | Electron | works | 2.38 s / 0.69 s | needs `allow_missing` for `libdbus-glib-1.so.2`, `libffmpeg.so`, `libindicator3.so.7`; no meeting, camera or microphone tried |
| Obsidian 1.14.5 | Electron | works | 2.56 s / 0.77 s | same `allow_missing` family |
| Zulip 5.13.2 | Electron, tray | works | 1.84 s / 0.80 s | same `allow_missing` family; no account |
| Neovim 0.12.6 | terminal | works | 0.54 s / 0.19 s (`--version`) | writes a file in the `documents` folder |
| Nextcloud 35.0.1 | Qt, tray | convert fails | | defect 1 |
| PrusaSlicer 2.8.1 | GTK 3 | convert fails | | defect 1 |
| Cura 5.13.0 | Qt 6 | convert fails | | defect 1 |
| FreeCAD 1.1.4 | Qt 6 | convert fails | | defect 1 |
| GIMP 3.2.6 | GTK 3 | convert fails | | defect 1 (`libLLVM`); entry needs `--entry usr/bin/gimp-3.2` |
| balenaEtcher 2.1.3 (prerelease) | Electron | convert fails | | defect 1 (`etcher-util`); entry needs `--entry`; root case not reached |
| VSCodium 1.143 | Electron | not converted | | "anylinux" AppImage is DwarFS, convert reads squashfs only (defect 6) |
| LMMS 1.2.2 | Qt 5 | fails | | image does not carry Qt 5 or `libFLAC.so.8`; host has none |
| OpenShot 4.0.1 | Qt/PyQt | fails | | image expects host libs not present (Qt5Designer, ImageMagick 6, OpenEXR 2.3) |
| Shotcut 26.9.27 | Qt 6 | fails | | many Qt 6 libs and `libCuteLogger.so` reported missing (defect 5) |
| OrcaSlicer 2.4.2 | GTK 3, GL | fails | | needs `libGLU.so.1`, not installed on the host |
| Bitwarden 2026.10.2 | Electron | fails | | converted entry is `AppRun`, which needs `APPDIR` (defect 7) |

Not done in this run: D-Bus logs for most apps, usage tests beyond start, checks of the
Docker/LXD socket probe, AppImage updater and Electron sandbox notes, tree/package sizes per app.
The D-Bus log with `xdg-dbus-proxy --filter --log` on a private bus was taken for MuseScore (asks
for `org.freedesktop.portal.Desktop` Settings, `org.a11y.Bus`, `org.gtk.vfs.Daemon`), PCSX2
(`org.a11y.Bus`) and qBittorrent (`org.freedesktop.Notifications`, `org.kde.StatusNotifierWatcher`,
`org.gnome.SessionManager`, `org.freedesktop.PowerManagement`).

## Signatures

- Nextcloud: valid, key 2880 6A87 8AE4 23A2 8372 792E D758 99B9 A724 937A (nextcloud.com).
- qBittorrent: valid, key D8F3 DA77 AAC6 7410 5359 9C13 6E4A 2D02 5B7C C9A2 (project README).
- Krita: valid, key E9FB 29E7 4ADE ACC5 E303 5B8A B69E B4CF 7468 332F (file linked from the release announcement).
- Cura: signed by C1A1B91069C4AF59; no official page with the key was found, not verified.
- MuseScore, Shotcut, OpenShot, FreeCAD, Etcher: sha256 from the author's file matched. Others: only a hash computed here.

## New defects (proposed priority, plan task)

1. High, convert/stub: `.dynstr` larger than 1 MiB is refused ("strtab too large"). Blocks Nextcloud, PrusaSlicer, Cura, FreeCAD, GIMP, Etcher (6 of 20).
2. High, symbol check: symbols that the loader provides (`__tls_get_addr`, `_rtld_global_ro`, `GLIBC_PRIVATE`) are reported undefined when the loader is not in the needed list; `doctor` shows KO and every start prints a warning.
3. High, view/PID namespace: process ids repeat in every view, so lock files left by a kill (qBittorrent `lockfile`) are never seen as stale and the app does not start again.
4. Medium, convert: absolute symlinks are accepted at convert but refused at first start (`symlink escapes root`).
5. Medium, resolution: libraries in directories that only `AppRun` adds to `LD_LIBRARY_PATH`, or `$ORIGIN` neighbours (`libffmpeg.so`), are reported missing.
6. Medium, convert: DwarFS AppImages unsupported; entry guessed from `Exec=` in `usr/bin` only, symlinks not followed (MuseScore, GIMP, Etcher).
7. Medium, convert: `AppRun` shell scripts as entry lose `APPDIR` (Bitwarden).
8. Low, convert: name and version default to the desktop name and `1.0`.
9. Low, `doctor` reports only the first unresolved library.

## D-Bus summary for step 5

Too few apps were logged to count rules. Seen so far: Settings portal (1 of 3), accessibility bus
(3 of 3), `org.freedesktop.Notifications` and StatusNotifier (qBittorrent), session manager and
power management (qBittorrent).
