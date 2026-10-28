# Guide for the external reviewer

Status: format v1, runtime ABI 1, Landlock V8 on kernel 7.0.0-38.
This guide says WHERE to look, not what to conclude. Measured facts
live in `BENCHMARKS.md`, `PILOT.md`, `INCREMENTAL-RESULTS.md`,
`STARTUP-CACHE.md`; threats in `THREAT-MODEL.md`; the norm in
`PANPAK-FORMAT.md`. Nothing in this repo is legal advice.

## 1. Architecture in one page

Three crates, one file, two phases:

- `panpak` (CLI, `panpak/src/main.rs`, ~1900 lines): `pack` (signs),
  `install` (verify + TOFU/pin + launcher), `update`/`rollback`,
  `open` (per-file grant), `convert` (AppImage→panpak), `doctor`,
  `install-runtime`, `release`.
- `panpak-core` (`panpak-core/src/lib.rs`, ~1500 lines): parsing and
  verification (footer, strict TOML manifest, zstd index, BLAKE3 hash,
  strict Ed25519 signature, TOFU/rotation), ABI resolution
  (H/B worlds, atomic Qt/glibc families, symlink farm).
- `panpak-stub` (`panpak-stub/src/main.rs`, ~1700 lines +
  `resolution.rs`, ~170 lines): static musl executable inside every
  `.panpak`; at every start it re-verifies everything, materializes the
  tree, applies Landlock, then `exec`s the entry. No execution before
  verification; no silent fallback.

A `.panpak` = ELF stub + zstd chunks + index + signed manifest +
80-byte footer (`PANP\0\1\0\0`). Flow: `pack` → `install`
(shows key fingerprint, pin) → launcher `~/.local/bin/<id>` →
stub `run` (verifies signature/pin/stub/index/chunks, Landlock, exec).
Local store `~/.local/share/panpak` trusted (same uid).
App data: `~/.local/share/<id>`; SQLite temp: `<id>/tmp`
(`SQLITE_TMPDIR`); shared `/tmp`.

## 2. File map (approximate sizes)

- `panpak/src/main.rs` ~1900 lines (CLI + update/rollback/open/convert)
- `panpak-core/src/lib.rs` ~1500 lines (verify + ABI)
- `panpak-stub/src/main.rs` ~1700 lines (sandbox + exec)
- `panpak-stub/src/resolution.rs` ~170 lines (ABI plan cache)
- `PANPAK-FORMAT.md` ~440 lines (normative), `THREAT-MODEL.md` ~170
- `tests/*.py`: conformance runner, `incremental.py` (hostile server),
  `x11.py`, `abi_cache.py`, `sqlite_tmpdir.py`, `mu_crash_rate.py`,
  `joplin_fts_probe.py`, `pilot_cycle.py`, `gui_session.py`,
  `pilot_saves.py`, `kdenlive_clip.py`, `range_server.py`
- `bench/`: `bench.py`/`measure.py` (primitives), `warm_compare.py`,
  `startup_cache.py`, raw JSON
- `conformance/`: `verify.py` (independent Python verifier),
  21 fixtures (3 valid, 18 invalid, including the old-magic legacy)
  + `expected.json`
- `fuzz/fuzz_targets/`: 10 targets (below)

Build/test: `cargo test --locked` (host + musl), `cargo clippy
--all-targets -- -D warnings`, `scripts/repro-build.sh` (double-build,
hashes in `REPRODUCIBLE-HASHES.md`), `python3 conformance/run.py --rust`,
`docker build -f Dockerfile.repro` for the clean container.

## 3. Untrusted surfaces (decreasing priority)

1. **Footer/offsets/lengths** (`panpak-core`, 80-byte footer read,
   `read_at`, anti-zip-bomb caps): fuzz `fuzz_footer`, conformance
   18 invalid, hostile-server e2e. NOT covered: input beyond 256 MiB
   (refused by cap, not parsed).
2. **Compressed zstd index** (up to 256 MiB, path/chunk entries):
   fuzz `fuzz_index`, conformance, `decode_capped`. NOT covered:
   trees beyond the caps (refusal, not beyond-refusal robustness).
3. **Strict TOML manifest** (unknown keys refused, permissions,
   `key`/`rotation_*`): fuzz `fuzz_manifest`, test `panpak_toml_strict`.
4. **Ed25519 signature + rotation chain** (`verify_strict`, TOFU pin,
   `rotation_old`/`rotation_sig`): fuzz `fuzz_sig`? no —
   `fuzz_pubkey`, `fuzz_rotation`, rotation/replay e2e in
   `tests/incremental.py`. First pin NOT authenticated (TOFU).
5. **Chunk/per-chunk hash** (BLAKE3 before writing):
   conformance invalid-chunk*, corruption/truncation e2e.
6. **Bound stub** (`[runtime].stub_hash`, `verify_stub_range`):
   fuzz `fuzz_stub`? no — `verify_accepts_*` / `verify_rejects_*` tests.
   The stub run in `run-mode` is the installed runtime.
7. **ELF scanner + SquashFS** (convert): fuzz `fuzz_elf`,
   `fuzz_squash`; explicit `allow_missing`. NOT covered: DwarFS
   (refused), upstream-signed AppImages (`signed_by_upstream=false`).
8. **HTTP update** (HTTPS only, localhost only with flag, max 3
   redirects without downgrade, verified Ranges): fuzz `fuzz_http`,
   e2e against hostile server (loop/slowloris/huge bodies/lying
   Ranges). NOT covered: HTTPS on the internet beyond the tests done.
9. **Materialization + symlinks** (traversal, absolutes refused,
   internal `../` legal, farm dedup): unit + fork tests,
   `fuzz_path`. NOT covered: TOCTOU races beyond the tests.
10. **Landlock rules** (per-SOM build, V6+ scope, V4+ net, V9 unix,
    fail-closed self-test): fork tests per area (network, scope,
    signals, open/write/rename, X11, sockets). Minimum kernel 5.13;
    documented per-ABI degradation (refusal, never silent clamp).

Fuzzing: 10 targets, CI runs a 2-minute smoke each
(`-max_total_time=120`). Long runs (hours/days) NEVER done: do that
before trusting beyond the smoke.

## 4. Known weaknesses and stated limits (facts, not intentions)

- Direct execution of a `.panpak` = trust in the download (TOFU not
  authenticated; compare the fingerprint out of band).
- UDP NEVER filtered by any ABI (TCP bind/connect only).
- X11 opt-in and bind permission open wide accesses (input/screen
  observation; port reachable from other machines too: Landlock
  filters ports, not addresses). Warnings at install and start.
- Up to and including V8, `connect()` to filesystem sockets NOT
  mediated (session D-Bus reachable); abstract scopes denied by default.
- No seccomp; `/proc` `/sys` `/run` readable; shared `/tmp`.
- Minimum kernel 5.13; degradation on old kernels (explicit refusal).
- NEVER verified: real GPU beyond the shared render node, kernels
  beyond 7.0.0-38, HTTPS on the internet, FUSE vs extraction beyond
  libfuse2.
- Honest measurements: Panpak slower on tiny CLIs (+36 ms AIL3 warm),
  faster where FUSE dominates (QON9 -67%); ~2x disk; Joplin GUI
  flakiness under Panpak (~10% starts with no window, cause not
  isolated); apps that do not start (upstream Mu SIGSEGV, OpenShot
  ephemeral bind, Audacity SONAME).

## 5. Specific questions for the reviewer (at least 8)

1. Is the verify order signature→index→chunk→stub correct and
   complete, or is there a path that runs before verifying?
2. Does the rotation chain hold against replay and downgrade (pin vs
   remote manifest, `rotation_sig` over the 32 raw bytes)?
3. Does symlink resolution have escapes (absolute, composed chains,
   `../` past the root, hardlinks into the tree)?
4. Does the Landlock single-file rule with `--write-rename` expose
   more than stated (WriteFile covers neighbors)?
5. Can an app use `SQLITE_TMPDIR` in `<id>/tmp` to exfiltrate or
   confuse other apps (predictable names, races)?
6. Does the ABI cache really invalidate on every change (package,
   runtime, kernel, ldconfig, dev/inode/mtime/ctime, symlink target)?
7. Is the TCP allowlist whole (bind+connect, ports, ABI<V4) or is a
   default-deny missing on some ABI?
8. Does `panpak open` grant exactly one file (canonicalization,
   denied sibling symlinks/hardlinks) even with `--write-rename`?
9. Does the `sockets_allow` fallback on ABI<V9 fail hard (never
   silent) and does the V9 rule cover only the listed paths?
10. Are the anti-DoS caps (manifest 1 MiB, index 256 MiB, chunk 1 MiB)
    applied before every allocation on all paths (pack, install,
    update, doctor)?

## 6. Explicitly out of scope

D-Bus broker, seccomp, mediated portal/file-picker, legal name
clearance, publication. None of this is decided here.
