# Panpak bench

## Public benchmarks (reproducible)

`inputs.py` downloads the official AppImages with a pinned SHA-256, `public_bench.py` measures
(update size, first and later starts, cost of the view, library resolution) and writes CSV files to
`docs/bench-data/`, and `make_charts.py` draws the SVG charts from them. Method, results, limits and
the commands to run them again are in [docs/BENCHMARKS.md](../docs/BENCHMARKS.md).

## Step 9b: Panpak and Flatpak on Fedora Silverblue

The series are run with `silverblue/run_series.sh` (the prerequisites, `scenario`, the cycles and the updates of one series, for Flatpak or for Panpak) and the reboots with `silverblue_bench.py reboot-series`, which refuses to start if the automatic login of GDM is not on; `run_app.sh` repeats the measures of one app. Every app is left running for 4 s after the first frame of its main window and then stopped with the same signal for both formats (never `flatpak kill`), and the exit is written (`exit_s`, `exit_clean`, and for an app that does not leave, a diagnosis of its processes read from `/proc`). `close-test` measures how long Kdenlive takes to leave when a person closes its window and whether it writes its settings; `prerequisites` measures the runtime of Panpak in a private home; `sandbox` reads how a browser engine (ONLYOFFICE) is sandboxed. `summarize --out` writes every measure with its median, minimum and maximum.

`silverblue/` holds the procedure ([PROCEDURE.md](silverblue/PROCEDURE.md)), the script that is run inside the two virtual machines (`silverblue_bench.py`: bytes that came, space added, time
to the first frame of the window, at the first start, after a reboot and at the following starts, with the median and the spread of each), the extension of GNOME Shell that reads
the moment of the first frame, `make_conversions.py` (the conversions of the official AppImages of GIMP, Kdenlive and ONLYOFFICE, with the permissions of `conversions/` and the SHA-256 check), `serve_https.py` (the packages over HTTPS with Range for the virtual machines), `make_ops_files.py` (the files of the operations: texts, a picture, a document, two test videos made with ffmpeg and a folder outside the user folders), `make_checklist.py` (a printable page of the operations, one row for each app and each machine), `seed_peer.py` (a controlled BitTorrent peer on the host, for the qBittorrent operation: one file of 8 MiB, always the same bytes, listening only on the address of the libvirt network) (the files of the operations), the recipes of qBittorrent 5.3.0 (the example of the build pipeline) and GNOME Text Editor 51.0 (the test of permissions and files), and `verify_sbom_build.py`, which makes a real `panpak build`
of each recipe twice and checks that the SBOM says `coverage = "build"`, names what the recipe pins, and is the same bytes both times. The Panpak packages of the three apps from AppImages are conversions made by us, not packages of the authors. `selftest.py` tests the script with fakes (no
virtual machine needed).

## Earlier scripts: environment variables


Scripts in `bench/` contain no absolute paths: data directories are
read from the environment. Defaults are meant for a dev machine, with
no hardcoded usernames.

| variable | use | default |
|---|---|---|
| `PANPAK_BENCH_DIR` | original AppImages for comparison | `~/panpak-bench` |
| `PANPAK_PKGS_DIR` | `.panpak` files to install/measure | temp dir `panpak-bench-pkgs` |
| `PANPAK_PKG_EXT` | package extension (to reuse old data) | `.panpak` |
| `PANPAK_FUSE_LIB` | userspace libfuse2 for AppImages | `~/fuse2/lib/x86_64-linux-gnu` |
| `PANPAK_PILOT_ORIG` | directory with the original Joplin AppImage (only `warm_compare.py`) | `~/panpak-bench/pilot` |
| `PANPAK_BENCH_WIPE` | `=1` confirms the bench store wipe (otherwise `--yes` is required) | unset |
| `PANPAK_STORE` | dedicated bench store (never the user one) | `<bench-home>/.local/share/panpak` |
| `PANPAK_BENCH_HOME` | dedicated bench HOME (store, launchers, desktop follow it); MUST be inside `PANPAK_BENCH_DIR` | `<PANPAK_BENCH_DIR>/home` |
| `PANPAK_APPIMAGE_DIR` | where `campaign.py` copies AppImages for the install timing; MUST be inside `PANPAK_BENCH_DIR` | `<PANPAK_BENCH_DIR>/appimages` |
| `PANPAK_ALLOW_REAL_HOME` | `=1` skips ONLY the check that HOME is inside the work directory and is not your real home (deleting outside the work directory stays refused) | unset |

`campaign.py` does nothing on import (everything in `main()`); without
`--yes` or `PANPAK_BENCH_WIPE=1` it refuses to wipe and prints the exact
path. The real `~/.local/share/panpak` store is never touched by default.

Example to reuse existing data elsewhere (read-only):

```sh
PANPAK_BENCH_DIR=~/panpak-bench \
PANPAK_PKGS_DIR=~/panpak-bench-pkgs \
PANPAK_PKG_EXT=.panpak \
python3 bench/warm_compare.py bench/probe.json cli-only
```

Historical JSON in `bench/` (`results.json`, `warm-compare.json`, …)
are dated measurements: the numbers are those of the original runs.
