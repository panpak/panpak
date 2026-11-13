# Panpak QUICKSTART: from a folder to a signed `.panpak` in 5 commands

Prerequisites: `panpak` + `panpak-stub` built (`./build.sh`; both are in
`target/x86_64-unknown-linux-musl/release/`, which is best put on the `PATH`).
`panpak build`, `convert` and `install` use the `panpak-stub` that is beside `panpak`; `--stub` names another one, and `pack`
always needs `--stub` (below).

## 1. Prepare the tree

```sh
mkdir -p hello/bin
printf '#!/bin/sh\necho hello from Panpak\n' > hello/bin/hello
chmod +x hello/bin/hello
```

Any Linux tree works (binaries, `.so` in `lib/`, symlinks);
`panpak pack` refuses setuid/setgid.

## 2. Generate the key

```sh
panpak keygen -o hello.key
# secret key written to: hello.key
# key:    ed25519:<64 hex>
# fingerprint (TOFU, first 16 hex): <...>
```

The secret key is written only to the file, created with mode 0600; `keygen`
never overwrites an existing file and never prints the secret. Signing
commands read it with `--secret-file <file>` or the `PANPAK_SECRET_FILE`
environment variable (a path, never the key itself). The file must be a
regular file (no symlink) with no group/other permissions; otherwise run
`chmod 600 <file>`. There is no option to pass the key on the command line.

## 3. Write `hello/panpak.toml`

```toml
[app]
id = "org.example.hello"   # reverse-DNS, REQUIRED
name = "Hello"
version = "0.1.0"
entry = "bin/hello"        # relative, no ..

[update]
key = "ed25519:<64 hex>"   # the public key from before
```

## 4. Pack (sign + join to the stub)

```sh
panpak pack hello --stub ./target/x86_64-unknown-linux-musl/release/panpak-stub \
  --secret-file hello.key -o hello.panpak
# pack: 1 references, 1 unique chunks, 2 entries, root=<...>
```

The official extension is `.panpak`: without `-o`, `pack` and `convert` write
`<app-id>.panpak`. `.pan` is an accepted alias (`-o hello.pan` writes exactly
that name). Every command that reads a package accepts `.panpak`, `.pan` or
any other name, because a file is identified by its magic and signature, never
by its extension.

To rotate the update key, sign with the new key and add
`--rotate-from-file <old-key-file>` (same permission rules as
`--secret-file`).

`panpak pack` chunks (FastCDC 16K/64K/256K), compresses (zstd),
dedups by BLAKE3 hash, signs `manifest || root_hash` and appends
index + manifest + 80-byte footer to the stub.

## 5. Verify and run

```sh
./hello.panpak
# panpak: org.example.hello 0.1.0 — key fingerprint: <...>
# hello from Panpak
```

The first line is the passed verification: signature, root-hash and
every chunk are checked *before* running anything. The app runs in a
Landlock sandbox (read-only tree; private data
folders under `~/.local/share/panpak/userdata/org.example.hello`, see `panpak data path`).

`panpak install-runtime <stub> --allow-unsigned` (a build of your own) and `panpak install-runtime <release.tar.gz> --release-key ...` (a signed
release) stay for the other cases: a runtime installed that way is not replaced by the one beside `panpak` when both have the same ABI.

## Then: install and update

```sh
panpak install hello.panpak               # puts the runtime that comes with panpak in your home if it is missing or older (one line);
                                          # TOFU: confirm with y/yes to pin the key
# panpak install --yes hello.panpak       # explicit non-interactive confirmation
~/.local/bin/org.example.hello       # launcher on the installed runtime
```

### See what is installed, and remove an app

```sh
panpak list                          # id, name, version, disk space of the app and of its data, status
panpak list --json                   # the same for scripts
panpak remove org.example.hello      # launcher, entry, packages and trees go; your settings and data stay
panpak remove --purge-data org.example.hello   # also deletes settings, data and cache, after asking (or --yes)
```

The entry of an installed app in the menu also has a right-click action "Uninstall" (it asks in a dialog and never deletes your
data; it needs `zenity` or `kdialog`, otherwise it only shows the command for a terminal). An app installed before this
version gets the action when you install or update it again.

`remove` refuses while the app is running (`PANPAK-E2002`). It prints where the data stays and how much it weighs. The
runtime and the shared blobs stay. If `remove` is interrupted, the next `panpak` command finishes it. An app that was
cleaned by hand, or half installed, is finished without errors. A launcher or an entry that panpak did not write is left alone.

### Trust on first use (TOFU) prompt

At the first install `panpak install` prints the key fingerprint (first 16
hex characters) and asks `Install and trust this key? [y/N]`. Only `y` or
`yes` (case-insensitive) accepts; any other input, including an empty line,
rejects (`PANPAK-E1003`). Verify the fingerprint out of band before answering.
With no terminal on stdin the command refuses to continue
(`PANPAK-E5004`) unless you pass `--yes`, the explicit non-interactive
confirmation; use it only after checking the fingerprint. The rule itself
(pin, rotation, atomic update) is specified in `PANPAK-FORMAT.md` §3.2.

## Open files (double click and `panpak open`)

```sh
panpak open org.example.hello ~/doc.txt              # read-only
panpak open --write org.example.hello ~/doc.txt      # write to the file
panpak open --write-rename org.example.hello ~/doc.txt  # + atomic save
```

The sandbox grants only that file. `--write-rename` (opt-in, with
warning) also allows temp+rename in the folder (e.g. vim with backup,
gedit), without reading/listing neighbors — but neighbors stay
overwritable and deletable. Double click is read-only (`--write` from
CLI only).

## Desktop integration (file type)

```sh
panpak desktop-integration install   # per-user MIME type for .panpak and .pan
panpak desktop-integration remove    # removes only what `install` added
```

`install` copies the shared-mime-info definition of `application/x-panpak` to
`$XDG_DATA_HOME/mime/packages/` (default `~/.local/share/mime/packages/`) and
runs `update-mime-database` there if it is installed; otherwise it prints a
warning. It never writes outside your user data directory and needs no root.
Packages are recognised by file name only, and the type is not an executable
type, so file managers do not offer to run a package. Installed apps do not
claim this type in their `.desktop` entries.

See `PANPAK-FORMAT.md` (format), `THREAT-MODEL.md` (guarantees and
limits), `distro-matrix.md` (multi-distro tests).

## Convert an AppImage (notes)

```sh
panpak convert Etcher.AppImage --id io.example.app -o app.panpak
# the key is the personal one of ~/.config/panpak/secret.key, made the first time (--secret-file uses another)
```

The manifest declares `[origin]` (file, version, SHA-256, never signed
by upstream): `panpak install` reports it explicitly and `panpak
doctor` shows it. See `distro-matrix.md` for the real cases tried.

## Reproducible builds

Pinned toolchain in `rust-toolchain.toml` (Rust 1.99.1) and `Cargo.lock`
(`cargo build --locked`). C compiler and linker: Zig, installed from the
`ziglang` wheel on PyPI with the version and hash pinned in
`tools/requirements-zig.txt`
(`pip install --require-hashes -r tools/requirements-zig.txt`; `./build.sh` does it). The musl
targets use it through `scripts/zig-cc` and `scripts/zig-ar`
(`.cargo/config.toml`); CI builds, tests and lints with the same Zig.

Check: `scripts/repro-build.sh` builds twice in different directories
and compares hashes — the `panpak` and `panpak-stub` binaries come out
identical (`--remap-path-prefix` on sources and registry zeroes
absolute paths).

## Trust (read before running other people's .panpak)

Directly running a `.panpak` (`./app.panpak`) runs the stub *contained
in the file*: after verification (signature, stub, index, chunks) you
know the file is intact and self-consistent, but the author is who
they claim to be only if *you* verified the key out of band. For
third-party apps always use `panpak install` (shows the fingerprint,
asks for confirmation, pins the key) and launch from
`~/.local/bin/<id>`: that way only the installed runtime runs and
every start re-verifies package and pin.

### C toolchain and cross-machine comparison

Binaries are identical given the full toolchain, *including* the C
compiler: another Zig version, or another C compiler for the musl target,
produces different but equally valid binaries. `scripts/repro-build.sh`
refuses to run with a Zig other than the pinned one, and `Dockerfile.repro`
reproduces the whole environment.

## Error codes

Every error starts with a stable code such as `PANPAK-E1001`. The full list,
with causes and fixes, is in `docs/ERRORS.md`.
