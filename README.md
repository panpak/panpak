# `Panpak`

Panpak (pronounced "pan-pak").

Verified application packages run in a sandbox (Linux + Landlock). A
`.panpak` holds a static executable stub, deduplicated compressed
chunks, a signed index and a signed manifest; the stub verifies
everything before running and confines the app with Landlock.

The official file extension is `.panpak`; `.pan` is an accepted alias. A file
is identified by its magic and signature, never by its extension.

Status: format v1, runtime ABI 1. See `QUICKSTART.md` for usage,
`PANPAK-FORMAT.md` for the format, `THREAT-MODEL.md` for the threat
model, `RELEASE.md` for releases, `REVIEW-GUIDE.md` for external
review.

## What it guarantees and what it does NOT (from `THREAT-MODEL.md`)

Guaranteed (tested):

- **No execution before verification**: Ed25519 signature over
  `manifest || root_hash`, BLAKE3 hash of the stub bound to the
  signature (`[runtime].stub_hash`), index root-hash, per-chunk BLAKE3
  hash before writing. Tampered file or replaced stub = refusal.
  Launchers run only the installed runtime, never the package's stub.
- **Read-only tree** (Landlock r-x + `chmod a-w` on trees and blobs).
- **Confined writes** to `~/.local/share/<id>` and `/tmp`
  (private SQLite temp in `<id>/tmp`).
- **No silent fallback**: without Landlock the stub aborts;
  post-restrict self-test.
- **TOFU + pin**: self-signed key shown at install and pinned,
  then enforced at every start; signed rotations supported.
- **Fail-closed ABI**: missing or incoherent dependencies = explicit error.
- **Explicit file open** (`panpak open`, double click): only that file
  (read; write only with `--write`); non-granted symlinks/hardlinks
  stay denied.

NOT guaranteed (known limits):

- **X11**: does not isolate clients (keylogging/screen spying across
  windows). Real mitigation only on Wayland.
- **Unix sockets / session D-Bus**: up to and including V8,
  `connect()` is not mediated — the app reaches Wayland, audio and
  D-Bus (wide surface).
- **Shared `/tmp`**: visible and writable by all user processes.
- **UDP**: never filtered by Landlock (TCP bind/connect only); even with
  `network = false` it remains a possible exfiltration channel.
- Ephemeral ports (`bind` on port 0 + rebind) not expressible with
  discrete allowlists (verified on ABI V8).
- **Scope V6+ and path Unix sockets (V9)**: only on recent kernels;
  on lower ABIs the entries are refused or not applicable.
- **No seccomp**; `/proc`, `/sys`, `/run` readable; process/host
  enumeration possible.
- **Initial TOFU** not authenticated: verify the fingerprint out of band.
- **Clipboard, notifications, portal**: not mediated; the file picker
  stays the job of the future broker (see `PORTAL-DESIGN.md`).

## Honest comparison (measured here, 27 October 2026)

Numbers: `BENCHMARKS.md` (AIL3/QON9/Joplin startup, 10+10 runs),
`PILOT.md` (10 real apps), `INCREMENTAL-RESULTS.md` (updates),
`STARTUP-CACHE.md`. Machine: Ryzen 7 7735HS, kernel 7.0.0-38, ext4.

| Aspect | Panpak (measured) | AppImage | Flatpak / Snap (not measured here) |
|---|---|---|---|
| Trust model | Signature + TOFU/pin; run only after verification | No verification (mounts and runs) | Repo signature; namespace sandbox |
| Sandbox | Landlock only (file+TCP), no seccomp/namespaces | None | Namespaces + seccomp + Landlock equivalents: **stronger than Panpak** |
| X11 isolation | Opt-in reopening abstract sockets (documented) | None | X11 with no isolation by default; Wayland preferred |
| D-Bus/portal | Not mediated (V8) | Not mediated | xdg-dbus-proxy + portal: **more mature** |
| Network | Default-deny filterable TCP; never UDP | Open | Filtered via namespace |
| Warm start | AIL3 +36 ms (+124%); QON9 **-196 ms (-67%)**; Joplin -205 ms (-13%) vs originals | Baseline (FUSE pays on QON9) | Not measured here |
| Disk (2 versions) | ~2x (signed package + tree) | 1x | Shared base (not measured here) |
| Update | Only new chunks via Range (e.g. QON 5.3 MB of 67.6) | Always 100% | Delta via OCI/ostree (not measured here) |
| App coverage | 3/10 GUI with real save; KO: Mu (upstream SIGSEGV), OpenShot (ephemeral bind), Audacity (SONAME) | All start (Mu crashes on original too) | Curated stores, declared dependencies |

Where Panpak loses, stated plainly: weaker sandbox than Flatpak (no
seccomp/namespaces, no portal), some apps do not start, slower startup
on tiny CLIs, double disk, occasional GUI flakiness under sandbox
(Joplin ~10% starts with no window, cause not isolated). Where it
wins: total cryptographic verification, real incremental updates, no
daemon, inspectable format.

## License

The source code is licensed under either of the Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE)) or the MIT license ([LICENSE-MIT](LICENSE-MIT)),
at your option. Third-party components are listed in
[THIRD-PARTY-LICENSES](THIRD-PARTY-LICENSES); `panpak licenses` and
`<package> --panpak-licenses` show the texts carried by a binary or a package.
No third-party binary is redistributed in this repository: the fuzz seeds are
generated by `fuzz/gen-seeds.py`, and `scripts/check-large-files.sh` (run in CI)
rejects tracked files over 1 MiB that are not listed in
`scripts/large-files-allowed.txt`.

The format specification and the other documents are licensed under
[CC BY 4.0](LICENSE-DOCS), with a patent non-assertion statement for
conforming implementations (see `PANPAK-FORMAT.md`, Appendix D).

Unless you state otherwise, any contribution is provided under these licenses;
contributions are signed off under the DCO (see [CONTRIBUTING.md](CONTRIBUTING.md)).

## Trademark

The code is free, but the name "panpak" and its logo follow a usage policy:
you may say a program is "compatible with panpak" or "distributed in panpak
format", but not use the name for a modified product or fork in a way that
suggests the project endorses it. See [TRADEMARKS.md](TRADEMARKS.md). The
name itself is provisional until the checks in `NAME-CLEARANCE.md` are done.

## Security

Report vulnerabilities privately to security@panpak.org, see
[SECURITY.md](SECURITY.md).
