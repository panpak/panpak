# App harness

`tests/apps/run.py` answers two questions about real programs: does `panpak`
convert or build them, install them and start them, and how long does a start
take. It is a test tool. It never touches the real home of the user: every run
uses an isolated `HOME` under its own work folder, and it stops if that is not so.

## What it does

For each app of a list:

1. **Package.** An AppImage is downloaded (once) and converted with
   `panpak convert`; a recipe is built with `panpak build` (see
   [SOURCE-BUILD.md](SOURCE-BUILD.md)). The `[permissions]` block of the app is
   added and the manifest is signed again with a key that the harness makes
   for itself in its work folder. The package is kept and made again only when
   the AppImage, the recipe, `panpak`, `panpak-stub` or the options of the app
   change.
2. **Install** in the isolated `HOME` (`panpak install --yes`), then check that
   the icon and the menu entry exist.
3. **Start** on a display made by the harness (Xvfb for an X11 app, a headless
   Sway for a Wayland app), wait for the first window of a useful size, end the
   app, and write down the time to that window.

At the end it prints a table, the duration of each phase and the peak memory of
the run. The exit status is 1 when an app did not start.

## Use

```
cargo build --release --target x86_64-unknown-linux-musl
python3 tests/apps/run.py --apps LIST.toml [--full] [--group qt,gtk] [--list] [--no-cache] [name ...]
```

| Option | Meaning |
|---|---|
| `--apps FILE` | The list of apps (also `PANPAK_APPS`). |
| `--full` | A first start, then five more; the result is the median of the five. |
| `--group g,g` | Only the apps of these groups (`kind` of the app, plus `source`, `appimage`, `cli`). |
| `--list` | Print the apps and their groups. |
| `--no-cache` | Make every package again. |
| `--work DIR` | Work folder (default `target/apps-harness`): packages, downloads, runs. |
| `--bin DIR` | Folder with `panpak` and `panpak-stub` (default: the musl release build). |
| `--memory-cap`, `--swap-cap` | `MemoryMax` and `MemorySwapMax` of the systemd scope the run is started in (default 10G and 6G; `--memory-cap none` for no scope). |
| `--json FILE` | Write the results as JSON. |

The tools needed are `bwrap`, `xdg-dbus-proxy` (for apps with D-Bus
permissions), `curl`, `xwininfo` and `Xvfb` for X11 apps, `sway` 1.9 or newer
for Wayland apps. Python 3.11 or newer is needed (TOML).
A build of a recipe needs network access to the registry and to the Debian
snapshot service, and the memory the recipe declares.

## The list of apps

The list is a TOML file that you keep **outside the repository**: it holds the
addresses of programs that are not ours. `tests/apps/apps.example.toml` is an
example with the three source recipes of step 13a (GNOME Text Editor,
qBittorrent, Ghostty) and nothing else. The fields are described at the top of
`tests/apps/appcfg.py`. An AppImage entry looks like this:

```toml
[apps.myapp]
kind = ["qt"]
id = "org.example.MyApp"
url = "https://example.invalid/MyApp-x86_64.AppImage"
sha256 = "<64 hex digits>"          # the download must have this hash
convert_opts = ["--entry", "usr/bin/myapp"]
permissions = "[permissions]\nx11 = true\n"
```

Without `sha256` the run still works and says that the download was not checked.

An app that listens for devices that come and go (Chromium and Electron apps, SDL games and emulators, video editors: libudev, `NETLINK_KOBJECT_UEVENT`) starts without
it, with no hot-plug notices. To give it the notices put `devices = true` in its block: `permissions = "[permissions]\nx11 = true\ndevices = true\n"`. It is a permission that
the user sees at install (level Attention) and it opens no device file.

## How to read the numbers

- **window s** is the time from the moment the launcher is started to the first
  window of at least `min_window` (200x120) that was not there before; the
  first window of the app counts even when it is shorter than that (as wide as
  `min_window` says: LMMS asks for its working directory in a dialog of 500x114). It
  includes the whole start: the outer stub, the check of the package, the
  sandbox, the app itself. In `--full` mode it is the median of five starts after
  a first one; the first start is left out because it unpacks the package into the
  store. In fast mode it is that single first start, so it is larger and noisy:
  use it to see that an app starts, not to compare.
- **package** is the time to make the package (`cached` when it was kept). For a
  recipe this is the whole `panpak build`; below the table the log gives its phases, one line each:
  `time image` (the base image and its packages), `time patch` (the rebuilt packages of the lock, GTK for
  example), `time source`, `time zig` (when the recipe has Zig), `time compile`, `time collect` (libraries,
  plugins, data) and `time pack`. `(kept)` after a time means the cache already had the result. They come
  from the lines `panpak build: time <phase>: <s> s` that `panpak build` prints.
- **install** and **starts** are the time spent in those phases. The `phases`
  line adds them over all apps.
- **peak memory** is `memory.peak` of the scope, page cache included, so it is an
  upper bound of what the run needed.
- To know what `panpak` adds, compare with the same program started directly
  (`tests/startup_budget.py` measures the fixed cost of a start against a
  baseline binary). A time to window includes the app's own start-up, which
  `panpak` does not change.
- Numbers depend on the machine. Compare runs from the same machine, and use
  `--full` for anything that goes in a document.

## In CI

`.github/workflows/apps-harness.yml` runs weekly (and by hand, on a push to main and on a pull request of this repository that changes the harness, the workflow
or what the cache below depends on): it builds Text Editor from source with `panpak build` and measures it in a headless Sway. The time of the job is in the summary
of the run. It is not part of the `gate` check.

### The cache of the patched packages

The first CI numbers (Text Editor, cold runner): image 66 s, **patch 841 s**, source 0.5 s, compile 12 s, collect 0.6 s, pack 0.9 s, 930 s in all. The patch phase is the
rebuild of the Debian GTK 4 with the dmabuf patch (`patched/<id>/` of the build cache); it depends on nothing but the base image, the snapshot and the patch. That folder is
what the CI keeps (`debs/`, `SHA256SUMS`, `.complete`; 123 MB: `out/`, a second copy of the same files, is no longer left behind).

- **The key** is made from the content of the files that define those packages: `panpak/src/source/image.rs` (the base image by digest, the snapshot, the build steps),
  `panpak/src/source/sandbox.rs` and `panpak/data/patches/*`, with a `v1` to bump by hand. Not a date, not a run number.
- **What comes back is not trusted.** `tests/apps/patched_cache.py verify` checks every folder against its `SHA256SUMS` (the list of files, their hashes, no link, `.complete`);
  a folder that does not match is removed with a warning, and `panpak build` makes it again.
- **Who writes.** Only a push to main, a manual run on main and the weekly run, and only when there is no entry for the key (a key is never rewritten). A pull request
  only reads (a pull request's own cache is out of reach of main).
- **The weekly run is always cold.** It puts the restored copy aside, builds the packages from nothing and compares the *root hash* (the hash of the `<id> <hash of its
  SHA256SUMS>` lines) of the new packages with the one of the cache. A difference fails the run, says so, and leaves the cache as it was: either the build is not
  reproducible, or something that matters is not in the key. Restoring the entry each week also keeps it from being evicted (GitHub drops a cache that was not used for 7 days).
- **Limits.** A bad entry that has the right key is never replaced (keys are immutable): bump the `v1` of the key to start again. The first run after a change of the key is cold and, on
  main, writes the new entry. A pull request that changes the key runs cold, as it should.

Measured on a machine (the same recipe, the cache restored from the folder of an earlier build, nothing else cached): image 76.0 s, **patch 2.4 s (kept)**, source 1.1 s, compile 6.3 s,
collect 0.7 s, pack 0.7 s; the package in 87 s and the whole run in 96 s, against 930 s cold. The CI numbers of the first warm run are in the step summary of that run.

## Tests of the harness itself

`python3 -m unittest discover -s tests/apps -p 'test_*.py'` (no network, no
display): the list of apps, the cache key, the signing (RFC 8032 vectors) and
the readers of window lists, and the checks of the cache of the patched packages (`test_patched_cache.py`).

## The library path check

Every window start of a run also reads the environment of every process under the launcher (`/proc/PID/environ`, in `display.process_rows`) and checks
(`display.library_path_offenders`) the values that take effect: an absolute path into the store (a tree, a farm, a package) in a process that is not a program of the
app is a fault; a program of the app (whose executable is under the `trees` folder of the store) must hold the relative value (`/$ORIGIN/.panpak-lib-<h>/...`,
PANPAK-FORMAT.md section 7.1), not a path of the store. The relative value in a program of the host (the `sh` or `bash` that runs the script of a converted AppImage) is
inert by construction (it names a folder that does not exist at the root) and is no fault. A fault ends the app's row with the pids and the executables. In a converted AppImage a program that holds another value was given it by a script of the app (Shotcut's `usr/bin/shotcut` exports its own absolute `LD_LIBRARY_PATH`): that is a note on the row (the scripts are not rewritten), not a fault; `panpak inspect` says the same in one line. The unit tests of the rule are in
`tests/apps/test_harness.py` (`LibraryPath`).
