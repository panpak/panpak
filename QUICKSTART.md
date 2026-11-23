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

## 2. Generate the keys

```sh
panpak key new -o hello.key        # without -o: ~/.config/panpak/secret.key, the key that `pack`, `build` and `convert` use
# secret key written to: hello.key
# key:    ed25519:<64 hex>
# fingerprint (TOFU, first 16 hex): <...>
# recovery commitment written to: hello.key.recovery (blake3:<...>)
# RECOVERY PHRASE: 24 words, shown once
```

A key is never made by itself: `pack`, `build` and `convert` without one stop and name this command. `key new` makes two keys. The **signing key** is in the file
(mode 0600). The **recovery key** is shown once as 24 words: write them on paper and keep them away from the computer; the file `hello.key.recovery` holds only its
commitment (a hash), which every package you make carries from the first one. If the signing key is ever stolen or lost, the 24 words (`pack --recover-with`) take
your apps back; nobody can do that without them. The 24 words are the English list of BIP-39, so any tool that reads BIP-39 can check them. To change the signing key on purpose, make the new package with `--rotate-from-file <old key> --recovery <the same
commitment>` once for each app: the succession is kept beside the new key (`<key>.chain`) and every later package of that app carries it by itself, so a user who stayed on the old key still reaches the new one; to sign with a token or an agent, `pack --sign-command <cmd> --public-key <key> --recovery <commitment>` (the command gets the bytes on its standard
input and writes the 64 bytes of the signature). `keygen -o` is the same command, for scripts.

The secret key is written only to the file, created with mode 0600; it
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
release) stay for the other cases: a runtime verified as a release is not replaced by the one beside `panpak` when both have the same ABI (one installed by hand from a build of your own is: the program that runs brings its own).

## Then: install and update

```sh
panpak install hello.panpak               # puts the runtime that comes with panpak in your home if it is missing or older (one line);
                                          # TOFU: confirm with y/yes to pin the key
# panpak install --expect-key ed25519:<the key the author published> hello.panpak   # no question; another key is refused and nothing is pinned
# panpak install --trust-first-key hello.panpak     # no question; you trust the key of this file (a script: add --yes for the other questions)
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
(`PANPAK-E5004`) unless you say whom to trust: `--expect-key <key>` (the key the author published: a package of
another key is refused, `PANPAK-E1004`, and nothing is pinned) or `--trust-first-key` (you trust the key of this file). `--yes` alone is not enough for a first install
(`PANPAK-E1003`), **unless the domain of the author confirms the key**: when the package names a domain, `install` asks
`https://<domain>/.well-known/panpak.toml` first (once, it says what that server learns), and a confirmed key needs only `--yes`; the domain is shown on a line of its own (in the `xn--` form if it has characters outside ASCII) and the key is
behind `--show-key`. A domain that says the key is not the author's stops the install (`PANPAK-E1006`, even with `--yes` and `--trust-first-key`; `--despite-domain-mismatch`
goes on, and you still say whom you trust). `--offline` (one command) and `panpak config set trust.confirm_domain off` do not ask; the key is then "not confirmed". The line "Updates: from <host>, named by the author" says where the new versions will come from. The rule itself
(pin, rotation, atomic update) is specified in `PANPAK-FORMAT.md` §3.2.

## Update, roll back, repair, and the software bill of materials

```sh
panpak update org.example.app      # downloads only the chunks that the installed version does not have, and moves `current` when all is on disk
panpak rollback org.example.app    # back to the previous version (the one that was installed before the last update)
panpak repair org.example.app      # the files right again, from the update URL or from `--from <package>`
panpak verify org.example.app      # reads every file against the signed index now; changes nothing
panpak gc                          # takes out of the store what no installed version reaches (`--dry-run` shows it)
panpak sbom pkg.panpak             # the SBOM (CycloneDX 1.6): the way to give a package to a scanner of dependencies
panpak sbom --export pkg.panpak    # the same, completed with the name, id and version, a serial number and the time
```

`panpak sbom` also takes an app id (the installed one) or an https address (only the parts of the package that hold the SBOM are downloaded).
A rollback to a version below the floor that the author set is refused. A rollback does not undo what the newer version did to the data of the app.

## Open files (double click and `panpak open`)

```sh
panpak open org.example.hello ~/doc.txt              # read-only
panpak open --write org.example.hello ~/doc.txt      # write to the file
panpak open --write-rename org.example.hello ~/doc.txt  # + atomic save
```

With the document portal (a desktop session has one), the file is given to the app as a document, with the rights that you have on it (read, and write when you can
write the file), and the portal lets the app save by replacing the file; `--write` and `--write-rename` are then not used, and the stub says so. Without the portal the sandbox
grants only that file, and read-only unless `--write`. `--write-rename` (opt-in, only without the portal) also allows temp+rename in the folder (e.g. vim with backup,
gedit), without reading/listing neighbors — but neighbors stay overwritable and deletable, and so is everything in the folders below
it. For that reason it is refused for a folder that is your home or
holds it, or that holds or lies in a place where a file is a way to
run a program later (`~/.config`, `~/.local`, `~/.ssh`, the shell
start files, `~/.config/autostart`, ...): use `--write`, or put the
file in a folder of its own.

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
# the key is the personal one of ~/.config/panpak/secret.key, made before with `panpak key new` (--secret-file uses another)
```

The manifest declares `[origin]` (file, version, SHA-256, never signed
by upstream): `panpak install` reports it explicitly and `panpak
doctor` shows it. See `distro-matrix.md` for the real cases tried.

A conversion has no permissions unless the converter declares them (the one guess that `convert` makes is `x11` for an app that has only the xcb plugin of Qt). To declare them, give a file with
the `[permissions]` tables (nothing else is read from it): `--permissions perms.toml`. The desktop shows the icon of an app in the task bar by the window class that its windows have, which is not always the one in the desktop file of the AppImage: if the task bar shows a gear, `--wm-class <class>` sets it (the class is the `app_id` or `WM_CLASS` of the window; `[desktop] wm_class` in the recipe of a `panpak build`). To publish updates of the conversions, `--update-url https://example.org/app.panpak` writes `[update].url`, and
`--previous old.panpak` makes the new conversion follow the old one (the revision goes up and the history continues, as with `pack --previous`).
`--append <path in the tree>=<file>` adds the text of a file at the end of a file of the program that the AppImage has (a setting that the converter chooses for everyone who installs the conversion; for example GIMP's system `gimprc`, where its check for updates is turned off because the updates of the conversion come from Panpak). The file of the tree must be there, a regular file, with no link on the way; the text is at most 4 KiB; when it starts with a comment line (`# ...`), the report of the conversion says it as the reason: `appended 93 bytes to usr/etc/gimp/3.0/gimprc (The check for updates is off: the updates of this app come from Panpak.)`. The program reads its own settings after this file, so a person can still say otherwise.

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
