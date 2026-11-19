# Panpak error codes

Every error printed by `panpak` and `panpak-stub` starts with a stable code:

```
PANPAK-E1001: invalid manifest signature: tampered or badly downloaded file; ...
```

The code is stable across releases; the text after it may change. Scripts
should match the code, never the text. Codes are grouped by the first digit:

| Group | Area |
|---|---|
| E0xxx | Format and index |
| E1xxx | Signature and trust (TOFU, rotation, pin) |
| E2xxx | Store and materialization |
| E3xxx | Sandbox and permissions |
| E4xxx | Updates and network |
| E5xxx | CLI usage and options |

`Exx00`-style generic codes (`E0000`, `E1000`, `E2000`, `E3000`, `E4000`,
`E5000`) are printed when an error has no specific code. The generic code
depends on the command that failed (see "Generic codes" below). The message
after the code always describes the actual problem.

## Specific codes

| Code | Message (start) | Probable cause | How to fix |
|---|---|---|---|
| PANPAK-E0001 | wrong magic: not a panpak file | The file is not a `.panpak`, or uses the pre-rename magic | Check you are opening the right file; repack it with the current `panpak` |
| PANPAK-E0002 | unsupported format version / unknown flags | The package was made by a newer format version | Update `panpak` / the runtime |
| PANPAK-E0003 | incoherent layout / manifest too large / compressed index too large | Truncated, extended or tampered file | Redownload from a trusted source |
| PANPAK-E0004 | root hash mismatch / remote root hash mismatch | Index does not match the signed root hash: corrupt or tampered file | Redownload from a trusted source; do not trust the file if it persists |
| PANPAK-E0005 | corrupt chunk | A chunk does not match its BLAKE3 hash or declared length | Redownload the package |
| PANPAK-E0006 | invalid app.id | The `app.id` is not a name the format allows: ASCII, two or more segments of letters, digits, `_` and `-`, no segment starting with a digit or `-`, 128 bytes at most; no `/`, `..`, empty segment or control character. A valid signature does not make it acceptable | Use an id such as `org.example.app`; ask the author for a corrected package |
| PANPAK-E0007 | invalid manifest field | Another manifest field is unsafe to use: `app.name`, `origin.converted_from` or `origin.version` with control, line-separator or bidirectional characters; `app.version` outside `[0-9A-Za-z._+~-]`; `app.entry` not a safe relative path; a library name in `[abi]` with `/` or spaces; a bad `origin.sha256`, `update.url` or `sockets_allow` | Fix the field in `panpak.toml` and repack, or ask the author for a corrected package |
| PANPAK-E0008 | key or section not known to this panpak | `panpak pack` (and `convert`) read a `panpak.toml` with a key or section that this panpak does not have: a misspelling, or a field of a newer format. The file is refused instead of packed without the field | Check the spelling. If the file is right, get a newer release and install it with `panpak install-runtime <release.tar.gz> --release-key <key>`; the new `panpak` is `bin/panpak` in the panpak store. A signed package that already exists is not checked this way: an older panpak reads it and ignores the fields it does not know |
| PANPAK-E0009 | the package has programs, and each folder with a program needs a link | `pack` found an executable ELF (with an interpreter, not named like a library) in a folder that has no link `.panpak-lib-<h>` to the root of the tree, or the link has another target, or one is missing in the root. The library path of the app is written relative to the program that is run, and the link is what makes it lead into the tree (PANPAK-FORMAT.md section 7.1). Also: a name that the runtime keeps (`.farm` in the root) is in the tree, or the place of a link holds something else | `panpak build` and `panpak convert` make the links. For a folder packed by hand, run the `ln -s` commands that the message lists, from the folder, and pack again |
| PANPAK-E0011 | the package has no `[format]` table | It was made before the table existed (a test version of panpak), so a reader cannot tell what it needs. It is refused before its signature is checked, so that it is not told that its signature is bad | Make the package again with the current `panpak pack` (or `convert`, `build`) |
| PANPAK-E0012 | `[format].critical` is not complete | The manifest uses a field that changes what is safe (`permissions.network`, `launch.env`, ...) that the list does not name: the manifest was changed after `pack` and signed again without making the list again, so an older panpak would not know to refuse the field. The package is refused as malformed | Make the package again with the current `panpak pack` (or `convert`, `build`); a tool that signs manifests must make the list again (`panpak_core::with_recomputed_critical`, or `critical_paths`) |
| PANPAK-E0010 | this file is the record of an installed version | A file of `<store>/packages/*.rec` (or a copy of one) was given where a package is needed (`install`, `update`, `doctor` of a package, `licenses`), or the stub was asked to run one from outside `<store>/packages/`. A record holds the signed index and manifest of a version, not the program or its files (PANPAK-FORMAT.md section 6.3) | Install the `.panpak` package. A record runs only from the store |
| PANPAK-E1001 | invalid manifest signature / invalid remote signature | Signature does not verify against the key in the manifest | Redownload from a trusted source and verify the key fingerprint |
| PANPAK-E1002 | stub does not match the signed one | The runtime part of the file was replaced or damaged | Reinstall from a `.panpak` published by the author |
| PANPAK-E1003 | installation cancelled: key not trusted / a first install that did not say whom it trusts (`--yes` alone) / new permissions not accepted / deletion not accepted | The key fingerprint, the permissions an update adds, or `data clear` was not confirmed at the prompt | Verify the fingerprint or the permissions, then answer `y` or `yes`, or pass `--yes` |
| PANPAK-E1004 | key differs from the pin / the key of the package is not the one given with `--expect-key` / the recovery commitment differs from the pinned one / release key differs from pin | The signing key is not the one pinned at first install: author changed key or possible attack | Verify with the author; a legitimate change must use a signed rotation or a manual reinstall (see RELEASE.md) |
| PANPAK-E1005 | succession refused: no succession, a bad signature, a step that does not start from the pin, a root step that changes the recovery commitment | Key changed without a valid chain of successions from the pinned key (spec §3.2) | Ask the author for a package made with `--rotate-from-file` (and the same recovery commitment), or with `--recover-with` (the 24 words) |
| PANPAK-E2001 | refusing to replace launcher / desktop entry / icon | The launcher in `~/.local/bin`, the `.desktop` file or an icon file `panpak.<id>.png` is a symbolic link, is not a regular file, or is a file that panpak did not create (it lacks the `X-Panpak-App-Id` marker for this app) | Check the file; move or remove it yourself if it is yours, then run the command again |
| PANPAK-E2002 | a copy of the app is running / cannot tell whether one is | `panpak remove` found the lock that the copies of an app hold (or another `remove` of the same app is working), or `XDG_RUNTIME_DIR` is not set | Close the app (all its windows and background processes) and run the command again; there is no option to force it |
| PANPAK-E2003 | removal incomplete / store folder cannot be used / unreadable removal journal | Part of a removal could not be done: a data or cache folder is a link or a file (not followed, not removed), a file could not be deleted, a store folder is a link, or the journal of an interrupted removal is damaged | Read the message: it names the paths. Remove a link yourself if it is yours; run `panpak remove` again to finish the rest |
| PANPAK-E2004 | `<id>` is not installed: nothing to remove / the app was removed | `panpak remove <id>` found no store folder, package, launcher, entry, icon, data or cache for the id (a second `remove` of the same id), or found only its data and `--purge-data` was not given (the message says where the data is); or a start came in while `panpak remove` took the app away and found it gone | Check the id with `panpak list`; install the app again to use it |
| PANPAK-E2005 | the system glibc is older than the app needs | `install` or `update` found `abi.glibc_min` (written by `pack` from the symbol versions of the files) higher than the glibc of this system | Install on a system with glibc `glibc_min` or newer; there is no workaround in the package |
| PANPAK-E2006 | the library folder of the app cannot be reached | At the start, `<tree>/.farm` is missing or does not point to `<store>/farms/<root>`, or that folder is not a folder. The library path of the app leads through it; with it gone the loader would skip the entry and the app would start with other libraries than the ones chosen, with no message | Remove the folder `trees/<root>` of the store, which the message names (it is made again at the next start), and start the app again; if it comes back, run `panpak install` on the package again |
| PANPAK-E2007 | the files of the app are missing from the store | The tree of an installed version (`trees/<root>`) is not there or is not marked complete: the record cannot make it again, since it holds no chunks. Also: `panpak update` found the files of the installed version missing; `panpak rollback` found those of the previous version missing; `panpak doctor <app id>` found files that do not match the signed index | `panpak repair <app id>` fetches what is missing from the update URL of the app (the same version), or from the package file with `--from <package>`; a running copy of the app stops it |
| PANPAK-E2008 | another panpak command is working on this app | `install`, `update`, `rollback`, `repair` or `remove` of the same app took the lock `<store>/locks/<id>.lock` (PANPAK-FORMAT.md 6.4) and the one that you ran waited 60 seconds for it | Wait for the other command to end (`ps` shows it), then run yours again; a command that was killed never leaves the lock held |
| PANPAK-E3001 | kernel without Landlock / Landlock unavailable | Kernel older than 5.13 or Landlock disabled; the app is never run unsandboxed | Run `panpak doctor`; use a kernel with `CONFIG_SECURITY_LANDLOCK=y` |
| PANPAK-E3002 | the seccomp filter could not be installed | Kernel without `CONFIG_SECCOMP_FILTER`, an architecture the filter does not cover, or the kernel refused the program; the app is never run without the filter | Run `panpak doctor`; use a kernel with seccomp filter support |
| PANPAK-E4001 | remote version older than installed | Update would downgrade the app | Use `--allow-downgrade` only if you intend it |
| PANPAK-E4003 | a revision below the floor | The author set a `floor` for the app (`[release].floor`): no revision below it is installed, updated to or rolled back to, not even with `--allow-downgrade` | Take a package of the floor revision or later |
| PANPAK-E4004 | two publications with the same revision | Two packages that differ carry the same `revision`: one of them is a fork or a replay. Nothing is changed | Ask the author which one is the release; do not install the other |
| PANPAK-E4005 | a different channel | The package is of another channel (`stable`, `beta`) than the installed app, which follows its channel from the install | `--switch-channel` if you mean to change channel |
| PANPAK-E4006 | the history does not continue the installed version | The installed release is not in the history that the new package carries (a fork or a forgery), the history was dropped, or the file is damaged. Nothing is changed | Do not install it; ask the author. `panpak repair` if the files of the installed version are damaged |
| PANPAK-E4002 | HTTPS only | Update URL is plain http (or not allowed) | Use an https URL; `--allow-insecure-http` is for localhost tests only |
| PANPAK-E5001 | unknown flag / unexpected argument / invalid option value | Mistyped or removed option (e.g. `--secret`, `--rotate-from`) | See the usage text printed with the error |
| PANPAK-E5002 | missing option or argument | A required argument or option value is absent, e.g. no `--secret-file` / `PANPAK_SECRET_FILE`, `keygen` without `-o` | Supply it as shown in the usage text |
| PANPAK-E5003 | secret file ... permissions / symlink / not a regular file | Key file readable by group or others, a symlink, or not a regular file | `chmod 600 <file>`; use a regular file |
| PANPAK-E5004 | non-interactive stdin | `install` needs a confirmation but stdin is not a terminal, or `install`/`update` found new permissions (`--yes` does not accept them) | Check what is asked, then pass `--yes` for the key, or `--accept-new-permissions` for new permissions; `data clear` needs `--yes` |
| PANPAK-E6000 | `init` or `build` failed | A recipe is not valid, a source or image could not be fetched or does not match its hash or digest, a build step failed, or a plugin that a linked module needs is not in the package; the message says which and what to do | Read the message; the end of the log of a failed step is in it |
| PANPAK-E6005 | not enough memory for this build | `build.memory` of the recipe (with `build.jobs`) is more than the memory that is available, even with one job | Free memory or close programs; the recipe's `build.memory` is what the build was measured to need |

## Generic codes

| Code | Printed when | Typical command |
|---|---|---|
| PANPAK-E0000 | Untyped format or index error (truncated file, bad index entry, invalid `panpak.toml`, unsafe path) | `pack`, `convert`, `dump-index`, `dump-manifest` |
| PANPAK-E1000 | Untyped signature, trust or release error (for example tarball SHA-256 mismatch, invalid release key) | `keygen`, `release`, `install-runtime` |
| PANPAK-E2000 | Untyped store or materialization error (missing app, unwritable store, unresolved libraries); also the default for `panpak-stub` | `install`, `rollback`, `open`, `doctor`, `desktop-integration`, `panpak-stub` |
| PANPAK-E3000 | Sandbox or permission refusal without a more specific code (partial Landlock, network not filterable, `sockets_allow` unsupported, a `home_paths` entry that is a symbolic link or, for `rw`, a file; an app folder that is a link or a file) | `panpak-stub` |
| PANPAK-E4000 | Untyped update or network error (HTTP failure, file changed during download) | `update` |
| PANPAK-E5000 | Unknown or missing subcommand, or an untyped CLI error | any |

When an error carries a specific code, that code is printed even if the
command's generic group is different.

## Exit status

Every `panpak` command ends with status 0 when it did what was asked and 1 when it printed an error with a code
from this table. `panpak remove` exits 1 when it could not complete the removal (`PANPAK-E2003`), but everything
it could remove is removed first; a second run finishes the rest. `panpak list` exits 0 whatever it lists.
