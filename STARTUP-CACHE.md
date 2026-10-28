# ABI cache and warm start — 2026-10-26

The cache holds only the ABI plan; signature/pin/index/stub and
Landlock are **not** stored or skipped. Local store trusted,
as in THREAT-MODEL. App code cannot write to it.

Key: BLAKE3 of the whole package bytes, of the executed runtime, of
the kernel (`osrelease`) and of the fresh `ldconfig -p` output. A
modified runtime invalidates even at equal ABI. Without a valid
`ldconfig` the full resolution is always used (minimal-distro scan
fallback). Every hit rechecks dev/inode/size/mtime/ctime in
nanoseconds and the canonicalized target of the plan's libraries,
loader, farm symlinks and directories. Missing, corrupt, over 4 MiB
or no-longer-valid cache: miss and rebuild. Atomic write; a cache
write error does not stop execution. Obsolete farms are updated (old
links replaced/removed).

`PANPAK_ABI_TRACE=1` prints hit/miss; `PANPAK_NO_ABI_CACHE=1`
disables the cache (diagnostics only). No option changes the trust
checks.

## Measurements (10 warm runs, warmup discarded)

Ryzen 7 7735HS, kernel 7.0.0-38-generic, ext4, same machine as the
pilot. AppImageLauncher-lite 3.0.0-beta-3, `ail-cli --help` (exiting
command, so no inference on GUI time-to-first-frame).

| case | median ms | standard deviation | min–max ms |
|---|---:|---:|---:|
| Panpak before | 286.37 | 22.83 | 264.33–339.38 |
| Panpak after, hit | **70.48** | 5.81 | 58.92–75.66 |
| new runtime, cache disabled | 294.23 | 9.77 | 272.15–303.15 |
| original AppImage (FUSE) | 27.73 | 5.85 | 19.28–36.18 |

Warm extra ~42.75 ms (<=100 ms goal reached on this app).
~75.4% reduction. `strace -c` profile: before 223.5 ms in `read`
(79% of syscall time), after 7.6 ms; repeated dependency parsing
dominated. The cache hit still reads the package for the full hash
and verifies all fingerprints: no shortcuts on signed bytes.

Repeat: `bench/startup_cache.py before-stub after-stub app.panpak --help`.
The driver uses `subprocess.run`/pipe and `perf_counter_ns`, without
the `Popen.wait(timeout)` polling that quantized the previous
benchmark's timings. Raw results in `bench/startup-cache.json`.
