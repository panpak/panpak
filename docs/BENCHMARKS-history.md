# Benchmark Panpak vs AppImage (earlier measurements)

These are the measurements of 26 and 27 October 2026, kept as a record. They were made with
earlier builds and with scripts that need local data. The current, reproducible numbers are in
[BENCHMARKS.md](BENCHMARKS.md); where the two disagree, use that file. In particular, the
"always redownloads 100%" remark in section 3 no longer applies: updates fetch only the missing
chunks (see [../INCREMENTAL-RESULTS.md](../INCREMENTAL-RESULTS.md) and BENCHMARKS.md).

## 0. Cold/warm start, current cycle (27 October 2026, no optimization)

Same machine as the previous cycle (Ryzen 7 7735HS, 14 GB RAM,
kernel 7.0.0-38-generic, `/home` ext4 on SSD, Landlock ABI V8,
rustc 1.99.1). Method: 10 warm runs (2 discarded warmups) + 10 cold
runs (`POSIX_FADV_DONTNEED` on the app's files before each run; host
libraries stay warm on both sides, symmetric). Median/min/max +
stdev. Script: `bench/warm_compare.py`, raw data in
`bench/warm-compare.json`. AppImages via userspace FUSE (libfuse2,
no extraction), as in the previous cycle.

Three different apps, same finish line on both sides
(identical command/flags on both sides):

- small: AppImageLauncher-lite 3.0.0-beta-3, `ail-cli --help`
  (process exits, exit 0);
- Qt: QOwnNotes 26.10.9, `--version` with `QT_QPA_PLATFORM=offscreen`;
- Electron that really starts under Panpak: **Joplin 3.7.21** (window
  working after the OB2 fix), first mapped window on dedicated Xvfb
  (poll `xwininfo`, warm profile with migrations already done).

Columns: original | Panpak with active ABI cache | Panpak with
disabled ABI cache (`PANPAK_NO_ABI_CACHE=1`, diagnostics only). Times
in seconds.

| app | original warm | original cold | panpak-on warm | panpak-on cold | panpak-off warm | panpak-off cold |
|---|---|---|---|---|---|---|
| AIL3 | 0.029 (0.021-0.033, sd 0.005) | 0.081 (0.075-0.087, sd 0.004) | 0.065 (0.051-0.074, sd 0.008) | 0.165 (0.048-0.181, sd 0.037) | 0.288 (0.259-0.298, sd 0.011) | 0.375 (0.353-0.402, sd 0.015) |
| QON9 | 0.294 (0.260-0.309, sd 0.015) | 0.387 (0.363-0.436, sd 0.022) | 0.098 (0.088-0.111, sd 0.007) | 0.133 (0.087-0.153, sd 0.018) | 0.561 (0.544-0.597, sd 0.015) | 0.665 (0.617-0.694, sd 0.022) |
| Joplin | 1.630 (1.619-1.637, sd 0.006) | 1.835 (1.828-1.841, sd 0.004) | 1.425 (1.419-1.434, sd 0.005) | 1.833 (1.270-1.841, sd 0.173) | 1.553 (1.429-1.635, sd 0.087) | 1.833 (1.826-2.041, sd 0.082) |

Active-cache Panpak overhead vs original (medians):

- AIL3: warm **+36 ms (+124%)**, cold +84 ms (+104%). Remains the worst
  case: tiny app, nearly fixed verify+resolve overhead.
- QON9: warm **-196 ms (-67%)**, cold -254 ms (-66%). Panpak is
  faster: the tree on ext4 does not pay the original's FUSE squashfs
  mount/access. Honest data, not optimized.
- Joplin: warm **-205 ms (-13%)**, cold even (-2 ms). Same finish line
  (editor window), same warm profile.

The ABI cache is decisive for CLIs (AIL3: 0.288→0.065,
QON9: 0.561→0.098); on GUI Joplin its relative weight is smaller
(1.553→1.425 warm). When cold the page cache dominates: original and
Panpak converge (Joplin cold identical at 1.833 in all columns).

Observed flakiness (reported, not hidden): on Joplin under Panpak,
4 starts out of ~40 showed no window within 90 s (main services
active, zero X children; retry succeeded on the next attempt).
Original: 0 flakes over 40+ runs. Cause not isolated — possible
investigation queue (renderer/GPU under Xvfb+sandbox). The medians
above exclude the failed runs (retry count in the raw data).

QON 26.10.8 is excluded from the 3 (same app as QON9); Simplenote stays
original-only (see below, unchanged).

## Data from the previous cycle (26 October 2026, superseded for startup)

Method: each timing measurement repeated 10 times (median + min/max +
stdev); "cold" = `POSIX_FADV_DONTNEED` on the involved files before
each run (unprivileged mmap/page-cache evict; host libraries stay warm
on both sides, symmetric). Scripts and raw results in `bench/`.

App (real versions, two consecutive releases where indicated):
- small Qt5: AppImageLauncher-lite 2.2.0 -> 3.0.0-beta-3 (`ail-cli --help`)
- Qt6: QOwnNotes 26.10.8 -> 26.10.9 (`--version`, `QT_QPA_PLATFORM=offscreen`)
- Electron: Simplenote 2.27.0 -> 2.27.1 (non-GUI measures only: see below)

## 1. Startup of the previous cycle — SUPERSEDED by §0 data (kept for history)

| app | panpak warm | panpak cold | AppImage warm | AppImage cold |
|---|---|---|---|---|
| AIL 2.2 | 0.365 (0.315-0.367) | 0.465 (0.365-0.465) | 0.032 (0.032-0.032) | 0.115 (0.115-0.117) |
| AIL 3.0 | 0.265 (0.264-0.266) | 0.365 (0.315-0.415) | 0.032 (0.032-0.032) | 0.115 (0.114-0.116) |
| QON .8 | 0.515 (0.465-0.517) | 0.565 (0.515-0.616) | 0.264 (0.264-0.267) | 0.366 (0.365-0.368) |
| QON .9 | 0.490 (0.465-0.516) | 0.565 (0.565-0.617) | 0.264 (0.264-0.315) | 0.366 (0.365-0.367) |

Panpak is slower: ~8-11x warm on small apps (nearly fixed ~0.3s
overhead: signature+root+index verification, transitive ABI resolution
over ~140-200 ELFs with a 142-symlink farm, Landlock, double exec),
~2x on QOwnNotes (where Qt startup dominates). When cold the gap
shrinks (Panpak pays ~0.1s of page cache like AppImage). Low spread
(sd 1-50ms). Not optimized: it is the stated cost of verification +
per-lib fail-closed resolution.
[Note 27/10: conclusions superseded — no ABI cache, no fix. See §0.]

Simplenote: GUI startup **not comparable** in this environment.
AppImage: time-to-marker ("Checking for update") 3.99s warm / 3.82s
cold (medians, sd 0.13/0.07 — no significant difference; the network
check dominates). Panpak: the app does not start headless here —
X11 blocked by the Landlock scope (abstract socket: EPERM by design),
with `DISPLAY=unix:...` silent crash in GPU/zygote init under sandbox
(to investigate; on real Wayland+GPU it works, proven in the previous
cycle). Cause documented, not hidden.

## 2. Disk space (2 installed versions of each)

| pair | AppImage | Panpak (packages + trees) |
|---|---|---|
| AIL 2.2+3.0 | 78.9 MB | 88.4 + 216.1 = ~304.5 MB (shared: 6.3 MB) |
| QON .8+.9 | 124.6 MB | 135.2 + 226.8 = ~362.0 MB (shared: 115.7 MB, 2216 files) |
| SN .0+.1 | 294.4 MB | 311.3 + 0 = 311.3 MB (never run: no tree) |
| **total 6 apps** | **497.9 MB** | **976.5 MB** (packages 534.9 + trees 439.3 + runtime 2.3) |

Panpak uses ~2x: it keeps both the signed `.panpak` (needed for
update/rollback) and the materialized tree; sharing between versions
exists (hardlinks, e.g. 115.7 MB on QON) but is at identical-file
level, not chunk level. The format supports per-chunk dedup, but the
materialized trees duplicate bytes: shared blob-store and tree GC are
future work (not done here, by choice).

## 3. Update size between two real versions

Theoretical payload from comparing the chunk tables (bytes that
*change* + manifest + index):

| update | new/total chunks | theoretical payload | whole file | share |
|---|---|---|---|---|
| AIL 2.2 -> 3.0-beta | 1293/1361 | 39.8 MiB | 41.1 MiB | 96.7% |
| QON .8 -> .9 | 151/3195 | 7.2 MiB | 64.5 MiB | 11.2% |
| SN .27.0 -> .27.1 | 78/5414 | 3.8 MiB | 148.4 MiB | 2.5% |

BUT the current implementation always redownloads **100%**
(measured: 2,263,402 bytes out of a 2,263,365-byte file in an e2e HTTP
update with per-Range byte counting): `cmd_update` downloads the whole
`[0..index_off)` prefix ("stub always from remote") and then
overwrites the reused chunks from the local package. Chunk reuse over
the wire is therefore irrelevant today: our bug, to fix in a dedicated
cycle (fetch `[0..stub)` + only missing chunks). AppImage: always 100%
(no delta; AppImageUpdate/zsync out of scope).

## 4. Install time (10 runs, median)

| app | `panpak install` | AppImage (cp+chmod+desktop) |
|---|---|---|
| AIL (45 MB) | 0.08s | 0.07-0.08s |
| QON (68 MB) | 0.11s | 0.11s |
| SN (156 MB) | 0.27-0.28s | 0.27s |

Install is not a differentiator: `panpak install` verifies and copies
(~disk speed, linear in size), like copying the AppImage. (Tree
materialization happens at first start, not at install.)

## What is NOT comparable

- Simplenote GUI startup under Panpak on this machine (no Wayland,
  only Xvfb): X11 blocked by scope; headless crash to investigate.
- Real delta update over HTTP for the 3 large apps (no `update.url`
  in the converted packages; mechanism validated on a small app).
- FUSE mount cost: AppImages run via real FUSE here (userspace
  libfuse2 installed); on systems without FUSE `--appimage-extract`
  would be needed (slower baseline, unmeasured).
- Network: timings include different behaviors (SN/AppImage does the
  update check, SN/Panpak is denied by default-deny).
