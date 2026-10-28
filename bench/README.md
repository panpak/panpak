# Panpak bench: environment variables

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
| `PANPAK_BENCH_HOME` | dedicated bench HOME (store, launchers, desktop follow it) | `<PANPAK_BENCH_DIR>/home` |

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
