# Application compatibility

Results of running converted AppImages under panpak. All tests ran with an
isolated HOME and XDG directories, a private Xvfb display and the Landlock
sandbox on. Dates are the day of the test.

Status values:

- works: starts and is usable with no extra step.
- works with notes: works after a step listed in the notes.
- not working: does not start or is not usable.

| App | Version | Status | Notes | Tested |
|---|---|---|---|---|
| Joplin | 3.7.21 | works with notes | Needs `launch.args` for the profile, the network ports for 2658 and `--allow-missing libdbus-glib-1.so.2`. See "Joplin" below. | 2026-10-28 |
| Simplenote | 2.27.1 | works with notes | Needs `APPDIR` and `APPIMAGE` (the `convert` wrapper for `Exec=AppRun --no-sandbox` does not set them), and a short path to the app folders: see "Simplenote" below. | 2026-10-28 |
| QOwnNotes | 26.10.9 | works with notes | Note folder `~/Notes` needs a `home_paths` entry (`rw`); 25 Qt6 Quick libraries had to be allowed as missing. | 2026-10-28 |
| Kdenlive | 24.12.3 | works | Starts, creates a project; reaches PipeWire and PulseAudio sockets. Needs a working directory the sandbox can enter. | 2026-10-28 |
| Mu | 1.2.0 | not working | Crashes with SIGSEGV in `QApplication(...)`, also without panpak. See "Mu" below. | 2026-10-28 |

Simplenote, QOwnNotes and Kdenlive were run for 30 seconds each in Xvfb with the socket probe (docs/SOCKET-PROBE.md); that is a start and light use, not a full test.

Apps get private config, data, state and cache folders (see `panpak data path <id>`), so a profile written by an older panpak in `~/.local/share/<id>` is not used any more.

## Joplin

Joplin 3.7.21 opens its main window (1024x720) in 30 of 30 starts with an
existing profile, in 30 of 30 starts with a new profile at every start, and in
10 of 10 starts without the `--no-sandbox` and `--disable-gpu` options. The
sandbox was on in all runs. Three things need a step.

### Configuration directory

With the private app folders, Joplin needs no link and no home path. The package
sets the profile with `--profile`, which Joplin accepts:

```toml
[launch]
args = ["--profile", "${XDG_CONFIG_HOME}/joplin-desktop"]
```

Ten starts in an isolated HOME with no adapter link gave a window every time,
the profile was created in the private config folder and nothing was created
under `~/.config`. The text below explains why the older versions needed a link.

Joplin stores its profile in `~/.config/joplin-desktop`. It builds this path
from the home directory and ignores `XDG_CONFIG_HOME`. The sandbox allows
writes only in the app data directory, so the start fails with
`EACCES: permission denied, mkdir '.../.config/joplin-desktop'`. The test
scripts create a symbolic link from that path to a directory inside the app
data directory. Starting Joplin with `--profile <directory in app data>` is
the other way.

### Library libdbus-glib-1.so.2

The only user of `libdbus-glib-1.so.2` is the bundled `libgconf-2.so.4`, which
the main executable does not link. Hosts without the library fail at start with
`PANPAK-E2000` unless the package is converted with
`--allow-missing libdbus-glib-1.so.2`. With the option, Joplin starts.

### Port 2658 (with the private folders)

Joplin opens a local server on `127.0.0.1:2658` for its IPC. Without network permissions the sandbox denies both the connect and
the bind, and the log shows `listen EACCES: permission denied 127.0.0.1:2658`.
The start is not blocked. With this manifest the error is gone and the server
starts:

```toml
[permissions]
network = true
network_allow_tcp = [2658, 443]
network_bind_tcp = [2658]
```

Landlock filters the TCP port, not the address.

### Start with no window after the app was ended

A start that ends at once with exit code 0 and no window, in about one start
of two, comes from Joplin and not from panpak. Joplin keeps a `lock` file in
its profile directory while it runs. If the instance is ended with SIGKILL or
SIGTERM, the file stays. The next start finds it, logs `[appClose] quit()
called`, removes the file and exits. The start after that works. Starting the
installed tree directly, with no panpak, shows the same sequence (start, exit,
start, exit). Removing the `lock` file before the start avoids it. The test scripts stop
apps with SIGTERM, wait for the exit and use SIGKILL only for what is left;
a script that restarts Joplin on the same profile must remove the file first.

## Simplenote

Simplenote (an Electron app) died with SIGTRAP at start when `TMPDIR` was a long path. Chromium makes
`$TMPDIR/scoped_dirXXXXXX/SingletonSocket`, a Unix socket path, and the limit is 107 bytes. With
`TMPDIR=<home>/.local/share/panpak/userdata/<id>/tmp` the path is 103 bytes for a user name of 8 characters
and `org.example.simplenote`, so a user name of about 13 characters or more, or a longer app id, fails.
A short home path was used for the test. The private `tmp` folder should have a shorter path (for example
under `$XDG_RUNTIME_DIR`) before apps like this are called working. The `AppRun` of this AppImage also
looks for its folder from its first argument, which `--no-sandbox` from the `Exec` line replaces:
set `APPDIR` and `APPIMAGE` to the tree.

## Mu

Mu 1.2.0 does not start: the process dies with SIGSEGV at the call
`QApplication(sys.argv)` in `mu/app.py` (Python's faulthandler shows line 322),
right after Mu logs that it found no `venv.json`. The same crash happens, with
a new profile in an isolated HOME and the same Xvfb display, in all three ways
tested: through panpak (3 of 3 starts), from the extracted tree with no sandbox
(4 of 4) and from the original AppImage with no panpak at all (4 of 4). So it
is not caused by panpak or by its sandbox. The package set `IPYTHONDIR` to the private config folder and a `mu_code` home path (read
and write). The home path was created and Mu looked for its settings in the private data
folder, but `IPYTHONDIR` could not be exercised because Mu crashes before it opens the
editor, so the application never shows a window. Missing libraries
are still handled with `--allow-missing` (48 libraries).

## GTK3 dark theme

GTK3 apps read the dark preference from dconf, which they cannot reach with a
private config folder. The stub reads it once at start with `gsettings` (at most
200 ms) and sets `GTK_THEME=<theme>:dark`. It costs about 4 ms with the real
`gsettings` (two queries at the same time, 40 runs, median; p95 5 ms). The theme does not change while the app is
open. The settings portal is planned to replace this.

## Update size

`panpak update` fetches the new index and only the chunks that the installed
package does not have. This section compares that with downloading the new
package or the new AppImage.

| App | Update | Delta | New package | New AppImage | Delta / package | Delta / AppImage | Chunks fetched |
|---|---|---|---|---|---|---|---|
| QOwnNotes | 26.10.8 to 26.10.9 | 5,280,898 | 67,413,526 | 62,347,768 | 7.8% | 8.5% | 151 of 3,200 |
| Simplenote | 2.27.0 to 2.27.1 | 1,731,921 | 155,482,783 | 147,197,267 | 1.1% | 1.2% | 78 of 5,419 |
| Joplin | 3.7.18 to 3.7.21 | 11,274,083 | 222,972,773 | 201,462,481 | 5.1% | 5.6% | 403 of 7,398 |

All sizes are in bytes. Joplin 3.7.18 is the stable release before 3.7.21; the
releases 3.7.19 and 3.7.20 do not appear in its release list.

How it was measured:

1. Each AppImage was converted with `panpak convert` (same app id and a test
   key for both versions of an app).
2. The old package was installed in an isolated HOME. Its manifest and the
   manifest of the new package were re-signed to point `[update].url` at a
   local HTTP server on `127.0.0.1`.
3. The server is `tests/range_server.py` in normal mode. It serves the new
   package with HTTP Range support.
4. `panpak update --allow-insecure-http <app-id>` ran with `PANPAK_RANGE_LOG`
   set. The runtime writes one line per request with the number of body bytes
   received. The delta is the sum over all requests (6 for QOwnNotes, 12 for
   Simplenote, 61 for Joplin). It includes the footer, the index, the manifest
   and the signature, and it does not include HTTP headers.
5. After the update, the installed package was compared byte for byte with the
   new package, and the chunks fetched were counted by comparing the chunk
   lists of the two packages.

The figure of a full AppImage download is the file size. Neither number
includes any compression done by the transport.

### Files measured

| File | SHA-256 |
|---|---|
| Joplin-3.7.18.AppImage | c7ed7eeb6985621b75f0d09088cd01efc9af7aa2cfa17649ed4a83a75b29aca5 |
| Joplin-3.7.21.AppImage | 5d202dcaca31e14206826aa3f04125e7c218dbc3dc3d98479d811efbd6b856c1 |
| QON-v26.10.8-QOwnNotes-x86_64.AppImage | c52b09c43101fec06664e3b701c99c6415c2b74aae5957453b0078bb06857d6c |
| QON-v26.10.9-QOwnNotes-x86_64.AppImage | 56c407e3a8c614afc4a0d6af1a511a2a7bec09326533dafbd2ec42d16c940be5 |
| SN-Simplenote-linux-2.27.0-x86_64.AppImage | 1d7dee1959c253335e2d6833d2006d7e1ede7e708208c8cd2f144f11dce7b0eb |
| SN-Simplenote-linux-2.27.1-x86_64.AppImage | 91544f51e3c2553edaf8ea37ab0d4083451e636b2d6a74dc3e654a8e333d3123 |

Joplin 3.7.18 comes from the Joplin GitHub release page, and its SHA-256 and the
published SHA-512 both match. The other files were already in the test
directory; their origin was not checked again.
