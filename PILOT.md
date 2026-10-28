# Pilot: current cycle, 26 October 2026

This result supersedes the conclusions of the historical table below.
A live process or a window is not proof of complete use.

## Current method

`tests/pilot_cycle.py` reuses the ten converted pilot payloads, re-signs
with a test-only key and explicitly enables X11 in the test manifests.
HOME/store are isolated; dedicated Xvfb display, no input or screenshots
on the user desktop. Previous `allow_missing` entries remain as
documented below. No sandbox rule added to obtain the saves;
destinations inside the app's writable data.

```sh
python3 tests/pilot_cycle.py /tmp/panpak-pilot-pkgs target/pilot-current
python3 tests/gui_session.py target/pilot-current start rawtherapee /path/input.png
python3 tests/gui_session.py target/pilot-current screenshot
python3 tests/pilot_saves.py target/pilot-current bench/pilot-saves.json
python3 tests/gui_session.py target/pilot-current stop
```

Xvfb, xdotool, xwininfo and ImageMagick are required. `gui_session.py`
also exposes `windowfocus`, `key`, `type`, `mousemove` and `click` for
the dedicated display. It is not an automatic coordinate replay:
dialogs depend on language and configuration. The validator checks the
produced files, it does not fake a save by writing them from Python.

| App | Current finding | GUI save |
|---|---|---|
| Audacity 4.0.1 | KO, missing SoundTouch symbol: host/bundle incompatibility, same SONAME | No |
| qBittorrent 5.3.0 | Real legal-notice window | Not tried |
| Kdenlive 24.12.3 | Editor with ffmpeg clip in bin and timeline (audio only, see below) | **Yes**, non-empty project validated |
| Krita 5.2.13 | Canvas created and stroke drawn | **Yes**, KRA document |
| Darktable 5.6.2 | Window and welcome with explicit X11 | Not tried in GUI |
| RawTherapee 5.13 | PNG opened, exposure changed +1.91 EV | **Yes**, decodable 640x480 JPEG and PP3 params |
| Bitwarden 2022.5.1 | Login window with `--no-sandbox --disable-gpu` (Electron sandbox off, Landlock on) | Not tried, no account used |
| Joplin 3.7.21 | Working editor like the original (same notes, same v54 DB, full FTS) after SQLite temp fix | **Yes**, parity (see below) |
| OpenShot 4.0.1 | Past X11; thumbnail server uses bind(0)+rebind: not expressible in Landlock (probe § below) | **KO**, no permission added |
| Mu 1.2.0 | 50/50 SIGSEGV in all configs (sandbox, direct tree, original), see below | **KO**, cause isolated: bundled Qt vs host X11 |

Joplin: original-AppImage comparisons showing only a live process
and no error on stdout do **not** prove a working editor with the same
profile. The specific cause of the failure was still undetermined at
that point. Network, remote accounts and updates of the ten payloads
not verified in this pilot; updates are measured separately in
INCREMENTAL-RESULTS.md.

## Joplin: cause isolated, our fix (current cycle)

Rigorous comparison, same machine, fresh profiles, same Xvfb:
direct converted tree (no sandbox) and original AppImage reached a
full editor, notes and FTS (DB v54, 5 notes); the sandboxed run stayed
at DB v34 with 0 notes and FTS warnings. Same engine (SQLite statically
linked), same SQL: `strace -f` shows the `EACCES` denial on
`/var/tmp/etilqs_*` (SQLite temp files). SQLite looks in
`$SQLITE_TMPDIR`, then `/var/tmp` (default-deny in the sandbox):
FTS index creation failed with `CANTOPEN` even though the DB was
writable. It was not the adapter symlink (with a real directory it
failed the same under sandbox) nor the app (the original is identical
except for temp).

Fix (product, minimal blast radius): the stub creates `<appdata>/tmp`
and points `$SQLITE_TMPDIR` there only if unset (never global
`TMPDIR`); fork test (`sqlite_tmp_private_and_var_tmp_denied`) + e2e
(`tests/sqlite_tmpdir.py`). After the fix, standard sandboxed run
(symlink included): zero FTS warnings, DB v54, 5 notes, complete FTS
tables — parity with the original. The non-blocking fallback at
migration v34 remains, also present upstream.

## Mu: 50x3 SIGSEGV measurement (current cycle)

`tests/mu_crash_rate.py` (dedicated Xvfb, Mu profile wiped at every
start, 8 s timeout, same Qt/X11 env in all configs), evidence in
`bench/mu-rate.json`:

- sandbox (launcher via panpak-stub, Landlock on): **50/50 SIGSEGV**,
  median 3.6 s;
- converted tree run directly (same env, no Landlock):
  **50/50 SIGSEGV**, median 3.4 s;
- original AppImage with userspace libfuse: **50/50 SIGSEGV**,
  median 1.8 s.

gdb backtrace (sandbox and original, identical frames): segfault in
`libX11` (`XOpenDisplay` → `XCreateGC`) called from the bundled PyQt5
QXcb integration, during `QApplication` creation (`mu/app.py run`);
faulthandler confirms the crash right after loading the bundled
`libqxcb.so`. In the sandbox the `libX11` comes from the host via the
farm, in the original it is the system one: both crash at the same
place with the same bundled Qt.

**App/environment** cause (2021 Qt5 vs host X11 stack), not Landlock
(the direct run crashes the same) and not farm mix (the original with
system-only libraries crashes identically). No fix on the Panpak side:
apps are not patched, nothing is widened.

## OpenShot: ephemeral-bind probe (current cycle)

`classes/thumbnail.py::find_free_port` does `bind(('', 0))`, reads the
assigned ephemeral port, closes; then the HTTP server binds
`('127.0.0.1', port+attempt)` for attempt in 0..2. Probe with a minimal
package (same pattern) on kernel 7.0.0-38, Landlock **V8**:

- default and `network = true` without ports: `bind(0)` denied (EPERM);
- `network = true` + `network_bind_tcp = [0]`: `bind(('', 0))`
  succeeds (e.g. port 44329), but rebinding the assigned port stays
  denied; the port-0 rule only covers the 0 request;
- `network = true` + `network_bind_tcp = [18081]`: positive control,
  only that port binds, `bind(0)` denied.

Ephemeral ports cannot be expressed with discrete allowlists: no
permission added (a 0 rule would be useless and misleading), network
not widened. KO also documented in THREAT-MODEL.

## Kdenlive: non-empty project (current cycle)

`clip-cycle.mp4` generated with ffmpeg (testsrc 320x240 10fps + 440Hz
sine, 5 s, 66 KB) directly in appdata (outside the sandbox, not in
`/tmp`, readable per-app area). GUI flow on dedicated Xvfb:
crash-recovery dialog reset, file dialog on unreadable `$HOME` closed
with Escape (non-blocking), Add clip with absolute path, 320x240/10fps
clip profile confirmed, timeline insert.

Honest result: the timeline holds the clip only on audio track A1 (two
instances); V1 stays empty despite the enabled track and the tried
methods (stepped drag, v/V/b keys, Insert/Overwrite menu). Cause not
isolated (source patching or profile constraint).
`tests/kdenlive_clip.py` validates `saved-clip.kdenlive` (20654
bytes): `mlt` root, two `chain`s with relative `clip-cycle.mp4`
resource existing and readable, 3 playlist/timeline references, out
00:00:04.960; evidence in `bench/kdenlive-clip.json`.

Beyond the file: reopen in a second Panpak run (bin with `[2]`, A1
blocks present, no error dialog or missing clip); render with the
tree's `melt` (outside the sandbox, documented): `render-clip.mp4`
12.78 s, 1920x1080 video (black: empty V1) + audio. Video track on V1
not obtained: not proof of video editing. Failed autosave dialog and
`$HOME` dialog are known sandbox noise (data only in appdata), not
project errors.

## Three real saves

Evidence with sizes/hashes in `bench/pilot-saves.json`, recheckable
with `tests/pilot_saves.py` while the local files exist:

- Krita: `saved-cycle.kra`, 191030 bytes; intact ZIP, Krita mimetype,
  document XML and composited image present. Ctrl+N, canvas creation,
  mouse stroke, Ctrl+Shift+S and absolute destination in app data.
- Kdenlive: `saved-cycle.kdenlive`, 12564 bytes; XML with `mlt` root.
  Initial dialogs closed, Ctrl+Shift+S and project name. Empty project:
  not proof of editing/exporting video. See above for
  `saved-clip.kdenlive` (non-empty project with clip).
- RawTherapee: `saved-cycle.jpg`, 10983 bytes; decoded 640x480 JPEG.
  Input generated with `convert -size 640x480 gradient:navy-orange input-cycle.png`
  inside appdata; open from argument, Exposure panel, slider to
  +1.91 EV, save-image button bottom left, name `saved-cycle`,
  quality-92 JPEG, confirm. Attached PP3 verifies the actual edit.

Explicit adapters: conventional symlinks into appdata for Kdenlive and
Krita (and for the apps listed in the script); Krita also needs
`PYTHONHOME=<tree>/usr` for the bundle's Python encodings. Short
runtime dir avoids the 108-byte sockaddr_un limit. These workarounds are
part of the pilot setup, not converter automatisms.

## Archive of the previous pilot (superseded conclusions)

The lines below are kept as history: in particular the old
"runs/OK", the presumed Bitwarden crash parity and "GUI save not
automatable" are not the current cycle's conclusions.

Method: conversion with `panpak convert` (+ explicit
`--entry`/`--allow-missing` where needed), `panpak install --yes`,
sandboxed run. Control: same app as original AppImage (userspace FUSE)
on Xvfb (`DISPLAY=:99`, windows verified with `xwininfo`) and headless.
Panpak GUI runs on native Wayland (filesystem socket, `connect()` not
mediated); X11 is blocked by the Landlock scope (by design): apps that
*require* X11 cannot start under Panpak in this environment. Every KO
is classified: **our bug** (fixed here), **model limit** (documented,
not worked around), **unsuitable app**, **environment**.

| app (toolkit) | conv. | panpak start | basic use | save | network | update | outcome / cause / class |
|---|---|---|---|---|---|---|---|
| Audacity 4.0.1 (wx/Qt) | OK (`--entry bin/audacity4portable`) | KO: `SoundTouch ... setPitchSemiTones` not found (host 1.x vs bundle 2.x, same SONAME) | — | — | — | n/a | KO: **model limit** (implicit version at equal SONAME; needs version-aware resolution or prefer-bundled) |
| qBittorrent 5.3.0 (Qt6) | OK (+pxbackend) | OK both, no window (tray app, same) | runs | n/a | default deny (DHT untested) | n/a | **OK with allow_missing**; AppImage parity |
| Kdenlive 24.12.3 (Qt/KDE) | OK (+pulsecommon) | OK on Wayland (alive+clean; window not verifiable headless) | runs | n/a GUI | default deny | n/a | **OK after our fixes** (transitive closure, atomic Qt, legal internal `../` symlinks) |
| Krita 5.2.13 (Qt) | OK (+pulsecommon) | KO: forces xcb, X11 blocked | — | — | — | n/a | **unsuitable app + model limit** (needs X11; AppImage OK on Xvfb) |
| Darktable 5.6.2 (GTK) | OK (+pxbackend, +pulsecommon) | OK (alive+clean) | OK: `darktable-cli` converts PPM->JPG | YES via CLI | default deny | n/a | **OK**; pickup: convert refused `.panpak`? no — all smooth |
| RawTherapee 5.13 (GTK) | OK (zero allow_missing) | OK (alive+clean; benign dconf warning) | runs; CLI --help OK | n/a GUI | default deny | n/a | **OK** |
| Bitwarden 2022.5.1 (Electron) | OK (+3 AM dbus) | identical crash both (SIGTRAP, old Electron on new system) | — | — | — | n/a | KO same both: **environment/dated app**, not a Panpak regression |
| Joplin 3.7.21 (Electron) | OK (+dbus-glib) | OK with `~/.config/joplin-desktop` symlink shim (hardcoded path) | runs | n/a GUI | default deny | n/a | **OK with documented shim** |
| OpenShot 4.0.1 (Python+Qt) | OK (+10 AM Qt/plugin) | KO: forces xcb like Krita | — | — | — | n/a | **unsuitable app + model limit** (AppImage OK on Xvfb) |
| Mu 1.2.0 (Python+Qt) | OK (+20 AM; our fix: Exec args and `$ORIGIN`) | OK GUI Wayland (lives+exits cleanly; venv created with network=true) | runs; first-start wizard OK | n/a GUI | needs network for venv (dedicated build) | n/a | **OK after our fixes** |
| VSCodium | — | — | — | — | — | — | **unsuitable**: DwarFS AppImage, convert supports only squashfs v4 |
| Java (JabRef, HMCL, GanttProject, ...) | — | — | — | — | — | — | **coverage gap**: no official AppImages found |

KO details and our fixes (in this cycle):
- **Our bug (fixed)**: transitive farm closure (AIL would not start);
  atomic Qt/KF with all-host fallback (Kdenlive); internal `../`
  symlinks wrongly refused (Kdenlive); `--allow-missing` flag not
  accumulated? no — that was my script; `$ORIGIN` in DT_NEEDED (Mu);
  `Exec` arguments dropped (Mu).
- **Model limit**: host/bundle mix at equal SONAME with no version
  (SoundTouch/Audacity); X11 blocked by scope (Krita, OpenShot);
  absolute symlinks always refused (correct).
- **Unsuitable app**: VSCodium/DwarFS; Krita/OpenShot forcing xcb;
  Java with no official AppImages available.
- **Environment**: old Bitwarden crashes the same everywhere; no real
  GPU here (shader/rusticl cache untested).

Allow_missing per app (explicitly declared, never glibc):
- kdenlive/krita: `libpulsecommon-17.0.so`
- darktable: `libpxbackend-1.0.so`, `libpulsecommon-17.0.so`
- bitwarden/joplin: `libdbus-glib-1.so.2` (+gtk-x11/dbusmenu for bitwarden)
- openshot: 10x Qt/plugin (`Designer`, `EglFSDeviceIntegration`, `Help`,
  `Network`, `OpenGL`, `PrintSupport`, `Test`, `WaylandClient`, `Xml`, `libmpd…`)
- mu: 20x Qt3D/EGLFS/Gamepad/Media/… + `libodbc`, `libpq`, `libpulsecommon`
- qbittorrent: `libpxbackend-1.0.so`; rawtherapee: none;
  audacity: `libpxbackend-1.0.so` (then KO for SoundTouch, see above).

Notes: `panpak open <id> <file>` (double click) tested during the pilot
for inspections; GUI save not headless-automatable (needs
interaction): N/A with reason where not runnable via CLI.
