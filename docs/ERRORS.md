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
| PANPAK-E1001 | invalid manifest signature / invalid remote signature | Signature does not verify against the key in the manifest | Redownload from a trusted source and verify the key fingerprint |
| PANPAK-E1002 | stub does not match the signed one | The runtime part of the file was replaced or damaged | Reinstall from a `.panpak` published by the author |
| PANPAK-E1003 | installation cancelled: key not trusted / new permissions not accepted / deletion not accepted | The key fingerprint, the permissions an update adds, or `data clear` was not confirmed at the prompt | Verify the fingerprint or the permissions, then answer `y` or `yes`, or pass `--yes` |
| PANPAK-E1004 | key differs from TOFU pin / release key differs from pin | The signing key is not the one pinned at first install: author changed key or possible attack | Verify with the author; a legitimate change must use a signed rotation or a manual reinstall (see RELEASE.md) |
| PANPAK-E1005 | rotation refused / invalid succession / rotation_sig ... | Key changed without a valid signed succession from the pinned key | Ask the author for a package with a correct `rotation_old` / `rotation_sig` |
| PANPAK-E2001 | refusing to replace launcher / refusing to replace desktop entry | The launcher in `~/.local/bin` or the `.desktop` file is a symbolic link, is not a regular file, or is a file that panpak did not create (it lacks the `X-Panpak-App-Id` marker for this app) | Check the file; move or remove it yourself if it is yours, then run the command again |
| PANPAK-E2002 | a copy of the app is running / cannot tell whether one is | `panpak remove` found the lock that the copies of an app hold (or another `remove` of the same app is working), or `XDG_RUNTIME_DIR` is not set | Close the app (all its windows and background processes) and run the command again; there is no option to force it |
| PANPAK-E2003 | removal incomplete / store folder cannot be used / unreadable removal journal | Part of a removal could not be done: a data or cache folder is a link or a file (not followed, not removed), a file could not be deleted, a store folder is a link, or the journal of an interrupted removal is damaged | Read the message: it names the paths. Remove a link yourself if it is yours; run `panpak remove` again to finish the rest |
| PANPAK-E2004 | nothing is installed or left over under the id | `panpak remove <id>` found no store folder, package, launcher, entry, data or cache for the id | Check the id with `panpak list` |
| PANPAK-E3001 | kernel without Landlock / Landlock unavailable | Kernel older than 5.13 or Landlock disabled; the app is never run unsandboxed | Run `panpak doctor`; use a kernel with `CONFIG_SECURITY_LANDLOCK=y` |
| PANPAK-E4001 | remote version older than installed | Update would downgrade the app | Use `--allow-downgrade` only if you intend it |
| PANPAK-E4002 | HTTPS only | Update URL is plain http (or not allowed) | Use an https URL; `--allow-insecure-http` is for localhost tests only |
| PANPAK-E5001 | unknown flag / unexpected argument / invalid option value | Mistyped or removed option (e.g. `--secret`, `--rotate-from`) | See the usage text printed with the error |
| PANPAK-E5002 | missing option or argument | A required argument or option value is absent, e.g. no `--secret-file` / `PANPAK_SECRET_FILE`, `keygen` without `-o` | Supply it as shown in the usage text |
| PANPAK-E5003 | secret file ... permissions / symlink / not a regular file | Key file readable by group or others, a symlink, or not a regular file | `chmod 600 <file>`; use a regular file |
| PANPAK-E5004 | non-interactive stdin | `install` needs a confirmation but stdin is not a terminal, or `install`/`update` found new permissions (`--yes` does not accept them) | Check what is asked, then pass `--yes` for the key, or `--accept-new-permissions` for new permissions; `data clear` needs `--yes` |

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
