# Distro × test matrix (OB2 + OB6)

Same `.panpak` everywhere (one-time pack on host; static musl stub).
Apps built on Debian bookworm (glibc 2.36 = old baseline). Active
Landlock sandbox in all containers (no special flags). Graphics: Xvfb
`:99` installed in the container, `GDK_BACKEND=x11`,
`GSETTINGS_BACKEND=memory`, 40s timeout (the app exits alone after 2s).

| distro | A (C+host libc) | B (C+++host libstdc++) | C (GTK3+Xvfb) | A2/B2 (glibc in bundle) |
|---|---|---|---|---|
| Debian 12 bookworm | OK | OK | OK | OK / OK |
| Ubuntu 24.04 noble | OK | OK | OK | OK / OK |
| Fedora 43 | OK | OK | OK | OK / OK |
| Alpine 3.22 (musl) | **expected KO**¹ | **expected KO**¹ | OK (B-world) | OK / OK |
| NixOS (nixos/nix) | **expected KO**¹ | **expected KO**¹ | OK (B-world)² | OK / OK |

¹ `Error: libc.so.6 missing on host and not included in the tree`, RC=1:
fail-closed with a clear message, no execution attempt. Musl/FHS-less
hosts need A2/B2 (glibc 2.36 + loader in the bundle): OK. NixOS has no
`/lib`, `/usr/lib` or `ldconfig`: the host map comes from the fallback
(empty) and the B-world runs without touching the FHS.
² Xvfb installed via `nix-env -iA nixpkgs.xorgserver.out` in the same
execution container (the profile is ephemeral across `docker run`s).

## aarch64 (qemu-user, OB6)

- Toolchain: rustup target `aarch64-unknown-linux-musl` + musl.cc
  `aarch64-linux-musl-cross` (linker via
  `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER`), `panpak-stub` and
  `panpak` build clean and static for aarch64.
- `qemu-aarch64-static` 7.2 (multiarch) and 8.2 (extracted from Noble
  .deb): the stub runs (`--abi-version` → 1), a static aarch64 C app
  runs, an aarch64 `.panpak` verifies signature/manifest/index and
  prints the fingerprint.
- Full execution **not verifiable**: no tried qemu-user implements the
  Landlock syscalls (create ok in dummy mode, restrict →
  `NotEnforced`) and the stub refuses with a hard error — fail-closed
  proven on another arch too, never run without sandbox.
- Dynamic aarch64 GTK: not attempted (would require an arm64 userland
  with glibc; no binfmt on this machine).

Farm C: 68/68 symlinks to the bundle on all distros (host=0);
A/B: all host, 0 libs from the bundle.

## KOs met during testing (all fixed, none hidden)

1. **entry script + glibc in bundle** → `invalid ELF header`: the stub
   passed the script to `ld-linux`. Fix: the loader is used only if the
   entry is an ELF (`is_elf`); scripts go to direct exec.
2. **loader 2.36 + host libc 2.39** (Noble) →
   `undefined symbol: __tunable_is_initialized, version GLIBC_PRIVATE`.
   Fix: atomic glibc family (all from the same build).
3. **bundle libc 2.36 + host libX11** (Noble, requires GLIBC_2.38) →
   `version GLIBC_2.38 not found`. Fix: B-world = bundle-first for
   everything in the bundle; only prefer_host/missing go to the host.
4. **`ld-linux` in needs** (it is libc's own `DT_NEEDED`): on Alpine
   (no `ldconfig`) it failed `decide`. Fix: `ld-*` never in the farm,
   it is the plan's `loader`.
5. **AppRun requires `$APPDIR`**: auto wrapper for non-ELF entries
   (`APPDIR/APPIMAGE/ARGV0/OWD`).
6. **Electron: unusable setuid `chrome-sandbox` without root** →
   `--no-sandbox` (Panpak's Landlock sandbox remains); denied
   `$HOME/.config` → XDG_* pattern inside appdata.

## Graphics: what had to be opened (OB2 request)

**Nothing.** With the OB1 policy (§9) test C passes unmodified:
- `/tmp/.X11-unix`: covered by RW `/tmp` (pre-existing rule);
- `connect()` to the X11 socket: not mediated by Landlock V1 → passes with RW `/tmp`;
- `/dev/dri`: not needed (X11 backend, no GL); `/dev` already RW since OB1;
- `$HOME` denied writing and `/run` read-only: GTK/glib degrade
  gracefully (in-memory gsettings, fontconfig with no on-disk cache);
- no D-Bus in containers: gio/at-spi warn and continue;
- fonts: on Alpine zero system fonts and zero bundled fonts — the window
  still opens (text rendering not visually verified).

Manifest-vs-stub decision: **no new field for the X11 profile**.
Still to test (not verifiable here): Wayland (`XDG_RUNTIME_DIR`
under RO `/run`: `connect()` probably passes like X11, but unproven
without a compositor), real GPU (`/dev/dri` present but unused),
portal file/camera.

## VERNEED fail-closed in practice

There is no symbol-version check: at equal SONAME the host (H-world)
or bundle (B-world) is chosen and the loader speaks. Observed effects:
- H-world (A: 1 need, B: 3 needs, 0 bundle libs): works for backward
  compatibility (apps built on glibc 2.36 run on the newer glibc of
  Noble/Fedora);
- B-world (C: 69 needs, 68 libs + bundle loader): total coherence, runs
  even on musl with no host glibc.
- glibc ends up in the bundle **only** when needed: pure-host apps (A/B)
  use 0 bundle libs on glibc distros; on musl (or with dependencies
  beyond the baseline) the glibc+loader bundle is mandatory. Not
  "nearly always": it depends on the target. Shippers targeting only
  recent glibc distros can ship lean bundles; whoever wants Alpine or
  old distros bundles glibc.

## AppImage → panpak (OB5, `panpak convert`, no FUSE)

| AppImage | conversion | sandboxed run |
|---|---|---|
| AppImageLauncher-lite 3.0.0 (36 MB, Qt5) | OK (`--entry usr/bin/ail-cli`; original Exec missing from the package: `--entry` needed) | OK (`--help` Qt under Landlock) |
| QOwnNotes 26.10.9 (62 MB, Qt6) | OK with 24 targeted `allow_missing` (QML plugins with DT_NEEDED but files missing from the AppImage: KO without) | OK (`--version` → 26.10.9, offscreen) |
| Simplenote 2.27.1 (147 MB, Electron) | OK with 3 `allow_missing` (libs removed from modern distros) + AppRun wrapper (`$APPDIR`, auto for non-ELF entries) | OK on real Wayland+GPU (45s+, zygote+NetworkService, renderD128) with `--no-sandbox` (Electron's internal sandbox incompatible no-root) and XDG_* inside appdata |

KOs found (none hidden):
1. `hsqs` false positive in the ELF runtime → superblock validation (block_size, version, inodes).
2. backhand without default-features = no decompressors and empty messages (full backhand needed).
3. `lib*.so.N` SONAMEs as symlinks: the inventory ignored them → `bundled_libs` now resolves them to the target (fix also for future bundles, not just convert).
4. `DT_NEEDED` present but file missing from the AppImage (QOwnNotes, 24 QML libs): new explicit `[abi] allow_missing` (never for glibc); without it, fail-closed.
5. AppRun requires `$APPDIR`: auto wrapper for non-ELF entries (`APPDIR/APPIMAGE/ARGV0/OWD`).
6. Electron: `chrome-sandbox` setuid unusable without root → `--no-sandbox` (Panpak's Landlock sandbox remains); `$HOME/.config` denied → XDG_* pattern inside appdata.
