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

## How to read the numbers

- **window s** is the time from the moment the launcher is started to the first
  window of at least `min_window` (200x120) that was not there before. It
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

`.github/workflows/apps-harness.yml` runs weekly (and by hand, and on a pull
request of this repository that changes the harness): it builds Text Editor from
source with `panpak build` and measures it in a headless Sway. The time of the
job is in the summary of the run. It is not part of the `gate` check.

## Tests of the harness itself

`python3 -m unittest discover -s tests/apps -p 'test_*.py'` (no network, no
display): the list of apps, the cache key, the signing (RFC 8032 vectors) and
the readers of window lists.
