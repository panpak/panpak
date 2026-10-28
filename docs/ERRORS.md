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
| PANPAK-E1001 | invalid manifest signature / invalid remote signature | Signature does not verify against the key in the manifest | Redownload from a trusted source and verify the key fingerprint |
| PANPAK-E1002 | stub does not match the signed one | The runtime part of the file was replaced or damaged | Reinstall from a `.panpak` published by the author |
| PANPAK-E1003 | installation cancelled: key not trusted | The key fingerprint was not confirmed at the TOFU prompt | Verify the fingerprint out of band and answer `y` or `yes`, or pass `--yes` |
| PANPAK-E1004 | key differs from TOFU pin / release key differs from pin | The signing key is not the one pinned at first install: author changed key or possible attack | Verify with the author; a legitimate change must use a signed rotation or a manual reinstall (see RELEASE.md) |
| PANPAK-E1005 | rotation refused / invalid succession / rotation_sig ... | Key changed without a valid signed succession from the pinned key | Ask the author for a package with a correct `rotation_old` / `rotation_sig` |
| PANPAK-E3001 | kernel without Landlock / Landlock unavailable | Kernel older than 5.13 or Landlock disabled; the app is never run unsandboxed | Run `panpak doctor`; use a kernel with `CONFIG_SECURITY_LANDLOCK=y` |
| PANPAK-E4001 | remote version older than installed | Update would downgrade the app | Use `--allow-downgrade` only if you intend it |
| PANPAK-E4002 | HTTPS only | Update URL is plain http (or not allowed) | Use an https URL; `--allow-insecure-http` is for localhost tests only |
| PANPAK-E5001 | unknown flag / unexpected argument / invalid option value | Mistyped or removed option (e.g. `--secret`, `--rotate-from`) | See the usage text printed with the error |
| PANPAK-E5002 | missing option or argument | A required argument or option value is absent, e.g. no `--secret-file` / `PANPAK_SECRET_FILE`, `keygen` without `-o` | Supply it as shown in the usage text |
| PANPAK-E5003 | secret file ... permissions / symlink / not a regular file | Key file readable by group or others, a symlink, or not a regular file | `chmod 600 <file>`; use a regular file |
| PANPAK-E5004 | non-interactive stdin | `install` needs a TOFU confirmation but stdin is not a terminal | Verify the fingerprint, then pass `--yes` |

## Generic codes

| Code | Printed when | Typical command |
|---|---|---|
| PANPAK-E0000 | Untyped format or index error (truncated file, bad index entry, invalid `panpak.toml`, unsafe path) | `pack`, `convert`, `dump-index`, `dump-manifest` |
| PANPAK-E1000 | Untyped signature, trust or release error (for example tarball SHA-256 mismatch, invalid release key) | `keygen`, `release`, `install-runtime` |
| PANPAK-E2000 | Untyped store or materialization error (missing app, unwritable store, unresolved libraries); also the default for `panpak-stub` | `install`, `rollback`, `open`, `doctor`, `desktop-integration`, `panpak-stub` |
| PANPAK-E3000 | Sandbox or permission refusal without a more specific code (partial Landlock, network not filterable, `sockets_allow` unsupported) | `panpak-stub` |
| PANPAK-E4000 | Untyped update or network error (HTTP failure, file changed during download) | `update` |
| PANPAK-E5000 | Unknown or missing subcommand, or an untyped CLI error | any |

When an error carries a specific code, that code is printed even if the
command's generic group is different.
