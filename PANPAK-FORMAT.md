# Panpak Format v1 (normative)

Status: version 1 (draft). This specification defines the format of
`.panpak` packages. Sections marked **[NORMATIVE]** bind every
implementation; **[INFORMATIVE]** sections describe the reference
implementation or the rationale and are not binding.

## 0. Conventions and terminology [NORMATIVE]

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT",
"SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this
document are to be interpreted as described in RFC 2119. All numbers
are little-endian. Hash = BLAKE3 (32 bytes). Signature = Ed25519.
All byte comparisons are exact (no normalization).

- **Writer**: produces a package (`panpak pack`, `panpak convert`).
- **Reader**: reads and verifies a package before using it
  (`panpak install/run/update/doctor`, `panpak-stub`, independent verifier).
- **Package**: a single `.panpak` file (stub + chunks + index + manifest + signature + footer).
- **Stub**: the initial executable bytes of the package (static ELF).
- **Runtime**: the stub installed under `runtimes/` that runs packages.
- **Store**: local data directory (`~/.local/share/panpak`, see §6).
- **TOFU pin**: public key saved at first `install`, see §3.2.
- **Previous format**: package without `[runtime].stub_hash`
  (pre-v1): a conforming Reader MUST reject it.

## 1. File layout [NORMATIVE]

```
+---------------------------+  offset 0
| static executable stub    |  ELF; the kernel runs it, ignores the rest
+---------------------------+
| chunk 0 | chunk 1 | ...   |  zstd, each independent
+---------------------------+  <- index_off
| index (zstd)              |
+---------------------------+  <- manifest_off
| manifest.toml | sig (64)  |
+---------------------------+
| footer (80 bytes)         |  last 80 bytes of the file
+---------------------------+
```

An HTTP client reads the footer first (Range over the last 80 bytes),
then manifest and index, then only the chunks it is missing
(`panpak update` does exactly this; chunks already in the store are
never transferred).

*Informative note on file names.* The official file extension is `.panpak` and
`.pan` is an accepted alias. A reader identifies a package by its magic and
its signature only, never by its extension. The provisional MIME type is
`application/x-panpak`; a registration with IANA as `application/vnd.panpak`
is planned.

## 2. Footer (80 bytes) [NORMATIVE]

| off | type     | field         |
|-----|----------|---------------|
| 0   | [u8;8]   | magic `PANP\0\1\0\0` |
| 8   | u32      | version (=1)  |
| 12  | u32      | flags (0)     |
| 16  | u64      | index_off     |
| 24  | u64      | index_len     |
| 32  | u64      | manifest_off  |
| 40  | u64      | manifest_len  |
| 48  | [u8;32]  | root_hash = BLAKE3(raw index) |

## 2.1 Versioning and compatibility [NORMATIVE]

- `version` (u32) is the format **generation**. 1 = this specification.
  Readers MUST reject any other value (fail-closed, never interpret
  unknown layouts); the error message reports the value found.
- `flags` (u32): all bits reserved, MUST be 0. Readers MUST
  reject other values, so future flags take effect instead of
  going unnoticed.
- Without a major bump, only what old Readers already handle is
  allowed: new **optional** TOML keys/sections (Readers ignore unknown
  ones; `panpak pack` rejects them on *input* instead, so it never
  unknowingly produces packages different from the intended ones), new
  runtime policies (Landlock/ABI), new CLI subcommands.
- A major bump is required for: footer/index layout, signed message,
  chunk encoding, stricter caps, new REQUIRED sections, changed
  semantics of existing fields, new chunks beyond the v1 caps.
- Normative v1 caps (part of the format, anti-DoS): a Reader MUST
  reject TOML manifests > 1 MiB, compressed or decompressed index
  > 256 MiB, raw chunks > 1 MiB, compressed chunks > 2 MiB, paths and
  symlink targets > 4096 bytes. Values beyond = non-v1 file, rejected
  (a future format may raise them with a major bump).
- The format version (footer) is INDEPENDENT of the runtime ABI version
  (`[runtime].min_runtime`): the former describes bytes, the latter
  the behaviors required of the runner.
- v1 canonicity notes (not negotiable in the future without a major
  bump, a Reader MUST apply all of them):
  little-endian everywhere; UTF-8 paths compared byte for byte with
  no Unicode normalization; empty files = zero chunks (never 0-byte
  chunks; a manifest missing the REQUIRED fields of §3.1 is invalid);
  `mode` only permission bits (0o777) (setuid/setgid rejected in pack,
  masked on read); absolute symlinks recorded but always rejected at
  runtime; no timestamps/owners in metadata (reproducible builds by
  construction).

## 3. Manifest [NORMATIVE]

`manifest_len` is the length of the **TOML alone**. Immediately after,
at `manifest_off + manifest_len`, come 64 bytes of Ed25519 signature.
The signature MUST cover `manifest_toml || root_hash`, where
`root_hash` is the 32 raw footer bytes (not hex) and `manifest_toml`
the exact stored bytes.

Mandatory layout consistency checks (reject on mismatch):

```
index_off + index_len == manifest_off
manifest_off + manifest_len + 64 == file_size - 80
```

No holes or extra data between index, manifest+signature and footer:
offsets MUST NOT point at arbitrary file regions.

### 3.1 Schema (`panpak.toml`) [NORMATIVE]

Fields marked "REQUIRED" MUST be present, otherwise the manifest is
invalid. Readers MUST ignore unknown keys and sections (read tolerance
for future extensions); it is `panpak pack` that rejects them on
*input*, so it never unknowingly produces packages different from the
intended ones.

```toml
[app]
id = "org.example.editor"      # REQUIRED, D-Bus style name (§3.1.1)
name = "Editor"                # REQUIRED
version = "1.4.2"              # REQUIRED
entry = "bin/editor"           # REQUIRED, relative path inside the tree

[abi]                          # generated by `panpak pack`
glibc_min = "2.28"
needs = ["libgtk-3.so.0"]
prefer_host = ["libGL*", "libvulkan*", "libEGL*"]

[permissions]                  # static, applied by Landlock
x11 = false                    # opt-in: X11 access, default false
audio = false                  # opt-in: the PipeWire and PulseAudio sockets of the session, default false
network = false                # master switch; false+ABI>=4 => TCP denied
network_allow_tcp = [80, 443]  # outbound ports (connect); only with network=true
network_bind_tcp = []          # listen ports (bind); only with network=true
sockets_allow = ["$XDG_RUNTIME_DIR/bus"]  # path Unix sockets (requires ABI V9);
                               # leading $XDG_RUNTIME_DIR and $HOME expanded

[[permissions.home_paths]]     # last resort, §3.1.2: a fixed path in the home folder
path = ".config/editor"        # relative to the home, no ".."
access = "rw"                  # "ro" or "rw"

[launch]                       # §3.1.2
args = ["--profile", "${XDG_CONFIG_HOME}/editor"]   # put before the user's arguments
[launch.env]
EDITOR_DIR = "${XDG_DATA_HOME}/editor"              # added to the environment

[permissions.portal]           # requested at use time
files = "picker"
camera = "ask"

[update]
url = "https://example.org/editor/index.panpak"   # optional
key = "ed25519:<64 lowercase hex>"              # REQUIRED
rotation_old = "ed25519:<...>"                  # optional: old key authorizing the new one
rotation_sig = "<128 hex>"                      # optional: old-key signature over the 32 raw bytes of the new public key

[runtime]                          # minimum runtime version (stub)
min_runtime = 1                    # optional, default 1
stub_hash = "<64 hex>"            # REQUIRED: BLAKE3 of the stub bytes
                                   # ([0..first chunk]); binds the stub to the signature.
                                   # Packages without it (previous format)
                                   # are rejected: reinstall from a recent .panpak.

[origin]                           # ONLY for converted packages (e.g. AppImage)
converted_from = "Foo-1.0-x86_64.AppImage"
version = "1.0"                    # version declared at conversion time
sha256 = "<64 hex>"                # SHA-256 of the original converted file
signed_by_upstream = false         # always false: the upstream author did not sign
```

### 3.1.1 Field rules [NORMATIVE]

A valid signature only says who wrote the manifest. A Reader MUST reject a
manifest whose fields do not follow the rules below even when the signature
is valid, and a Writer MUST NOT produce one. These fields become file names,
paths, shell words, `.desktop` lines and terminal text, so a Reader MUST NOT
use any of them before this check passes.

`app.id` MUST match this grammar (RFC 5234), in at most 128 bytes. It is
a D-Bus and Flatpak style name, because the XDG portals identify an app by it:

```
app-id  = segment 1*( "." segment )
segment = ( ALPHA / "_" ) *62( ALPHA / DIGIT / "_" / "-" )   ; 1 to 63 bytes
```

So an id is ASCII, has at least two non-empty segments, no segment starts with
a digit or a hyphen (a launcher is named after the id, and a name starting
with `-` would be read as an option), it does not start or end with a dot, and it contains no `/`, no `..`,
no control character and no NUL.

| Field | Rule |
|---|---|
| `app.name` | 1 to 256 bytes of UTF-8. MUST NOT contain a control character (U+0000..U+001F, U+007F..U+009F), U+2028, U+2029, U+200E, U+200F, U+202A..U+202E or U+2066..U+2069 |
| `app.version` | `alnum *63( alnum / "." / "_" / "+" / "~" / "-" )` |
| `app.entry` | A relative path: no empty, `.` or `..` segment, no leading `/`, no control character, at most 4096 bytes |
| `[abi]` `needs`, `prefer_host`, `allow_missing` | Each item: 1 to 255 bytes of printable ASCII (%x21-7E), without `/`, and not `.` or `..` |
| `origin.converted_from` | Like `app.name`, and without `/` |
| `origin.version` | Like `app.name`, at most 64 bytes |
| `origin.sha256` | 64 lowercase hexadecimal digits |
| `update.url` | At most 2048 bytes, no byte %x00-20 or %x7F |
| `permissions.sockets_allow` | Each item: 1 to 4096 bytes, no control character |
| `permissions.home_paths` | At most 32 items, see §3.1.2 |
| `launch.args`, `launch.env` | At most 64 items each, see §3.1.2 |

Paths and symlink targets in the index MUST NOT contain a control character
(U+0000..U+001F, U+007F..U+009F) either.

*Informative note.* `panpak` reports a bad `app.id` as `PANPAK-E0006` and any
other bad field as `PANPAK-E0007` (docs/ERRORS.md).

### 3.1.2 Launch fields and home paths [NORMATIVE]

**`[launch]`.** `args` is a list of strings put before the arguments the user
gives; `env` is a table of variable names and string values added to the
environment of the app. The runtime passes `args` to the executable as a vector:
there is no shell and no word splitting. In every `args` item and every `env`
value, the text `${NAME}` is replaced by the private folder of the app
(§9.1) when `NAME` is one of:

```
XDG_CONFIG_HOME  XDG_DATA_HOME  XDG_STATE_HOME  XDG_CACHE_HOME  TMPDIR
```

Any other `${...}`, and an unterminated `${`, make the manifest invalid; a `$` not
followed by `{` is plain text. A value has at most 4096 bytes and no control
character. A variable name matches `[A-Z_][A-Z0-9_]*` (at most 128 bytes). These
names MUST NOT be set, because they change how code is loaded or what the
sandbox protects, or because the runtime sets them itself:

```
LD_*  PANPAK_*
PATH  HOME  XDG_RUNTIME_DIR  PYTHONPATH  PYTHONHOME  PERL5LIB  NODE_OPTIONS
ELECTRON_RUN_AS_NODE  GIO_MODULE_DIR  GTK_MODULES  GTK_PATH  QT_PLUGIN_PATH
XDG_CONFIG_HOME  XDG_DATA_HOME  XDG_STATE_HOME  XDG_CACHE_HOME
XDG_CONFIG_DIRS  XDG_DATA_DIRS  TMPDIR  SQLITE_TMPDIR
```

**`permissions.home_paths`.** An app that insists on a fixed place in the home
folder can ask for it: a list of tables with `path` and `access`. This is the last
resort: prefer `launch.args` or `launch.env` to point the app at its private
folders. `path` is relative to the home folder: printable ASCII (%x20-7E), no
leading or trailing `/`, no empty, `.` or `..` segment, at most 4096 bytes. `access`
is `ro` or `rw`. A path appears once. These are refused:

- the home folder itself (`.` or an empty path), `.config`, `.local`,
  `.local/share` and `.cache` (the broad folders);
- any path equal to, below, or containing one of these protected paths:

```
.config/autostart  .config/systemd  .config/environment.d  .local/bin
.local/share/applications  .local/share/nautilus  .local/share/nemo
.local/share/kio  .config/panpak  .local/share/panpak  .cache/panpak
.config/mimeapps.list  .config/git  .gitconfig  .config/fish  .config/nvim
.vimrc  .vim  .emacs  .emacs.d  .config/i3  .config/sway  .config/hypr
.config/openbox  .config/kitty  .config/alacritty  .xinitrc  .xsession
.xsessionrc  .xprofile  .Xresources  .bashrc  .bash_profile  .bash_login
.profile  .zshrc  .zprofile  .zshenv  .pam_environment  .ssh  .gnupg  .pki
.password-store  .local/share/keyrings  .mozilla  .config/google-chrome
.config/chromium  .netrc  .git-credentials  .config/gh  .docker  .kube
.aws  .cargo  .npmrc
```

The protected list is incomplete by nature: it names places that run code at
login or hold credentials, and a Writer SHOULD NOT ask for any other place of that kind.
At start, the runtime MUST refuse to run if the path, or any folder above it
inside the home, exists and is a symbolic link. For `rw`, a missing folder is created
and an existing file is an error; for `ro`, a missing path is skipped. The access
is granted by Landlock to the folder opened at start (§9.1).

### 3.2 Public key and TOFU [NORMATIVE]

Strict parsing of `[update].key`: exact prefix `ed25519:`
(lowercase), then exactly 64 characters of `0-9a-f` only — no
uppercase, spaces, or `0x`, so every key has a single representation.
At parse time use `VerifyingKey::from_bytes` and for the signature
`verify_strict`, which rejects malleable keys and signatures.

Security note: the key lives in the self-signed manifest and alone
proves nothing about the author's identity. The TOFU rule applies:

1. At the first installation the client MUST show the key fingerprint
   (first 16 hex characters) and obtain explicit confirmation from the user.
2. The client saves the confirmed key to `apps/<id>/pinned-key`.
3. Updates MUST use the same key or a signed succession: the new manifest
   includes `rotation_old` (equal to the pin) and `rotation_sig` (old-key
   signature over the 32 raw bytes of the new public key).
4. The client verifies the succession against the pin and only then updates
   the pin, atomically with the verified install. Without a valid
   succession, a key change means manual reinstall.

How the confirmation is requested (prompt text, accepted answers,
non-interactive mode) is a client interface matter, not part of the format;
see QUICKSTART.md for the `panpak` client.

### 3.3 Update protocol [NORMATIVE]

A client updating a package MUST:
- verify the remote manifest signature against the pin (NOT against
  the key in the remote manifest: an attacker could self-sign);
- refuse versions older than the installed one, unless explicit
  `--allow-downgrade`;
- assemble the new `.panpak` (missing parts via Range, chunks already
  present copied from the local package), verify it fully as in §5,
  and only then move `current` (the previous version stays in
  `previous`; `panpak rollback` swaps them). Installed launchers use
  the runtime in `runtimes/` and the stub verifies its pin at every
  start; direct execution of a `.panpak` only shows the fingerprint.

A client MUST NOT publish a new version that asks for permissions the
installed version did not have (§3.1, §3.1.2: a feature turned on, a port,
a socket, a home path, `ro` raised to `rw`) unless the user accepted them.
The same holds for installing a package over an installed app. The client
MUST show the new permissions and then either ask at a terminal or take an
explicit option (`--accept-new-permissions` in `panpak`). A generic
"yes" option (`--yes`, meant for trusting the key of a new app) MUST NOT
accept them: with `--yes` and new permissions the client stops with an
error that names the explicit option.

`install` and `update` MUST also print a warning when the app is not
isolated from local sockets: on a kernel without Landlock V9 (D-Bus,
ssh-agent and gpg-agent sockets cannot be blocked), when the manifest lists a
D-Bus socket, and when it asks for X11 (abstract sockets stay open).

### 3.4 Update HTTP transport [NORMATIVE]

The transport MUST use HTTPS only (HTTP only toward localhost/test
with an explicit flag, never following redirects); max 3 redirects,
never with an https->http downgrade; global per-request timeout; caps
BEFORE allocating (manifest 1 MiB, index 256 MiB, file 8 GiB); reads
limited to `expected+1` bytes. Incoherent Content-Range, non-206
responses, or bodies whose length differs from the agreed one MUST be
treated as errors; no writes outside the staging dir.

## 4. Index (after zstd decompression) [NORMATIVE]

```
u32  n_chunks
n_chunks x { [u8;32] hash, u64 offset, u32 comp_len, u32 raw_len }
u32  n_entries
n_entries x {
  u16 path_len, path (UTF-8, relative, canonical form: no "//",
    empty segments, "." or "..", no leading or trailing "/")
  u8  kind        // 0=file 1=dir 2=symlink
  u32 mode        // permission bits only (0o777); setuid/setgid rejected by `panpak pack`
  kind=file:    u64 size, u32 n, u32 chunk_id[n]
                // size MUST equal the sum of the raw_len of the
                // indicated chunks (mandatory coherence check)
  kind=symlink: u16 len, target
}
```

Chunk offsets are relative to the start of the file. Chunks are
produced with FastCDC (min 16K, avg 64K, max 256K) — the exact split
parameters are NOT part of the format: a Reader MUST accept any split
that respects the §2.1 caps. `chunk_id` indexes the chunk table:
identical chunks SHOULD be stored once (a Reader also accepts
duplicates, but still verifies them one by one). Overlapping chunk
ranges are structurally allowed, but every chunk MUST be verified
independently (§5 point 5).

## 5. Verification rules (mandatory) [NORMATIVE]

A Reader MUST run these checks, in this order, and MUST reject the
package at the first failure:

1. Read the footer, check magic and version.
2. Verify the manifest signature, then check the field rules of §3.1.1.
3. Verify that the stub bytes (`[0..first chunk)`) match
   `[runtime].stub_hash` (REQUIRED; absent = previous format, rejected).
   No path ever runs or installs a stub different from the signed one:
   `install`/`update`/`doctor` never run any stub, `run` runs only
   the installed runtime.
4. Decompress the index, verify `BLAKE3(index) == root_hash`.
5. Every chunk MUST be verified (`BLAKE3(raw) == hash`) **before** being written to the store.
6. Reject paths with `..`, absolute paths, paths or symlink targets with
   control characters, or symlinks whose target —
   resolved against the directory containing the link — escapes the
   root. Absolute symlinks are always rejected; `..` staying inside
   the root is legal (e.g. `usr/translations/x -> ../share/...`,
   which means `usr/share/...`).
7. A client MUST NOT run app code before point 4. (Note: on direct
   execution it is the kernel that launches the stub before any
   verification; trust comes from the §5.3 check, which precedes any
   use of the contents.)

## 6. Local store [INFORMATIVE, except §6.1]

Reference-implementation layout (paths under
`~/.local/share/panpak/`):

```
~/.local/share/panpak/
  chunks/<hash>          # (reserved: verified raw-chunk cache)
  blobs/<filehash>-<mode> # whole verified files, shared via hardlink across trees
  trees/<root_hash>/     # materialized tree (hardlinks from identical blobs)
  farms/<root_hash>/     # ABI symlink farm: one link per SONAME (host or bundle, §7)
  runtimes/<abi>/panpak-stub  # installed runtimes (`panpak install-runtime`)
  packages/<id>-<ver>.panpak  # installed packages (signed, verified)
  apps/<id>/current -> ../../trees/<root_hash>
  apps/<id>/previous -> ...   # for rollback
  apps/<id>/pinned-key      # TOFU pin of the update key
  apps/<id>/update.url      # update URL (if declared)
```

Outside the store: launcher `~/.local/bin/<id>` (`exec <runtime> run
<package> "$@"`) and `~/.local/share/applications/<id>.desktop`.

### 6.1 Launcher/runtime contract [NORMATIVE]

The stub MUST refuse packages with `min_runtime` above its own
(`RUNTIME_ABI`, v1 = 1; `panpak-stub --abi-version` prints it). The
launcher MUST run the package only through the installed runtime;
direct execution of the `.panpak` file is allowed but MUST be limited
to showing the key fingerprint (no trust assumed).

## 7. ABI resolution (`[abi]`) [NORMATIVE]

`panpak pack` compiles `needs` by scanning `DT_NEEDED` of the ELF
files in the tree (union with any declared `needs`; names containing
`$ORIGIN` MUST be resolved against the directory containing them).
The stub resolves at SONAME granularity (names, not symbol versions
yet) with two worlds:

- **world H** (no glibc in the bundle): the host wins (`ldconfig -p`,
  fallback to scanning `/lib*` `/usr/lib*`), fallback the copy in
  `trees/<root>/lib/`. Safe for backward compatibility if pack starts
  from an old baseline.
- **world B** (glibc in the bundle): everything in the bundle comes
  from the bundle, loader included (`ld-linux` run with
  `--library-path`). The glibc family (loader+libc+libm+...) is
  atomic: mixing builds dies with `GLIBC_PRIVATE` or
  `version GLIBC_X.YY not found` (both observed in tests). Only
  `prefer_host` (e.g. libGL) and SONAMEs missing from the bundle come
  from the host.

For each decided SONAME the stub creates a symlink in `farms/<root>/`
(to host or bundle) and launches with that `LD_LIBRARY_PATH`:
per-lib control despite the per-directory path. The loader (`ld-*`,
libc's own `DT_NEEDED`) is not part of the farm: it is the plan's
`loader`.

The decision closes over the transitive closure at a fixed point: the
`DT_NEEDED` of already-decided libs (read from the real files, host
or bundle) are decided with the same policy. Without this, bundle
libdirs in `LD_LIBRARY_PATH` would shadow system libs for transitives,
mixing versions (observed: newer host libgio + older bundle
libmount). Never a silent fallback: every missing lib not declared in
`allow_missing` stays a hard error.

Libs in `prefer_host` MUST never be substituted: if missing on the
host it is an error, never the bundle. `prefer_host` + included glibc
is a combination rejected by `panpak pack`.

The Qt/KF families (`libQt5*`, `libQt6*`, `libKF5*`, `libKF6*`) follow
the same atomicity as glibc: if the bundle covers ALL needs of the
family, the whole family comes from the bundle; otherwise, if the host
covers all of them, the whole family comes from the host. Only if
neither coverage is complete does it fall back to per-SONAME rules
(known mix risk: see NOTE v1 below).

`allow_missing` (globs like `prefer_host`) declares SONAMEs the loader
MAY not find: typically plugins loaded via `dlopen` whose DT_NEEDED
exceed the files included in the original package. If actually needed,
the loader fails explicitly. Never usable for the glibc family
(rejected). MUST be declared explicitly (`--allow-missing` of
`panpak convert`): not a silent fallback.

NOTE v1: symbol-version comparison (`VERNEED` vs host) is not part of
v1 — at equal SONAME the §7 resolution applies and a possible
incompatibility fails the loader explicitly (fail-closed). See also
Appendix C.

## 8. Out of scope of v1 [INFORMATIVE]

zstd dictionaries, Landlock-filtered networking (requires ABI >= 4,
kernel 6.7), erofs mounts.

## 9. Runtime sandbox (stub policy) [NORMATIVE for [permissions]; kernel details are INFORMATIVE]

`[permissions].x11` is optional, default false. Only with `true` does
the runtime NOT apply the `AbstractUnixSocket` scope and add the
explicit rule for `/tmp/.X11-unix` (also ResolveUnix if supported);
the signal scope stays active. `install` MUST warn that X11 allows
observing other apps' input and screen. `convert` declares the
permission if it finds a script forcing `QT_QPA_PLATFORM=xcb` or xcb
plugins without Wayland plugins. It is a signed concession to review
before distributing the converted package.

- App tree: Landlock read+execute, never write. After materialization,
  tree and blob files and dirs lose their write bits (`chmod a-w`,
  marker `trees/<hash>.readonly`): blobs are shared across trees and
  apps, a write would alter them everywhere.
- Writable: the two app roots of §9.1, the `rw` home paths (§3.1.2) and
  `/tmp`, all EXCEPT `Refer` (from ABI V2 `from_all` would include it: it
  would allow hardlinks to tree files; without it, same-dir renames stay
  possible). `~/.local/share/<id>` is no longer writable.
- Read-only: the user's theme, font and icon files listed in §9.1 and the
  `ro` home paths.
- Read-only: `/usr` `/lib` `/lib64` `/etc` `/proc` `/sys` `/run`.
  On Landlock below V9 this also lets an app connect to any Unix socket
  under `/run`, including the D-Bus session bus (THREAT-MODEL.md §3).
- `/dev`: only nodes `null`, `zero`, `full`, `urandom`, `random`, `tty`
  + dirs `dri`, `shm`. Explicit fallback (with warning) to whole
  `/dev` if the kernel rejects per-file rules; privileged nodes stay
  protected by DAC/capabilities.
- TCP networking: denied by default (`network = false`); with
  `network = true` only manifest-allowlisted ports (`network_allow_tcp`
  for connect, `network_bind_tcp` for listen). Landlock filters the TCP
  port, not the address: a granted port is open to every address,
  loopback or not. Needs Landlock V4 (kernel 6.7 or later). On kernels without
  Landlock V4 the network is not filterable: the runtime MUST refuse
  by default, and MAY start ONLY with `--network-unfiltered` from the
  CLI (never from the manifest), with a warning at every start. UDP is
  never filtered.
- Landlock ABI: highest supported detected with HardRequirement (on
  kernel 7.0: V8; V9 declared by the crate but unsupported by the
  kernel). Minimum: V1 (kernel 5.13).
- Fail-closed self-test after `restrict`: `/dev/null` MUST open and
  creating a file in the tree MUST fail, otherwise hard error.
- If Landlock is not applicable: hard error, NEVER silent fallback.

### 9.1 Private folders and environment [NORMATIVE]

Before the sandbox is applied, and after the manifest has been verified, the
runtime creates these folders with mode 0700 (an existing folder is kept; a symbolic link
or a file in its place is an error):

| Folder | Path |
|---|---|
| persistent root | `$XDG_DATA_HOME/panpak/userdata/<id>` (default `~/.local/share/...`) |
| config, data, state, tmp | `config`, `data`, `state`, `tmp` below the persistent root |
| cache | `$XDG_CACHE_HOME/panpak/<id>` (default `~/.cache/...`) |

They are not inside the store. The runtime sets, for the app:

| Variable | Value |
|---|---|
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` | `config`, `data`, `state` |
| `XDG_CACHE_HOME` | the cache folder |
| `TMPDIR`, `SQLITE_TMPDIR` | `tmp`; `/tmp` inside the isolated view (§9.2); the placeholder `${TMPDIR}` in `[launch]` always means the `tmp` folder |
| `XDG_CONFIG_DIRS`, `XDG_DATA_DIRS` | the existing value, or the XDG default (`/etc/xdg`; `/usr/local/share:/usr/share`), then the user's real config or data folder |
| `GTK_THEME` | see below |
| `SSH_AUTH_SOCK`, `GPG_AGENT_INFO`, `SSH_AGENT_PID` | removed |
| `DBUS_SESSION_BUS_ADDRESS`, `DBUS_SYSTEM_BUS_ADDRESS` | `disabled:`, an address no D-Bus library can use, unless `sockets_allow` lists that bus (a socket named `system_bus_socket` is the system bus, any other D-Bus socket the session bus); then unchanged |
| from `[launch]` | `env` values, and `args` before the user's arguments |

`HOME` and `XDG_RUNTIME_DIR` are not changed. Hiding the agent and bus variables is hygiene, not a barrier: below Landlock V9 an app can still open `$XDG_RUNTIME_DIR/bus` or an agent socket by path. Libdbus, GDBus and sd-bus do not look for the bus when the address is `disabled:` (tested with a private bus). The app can write only in the two roots,
the `rw` home paths and `/tmp` (to be narrowed later). Landlock grants these
read-only rights, for the entries that exist, and nothing else of the user's real
folders:

- in the config folder: `fontconfig`, `gtk-3.0`, `gtk-4.0`, `kdeglobals`,
  `user-dirs.dirs`, `mimeapps.list`, `qt5ct`, `qt6ct`, `Trolltech.conf`;
- in the data folder: `fonts`, `icons`, `themes`, `mime`;
- in the home folder: `.fonts`, `.icons`.

*Fonts.* Fontconfig reads user fonts only from `$XDG_DATA_HOME/fonts` and the
configuration in `$XDG_CONFIG_HOME/fontconfig`, which are private now. At every start the runtime
writes `$XDG_CONFIG_HOME/fontconfig/fonts.conf` in the private folder. It includes (with
`ignore_missing`) the user's real `fonts.conf` and `conf.d`, and adds as `<dir>` the real
font folder and `~/.fonts`. The file carries the marker `X-Panpak-Generated`; a file
without the marker is the user's and the runtime does not touch it.

*Dark theme.* GTK3 reads the dark preference from dconf, which the app cannot reach
with a private config folder. If `GTK_THEME` is not set and `gsettings` is at `/usr/bin/gsettings` or `/bin/gsettings` (never searched in `PATH`),
the runtime reads `org.gnome.desktop.interface` `color-scheme` and `gtk-theme` before
the sandbox, waiting at most 200 ms in total. If the scheme is `prefer-dark`, it sets
`GTK_THEME=<gtk-theme>:dark` (`Adwaita:dark` when `gtk-theme` is empty). Any failure
is ignored. The value is read once at start: the theme does not follow changes while
the app is open. The settings portal is planned to replace this.

Landlock binds each rule to the folder opened when the rules are built, so a later
replacement of a path by a link does not extend the access.

### 9.2 Isolation layers and the isolated view [NORMATIVE for the policy; the mechanism is INFORMATIVE]

The runtime keeps an app away from local Unix sockets (D-Bus, systemd of the user, gpg-agent,
ssh-agent, audio servers) with independent layers, and uses every layer the machine offers:

1. *Isolated view.* If `/usr/bin/bwrap` exists and works, the app runs in a view built by
   bubblewrap. The runtime MUST take bubblewrap only from that fixed path (never from `PATH`)
   and MUST NOT use it when it is writable by others. The view has: `/` read-only; `/dev` as
   it is; a new `/proc`; a new tmpfs for `/tmp` and for `/run`; a new tmpfs for `$XDG_RUNTIME_DIR`
   with only the sockets of the policy below; the private folders of §9.1 and the `rw` home paths
   writable; the folder in `/run` that holds the target of `/etc/resolv.conf` read-only (name resolution);
   the X11 socket of `$DISPLAY` only with `x11 = true`. The view uses its own user, PID and mount
   namespaces, and its own IPC namespace unless `x11 = true`. It shares the network namespace. The app
   is in a new session and ends with the runtime. Inside, `TMPDIR` and `SQLITE_TMPDIR` are `/tmp`.
2. *Landlock*, as in §9, applied inside the view when there is one. With Landlock V9 (path
   sockets) the sockets of the policy get a `ResolveUnix` rule and every other path socket is denied.
3. *None*: the runtime starts the app with Landlock alone, as before the view existed, and `install`,
   `update` and `doctor` MUST say that path sockets are not blocked.

The sockets of the policy are the `sockets_allow` entries and these, when they exist: the Wayland
socket named by `WAYLAND_DISPLAY`, the journal sockets `/run/systemd/journal/socket` and
`/run/systemd/journal/dev-log`, the resolver socket `/run/systemd/resolve/io.systemd.Resolve`, and, with
`audio = true`, `$XDG_RUNTIME_DIR/pipewire-0` and `$XDG_RUNTIME_DIR/pulse/native`.
`sockets_allow` is accepted when at least one of Landlock V9 and the view is available.
The runtime reports the active layers (`panpak doctor`, `install`, `update`). Abstract Unix sockets are
not hidden by the view; they are blocked only by the Landlock scope, which `x11 = true` turns off.
The outer runtime verifies the package; the inner one, which Landlock then restricts, receives the
decisions of the outer one and does not verify the package again (THREAT-MODEL.md §3).

## Appendix A — test vectors [NORMATIVE]

Minimal valid package (1 file `bin/hi` = `hi\n`, 3 bytes; fake stub of
21 bytes `PANPAKTEST-VECTOR-v1\0`; fixed test key — NEVER use in
production). Verified by both the independent verifier
(`conformance/verify.py`) and the Rust side (`panpak doctor`): both
MUST accept it and report the same values.

- total size: 536 bytes
- stub `[0, 21)`: `50414e50414b544553542d564543544f522d763100`
  (`PANPAKTEST-VECTOR-v1\0`)
- chunk 0 `[21, 37)`, zstd level 3 of `hi\n`:
  `28b52ffd045819000068690a343d5092` (16 compressed bytes, 3 raw)
- compressed index `[37, 140)` (103 bytes):
  `28b52ffd0458d50200e404010000000b8b60248fad7ac6dfac221b7e01a8b91c772421a15b387dd1fb2d6a94aee438150010000000030000000262696e01ed010000060062696e2f686900ed010000000001000000000000000300859ab072a5074006494a273f`
- raw index (95 bytes, whose BLAKE3 is the root_hash below):
  `010000000b8b60248fad7ac6dfac221b7e01a8b91c772421a15b387dd1fb2d6a94aee4381500000000000000100000000300000002000000030062696e01ed010000060062696e2f686900ed01000003000000000000000100000000000000`
  (n_chunks=1: hash=`0b8b6024…f7e01`, off=21, comp=16, raw=3;
  n_entries=2: dir `bin`, file `bin/hi` mode 0755 size 3 chunk [0])
- root_hash = `BLAKE3(raw index)` =
  `ad7a506ac9fbaf289065933e9217f2a552593b0ff77fd458ad16d0695f9853f5`
- manifest TOML `[140, 392)` (252 bytes):
  `[app]\nid = "org.example.tv"\nname = "TV"\nversion = "1.0"\nentry = "bin/hi"\n[runtime]\nstub_hash = "503da793bbca6afccdd883c02e32c139ef3a589894325916eddc2bf67b87520c"\n[update]\nkey = "ed25519:e1f41a83ba433a12cef2009dc083b0af7eb8c92a9edd35109ae291b3373a6f0b"\n`
- signature `[392, 456)` (64 bytes Ed25519 over `manifest_toml || root_hash`):
  `a5494d40cdbb4b4e254825dcd258d3ab490c39f6e8e2407c73bea2c331791e4f9c6bb09e7be4c9e87fd958608261aa5de01601ec3d880a365a9edc483443f108`
- footer `[456, 536)` (80 bytes):
  `50414e50000100000100000000000000250000000000000067000000000000008c00000000000000fc00000000000000ad7a506ac9fbaf289065933e9217f2a552593b0ff77fd458ad16d0695f9853f5`
  (magic `PANP\0\1\0\0`, version=1, flags=0, index_off=37, index_len=103,
  manifest_off=140, manifest_len=252, root_hash=see above)

Invalid vectors: the conformance suite (`conformance/fixtures/`,
18 cases: wrong magic/version/flags, incoherent layout, tampered
signature/sizes/root/chunk, non-canonical paths, plus `unknown-magic.bin`:
a file whose magic is not exactly the one defined in this section MUST be rejected) with expected
outcome in `conformance/fixtures/expected.json`.

## Appendix B — conformance file index [INFORMATIVE]

- `conformance/verify.py`: independent verifier (pure BLAKE3 in
  `conformance/b3.py`, Ed25519 via `cryptography`, zstd via CLI).
  Reads only this specification.
- `conformance/mkpanpak.py`: package builder for tests.
- `conformance/gen.py`: generates `conformance/fixtures/*.panpak` +
  `expected.json` (3 valid, 18 invalid with expected error, including the legacy one with old magic).
- `conformance/run.py`: runs the verifier over every fixture and
  compares against the expected; with `--rust` re-checks the valid
  ones with `panpak doctor` (agreement between implementations required).
- `fuzz/corpus/`: fuzzing corpus, including `real_panpak-*.panpak`
  (valid footers with current magic).

## Appendix C — resolved ambiguities and notes [INFORMATIVE]

Ambiguities found comparing the two implementations (fixed without
changing behavior):
- chunk record size: 48 bytes (32+8+4+4), not 44;
- file `size` MUST equal the sum of `raw_len` (§4);
- paths with `.` or empty/`//` segments rejected (canonical form, §4);
- `doctor` also verifies chunks, not just metadata.
Notes: on direct execution the kernel launches the stub before any
verification (trust comes from the §5.3 check, which precedes any use
of the contents); overlapping or duplicate chunks are structurally
allowed but verified one by one; symbol-version comparison
(`VERNEED`) is not part of v1.

## Appendix D — licensing, embedded licenses and patents [INFORMATIVE]

**Embedded license texts.** `panpak pack` adds NOTICE and the license texts
(`LICENSE-MIT`, `LICENSE-APACHE`, `THIRD-PARTY-LICENSES`) to every package as
ordinary regular files under `.panpak/licenses/`, because the runtime stub is
redistributed inside third-party packages. This uses the normal tree and index
rules of §4 and changes nothing in the file format, the manifest or the
verification rules. The top-level name `.panpak` is reserved for this purpose:
`pack` refuses a tree that already contains it. Readers show the texts with
`<package> --panpak-licenses` (a stub flag, removed before the application's
arguments) or `panpak licenses <package>`.

**License of this specification.** This document and the other documentation
of the project are licensed under the Creative Commons Attribution 4.0
International license (CC BY 4.0, text in `LICENSE-DOCS`). The source code is
licensed under MIT OR Apache-2.0.

**Patents.** CC BY 4.0 does not license patent rights. To make independent
implementations safe, each contributor to this specification states that they
will not assert any patent claim they own or control against an implementation
that conforms to this specification, to the extent the claim is necessarily
infringed by implementing the normative parts of it. This statement is not a
patent license for anything beyond conforming implementations, and it does not
cover use of the name "panpak" (see `TRADEMARKS.md`).
