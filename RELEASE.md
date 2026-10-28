# Distributing Panpak releases

This document describes the format of official releases (`panpak` +
`panpak-stub`) and how to publish/verify the signing key **on a
channel separate** from the binaries themselves.

## Format

`panpak-<version>-<target>.tar.gz` (e.g.
`panpak-1.4.0-x86_64-unknown-linux-musl.tar.gz`):

```
panpak-1.4.0-x86_64-unknown-linux-musl/
  panpak         # CLI (same arch as the target)
  panpak-stub    # runtime
  VERSION      # "1.4.0"
```

Sidecar files (same directory, same name + suffix):

- `.sha256`: line in `sha256sum` format (`<hex>  <filename>`).
- `.sig`: 64 raw bytes = Ed25519 signature of the tarball (release key).

Creation (after `cargo build --release --target <triple>`):

```sh
panpak release --version 1.4.0 --secret-file release.key \
  --target x86_64-unknown-linux-musl --out-dir dist/
```

`panpak install-runtime <tarball> --release-key <ed25519:...>`
verifies, in order: presence of the sidecar files, SHA-256, signature
against the key (pinned in
`~/.local/share/panpak/runtimes/release-key` after first use),
extracts to staging (validated paths: no absolutes/`..`), asks the
binary for the ABI (`panpak-stub --abi-version`) and installs into
`runtimes/<abi>/panpak-stub`. Any failure = nothing installed.

## Release key: separate channel

The release public key NEVER travels together with the binaries.
Planned channels (in reliability order):

1. **This repository** (`RELEASE_KEY` file, signed commit): whoever
   clones or verifies the source gets the key from the same channel as
   the code.
2. **Signed announcement**: post/announcement signed with a known
   personal key (e.g. the developer's GPG key) reporting the
   fingerprint (first 16 hex) of the release key.
3. **Explicit TOFU**: `panpak install-runtime` never does TOFU alone —
   first use requires `--release-key`. Verify the fingerprint out of
   band.

Current release key (channel no. 1 — this file):

```
ed25519:494b1e4620b480b39c3e9a569244a86825b676c7b9953b19cb571aa8cdcd5938
```

(Test key generated for development; official releases will use a
dedicated key published here before the first announcement.)

## Release-key rotation

No automatic succession exists for runtimes (unlike apps). Procedure:

1. Generate the new pair (`panpak keygen -o new.key`) and publish its public part
   on the same channels as above, with a rotation notice.
2. Sign new releases with the new key.
3. Existing users see `release key DIFFERENT from pin`: they must
   verify the new announcement and then remove the pin
   (`rm ~/.local/share/panpak/runtimes/release-key`) and reinstall with
   `--release-key <new>`. No automatism downloads new keys.

## Reproducibility check

`scripts/repro-build.sh` builds twice in different directories and compares
the hashes. The working directory is `${XDG_CACHE_HOME:-$HOME/.cache}/panpak-repro`
by default (not `/tmp`); override it with `PANPAK_REPRO_DIR`. The script never
deletes an existing directory: if the default one exists, a timestamped
sibling (`panpak-repro-<date>-<time>-<pid>`) is used; if `PANPAK_REPRO_DIR`
points to an existing path, the script exits with an error. The directory is
kept after the run for inspection and must be removed manually.

Previous work directories are never deleted, and each one takes about 1 GB.
To clean them up: `rm -rf ~/.cache/panpak-repro*`

## What is NOT guaranteed

- The first install stays TOFU on the *channel*: if the attacker
  controls repository + announcements, they can replace key and
  binaries together. Mitigation: multiple independent channels +
  short fingerprint verifiable by voice/print.
- `panpak install-runtime <file> --allow-unsigned` installs local
  binaries with no verification: only for your own builds, never for
  downloads.
