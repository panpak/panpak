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

### 0.1 What a reviewer should look at first [INFORMATIVE]

Three things carry the trust of the whole system. If they are right, the rest (the sandbox, the store, the update flow) can fail without a package being accepted that its
author did not sign; if one is wrong, nothing else helps. Read these before anything else, in this order. For each: where it is written, where it is done, how it is tested
without the code that does it, and what to try to break.

1. **The signed message** (§3.0; `package_message` in `panpak-core/src/lib.rs`). What is signed is `"panpak/package/v1\0" || version || flags || root_hash || manifest`: the
   manifest (which holds the permissions, the stub hash and the list of critical fields) and the root of the index (which holds every file by hash). *Look for:* anything that
   is read before the signature is checked (the order in `verify`: layout, signature, then the fields); anything outside the message that changes what runs (the signature
   block is outside by construction: §3.0 says nothing may depend on it unless a signed datum requires it); two readings of the same bytes (the root comes before the manifest
   because it has a fixed length); the stub, which is outside the index (`[runtime].stub_hash`, checked at every start). *Test without the code:* Appendix A (a complete package
   as bytes, with the message spelled out), the fixtures of `conformance/` and `conformance/verify.py`, a second reader written in Python from this text only.
2. **The signature block** (§3.0, §2.1). A list of typed entries that fill the block exactly, with a critical bit. *Look for:* an entry that a reader may skip and should not
   (the rule: unknown and critical is a refusal), a block that is read differently by two readers (exact fill, one signature entry, at most 64 KiB), and what is reserved but
   not defined (types 2, 3 and 4: a reader of this generation implements none, and must say "update panpak" for a critical one). *Test:* the `invalid-sig-*` fixtures.
3. **The chain of keys, with recovery** (§3.2; `panpak-core/src/trust.rs`: `follow`, `succession_message`). The root signs the packages; the commitment to a recovery key is in
   every package from the first one; a succession (at most 8 steps) is signed by the key that was and bound to the app id; a succession by the root cannot change the
   commitment, and one by the recovery key wins over any by the root. *Look for:* a way for the holder of the root alone to change the commitment, or to make a step that
   another app, another step or the other kind of key can reuse (the labeled message of §3.0 binds the app, the kind and the keys); a chain that ends somewhere other than the
   root and commitment that the package names; a pin that moves before the version is complete; what a thief of the root can still do (move the pin until the author
   recovers: `THREAT-MODEL.md` §1.1 says it is not covered, on purpose); and the first trust (§3.2 rule 1 and §3.2.1: the domain confirmation is only a confirmation, never a
   substitute for the checks above). *Test:* `tests/trust_update.py` (the whole story over HTTP), `tests/domain_confirm.py` (the domain, with a real TLS server), the conformance
   fixtures `valid-trust-chain` and `invalid-trust-*`.

What is **not** covered, and is said so in `THREAT-MODEL.md` §1.1 and §5: a malicious or coerced author, the author's server at the first install, look-alike domains, a freeze
with one source, the window between a theft and its discovery, mother and recovery keys lost together, the local store (KL-18), human review of apps, and any third-party
review before this one.

## 1. File layout [NORMATIVE]

```
+---------------------------+  offset 0
| static executable stub    |  ELF; the kernel runs it, ignores the rest
+---------------------------+
| chunk 0 | chunk 1 | ...   |  zstd, each independent
+---------------------------+  <- index_off
| index (zstd)              |
+---------------------------+  <- manifest_off
| manifest.toml             |
+---------------------------+
| signature block           |  (§3.0)
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
- `version` and `flags` are covered by the package signature (§3.0): a
  footer that says anything else than the signer saw does not verify.
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
at `manifest_off + manifest_len`, comes the **signature block**, which
ends where the footer starts.

### 3.0 The signed message and the signature block [NORMATIVE]

Every signed object of Panpak starts with an ASCII label that ends in a
NUL, so that a signature made for one kind of object can never be read as
another one (the same key signs packages and successions). The signature
of a package covers:

```
"panpak/package/v1\0" || u32 version || u32 flags || root_hash || manifest_toml
```

`version` and `flags` are the footer fields (little-endian, as in the
footer), `root_hash` the 32 raw footer bytes (not hex), `manifest_toml`
the exact stored bytes. The fixed-length root comes before the manifest,
so the message has one reading only.

The signature block is a list of entries that fill the block exactly:

```
u16 type | u16 flags | u32 length | data[length]       (little-endian)
```

- bit 0 of `flags` is **critical**; the other bits are not defined and a
  Reader MUST reject an entry that sets one.
- Type 1 is the package signature: exactly one entry per block, critical,
  `length` = 64 (Ed25519, `verify_strict`).
- A Reader MUST ignore an entry of a type it does not know when it is
  not critical, and MUST reject the package when it is critical (the
  message says to update panpak). Types 2 (transparency proof), 3 (a
  signature with another algorithm) and 4 (countersignature of a third
  party) are reserved; no Reader of this generation implements them.
- The block is at least 72 bytes (one signature) and at most 64 KiB.
- The block is not covered by the package signature (it cannot be).
  Rule of design: nothing relevant to security may depend on the presence
  of an entry unless a signed datum says it is required.

A succession (a step of `[trust].chain`, §3.2) covers a message of its own:

```
"panpak/succession/v2\0" || u16 len(app id) || app id || u8 who || signing public key || new root public key || new recovery commitment
```

(the keys and the commitment are the 32 raw bytes; `who` is 0 when the root signs and 1 when the recovery key signs). The signature belongs to one app and
one step: the same bytes do not rotate another app, and a step of the root cannot be read as a step of the recovery key.

Mandatory layout consistency checks (reject on mismatch):

```
index_off + index_len == manifest_off
72 <= file_size - 80 - (manifest_off + manifest_len) <= 65536
```

No holes or extra data between index, manifest, signature block and
footer: offsets MUST NOT point at arbitrary file regions.

### 3.1 Schema (`panpak.toml`) [NORMATIVE]

Fields marked "REQUIRED" MUST be present, otherwise the manifest is
invalid. Readers MUST ignore unknown keys and sections (read tolerance
for future extensions), with two exceptions: the `[format]` table is closed
(an unknown key in it refuses the package) and a field that `[format].critical`
names MUST be known (§3.1.9). It is `panpak pack` that rejects unknown keys on
*input*, so it never unknowingly produces packages different from the
intended ones.

```toml
[app]
id = "org.example.editor"      # REQUIRED, D-Bus style name (§3.1.1)
name = "Editor"                # REQUIRED
version = "1.4.2"              # REQUIRED
entry = "bin/editor"           # REQUIRED, relative path inside the tree

[format]                       # REQUIRED, written by `panpak pack` (§3.1.9)
min_panpak = 1
critical = ["permissions.network", "runtime.stub_hash"]
compression = "zstd"
chunking = "fastcdc:16384:65536:262144"
packed_by = "panpak 0.1.0 zstd 1.5.7"

[release]                      # REQUIRED, written by `panpak pack` (§3.1.10)
revision = 4
floor = 3
security = 2
channel = "stable"
history = "<64 hex>"           # BLAKE3 of .panpak/history, from the second release of a channel on

[abi]                          # generated by `panpak pack`
needs = ["libgtk-3.so.0"]
prefer_host = ["libGL*", "libvulkan*", "libEGL*"]
modules = "<64 hex>"           # BLAKE3 of .panpak/modules (§7.2)

[host]                         # what the package needs of the system (§3.1.11); `arch` and `glibc` are written by `panpak pack`
arch = "x86_64"
glibc = "2.28"
kernel = "5.15"                # the rest is given by the author: optional
landlock = 3
display = ["x11", "wayland"]   # one of them is enough
portals = ["document"]
bus = "private"                # "none" or "private" (the filtered session bus)

[sbom]                         # written by `panpak pack` (§3.1.12); not critical
hash = "<64 hex>"              # BLAKE3 of .panpak/sbom.cdx.json (§7.3)
coverage = "best-effort"       # "build" or "best-effort"

[permissions]                  # static, applied by Landlock
x11 = false                    # opt-in: X11 access, default false
audio = false                  # opt-in: the PipeWire and PulseAudio sockets of the session (playing and recording), default false
network = false                # master switch; false+ABI>=4 => TCP denied; true with no port below => the whole network, no filter
network_allow_tcp = [80, 443]  # outbound ports (connect); only with network=true; with any port the filter stays
network_bind_tcp = []          # listen ports (bind); only with network=true; with any port the filter stays
sockets_allow = ["$XDG_RUNTIME_DIR/bus"]  # path Unix sockets (requires ABI V9);
                               # leading $XDG_RUNTIME_DIR and $HOME expanded
notifications = false           # opt-in, §3.1.5: desktop notifications
tray = false                   # opt-in, §3.1.5: a tray icon
dbus_own = []                  # §3.1.5: names of the app on its private bus (no effect since step 13p; kept for old packages)
mpris = false                  # §3.1.5: a media player: one alias org.mpris.MediaPlayer2.panpak.<id> on the host bus
devices = ["gamepad"]          # §3.1.5: categories of devices; hear of devices that come and go: the uevent socket of netlink
exec_written = false           # §3.1.8: the app may run programs that it wrote itself (downloaded, unpacked, built)
gvfs = false                   # §3.1.5: the file services of the system (GVfs): network locations, shared folders, drives; weighs as the whole home

[[permissions.home_paths]]     # last resort, §3.1.2: a fixed path in the home folder
path = ".config/editor"        # relative to the home, no ".."
access = "rw"                  # "ro" or "rw"

[[permissions.xdg_dirs]]       # §3.1.4: a folder of user-dirs.dirs, found at install
name = "music"                 # music, documents, pictures, videos or download
access = "ro"                  # "ro" or "rw"

[desktop]                      # §3.1.4
schemes = ["myapp"]            # URL schemes the app opens (x-scheme-handler/<scheme>)
mime_types = ["text/plain"]   # file types the app opens (MimeType of the menu entry)
wm_class = "Editor"            # §3.1.5: the window class the app sets (StartupWMClass)

[launch]                       # §3.1.2
args = ["--profile", "${XDG_CONFIG_HOME}/editor"]   # put before the user's arguments
[launch.env]
EDITOR_DIR = "${XDG_DATA_HOME}/editor"              # added to the environment

[permissions.portal]           # requested at use time
files = "picker"
camera = "ask"

[update]
url = "https://example.org/editor/index.panpak"   # optional

[trust]                            # written by `panpak pack` from the signing key (the author gives only `domain`); critical (§3.1.9)
root = ["ed25519:<64 lowercase hex>"]             # REQUIRED: the key that signs the package; a list, exactly one for now
threshold = 1                                     # REQUIRED: 1; a larger number is reserved
recovery = "blake3:<64 hex>"                      # REQUIRED: the commitment to the recovery key, from the first package on
domain = "example.org"                            # optional: the domain of the author; read backwards, the beginning of the app id (two labels at least)
[[trust.chain]]                                   # optional: the successions from an older root to `root[0]`, oldest first, at most 8
by = "root"                                       # "root" (signed by the root that was) or "recovery" (signed by the recovery key, revealed in `from`)
from = "ed25519:<...>"                            # the key that signs: the old root, or the public key of the recovery key
to = "ed25519:<...>"                              # the new root
recovery = "blake3:<...>"                         # the commitment after this step
sig = "<128 hex>"                                 # signature of `from` over the succession message (§3.0)

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
| `[abi]` `needs`, `prefer_host` | Each item: 1 to 255 bytes of printable ASCII (%x21-7E), without `/`, and not `.` or `..` |
| `origin.converted_from` | Like `app.name`, and without `/` |
| `origin.version` | Like `app.name`, at most 64 bytes |
| `origin.sha256` | 64 lowercase hexadecimal digits |
| `update.url` | At most 2048 bytes, no byte %x00-20 or %x7F |
| `permissions.sockets_allow` | Each item: 1 to 4096 bytes, no control character |
| `permissions.home_paths` | At most 32 items, see §3.1.2 |
| `permissions.xdg_dirs` | At most 5 items, each `name` once, see §3.1.4 |
| `desktop.schemes` | At most 8 items, see §3.1.4 |
| `desktop.mime_types` | At most 128 items, see §3.1.4 |
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

### 3.1.4 User folders and URL schemes [NORMATIVE]

**`permissions.xdg_dirs`.** A list of tables with `name` (`music`, `documents`, `pictures`,
`videos` or `download`) and `access` (`ro` or `rw`), each `name` at most once. It grants the
folder the user has chosen for that purpose, which the app cannot know (it is not in `HOME`
under a fixed name). It does not change `home_paths`.

*Finding the folder.* `panpak install` and `panpak update` MUST find the folder outside the
sandbox, in `$XDG_CONFIG_HOME/user-dirs.dirs` (`~/.config/user-dirs.dirs`). The parser is strict about
each line and tolerant about the file: it skips what it does not understand, one line at a time. The file is at most 16 KiB (a larger one is not
read). A blank line and a line that starts with `#` are ignored. A line `XDG_<KEY>_DIR="<value>"` with `<KEY>` one of `DESKTOP`, `DOWNLOAD`,
`TEMPLATES`, `PUBLICSHARE`, `DOCUMENTS`, `MUSIC`, `PICTURES`, `VIDEOS` names that folder (at most 600 bytes per line, no control character). A line with another
key (`PROJECTS`, written by xdg-user-dirs 0.19, or one that a later version adds) is ignored and its folder is never granted; a line that is not understood, or a known key
with a bad value, gives that one folder no value (`install` and `update` say why) and does not touch the others; a key written twice is dropped
(which of the two is meant is not guessed). `<value>` is
`$HOME`, `$HOME/<path>`, or the absolute path of the home folder followed by `/<path>`; it
contains no quote, backslash, backquote or other `$`. A trailing `/` is dropped. If the file
does not exist, the folder is `Music`, `Documents`, `Pictures`, `Videos` or `Downloads` in
the home folder. If the file exists and does not name the folder, or names the home folder
itself, the folder is not granted. The path found is relative to the home folder and MUST
pass the rules of `home_paths` (§3.1.2: not broad, not protected, no `..`, printable ASCII) and be at
most 400 bytes; no part of it may be a symbolic link and every existing part must be a folder.
A folder that is refused is not granted and `install` and `update` MUST print a warning that
says why.

*Pin.* `install` and `update` MUST print each folder found (`music: ~/Music (rw)`) before
asking for confirmation, and store `name=path` lines next to the pinned key (`apps/<id>/xdg-dirs`).
At every start the runtime MUST find the folder again in the same way and grant it only if the
result equals the pin and no part is a symbolic link; otherwise it MUST NOT grant it and MUST print a
warning that says to reinstall. An app that is run without being installed gets no user folder.
An `rw` folder is granted for reading and writing, an `ro` folder for reading; the folder is
not created.

*Folders seen by the app.* When the runtime writes `$XDG_CONFIG_HOME/user-dirs.dirs` for the app
(whenever the manifest has `xdg_dirs`) the file has a first line with `X-Panpak-Generated`, the granted folders at
their real path, and every other key at `$HOME/`, which GLib, Qt and `xdg-user-dir` read as "not
available". A file without that line is the user's and is left alone. In the isolated view
(§9.2) the home folder is the private home of the app: it holds the app's own files, the empty mount points of what is
mounted into it, and the granted folders; without the view the listing right is never given.

**`desktop.schemes`.** URL schemes the app registers. Each is 1 to 32 bytes, starts with a
lowercase letter and has only lowercase letters, digits, `+`, `.` and `-`. These are refused:
`http`, `https`, `file`, `ftp`, `ftps`, `sftp`, `ssh`, `mailto`, `javascript`, `data`,
`about`, `blob`, `ws`, `wss`, `chrome`, `view-source`, `vbscript`, `ms-msdt`,
`search-ms`, `x-scheme-handler`. `install` MUST show the schemes, and writes them into the
desktop entry as `MimeType=x-scheme-handler/<scheme>;` with `Exec=` the launcher and `%U` (the
address arrives as an argument of the launcher). It then runs `update-desktop-database` on the
folder of the entry, when the tool exists at `/usr/bin` or `/bin`; a failure is only a note.

**`desktop.mime_types`.** The file types the app opens, written into the same `MimeType=` line after the
schemes, so that the desktop offers the app in "Open with" for those files. At most 128 items, each
`type/subtype` of lowercase letters, digits, `+`, `.`, `_` and `-` (at most 100 bytes), each at most once.
These are refused: `x-scheme-handler/*` (use `desktop.schemes`), `inode/*`, `application/x-panpak`,
`application/x-desktop`, `application/x-executable`, `application/x-sharedlib`,
`application/x-pie-executable` and `application/x-shellscript`. The field only makes the app a candidate:
it does not give the app access to any file (the file dialog and `panpak open` do). The package does not choose the
default app, but a desktop that has no default for a type for the user may take the first entry it finds, and an entry in
the user's own folder comes first (seen on GNOME: `text/plain` went to the panpak Text Editor): the user changes it in the
settings of the desktop. `convert` and `build` copy the types of the original
`.desktop` file (the types that are refused are listed in a note, and the schemes are not copied).

### 3.1.5 Bus permissions and the window class [NORMATIVE]

These fields are used only when the filtered session bus (§9.5) is active.

**`permissions.notifications`** (boolean). Lets the app show desktop notifications: the notification
portal (`org.freedesktop.portal.Notification`) and the methods `Notify`, `CloseNotification`,
`GetCapabilities` and `GetServerInformation` of `org.freedesktop.Notifications`. The signals of that
older service (`ActionInvoked`, `NotificationClosed`) are broadcasts that every client on the bus
can see; the runtime MUST NOT forward them (measured on GNOME Shell: `NotificationClosed` is sent with no destination). Actions
are delivered only by the portal.

**`permissions.tray`** (boolean). Lets the app put an icon in the notification area: it may talk to
`org.kde.StatusNotifierWatcher`. The runtime MUST NOT let the app own any `StatusNotifierItem` name for this
permission, and MUST NOT use a wildcard. An app that cannot own such a name registers the icon with the unique name of
its connection (measured with Chromium and Qt 6), which the host can read and activate through the filter.

**`permissions.mpris`** (boolean). The app is a media player: the runtime owns the one name
`org.mpris.MediaPlayer2.panpak.<id>` (the id with each `.` and `-` written `_`) on the host bus for it, and the media keys and the shell's media
widget reach the player through it (§9.5). The app never owns a name on the host bus.

**`permissions.gvfs`** (boolean, default false; a critical field). The app may use the file services of the system (GVfs): network locations (`sftp://`, `smb://`, `https://` through GIO), shared folders and
connected drives. The runtime lets the app's calls to the names `org.gtk.vfs` and `org.gtk.vfs.*` through to the session bus of the host, as `--talk-name=org.gtk.vfs.*` does for Flatpak (calls, replies and
signals; the app owns none of those names), and opens the folder of the sockets that the daemon makes for the mounts (`$XDG_RUNTIME_DIR/gvfsd`) for reading and writing, after making it (mode 0700, owned by the user) when it is not there yet: the daemon starts on request, and an app that is the first to use GVfs in the session would otherwise find no folder to open, and the mounts with sockets of their own (`sftp://`, `smb://`) would fail (the daemon accepts a folder that exists; a link, a file or a folder of another user in that place is not opened); the FUSE tree `$XDG_RUNTIME_DIR/gvfs` (the mounts of the session as paths of files, made by `gvfsd-fuse`) is opened too, for reading and writing, as Flatpak does with `--filesystem=xdg-run/gvfs`: it is put in the isolated view (§9.2) as a bind mount and gets a Landlock rule. The runtime MUST NOT make it. It MUST open it only when it is the mount point of a FUSE file system that the user mounted, read from `/proc/self/mountinfo` (the fields `user_id=` and the type `fuse*`; the last line for the path counts), and MUST NOT `stat` or resolve (`canonicalize`) it in the process that starts the app (an open with `O_PATH`, which the rule of Landlock needs, does not wait and is allowed; measured in `docs/COMPATIBILITY.md`), because the stat of the root of a stuck FUSE mount waits for its daemon and cannot be ended (this includes the folders that are resolved to share the files given on the command line: the root of a FUSE mount is not among them). It MUST find out that the mount answers with a `stat` made by a child process that it does not wait for longer than half a second; a mount that does not answer (a stuck daemon, or a dead one whose mount was left behind and says "Transport endpoint is not connected") is left out, the app starts without it, and the runtime says so on standard error; a link, a file, a plain folder or a mount of another user in that place is not opened. The tree is opened at the start of the app: **an app that starts when `gvfsd-fuse` has not mounted it has no FUSE paths until its next start**. A daemon that stops answering **between** that check and the `stat` that bubblewrap makes for the bind (a window of a few milliseconds) can still delay the start of the app (the same limit as Flatpak's `xdg-run/gvfs`; `docs/COMPATIBILITY.md`). Without the permission those names, the folder of the sockets and the FUSE tree stay closed (§9.5). It weighs as the whole home folder: through GVfs the app can read and change any file that the user can, also
outside its sandbox (`file://`), so `install` and `update` MUST show it as a high risk, with the sentence "Can access network locations, shared folders and connected drives through your system's file services. This lets it
read and modify any of your files.", MUST count it as a new permission when an update adds it, and MUST NOT tell the person that the app cannot see their personal files. A system without GVfs gives the app nothing
more and nothing less than before; `panpak doctor` says whether GVfs is installed.

**`permissions.devices`** (list of one to four different strings; absent means none). The categories of devices that the app is for. The boolean of the test versions is gone: a
Reader MUST refuse a package whose `devices` is not a list, an empty list and a repeat (a malformed field), and MUST refuse with "update panpak" a name that it does not
implement, which includes the names `input` and `usb`, which are reserved for categories that will give access to device files and do nothing now. The categories defined are
`gamepad` and `camera`. In this version each of them gives exactly one thing: it lets the app open the uevent socket of netlink (`socket(AF_NETLINK, ..., NETLINK_KOBJECT_UEVENT)`),
the way libudev, SDL and Chromium hear of a game pad, a camera or another device that is plugged in or removed (§9.8). Without any category the call returns `EPROTONOSUPPORT` and
the app starts all the same, without hot-plug notices. The notices carry the names and the numbers of every device of the machine that comes or goes (disks and network
cards too), whatever the category, so the permission is shown at the level Attention, with the categories named in the sentence. **No category gives access to a device file**: the view and Landlock
stay as they are. A category is implemented, and its access to the files of its type added, only when an app of the harness uses it for real; the camera of a window of a desktop
goes through the portal (`portal.camera`), not through this field.

**`permissions.dbus_own`** (list of strings, at most 8, each at most once). Before step 13p: names the app may own
on the session bus besides its own `app.id`. Since 13p every name the app takes is on its private bus (§9.5) and none is on the host bus, so the
field has no effect; it is still validated, but it is not a permission any more: `install` and `update` do not show it and do not ask about it. Each is a D-Bus well-known name of at least three
elements (`[A-Za-z_][A-Za-z0-9_]*` separated by `.`, at most 255 bytes), with no `*`. A name is refused
by `pack`, `install`, `update` and the verifier when it is, or is below, one of these: `org.freedesktop`,
`org.gtk`, `org.gnome.Shell`, `org.gnome.SessionManager`, `org.gnome.Mutter`, `org.gnome.SettingsDaemon`,
`org.gnome.keyring`, `org.gnome.evolution`, `org.gnome.OnlineAccounts`, `org.gnome.ScreenSaver`, `org.a11y`,
`ca.desrt.dconf`, `com.canonical`, `com.ubuntu`, `org.ayatana`, `org.ibus`, `org.fcitx`, `org.mpris`,
`org.xfce`, `org.mate`, `org.cinnamon`, `org.pantheon`, `org.lxqt`, `org.bluez`, `net.hadess`, `org.pulseaudio`,
`org.PulseAudio1`, `org.pipewire`, `fi.w1`, `com.system76`, `com.redhat`, `org.fedoraproject`, `org.opensuse`,
`org.cups`. Names that start with `org.kde.` are accepted only as `org.kde.<word>` with `<word>` in lower
case letters and digits that equals the last element of `app.id`, `app.name` or the file name of `app.entry`, with
letters and digits only and lower case (KDE applications register that name for single instance). The same list applies to the
app's own id: an id below a listed family is not owned on the bus. `install` and `update` MUST show each name and
count a name that the installed version did not have as a new permission.

**`desktop.wm_class`** (string, 1 to 100 bytes of `A-Z a-z 0-9 . _ + -` and inner spaces, none at either end). The class the app sets on its windows
(Wayland `app_id` or X11 `WM_CLASS`). `install` and `update` write it as `StartupWMClass=` in the desktop entry,
which is how the desktop groups the windows under the entry. Without it the entry has no `StartupWMClass`.

**`desktop.icons`** (array of at most 6 strings, each a relative path to a `.png` file, no duplicates). The icons of the app in the
menu. In the `panpak.toml` that `pack` reads they are the *sources*: regular files of the tree (same path rules as `app.entry`; a path that goes
through a link is not a regular file of the tree and is refused). `pack` reads them, cleans them (below) and adds the result to the package as
generated entries `.panpak/icons/<side>.png`, where `<side>` is the side of the image; of two sources with the same side the first one stays, and
at most 6 sizes are kept, the largest first. The manifest that `pack` signs lists those generated paths, not the sources, so in an installed
package `desktop.icons` always points below `.panpak/icons/`. `pack` also takes icons from its caller in memory (`panpak convert` does this) and
never writes them to the tree. `install` and `update` check each file again before anything is written, and refuse the package if one fails.
A file in a package MUST be a PNG of at most 256 KiB whose chunks are all in this list: `IHDR` (first, 13 bytes), `PLTE`,
`tRNS` (each at most once, before the image data), `IDAT` (together) and `IEND` (last, nothing after it); every chunk
checksum MUST be right. Real PNG files carry other chunks (text, colour profile, gamma, resolution, time, background, animation),
so `pack` and `convert` clean the icons first with one deterministic function: the signature, then `IHDR`, `PLTE`, `tRNS`, every
`IDAT` and `IEND` copied byte for byte in the order of the file, and no other chunk; the image data and the pixels are not
touched, and `tRNS` is kept so that the transparency stays. Each chunk of the input, also a dropped one, MUST have a right
checksum. The tool prints which chunks it removed from which icon. The file in the tree is not changed by `pack`; the package holds
the cleaned file. A reader (`install`) does not clean: it accepts only a file that is already in this form. The image is square, 16 to 1024 pixels on a side, 8 bits per channel, colour type 0, 2, 3, 4 or 6, not interlaced,
and the inflated image data is exactly `height * (1 + width * channels)` bytes, each row starting with a filter type from 0 to 4,
with nothing after the end of the zlib stream. SVG and every other format are refused. The file is installed as it is (never
re-encoded) as `~/.local/share/icons/hicolor/<S>x<S>/apps/panpak.<app.id>.png`, where `<S>` is the side taken from the image
header; of two files with the same side the first one is used. `install` and `update` remove the sizes of the app that the new
version no longer has; `remove` deletes exactly these files. A destination that is a link or not a regular file stops the command
with `PANPAK-E2001`. The entry says `Icon=panpak.<app.id>` when at least one icon is installed and `Icon=application-x-executable`
otherwise.

The desktop entry is `panpak.<app.id>.desktop` (not `<app.id>.desktop`): the portals know the app as `panpak.<app.id>` (§9.5)
and look for the entry of that name, and the prefix keeps the entry apart from the Flatpak or distribution entry of an
app that has the same upstream id. `install` and `update` MUST remove `<app.id>.desktop` when panpak wrote it
(it has the `X-Panpak-App-Id` marker or the `Exec` line panpak writes) and MUST leave it when it did not.

*Content of the entry.* The entry is made from a fixed template, never from a `.desktop` file of the package. The package
contributes `Name` (`app.name`, escaped as a Desktop Entry string), `StartupWMClass` (`desktop.wm_class`) and the
`x-scheme-handler/<scheme>` items of `MimeType` (`desktop.schemes`) and the other items of `MimeType` (`desktop.mime_types`); everything else is fixed or derived from the store:
`Type`, `Exec` (the launcher, with `%U`), `Icon`, `Terminal`, `Categories`, `Actions` and the group `Uninstall`
(`Exec` is `<store>/bin/panpak remove --ask <app.id>`, §9.6), and the two keys `X-Panpak-Desktop-Version` and
`X-Panpak-App-Id`. No other key is ever written (`TryExec`, `Path`, `DBusActivatable`, `X-GNOME-Autostart-*`, `Hidden`,
`NoDisplay`, `OnlyShowIn`, extra actions or any other `MimeType`).

*Version of the entry.* `X-Panpak-Desktop-Version=<n>` is the second line of the entry; the launcher carries the same number in
`# X-Panpak-Desktop-Version: <n>`. A runtime that starts an installed app reads the first 128 bytes of its entry. If the version
is there and is not lower than its own, it does nothing. Otherwise (lower, or not inside the 128 bytes) it reads the whole file,
and only if the file is a regular file that panpak wrote for this app (same marker as above) it rewrites the launcher and then the
entry, each through a temporary file, `fsync` and `rename`. It never creates a missing entry, never touches a link or a file that
is not its own, and a failure never stops the start.

### 3.1.6 Environment of the tree [NORMATIVE]

**`[env]`** (table of strings, at most 16). Variables that point the app at files of its own tree. Without it a program that needs `GSETTINGS_SCHEMA_DIR`
or `QT_PLUGIN_PATH` needs a shell script as its entry, which works the path out from where it is: code that nobody reviews. Here a value is data.

- A name MUST be one of the table below; any other name is refused (`PANPAK-E0007`), and in particular `LD_*`, `PANPAK_*`, `PATH`, `HOME`,
  `XDG_RUNTIME_DIR` and the private folders of §9.1, which change how code is loaded or what the sandbox protects, or which the runtime sets itself.
- A value is one of three kinds. *Path*: `${PANPAK_ROOT}/` and a relative path. *Path list*: up to 8 paths of that kind separated by `:`.
  *Word*: one of the words listed for the name. `${PANPAK_ROOT}` is the folder of the package tree. The relative path MUST NOT be empty, MUST NOT have an
  empty, `.` or `..` segment, a control character, `:` or `$`, and is at most 512 bytes. An absolute path, any other variable and a value that leaves the tree are refused.
- `pack` also refuses a path that is not in the tree. `install`, `update` and the runtime check the table again.
- The runtime sets each variable in the environment of the app, with `${PANPAK_ROOT}` replaced by the tree. A path list for `XDG_DATA_DIRS` goes in front of what
  the runtime would have set; the other variables replace it.

| Name | Kind | Why it is allowed |
|---|---|---|
| `XDG_DATA_DIRS` | path list | icon themes, MIME data, schemas and data folders of the toolkits and the app |
| `GSETTINGS_SCHEMA_DIR` | path list | the compiled GSettings schemas |
| `GDK_PIXBUF_MODULE_FILE` | path | the cache of the image loaders (the modules are named by file name in it and found through the library path; list them in `abi.needs`) |
| `GDK_PIXBUF_MODULEDIR` | path | the folder of the image loaders |
| `GIO_EXTRA_MODULES` | path list | GIO modules the app ships |
| `GTK_PATH`, `GTK_IM_MODULE_FILE` | path list, path | GTK modules and input methods the app ships |
| `QT_PLUGIN_PATH`, `QML_IMPORT_PATH`, `QML2_IMPORT_PATH` | path list | Qt plugins and QML modules |
| `GST_PLUGIN_PATH` | path list | GStreamer plugins the app ships |
| `TERMINFO_DIRS` | path list | the terminfo entry of a terminal emulator |
| `XCURSOR_PATH` | path list | cursor themes the app ships |
| `GSETTINGS_BACKEND` | `keyfile`, `memory` | settings in a file of the private config folder, because dconf is not reachable from the sandbox |
| `GDK_BACKEND` | `wayland`, `x11`, `wayland,x11`, `x11,wayland` | the display protocol of GTK |
| `QT_QPA_PLATFORM` | `wayland`, `xcb`, `wayland;xcb`, `xcb;wayland` | the display protocol of Qt |
| `GTK_A11Y` | `none` | no accessibility bus (the filtered bus does not carry it) |
| `ELECTRON_OZONE_PLATFORM_HINT` | `auto`, `wayland`, `x11` | the display protocol of Electron |

Every path stays inside the tree, so what is loaded is what the signed package contains and nothing else. A package with an `[env]` table that is run by an older runtime
is run without it (readers ignore what they do not know): a package that cannot work without it SHOULD carry `[runtime] min_runtime` of a runtime that reads it (see the open point in docs/SOURCE-BUILD.md).

### 3.1.7 Provenance of a build from source [NORMATIVE]

**`[provenance]`** is written by `panpak build` and refused by `pack` in an input file written by hand. It says how the package was made, and `install` shows it:
`source` (`git <url> @ <commit>` or `url <url> sha256 <hex>`), `image` (the name of the build image, the digest of its manifest and the date of the package snapshot) and `image_deps`: the
dependencies that were taken from the build image instead of being built from the source that the author pinned (`name`, the image `package`, and the `version` found), and `deps`: the libraries that the recipe built from a pinned archive (`[[build.deps]]`), each as `<name> url <url> sha256 <hex>`. A reader MUST ignore a
`[provenance]` it cannot use. It carries no measurement that changes from one build to the next (the peak memory of a build is not in it): the same inputs give the same bytes.

### 3.1.8 Programs the app writes [NORMATIVE]

**`permissions.exec_written`** (boolean, default false). By default the runtime grants no right to execute on anything the app can write: its private home (§9.1), its cache, `/tmp`, `/var/tmp`, its `/dev/shm`, the folders granted by `xdg_dirs` and `home_paths`, and the parent folder of a file opened with write and rename. A program that the app wrote there (a downloaded updater, a plugin binary, a unpacked helper, a build result) is refused by `execve` with `EACCES`, whatever its mode bits say. The tree of the app, the system folders and the read-only places keep their right to execute. With `exec_written = true` the rights of those folders include execute. Install shows it, at the level Attention, as "Run programs that it downloads".

Limits, stated here so that nobody reads more in the rule than it gives:

- It is a rule of Landlock, which has no right for `mmap(PROT_EXEC)`: an app can still map a file it wrote as executable code (a plug-in loaded with `dlopen`, a JIT cache on disk), and it can run code from memory. The rule stops a program being started from a written file; it does not stop code from being loaded from one. The seccomp filter does not police `mmap` either.
- An interpreter is not a program written by the app: `sh script`, `python file.py` and `node app.js` read the written file as data and run. The rule is about the file that `execve` opens.
- Landlock tells nobody when it refuses an `execve`. When the app ends with an error after it wrote a program file in its private folders, the outer stub writes one line in the log that names the file and the permission (a hint, not a proof; the outer stub exists only when the app runs in the isolated view). `panpak trace` (step 13t) is to name the refusal itself.
- Without the isolated view (no bubblewrap) the rule is the same: it is a Landlock rule. What changes is which folders the app can write: there is no private `/tmp`, so `/tmp` of the machine is writable and is covered by the same rule, and the hint of the outer stub is not given.

### 3.1.9 The `[format]` table [NORMATIVE]

`[format]` says what a reader must understand to read the package. It is REQUIRED, written by `panpak pack` (an author does not write
it; `pack` refuses an input that has one), and closed: a key not listed here refuses the package, and a key added later comes with a
higher `min_panpak`. A package without it was made before the table existed (a test version of panpak); a Reader refuses it with
`PANPAK-E0011` ("make it again") before it checks the signature, so that it is not told that its signature is wrong.

| key | type | meaning |
|---|---|---|
| `min_panpak` | integer ≥ 1 | the level of the manifest that a reader needs. This format is level 1. It goes up only when the **meaning** of something that exists changes; a new field that changes what is safe does not need it, it is named in `critical`. A Reader with a lower level refuses (`PANPAK-E0002`, "update panpak"). It is a separate number from `[runtime].min_runtime`, which is about the behaviours the runner must have |
| `critical` | list of strings | the fields of the manifest that change what is safe and that the package uses, as `section.key` (`permissions.network`, `launch.env`) or `env`. Every key used in a critical section is listed, `runtime.stub_hash` among them. The list is **derived**: whoever signs a manifest makes it again from the manifest and the table of sections below (`pack`, `convert`, `build`, and every tool that edits a manifest and signs it again), and nobody writes it by hand; a list that was there before is replaced. A Reader that does not know one of the paths refuses (`PANPAK-E0002`, "update panpak"): it would otherwise run the app with less confinement than the author declared. A Reader also checks that the list is **complete** for the fields it knows: a manifest that uses a field of a critical section that the list does not name is malformed and is refused (`PANPAK-E0012`), so a restricting field added after `pack` and signed with the old list cannot hide from an older Reader. At most 256 entries of at most 64 characters of `a-z 0-9 _ .` |
| `compression` | `"zstd"` | how the chunks and the index are compressed. Critical by definition: a Reader that does not know the value refuses |
| `chunking` | text | how the files were cut (`fastcdc:<min>:<avg>:<max>`). Informative and signed. A Reader accepts any cut within the caps; the value is for the tools of the author (to cut the next version alike so that the delta stays small, to make the same bytes again) |
| `packed_by` | text | the tool that made the package. Informative |

The critical sections are `abi`, `release`, `trust`, `permissions`, `launch`, `runtime` and `env`; `app`, `format`, `desktop`, `origin`, `update` and
`provenance` are not. A new section MUST be classified before a Reader can read it. Both the client (`install`, `update`, `doctor`) and
the runtime that starts the app (`panpak-stub`) apply this check, so an old runtime used by a launcher does not skip a restricting field that
only a newer client understands.

### 3.1.10 The `[release]` table and the history [NORMATIVE]

`[release]` says where the package stands in the sequence of releases of its app. It is REQUIRED (a package without it is refused with
`PANPAK-E0011`, like one without `[format]`), closed (an unknown key refuses the package), and a critical section (§3.1.9). The author gives
the numbers in `panpak.toml` (all optional there; `history` may not be given) and `panpak pack` writes the table.

| key | type | meaning |
|---|---|---|
| `revision` | integer ≥ 1 | the number of the release in its channel. It grows with every release and it is **the only thing a client compares** to tell old from new: the version text is for people, a date depends on the clock, and a version has no order across apps. `pack --previous <package>` (and `convert` and `build`, from the installed version when it is of the same app and key) gives the revision after the earlier package; without an earlier package it is the number the input says, or 1 |
| `floor` | integer, 0 to `revision` | a revision below which nothing enters: not an install, not an update, not a rollback. It only grows from one release to the next (`pack` keeps the highest of the input and the earlier package), and a client keeps the highest floor it has seen for the app (`apps/<id>/floor`) |
| `security` | integer, 0 to `revision` | the number of the latest revision that fixed a vulnerability. A version whose `security` is above the installed revision fixes something that the installed one still has: a number and not a yes or no, so that a user who skipped a version does not lose it. It never skips the question about new permissions |
| `channel` | `a-z 0-9 -`, 1 to 32 | the line of releases (`stable`, `beta`). Fixed at install: a package of another channel is refused over an installed app unless the person says so (`--switch-channel`), and then it may have a lower revision and no common history with the installed one: the floor is the only check that stays. Revisions are per channel |
| `history` | 64 lowercase hex | the BLAKE3 hash of the file `.panpak/history` of the tree, when the package has one |

**What a client does when a package enters over an installed version** (`install` of a file, `update`, `apply`; the staging and the apply
check again, because the floor may have risen):

1. A channel that differs is refused (`PANPAK-E4005`).
2. A revision below the highest floor seen for the app is refused (`PANPAK-E4003`), and no option allows it.
3. A revision below the installed one is a downgrade (`PANPAK-E4001`), allowed only by `--allow-downgrade`.
4. The same revision: if the signed message is the installed one, there is nothing to do; if it differs, two publications carry one number, which is a fork or a replay, and the package is refused (`PANPAK-E4004`).
5. A later revision enters, and its history is checked.
6. **A rollback** (`panpak rollback`) to a version whose revision is below the floor is refused (`PANPAK-E4003`).

**The history.** `.panpak/history` is a file of the tree (so the signed index covers it and `[release].history` names its hash), written by
`pack --previous`. It is text: the line `panpak-history 1`, then one line `<revision> <hash>` for each earlier release of the channel, in rising
revisions from 1, the hash being the BLAKE3 of the message that the signature of that release covers (§3.0), in lowercase hex, and a newline
at the end; at most 4096 lines and 1 MiB. `pack` builds it from the earlier package: its history, then the earlier release itself.
It is read only when a package enters, never when an app starts. The client requires that:

- the installed release is in the history of the new one, with the same hash;
- the history that the installed release had is the beginning of the new one's;
- the revisions in the history are all below the revision of the new package;
- a release whose installed predecessor had a history has one too (a history cannot be dropped); when neither has one nothing is claimed and the update passes.

Otherwise the package is not a continuation of the installed version (a fork or a forgery) and is refused (`PANPAK-E4006`); at an update the
downloaded record is taken away. Stable and beta are legitimate branches: each channel has its own history.

**Protected mode.** A version that is installed and whose revision is below the floor that a client has seen for the app (from a package that
entered, or was refused, or was staged) is not blocked: the person keeps the app and the data. The client MUST run it in protected mode:

- **no network but the loopback of the app itself.** Not only TCP (Landlock covers nothing else): UDP, QUIC, name lookups and every other protocol
  too. In the isolated view this is the network namespace that an app with no network permission already has (only the loopback of the view); the
  manifest is read, in memory at each start, as `network = false` with no TCP port, and the signed manifest is not changed. Without a view there is no
  namespace to take the network away with, so the client MUST make every IPv4 and IPv6 socket fail (a seccomp filter on `socket` and `socketpair`);
  the loopback of the app goes too, and that is the only case where it does;
- no program that the app wrote is run: the manifest is read as `exec_written = false`;
- **no address is opened** (`xdg-open`, `OpenURI`): an address in the browser of the person (`https://host/?data=...`) is a way for data to leave. The
  app is told that it failed, and **the person is told too, in the way that fits the app** (a notification for a program with windows, one line on the
  standard error for the other; at most once in 20 seconds, whatever the number of links): "<App> is in protected mode: links do not open until you
  update." The addresses of every kind are refused (the ones that are ever allowed are `http`, `https` and `mailto`; §9.3). Without an isolated view the
  app can run the `xdg-open` of the system itself, so the addresses are not stopped in that case;
- the person is told at every start, in the way that fits how the app was started, and never has to type a command to answer:
  - a program with windows (its package has a window class or icons, or asks for X11), started from the menu or from a terminal: a desktop
    notification with the text and, **only when an update that fixes the problem is ready**, one button "Update now"; the press counts only when the signal
    is sent by the owner of the name `org.freedesktop.Notifications` and carries the number of that notification (§6.10): the signals are broadcasts, and
    another client of the bus must not be able to start the window of the update; in a terminal the most that is
    written is one line that says what is happening (no instruction);
  - a program of the terminal started with an interactive terminal (standard input and error are terminals): the text, and the questions are asked
    there with one key: the update when one is ready (with the new permissions it asks for, if any), then the internet; one key each, `Enter` is the
    default;
  - a start with nobody to ask (a script, a pipe): no question, one line on the standard error, and the app goes on in protected mode;
- "Update now" opens the window of the system (`zenity` or `kdialog`, the one of the menu action "Uninstall") with the new permissions, when the update asks
  for any; "No" changes nothing; "Yes" updates. Without a window tool the question is a notification with two buttons (§6.10); with no notification service and no portal either, nothing is accepted and the person is told;
- the menu of the app has, while the mode lasts, the action "Turn the internet back on…" (and "Turn the internet off again…" when it is on), with a
  window that explains the risk;
- the state is shown in `panpak list`, `panpak doctor` and as JSON by `panpak protected --json [<app-id>]` (the state, whether an update is ready and the
  permissions it asks for, the actions the person has, and the texts in the language of the person); the actions are `panpak protected-action <update |
  internet-on | internet-off>`. The commands `panpak update` and `panpak allow-internet` stay for scripts and are not what a message tells a person to
  type. Another front end (the Panpak app) takes the place of the notification, the windows and the questions without changing any rule;
- turning the internet back on is the person's choice, kept in the state of the client, for that app only, taking effect at the next start (at once
  when it is made from the question of a terminal); it gives back the network the app asked for and the addresses, never the programs the app wrote; it
  can be cancelled; it ends with the mode;
- when a version at or above the floor is put in place, the mode ends: the choice is cleared and the person is told. What enters below the floor
  stays refused (rule 2 above). An update that asks for permissions that the person accepted while a copy of the app was running is applied at the
  next start without asking again.

A catalog or a distribution MAY in the future put an app in protected mode (never block it) with a required `reason` field (one sentence and a
link); only the name is reserved here.

### 3.1.11 The `[host]` table [NORMATIVE]

`[host]` says what the package needs of the system. It is a critical section (§3.1.9): its keys are in `[format].critical`, and a Reader that does not know one
refuses the package. Every key is optional. `panpak pack` writes `arch` and `glibc`; the author gives the rest in `panpak.toml`, and `pack` refuses an `arch` that the
author gives and checks the forms.

| Key | Value | Written by | Met when |
|---|---|---|---|
| `arch` | `x86_64` or `aarch64`: the machine that packed (the runtime in the package is built for it) | `pack` | the machine has this architecture |
| `glibc` | a version of two to four numbers of at most four digits (`2.39`): the highest symbol version of the files, which an author may raise (§7) | `pack` | the glibc of the system is this or newer; a system whose glibc cannot be read (musl, no `ldd`) is not refused |
| `kernel` | a version like `glibc` (`5.15`) | the author | the Linux kernel is this or newer (the digits of `uname -r` before the first other character) |
| `landlock` | a number from 1 to 9 | the author | the Landlock ABI of the kernel is this or higher |
| `display` | a list of one to eight different names of `x11` and `wayland`; one is enough | the author | (at the start) the session has one of them (`DISPLAY` is set for `x11`; `WAYLAND_DISPLAY` leads to a socket for `wayland`) |
| `portals` | a list of one to eight different names; only `document` is defined | the author | (at the start) the session has the portal (for `document`, `$XDG_RUNTIME_DIR/doc/by-app`) |
| `bus` | `none` or `private` | the author | (at the start, a note and not a refusal) `none` asks nothing; `private` asks for a session bus and `xdg-dbus-proxy`; without them the app starts with no bridge to the session |

A Reader MUST refuse a value that is not of the form (a malformed field), and MUST refuse with "update panpak" an `arch`, a display, a portal or a bus that it does not
know: it cannot check it. An unknown key in the table is refused too. The name `host_contract` is reserved for a numbered promise by Panpak about what the view gives
(the layout, the environment, the portals relayed); it has no meaning yet and a Reader that finds it refuses the package like any unknown key.

The keys are of two kinds. `arch`, `glibc`, `kernel` and `landlock` are properties of the **machine**; `display`, `portals` and `bus` are properties of the **session** that
runs the command. An install over ssh, from a script or in the build of an image has no display, no portal and often no bus, and must work.

- `install` and `update` (before anything is downloaded or written, also for a version that is only staged) and `doctor <package>` compare the keys of the machine with the
  system and MUST refuse a package that the system cannot run, with every requirement that is not met, each with what is needed, what is there and what to do
  (`PANPAK-E2009`; `PANPAK-E2005` for the glibc). They MUST NOT refuse for a key of the session. The option `--force-host` of `install` and `update` goes on; it says what is
  missing, and the client keeps the choice (`forced_host=1` in the state of the app, §3.2): `doctor` reports the requirements of the machine that are still not met, an
  update of the app does not need the option again, and a version that the system can run clears the choice.
- The start of an app (installed, or run from a file) checks `display`, `portals` and `landlock` in the session that starts it, and MUST refuse with the same three
  parts when one is not met. `bus = "private"` is not a refusal: where there is no session bus, or no `xdg-dbus-proxy`, the app starts without any bridge to the
  session, and the client MUST say, at the start and in `doctor`, what that costs (what goes through the bus: the portals and the notifications). A key of the session is not forced: `--force-host` is for the machine. An app that was installed with `--force-host` is not refused for
  `landlock`. A package that names none of these keys costs the start nothing.
- `doctor` says, as a note and not as a failure, what the session that runs it lacks of what an installed app or a package needs.

The messages for the person are in the catalog (English and Italian), like the other messages of the client; the names (`glibc`, `Linux`, `Landlock`, the display servers) are
not translated.

### 3.1.12 The `[sbom]` table [NORMATIVE]

`[sbom]` names the software bill of materials of the package (§7.3): `hash` is the BLAKE3 hash of `.panpak/sbom.cdx.json`, in 64 lowercase hex characters, and
`coverage` says how much of the package the document covers. `panpak pack` writes both; `pack` refuses a `[sbom]` in `panpak.toml` (`PANPAK-E0008`). The table is **not
critical**: a Reader that does not read SBOMs loses nothing that matters for safety, and no key of it is in `[format].critical`. An unknown key is ignored, like the keys of
the other tables that are not critical.

| Key | Value |
|---|---|
| `hash` | 64 lowercase hex characters |
| `coverage` | 1 to 16 characters, lowercase ASCII letters and `-`. Defined: `build`, `best-effort`. A Reader MUST accept a value that it does not know and show it as it is |

A Reader MUST refuse a `hash` or a `coverage` that is not of the form, a `[sbom]` that lacks one of the two, and a document whose BLAKE3 hash is not `hash`. A package
made before the table existed has none, and is not refused for that.

### 3.2 Keys, trust on first use, and successions [NORMATIVE]

Strict parsing of a key (`[trust].root`, a succession): exact prefix `ed25519:`
(lowercase), then exactly 64 characters of `0-9a-f` only — no
uppercase, spaces, or `0x`, so every key has a single representation.
At parse time use `VerifyingKey::from_bytes` and for the signature
`verify_strict`, which rejects malleable keys and signatures. A commitment is
`blake3:` and 64 lowercase hex characters: the BLAKE3 hash of the 32 bytes of a public key.

**Three keys.** The **root** (the "mother key") signs the packages; it is `[trust].root[0]`. The **recovery key** is made with it (`panpak key new`) and never
appears in a package: the package carries only its commitment, from the first release on. It is shown once as 24 words (the 256 bits of its secret and the 8 bits of check of BIP-39, with the English word list of BIP-39), to be kept on paper away from the
machine. A reader MUST refuse a package without `[trust]`, with more than one root, with a threshold other than 1, or with a step whose `by` it does not know
("update panpak": these are reserved), and MUST refuse a `domain` that does not fit the app id (the domain, read backwards, is the beginning of the id, with
at least two labels: `example.org` for `org.example.app`). `[update].key` is not part of a package: a package that has it is refused. The domain in a package is ASCII, in lowercase,
with every label that has other characters in its Punycode form (`xn--...`); a Reader refuses any other form. A Writer that takes the domain from a person MUST bring it to that form
first: Unicode normalization form C, lowercase, normalization form C again (not a compatibility form: a fullwidth letter stays a different character, and is shown as `xn--`),
then Punycode (RFC 3492) for each label with characters outside ASCII.

**Trust on first use.** The key lives in the self-signed manifest and alone proves nothing about the author's identity:

1. At the first installation the client MUST NOT go on unless someone has said whom to trust, and it asks the domain of the author first when the package names
   one (§3.2.1). That someone is the domain (it **confirms** the key), or the person: the key they were told (`--expect-key`; a package of another key is refused and
   nothing is pinned or made ready, whatever else was said), or that they trust the first key they see (`--trust-first-key`), or an answer at a terminal. A bare
   `--yes` is not that, unless the domain confirms the key. A domain that **contradicts** the key stops the installation whatever else was said, unless the person
   gave the explicit option for it (and still says whom they trust). The client MUST show the full key and its fingerprint (the first 16 hex characters) whenever the
   domain does not confirm it; when it does, the name of the domain is the main text and the key MAY be left to the details.
2. The client keeps the confirmed root and the recovery commitment in `apps/<id>/trust` (§6), with where updates come from (`source`, `author`: the
   `[update].url` the author named; other values are reserved for a catalog or a distribution) and the floor and the highest revision seen.
3. Updates MUST have the pinned root and the pinned commitment, or a chain that leads to them (rule 4). The client shows where updates come from
   (the host of `[update].url`, or that the app does not update itself) when it installs, and in `list` and `doctor`.
4. **The chain.** A package signed by a new root carries `[trust].chain`. The client takes the steps that start from its pin (the ones before are history and are
   ignored) and checks each: a step `by = "root"` starts from the root that was, is signed by it, and keeps the commitment (**a succession signed by the root
   cannot change the recovery commitment**); a step `by = "recovery"` reveals in `from` a key whose BLAKE3 hash is the commitment that was, is signed by it, and sets
   the next commitment. A recovery step is valid from any pin that has that commitment, so it wins over a succession of the root that a thief made. The chain MUST end at the root and the
   commitment that the package names. No delay: a valid succession is accepted at once, and the client says so in one line. The pin changes atomically with the
   verified update (the record holds two `root` lines for the moment between the two links, and a start accepts either).
   A new root (a succession) is asked of the domain of the author too, before any of the new version is downloaded (§3.2.1).
5. Without a valid chain a key change means that the person removes the app and installs it again.

### 3.2.1 Confirmation of the domain [NORMATIVE]

An author who names a `[trust].domain` (the domain, read backwards, is the beginning of the app id, §3.2) can make the first trust something other than a leap of faith by
publishing, at `https://<domain>/.well-known/panpak.toml`, the keys that may sign the apps of that domain. This is a confirmation and nothing more: it never makes a
package acceptable that would not be without it (the signature, the pin and the chain are checked as always), and it never reaches the start of an app.

```toml
[[app]]
id = "org.example.editor"       # an app id, or "org.example.*": every id that is "org.example." and at least one more label
root = ["ed25519:<64 hex>"]     # the keys that may sign it now: 1 to 8, different
replaces = ["ed25519:<64 hex>"] # the keys that no longer may: 0 to 16, different, none of them also a root
```

One file serves a whole domain. A Reader MUST read it as UTF-8 TOML of at most 65,536 bytes with at most 256 `[[app]]` entries, and MUST ignore a key or a table that it
does not know. It MUST refuse the whole file if an entry's `id` (or the prefix before `.*`) is not an app id, or is not under the domain that the file was asked of (a
domain speaks for its own apps only), if a key is not of the form of §3.2, if a list is out of the limits, or if a key repeats or is at once a root and replaced.

**The request.** HTTPS only, the certificate checked against the trust of the system (or the authorities of the settings), one address, at most 3 redirects and **only
inside the same host** (same scheme and authority: a redirect to another host, another port or plain HTTP ends the check as "not confirmed"), a body of at most 65,536
bytes, and 5 seconds for all of it. Nothing but the request line and the usual headers is sent: no identifier of the person, the app or the version, other than the
domain. The client MUST say what the server learns (that an address asks for the file of this domain, which says that someone is installing an app of it) before the first
request it ever makes, and MAY say it only once.

**The three outcomes.** Let *K* be `[trust].root[0]` of the package and *A* its app id. Among the entries that cover *A* (the id is the same, or the entry is `prefix.*`):

- **Confirmed**: *K* is a `root` of at least one of them, and is the `replaces` of none.
- **Contradicted**: there is such an entry, and *K* is a `root` of none of them, or *K* is in the `replaces` of one of them.
- **Not confirmed**: everything else. The package names no domain; the check is off (a setting) or was not asked for (`--offline`); the network, the certificate or the
  time failed; the status is not 200; the file is too big, not valid or redirects elsewhere; no entry covers *A*. The client MUST say why.

A Reader MUST NOT treat "not confirmed" as a refusal or as a contradiction: a domain that is down, or an author with no file, leaves the trust as it was before this
section. A contradiction is the only outcome with a consequence of its own:

- **First install.** *Confirmed*: the client MAY take `--yes` as enough (no `--trust-first-key`, no `--expect-key`); without `--yes` it asks. *Not confirmed*: as in §3.2
  rule 1. *Contradicted*: the client MUST refuse, with `PANPAK-E1006`, whatever else was said (`--yes`, `--trust-first-key` and `--expect-key` included), before
  anything is made ready or pinned; it MAY have one explicit option (`--despite-domain-mismatch`) that goes on to the question of rule 1, and says so.
- **Succession.** When the new package carries a root that the pin does not have (a chain that the pin signs, rule 4), the client asks about the new root, after the chain
  and the signature are checked and before anything of the new version is downloaded. *Contradicted* refuses the update with `PANPAK-E1006` (the version that is
  installed stays), unless the same option; *confirmed* and *not confirmed* are said in one line.
- Not at an ordinary update (the key is the pin), not at `apply`, not at the start.

What a confirmation is worth: it shows that, now, whoever controls the web server of the domain (and the certificate for it) lists this key for this app. It is no better than
that: a look-alike domain is another domain (the client shows the domain that the package names, on a line of its own as the main text, and a name with characters outside ASCII in its Punycode form `xn--...`, as a browser does, so that a look-alike in another script does not read as the name it imitates); a domain whose server is taken over can
confirm a key that the thief chose, which is the same loss as the author's; and an offline machine, or one with the check turned off, has nothing. What it adds is that a
first key supplied by a hostile channel (a mirror, a forum, an e-mail) is no longer enough on its own where the author published the domain file, and that a succession that
the pin signs is still stopped when the domain of the author says that it is not theirs. A domain can also stop an installation by contradicting (a denial by whoever controls it);
that is why the option exists.

The client keeps the check off and on in its settings (`trust.confirm_domain`) and for one command (`--offline`); in both cases the outcome is "not confirmed", and the
message says which. The names of the options and of the setting are a client interface matter. The messages for the person are in the catalog (English and Italian).

How the confirmation is requested (prompt text, accepted answers,
non-interactive mode) is a client interface matter, not part of the format;
see QUICKSTART.md for the `panpak` client.

### 3.3 Update protocol [NORMATIVE]

A client updating a package MUST:
- verify the remote manifest signature against the pin (NOT against
  the key in the remote manifest: an attacker could self-sign);
- tell old from new by `[release].revision` (§3.1.10), not by the version text: refuse a lower revision unless explicit `--allow-downgrade`, a
  revision below the floor in any case, another channel unless explicit `--switch-channel`, two different publications of one revision,
  and a package whose history does not continue the installed one. A package of another channel that the person chose with `--switch-channel` is the exception: the counters of two
  channels are separate, so it may carry a lower revision (no `--allow-downgrade`) or the same revision with other bytes, and the history of the old channel is not asked
  of it; only the floor still holds (the history of the new channel is the one the new package carries);
- fetch only the chunks that the installed version does not have (Range; for a file that has a patch (§4.1) whose starting file is installed, the patch instead of the
  chunks that only that file needs;
  the others are read from the files of its tree, and every blob that is
  reused is checked against the signed index, §6.3), make the new tree,
  verify as in §5, and only then move `current` (the previous version stays
  in `previous`; `panpak rollback` swaps them). Installed launchers use
  the runtime through `bin/panpak-stub` (§6) and the stub verifies its pin at every
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
                u8  flags       // bit 0 ELF file, bit 1 script that sets LD_LIBRARY_PATH itself,
                                // bit 2 the SONAME follows, bit 3 a patch follows; bits 4-7 are 0
                [u16 len, soname]   // only when bit 2 is set: 1 to 255 bytes of UTF-8, no control character, no "/"
                [[u8;32] base, u64 patch_off, u32 patch_len, [u8;32] patch_hash]
                                // only when bit 3 is set: a patch (§4.1)
                u16 ext_len, ext    // the extension area, at most 1024 bytes
  kind=symlink: u16 len, target
}
```

The flags, the SONAME, the patch and the extension area are what `panpak pack` read from or made of the file; they are signed with the index, so a reader that has the index does not
open the tree to know what a file is. A Reader MUST refuse an entry that sets a flag bit it does not know ("update panpak"), a SONAME with no ELF flag, and a
file that is both an ELF file and a script. Bit 0 is set for a little-endian ELF64 file (`ET_EXEC` or `ET_DYN`) whose first 64 bytes are readable; bit 1 for an
executable of at most 64 KiB, whose name has no `.so`, that starts with `#!` and has a line that assigns `LD_LIBRARY_PATH` (not a comment, not `unset`, not another
name that ends in it).

The extension area is a list of entries `u16 type, u16 length, data`, so that fields about a file can be added after the format is frozen (an fs-verity digest is one
that is studied) without a new format. The top bit of the type makes an entry critical: a Reader that does not know a critical entry MUST refuse the package ("update
panpak"); any other unknown entry is ignored. No entry is defined yet. An entry that is longer than the area, or a cut-short entry, makes the index malformed.

Chunk offsets are relative to the start of the file. Chunks are
produced with FastCDC (min 16K, avg 64K, max 256K) — the exact split
parameters are NOT part of the format: a Reader MUST accept any split
that respects the §2.1 caps. `chunk_id` indexes the chunk table:
identical chunks SHOULD be stored once (a Reader also accepts
duplicates, but still verifies them one by one). Overlapping chunk
ranges are structurally allowed, but every chunk MUST be verified
independently (§5 point 5).

**Caches that the writer makes.** `panpak pack` writes the file `icon-theme.cache` into every icon theme folder (`share/icons/<theme>/`) that has icons and none, in the format that
GTK 3 and Qt read (the bytes `gtk-update-icon-cache -f -t -i` writes: no image data, the index of the theme ignored). The file is data of the package like any other (it is in the index, chunked and hashed), and
the reader gives it the time of §6.9 so that GTK and Qt keep it. A Reader does not need to know of it.

**The compression level is not part of the identity of the content.** The hash of a chunk is the BLAKE3 of its raw bytes (§5 point 5), and the name of a file in the store
is made of those hashes (§6), so the same file compressed at another level has the same chunks and the same name in the store: an update between a package made at one level and a package
made at another fetches only the chunks that really differ, and a store never holds a file twice because of the level. Only the bytes of the package (the index, with the sizes and offsets of the compressed
chunks, and so the root hash and the signature) change with the level. A Writer MAY use any level (the reference `panpak pack` uses 19 since 2026-11-23, before it 3, and `panpak convert` uses 3; both take `--level`); a Reader MUST accept
any level. A Writer SHOULD make frames whose window is at most 8 MiB (the window of level 19), and a Reader MAY refuse a frame that asks for a window of more than 128 MiB.
Two publications of the same release at two levels are two different packages with two different root hashes, and a release is published once (the rule of the same revision, in the release checks of §3): the level is chosen when the release is made, never changed for a release that exists.

### 4.1 Patches [NORMATIVE]

A file of a new release MAY carry a *patch*: the file written as one zstd frame that uses the same file of the immediately previous release as its dictionary (a
reference prefix, as `zstd --patch-from`). It exists so that a client that has the previous release downloads the difference of a file whose chunks all changed (a program that was
built again, a database) and not its chunks. It is an optimisation and nothing depends on it: a client that does not use a patch downloads the chunks, as always.

*The entry.* Bit 3 of `flags` says that four fields follow the SONAME: `base` (BLAKE3 of the whole file of the previous release that the patch starts from), `patch_off` and
`patch_len` (where the patch is in the patches file, below), and `patch_hash` (BLAKE3 of the `patch_len` bytes of the patch, as they are in that file). They are signed with
the index. A Reader MUST refuse a `patch_len` of 0 or above 2^30 and an offset that with the length passes 2^64. The field belongs to the format; it is not an extension, and
a Reader that does not know bit 3 refuses the package like any other flag it does not know.

*The patches file.* A Writer that makes patches writes them one after the other, with nothing between them, in the file that has the name of the package and `.patches`
(`app-1.2.panpak` has `app-1.2.panpak.patches`), beside the package. A client asks for it at the address of the package and `.patches` (an address with a query or a fragment has none) with the
range requests of §3.4. The file has no index, no footer and no signature of its own: each patch is identified, and checked, by the hash in the signed index. A server that
does not have the file, or does not answer, costs the client nothing but the chunks.

*Making.* A Writer MAY make a patch only from the file at the same path in the immediately previous release, or, for a shared library that a new version renamed (`libQt6Core.so.6.12.0` and `libQt6Core.so.6.12.1`), from the one library of the same folder that has the same name without its version, in both releases (no guess between two); never from an older release, and only for a file of at least 64 KiB whose
previous file and new file together are at most 2^28 bytes (the window of the frame is 2^28 at most; the Writer sets the window log to what the two files need, with long-distance matching).
A Writer MUST NOT publish a patch that weighs as much as or more than the compressed chunks of the file that a client that has the previous release would download (the chunks the previous release does
not have). The patch is made when the package is made, so it is signed with it.

*Using.* A Reader that uses a patch MUST, in this order: (1) have the file of the previous release in the installed tree, at the same path or found by the name without version as above, and check that its BLAKE3 is `base` (a file found wrongly costs only the patch: the hash does not match and the chunks are downloaded); (2)
have enough free memory (twice the two files together, and a margin); (3) download the patch and check that its BLAKE3 is `patch_hash` **before** it gives a byte of it to the
decompressor; (4) decompress with the file of (1) as the dictionary, a window limit of 2^28 and an output limit of the size that the index gives for the file, and refuse a frame that
ends before or after that size or has bytes after it; (5) check the result against the chunks of the index like any file (§5 point 5). A Reader that finds any of this wrong MUST fall
back to the chunks of the file. So only bytes with a signed hash reach the decompressor, and a patch cannot make a file that the index does not describe.
## 5. Verification rules (mandatory) [NORMATIVE]

A Reader MUST run these checks, in this order, and MUST reject the
package at the first failure:

1. Read the footer, check magic and version.
2. Read the signature block (§3.0) and verify the manifest signature over the labeled message, then check `[format]` (§3.1.9), `[release]` (§3.1.10) and the field rules of §3.1.1.
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

*Times in the tree.* A package carries no times. The runtime gives every link and mark of a tree the modification time 2000-01-01T00:00:00Z (`946684800`). A file has the time of its content: `946684800` plus a number of seconds from 0 to 2^25 - 1 (a little more than a year) that is the first four bytes of the name of its blob (the BLAKE3 of the mode and the chunk hashes of the file, §6) as a number, modulo 2^25. A folder has the time of its list, made the same way from the BLAKE3 of `panpak-folder-v1` and, for each entry of the folder in the order of its name as bytes, the length and the bytes of the name and the length and the bytes of `f:` and the name of the blob of a file, `l:` and the target of a link, or `d:` for a folder. A cache file named `icon-theme.cache` has the latest time of its folder and of the folders below it. The same content has the same time in every tree and every version (a file that is shared between trees has one inode, so one time), another content has another time but for a chance of one in 33 million, and the same list has the same time. Three reasons. Qt ignores an icon cache that is older than its folder or than a folder below it, and GTK one older than its folder: the cache takes the latest of them (measured before: 49,000 failed lookups, 0.2 s at the start of qBittorrent, when the time at which each entry happened to be created made the cache older than the folders that sort after it). A program that keeps what it learned about a file or a folder by its time and compares it for equality asks again for what changed and for nothing else: GIMP keeps the time of each plug-in it has queried in `pluginrc`, and with one time for every file it would not see that 43 of the 132 plug-ins of 3.2.8 are not those of 3.2.6 (counted in the signed indexes of the two conversions; measured on the update of 2026-11-24, GIMP 3.2.6 to 3.2.8: 46 plug-ins are queried again at the first start, the 43 that changed and 3 that GIMP always queries, and 3 at every later start); fontconfig keeps the time of each font folder and the number of its entries in its cache; Python (`.pyc` with a time), QML (`.qmlc`), GStreamer (its registry) and the MIME database compare the time of a file or a folder for equality. A program that looks only for files newer than its cache (KDE's ksycoca, which keeps the latest time that it saw) cannot be served by times, since the time of a file is not ordered by version: for them the cache of the app is emptied (§6.9). A Reader of the store MAY give other times (nothing is signed over them); the guarantees above are those of the reference runtime.

Reference-implementation layout (paths under
`~/.local/share/panpak/`):

```
~/.local/share/panpak/
  chunks/<hash>          # (reserved: verified raw-chunk cache)
  blobs/<name>-<mode>      # whole verified files, shared via hardlink across trees; <name> is the BLAKE3 of the mode and the chunk hashes of the file
  trees/<root_hash>/     # materialized tree (hardlinks from identical blobs)
  farms/<root_hash>/     # ABI symlink farm: one link per SONAME (host or bundle, §7)
  runtimes/<abi>/panpak-stub  # installed runtimes (`panpak install-runtime`)
  bin/panpak-stub -> ../runtimes/<abi>/panpak-stub  # the best installed ABI; launchers use this path
  bin/panpak              # the CLI, for the menu action "Uninstall"; written by `install-runtime`
  packages/<id>@<ver>@<root>.rec  # the record of an installed version (§6.3); `@` is in no app id, the version is escaped (`%XX`), the whole root is in the name; the store keeps no copy of the package
  abi-cache/             # the cached resolutions (§7)
  staging/<id>/<root>/   # the chunks of an update that is being downloaded, one file per chunk
  apps/<id>/current -> ../../packages/<record>
  apps/<id>/previous -> ...   # a record again, for rollback
  apps/<id>/pending -> ...    # a staged update (§6.6), with apps/<id>/pending-state
  apps/<id>/update-state      # the background job's schedule and findings (§6.7)
  apps/<id>/trust           # `key=value` lines: root (two while a key is replaced), recovery, source, floor, seen_revision, switch_note, forced_host, and reserved hold, confirmed (§3.2)
  apps/<id>/update.url      # update URL (if declared)
  apps/<id>/xdg-dirs        # user folders fixed at install (§3.1.4)
  removing/<id>.<n>/        # a removal in progress or interrupted (§9.6)
```

Every link that the store writes is relative to its own folder: `current`, `previous` and `pending` (`../../packages/<record>`), `trees/<root>/.farm`, `bin/panpak-stub`, and the links of a farm to a library of the bundle (`../../trees/<root>/<path>`). The one exception is a link of a farm to a library of the system, which is the absolute path of that library. A store therefore works after it is copied or moved to another path (`tests/store_move.py`). A link that is absolute is not read: it is a store of an earlier version, and the command says to reinstall the app. What cannot be relative, because it must be absolute where it is used, lies outside the store: the launcher in `~/.local/bin` (a shell script, not a link) and the menu entry. Every command that opens the store (`list`, `doctor`, `update`, `apply`, `rollback`, `repair`, `remove`, `install`, ...) reads the launcher of each installed app, and when it names another place for the store it writes the launcher and the entry again (only files that panpak wrote). The cache of the resolution is checked against the files and made again when they are not where it says.

Several starts can overlap on a store that does not have the tree yet. The runtime therefore takes an
exclusive `flock` on `trees/<root_hash>.lock` before it builds the tree, checks again whether the tree is
complete once it has the lock (a start that waited then just uses it), builds in `trees/<root_hash>.partial`
(a folder left by a start that was killed is removed first) and publishes it with one rename. The lock goes
with the process, so a builder that dies never blocks the others. Temporary blob files carry the process
number, since two trees can write the same blob at the same time.

Outside the store: launcher `~/.local/bin/<id>` (`exec <store>/bin/panpak-stub run
<store>/apps/<id>/current "$@"`), `~/.local/share/applications/panpak.<id>.desktop` and the icons
`~/.local/share/icons/hicolor/<S>x<S>/apps/panpak.<id>.png` (§3.1.3).

`install-runtime` installs the stub of the runtime, switches `bin/panpak-stub` to the best ABI with one `rename` of a new link, and,
from a release tarball, replaces `bin/panpak` with the `panpak` of the release; every file is written through a temporary file and
`rename`, so an app that is running keeps its old file and the next start uses the new one. From a single stub it only moves the
link. `install` and `update` copy the running `panpak` to `bin/panpak` when there is none; they never replace one that is there, and
print a warning when it is older than the running one. Nothing in a launcher or an entry names the folder from which `panpak` was
run. The folders `runtimes/<abi>` stay: launchers written before the link existed point into them.

### 6.1 Launcher/runtime contract [NORMATIVE]

The stub MUST refuse packages with `min_runtime` above its own
(`RUNTIME_ABI`, v1 = 1; `panpak-stub --abi-version` prints it). The
launcher MUST run the package only through the installed runtime;
direct execution of the `.panpak` file is allowed but MUST be limited
to showing the key fingerprint (no trust assumed).

### 6.2 Making the store durable [NORMATIVE for the order; the mechanism is INFORMATIVE]

One rule: **a link is made only after everything it leads to is written back to the disk.** The links are `trees/<root>` (published by a rename of
`trees/<root>.partial`), `apps/<id>/current` and `previous`, and the record in `packages/`. The runtime makes a version durable in this order:

1. the blobs that the tree needs and `blobs/` does not have are written into a work folder of this start (`blobs/.work-<pid>-<time>/`), the kernel is asked
   to write each back as it is written, and when all are written each is synced (several at a time), then renamed into `blobs/`, then `blobs/` is synced.
   **A blob is visible in `blobs/` only when it is on disk**: whoever finds a blob there, at any time, may link it without syncing it again. A start
   that is killed leaves its work folder, which the next start removes when its process is gone. Every file goes this way, whatever its size: a large file
   is written into the work folder by several threads, each a part of it, and the memory of a start is a chunk for each thread, never a file;
   **A blob that is already in `blobs/` is checked before it is linked**, by `install`, `update` and `repair` alike: every chunk of it, read from the
   blob, MUST have the hash that the signed index gives, and a blob that does not is taken out of the store and made again from the chunks. The file of the
   tree is that very file (a hard link), so a tree that took a damaged or altered blob on trust would inherit it. The cost is one read of the file and
   BLAKE3 over it (see `docs/BENCHMARKS.md`). Before it writes anything, the runtime adds up the size of the blobs that the tree needs and `blobs/` does not have
   (the index gives it) and stops when the free space of the store (what a user that is not root can use) is below that and a reserve of 16 MiB, with a message that says
   so and that nothing was written; a file system that does not say how much is free is trusted;
2. every folder of the tree (their entries are the hard links and links of the tree) and the mark `.complete`;
3. the rename of `<root>.partial` to `<root>`, then `fsync` of `trees/`;
4. the record: written to a temporary file, `fsync`, rename, `fsync` of `packages/`;
5. only then the link `current` (written as a temporary link and renamed, then `fsync` of the folder).

It never uses `syncfs`, which writes the data of every program on the file system. The `fsync`s are on the files and folders of Panpak only,
and run on several threads, since syncs that wait for the disk together share the wait: on ext4, 938 folders took 4.4 s one after the other and
15 to 78 ms on 16 threads; 2,233 files took 0.25 s (on this machine, one thread). Blobs that are already in `blobs/` are not synced again, and no rule depends on the clock. A tree that a crash left half made is `<root>.partial`, never `<root>`, and the
next start that needs it removes it (also when it was already read-only) and makes it again.

### 6.3 The record of an installed version [NORMATIVE]

The store keeps one copy of the content of an app: its tree. What stays of the package is the part that a start needs, in a file with its own
magic: the compressed index, the manifest, the signature and a footer, in this order,

```
+---------------------------+
| index (zstd)              |  index_off = 0
+---------------------------+
| manifest.toml | sig (64)  |
+---------------------------+
| footer (80 bytes)         |  magic `PANR\0\1\0\0`, the fields of §2 with index_off = 0
+---------------------------+
```

The signed message is the one of §3.0 (label, `version`, `flags`, `root`, manifest), where `root` is the hash of the raw index, so the signature of the package is the signature of the record and
`verify` reads a record as it reads a package. The record keeps the signature block whole, between the manifest and its footer. A record has no stub and no chunks.

- A reader of **packages** (`install`, `update`, `doctor <package>`, `licenses`, every other use of the footer of §2) MUST refuse a record with
  `PANPAK-E0010`: the magic differs from §2 in its third byte, so no other reader takes one for the other.
- `panpak-stub run` runs a record only when the path resolves to a file directly inside `<store>/packages/`; anywhere else it is `PANPAK-E0010`. A
  record cannot leave the store and be used as a package, and a package cannot be put in the store and run as a record.
- A start with a record needs `trees/<root>/.complete`; without it the start stops with `PANPAK-E2007` (the record holds no chunks to make the tree
  again). `panpak repair <app id>` makes the files right again: the tree is compared with the signed index, the chunks that intact files hold are
  kept, and the others come from the update URL of the app (refused if it now serves another version) or from a package given with `--from`.
- `install` makes the app ready: `panpak-stub prepare` verifies, makes the tree and the resolution (the farm and its cache entry) with the code and
  the stub identity of a start, so that the first start is a cache hit. `update` downloads only the chunks that the installed version does not
  have into `staging/<id>/<root>/`, makes the new tree from the files of the old one and those chunks, checking every reused blob against the signed
  index (`install` does it too, see 6.2), and only then moves `previous` and `current`. The chunks are asked for by ranges [INFORMATIVE]: neighbours with at most
  16 KiB between them in one request (the bytes between are dropped), at most 4 MiB for each response, 6 requests at once; a server that answers 429 or 503 is waited for
  as it asks (`Retry-After`) and the rest goes one request at a time. Nothing that comes is trusted: each chunk is checked by its hash when it is used.

### 6.4 Locks [NORMATIVE for the order; the mechanism is INFORMATIVE]

- **One lock per app** for the commands that change what is installed: `<store>/locks/<id>.lock`, `flock(LOCK_EX)`, taken by `install`, `update`,
  `rollback`, `repair` and `remove` of that app, waited for up to 60 seconds (then `PANPAK-E2008`). It goes with the process: a command that is killed never
  leaves it held. It is outside `apps/<id>/` because `remove` deletes that folder, and a lock file that is deleted while another process waits for it locks nothing.
  `update` lets it go while the chunks come and takes it again before it publishes, and checks that `current` is still what it started from.
- **The copies of the app** hold a shared lock in the runtime folder while they run (`PANPAK-E2002`); `remove` and `repair` take it exclusively and
  do not wait for it: a command that finds a copy running stops. A start does not take the lock of the app, only the lock of the tree it builds.
- **The tree** has its own lock, `trees/<root>.lock`, taken while a tree is made (several starts or installs that need the same tree wait, and the
  second finds it done).
- **Order**: the lock of the app first, then the lock of the copies (which is only tried), then the lock of a tree; so none of them waits for one that waits for it.

### 6.5 Test points [INFORMATIVE]

A test build (the feature `failpoints` of `panpak-core`, `panpak` and `panpak-stub`) has named points, one at each step of `install`, `update`, `rollback`,
`repair` and of the making of a tree (`install.record_written`, `prepare.blobs_named`, `update.previous_linked`, ...). When `PANPAK_FAILPOINT=<name>`
(or `<name>:<n>` for the n-th hit) is set, the process stops there with `abort`, as a kill does. `tests/failpoints.py` stops every command at every point that the sources name and
checks that the store is whole and that the command run again finishes. A release build has none of this: the macro is empty, and CI
(`scripts/check-no-failpoints.sh`) fails if a release binary holds a name of a point, the message or the name of the variable.

### 6.6 A staged update and its application [NORMATIVE for the order and the checks; the mechanism is INFORMATIVE]

`panpak update --stage <app id>` does everything of an update except the last step: it checks the remote version as `update` does (the signature against the
pin or a succession that the pin signed, the id, the runtime, the glibc, a version that is not older), downloads the chunks the installed version does not have into
`staging/<id>/<root>/`, writes the record, makes the tree and the resolution (`panpak-stub prepare`), and then names the record with the link
`apps/<id>/pending` (after `pending-state`, which holds `needs_accept=1` when the version asks for permissions that the installed one does not have). `current` is
not changed, so a running copy and a start see the installed version until the staged one is applied. A version that is already staged is not downloaded again.

`panpak apply <app id>` (and the update that `panpak update` makes when the user is there) moves `current` to the staged version, in this order, under the lock of the app (6.4):

1. the staged record is opened and verified again (signature, root of the index); its tree must be complete; if it is the installed version, or its files are gone, the link is taken away and nothing else is done;
2. the key of the staged version is the pin or a chain that leads from the pin to it (§3.2, checked again here, not trusted from the staging); a key that replaces the pin is written to `trust` as two `root` lines (the old key, the new) so that a stop between the next steps leaves a start that works (the stub accepts either line);
3. **new permissions**: when the staged version asks for permissions that the installed one does not have, it is applied only when the user accepts them (`--accept-new-permissions`, or an answer at a terminal); the stub never applies such a version;
4. the exclusive lock of the copies of the app is tried: **no copy of the app may be running** (two versions of one app never run at once); if one is, the update stays staged;
5. `previous` is made to name the installed record, then `current` the staged one (each link written as a temporary link and renamed, then synced), the pin is written with the new key alone, `pending` and its state are removed, then the launcher, the entry, the icons and the pinned user folders of the new version are written.

At the start of an installed app the stub looks for `apps/<id>/pending` (one `lstat`) and, when it is there and does not wait for the user, runs `panpak apply --at-start <id>`: it takes the locks without
waiting, applies and says so on standard error, or leaves the update for later (a copy runs, another command is working on the app, new permissions) and the start goes on with the installed version.
`panpak list` shows a staged update: `update <v> is ready: applied at the next start`, or `update <v> waits for you: ...`.

### 6.7 The background job and its state [NORMATIVE for what it must not do; the rest is INFORMATIVE]

`panpak background [--force] [--exit] <app id>` is the job of one app. It is started in two ways, always detached (its own session), at the lowest CPU priority (nice 19) and I/O class (idle):

- **When the last copy of the app ends**, with `--exit`. The outer stub stays while the app runs; when the app ends and the exclusive lock of the copies (`$XDG_RUNTIME_DIR/panpak/<id>/lock`) can be
  taken, no other copy is alive: the stub lets the lock go and starts the job. It does, in this order, what the app left undone: it applies the staged update (6.6, with its rules: a version that asks for new
  permissions waits), repairs the files that were found damaged (this may use the network), and then does the rest of the job if it is due.
- **At the start of an app** when `apps/<id>/update-state` says the daily job is due (`next_check` is in the past, or there is no file; one small read), 20 s after the start, for the app that never ends.

A start never waits for the network. At a start the stub only does what needs no network and no wait: it applies a staged update when no copy runs (the fallback for what the exit work did not do, for example after a
shutdown), and when files are noted as damaged it says so on standard error with the command (`panpak repair <id>`) and starts the app anyway; an app whose files are missing does not start (PANPAK-E2007, with the same command).
The job does nothing when another job of the app holds `locks/<id>.bg`, or when it is not due (without `--exit`). The environment variable `PANPAK_NO_BACKGROUND` keeps the stub from starting it at all. A job MUST NOT change what a
running app runs: it stages (6.6), applies only when no copy runs, and repairs only when no copy runs.

1. **The check**, when `background.updates` is on and the app has an update URL: `panpak update --stage` (6.6), so with the same checks of the signature and the pin, and nothing is asked. Success: `failures=0`, next check in 24 h.
   A failure counts in `failures` and the next try is in `3600 * 2^(failures-1)` seconds, at most 24 h.
2. **The notice of a security update.** A staged version that carries the mark of a security update (`record::security_mark`; the mark belongs to the signed index and its format is not fixed
   yet, so nothing is a mark today) and that is not applied because a copy of the app runs is shown once to the user by the notification portal (`org.freedesktop.portal.Notification.AddNotification`, called with the D-Bus client of panpak
   when a session bus exists): it says that a restart applies it. `notified` keeps the root of the version already announced.
3. **The verification**, when `background.verify_mb` is not 0: the files of the installed version are read in the order of the signed index from `cursor`, until the megabytes of the day (`verify_day`,
   `verified_bytes`) or 20 s are used, always at least one file; the end of the list is a round (`last_round`, cursor back to 0). A file that is missing or does not match is counted in `damaged`
   (with the first path in `damaged_first`) and the job repairs it as `panpak repair` does when no copy runs; otherwise it is repaired when the last copy ends. `panpak repair` clears the note.

`update-state` is a text file of `key=value` lines (`last_check`, `next_check`, `failures`, `cursor`, `verify_day`, `verified_bytes`, `last_round`, `damaged`, `damaged_first`, `notified`); a reader treats an
unknown line as absent and a bad number as 0, and nothing in it is trusted for anything but the schedule of the job and what to tell the user.

The settings are `~/.config/panpak/config.toml` (`panpak config`, `panpak config set <key> <value>`): `[background] updates = true`, `verify_mb = 64`, and `[network] ca_file = "<file>"` (certificate authorities in PEM that REPLACE the built-in list for every download of panpak, for a network with its own authority; a file that cannot be read or holds no certificate stops the download, it never falls back to the list). A file that is not valid TOML means the defaults.

### 6.8 Garbage collection [NORMATIVE for what is kept and for the lock; the order is INFORMATIVE]

`panpak gc [--dry-run]` takes out of the store what no installed version reaches. Liveness is read from the records, never counted:

- **records**: the ones that `current`, `previous` or `pending` of an app name. Every other record goes (one version of rollback), as does a `.rec.tmp<pid>` of a dead process, and a `*.panpak` in `packages/` that no link names (a package kept by a store from before step 8a: there is no compatibility with that layout);
- **trees**: `trees/<root>` and its marks (`.readonly`, `.lock`) live while a kept record has that root; leftovers of an interrupted make or repair (`.partial`, `.damaged<pid>`) go;
- **farms**: `farms/<root>` lives while any record with its root lives. (The farm is named by the root, and a tree is shared by every version with that root, so it cannot be told apart by version.)
- **blobs**: a blob (`<hash>-<size>`) that no tree links (link count 1) goes, and so does the work folder of a dead process (`.work-<pid>-<time>`);
- **caches and leftovers**: an entry of the ABI cache whose farm is gone, or that is older than 30 days (it is rebuilt at the next start); a download folder in `staging/<id>/<root>` that nothing wrote to for 7 days (at once for an app that is not installed); a runtime `runtimes/<n>` that neither the stable link, nor the best runtime, nor a launcher or entry of an installed app names. Time is used only here, where it decides nothing but space.

**The store lock (a guarantee that does not depend on the clock).** `locks/.store` is a `flock` in two modes. Install, update (and its staging), apply, repair, rollback, remove, and the making of a tree (also by the start of a package that is not installed) hold it
SHARED for as long as they work, and take it before the lock of an app. `panpak gc` takes it EXCLUSIVE without waiting; when it cannot, it does nothing, says that it was left for the next time, and ends well. So a collection never sees work that is half done (blobs named and not yet
linked, a record written and not yet named by a link), whatever the time that work takes. A command that is short of time (the application of a staged update at a start) does not wait for a collection: it leaves its work for later.

Other rules: the app lock of each installed app is taken without waiting for long, and an app that is busy, or that has a copy running, keeps everything it has; the caller that holds the lock of an app passes it. A name that is not of the forms above is never touched. The order is
records, trees, farms, blobs, then the caches, so that a stop in the middle leaves things that the next run takes out and never a link that names nothing (the test points `gc.*` check it). It runs after an apply (not at a start), after an update or a removal, and when the last copy of an app ends,
always after the command has let go of its own store lock; `userdata/` and the data of the apps are never touched.

### 6.9 The private cache folder [NORMATIVE for what is emptied and when; the mechanism is INFORMATIVE]

The cache folder of an app (the `XDG_CACHE_HOME` of the app, `$XDG_CACHE_HOME/panpak/<id>` on the host) holds only what the app can make again. Some caches take a file for stale only when it is newer than the cache (KDE's ksycoca keeps the latest time of the files it saw; GTK and Qt drop an icon cache that is older than its folder), and the times of a tree come from its content (§6, *Times in the tree*), so a file that changed in an update, or came back with a rollback, may have an older time than a cache made from the other version. So:

- `panpak update`, `panpak repair` and `panpak rollback` empty the cache folder of the app when they have changed the installed version, unless a copy of the app is running (the lock of the copies, §6.4), in which case they leave it;
- the first copy that starts alone (no other copy runs) compares the file `.panpak-cache-of` of the cache folder with the root hash of the tree that it starts; when it is another one, or the file is not there, it empties the folder and writes the file. This covers a copy that was running when the command ran, and a tree that came by any other way;
- never emptied: `tmp` (the app's `TMPDIR`, which the runtime empties by its own rule, §9.4) and `.panpak-cache-of`. A link in the folder is removed itself and never followed. The data of the app (`data`, `config`, `state`, the folders that the user granted) are never touched.

### 6.10 Kept versions and the outcome of starts [NORMATIVE for what is kept and when a person is asked; the mechanism is INFORMATIVE]

A client keeps, for an installed app, the installed version, a staged version, and the *last good* version: the last version that worked, named by the link `apps/<id>/last_good`
(the record, as `current` names one). The previous version (`previous`) is kept until the installed version has worked, and always when the person asked (the file
`apps/<id>/keep_previous`, made by `panpak keep-previous`). A rollback goes to the last good version when there is one that is not the installed version, else to the previous one;
it is refused for a version below the floor (§3.1.10). A client MUST NOT keep a previous or last good version that is below the floor; the collection takes it out and leaves a note (`apps/<id>/withdrawn`, `floor=<n>`), and a rollback when there is no version to go back to because of it MUST say that the author withdrew it (`PANPAK-E4003`), not that a file is missing. With free space under 2 GiB or under 5 percent the collection (§6.8) drops the last good version first (never
the installed or staged one).

*The outcome of a start.* The process that holds the app counts how each start ended, in `apps/<id>/starts` (lines `root=<hex>`, `good=<n>`, `failed=<n>`; the counters are of the
version `root`; the end of a start of a version that is not the installed one (the one `current` names) is not counted, and a start of another version begins them again). A start is **good** when the app ran for 20 s or more and did not end by a signal that the person or the
system sent; **failed** when it ended within 20 s with a non-zero status or a fatal signal of the program (SEGV, ABRT, BUS, ILL, FPE, SYS and the like); and counts **neither way**
when it ended within 20 s with status 0 (a program that hands over to a copy that runs), by SIGKILL, by a stop signal that the client passed on (the end of the session, a window
closed), or when the arguments were `--version` or `--help`. A good start sets `failed` to 0, and so do the changes of the installed version (an install, an update that is applied, a rollback) and the answer "Try again": the failed starts in a row are forgotten, the good ones are kept, so that the question comes back only after 2 new failed starts in a row (the start that follows "Try again" is the first). After 3 good starts the version is *proven*: it becomes the last good version. After 2
failed starts in a row a client MUST ask the person, in a window, whether to go back to the last good version, when there is one and it is not the installed one; it MUST NOT go back by itself.
The question says the name of the app (not its id), the two versions, and that the documents stay as they are; the answers are "Go back to <version>" and "Try again". The line of
`panpak rollback` says that the documents are not touched and that settings changed by the newer version may not work in the older one: a client cannot undo a migration of the
data of the app, and it does not pretend to. After "Go back" the client MUST tell the person how it ended, in a notification titled with the name of the app: that the app is back to the version ("<name> is back to version <old>. Your documents stay as they are.") or that it could not go back and why ("<name> could not go back to version <old>: <reason>"; the reason is the first line of the error, without control characters, at most 200 characters, and comes from the client, never from the app). After "Try again" it says nothing: the app itself is the answer. A notification that cannot be shown (no tool, no bus) changes nothing.

*The window, and the question that nobody answered.* The question is a window of `zenity` or `kdialog`. Where neither exists, a client MUST ask with a notification with the
two buttons as the only answers (a click on the body is no answer): to the notification service of the desktop (`org.freedesktop.Notifications`, `Notify`) first, and, when the service is
not there, does not show buttons (its `GetCapabilities` does not list `actions`) or does not take it (the call fails, or the subscription to its answer signals fails), to the notification portal (`org.freedesktop.portal.Notification`), and only from a process whose cgroup ends in a scope or service of an app (`app-...scope`, `app-...service`). The service is first because a process that the shell starts from the menu may be in
no application scope (for example `org.gnome.Shell@user.service`), where the portal cannot tell which app it is: it answers `ok` and refuses the notification in silence (`InvalidApp` in its journal, nothing on the screen), while the service takes any caller. From a process that is in no scope of an app a client MUST NOT use the portal: the question stays pending and the line of the command is shown, as with no window tool. The action "Uninstall" (`remove --ask`) leaves the scope of the app for a scope of its own (`panpak-remove-...`, so that the document portal does not take the process for the app) *before* it asks, so for it the portal is never used: with no notification service nothing is asked, nothing is removed, and the command is told. A notification of the service has the action `default` (what a click on the body sends) next to the two buttons, and the hint `resident`, so that a click on the body neither answers nor withdraws it; a notification of the portal has a `default-action` that nobody listens to, so that the shell does not start the app that the portal names. The answer signals of the service (`ActionInvoked`, `NotificationClosed`) are
broadcasts and the number of a notification is small and can be guessed, so a client MUST accept them only from the owner of the name `org.freedesktop.Notifications`, and only for the number that
`Notify` returned. A client MUST NOT take a closed or unseen notification, or a window that was closed with the cross, for an answer: a notification that the person dismisses (or that expires) ends the wait at once and the
question stays unanswered; a client MUST NOT count the question as answered until a button was pressed. The process that asks stays alive and
waits for the button for at most 5 minutes on the service (a person who started "Uninstall" may leave for a moment, and the notification is surely shown) and for at most 1 minute on the portal (it can be refused later without a sign, so a question that nobody answers is not held for long), then it withdraws the notification (`CloseNotification`, or `RemoveNotification` of the portal), so that no button is left that nobody listens to. The question is
**pending** from the moment it is asked until a button answers it: the file `apps/<id>/pending-question` (`kind=kept-versions`, `root=<record>`) holds it, and one process at a
time (a lock on the file) waits for the answer. At the next start of the app the client asks again when the file names the installed version, and writes one line on its
standard error with the command (`panpak rollback <id>`); a file about another version, or a question that has no last good version to go back to, is dropped. `panpak rollback` ends the
question. With no window tool, no notification service, no portal and no bus, the question stays pending and only the line is shown. The action "Uninstall" is not kept: with no answer nothing is removed.

*Privacy.* The outcomes never leave the computer: they are a few numbers in a file of the store, read by the client and the runtime, and no client sends them anywhere. The file
is not trusted for anything but this question: a damaged or edited file can make the question appear or not appear, and it changes no version.

## 7. ABI resolution (`[abi]`) [NORMATIVE]

`panpak pack` compiles `needs` by scanning `DT_NEEDED` of the ELF
files in the tree (union with any declared `needs`; names containing
`$ORIGIN` MUST be resolved against the directory containing them).

`panpak pack` also writes `[host].glibc` (§3.1.11): the highest `GLIBC_x.y` version name that the ELF files of the tree ask of the
glibc libraries (`libc.so.6`, `libm.so.6`, `libpthread.so.0`, `libdl.so.2`, `librt.so.1`, `libresolv.so.2`, `libutil.so.1`, the loader
and the like; read from `.gnu.version_r`; `GLIBC_PRIVATE` does not count). A declared value can only raise it, never lower it.
A tree that carries its own glibc (world B) gets none from the files. The value is a version of two to four numbers (`2.39`).
`install` and `update` MUST refuse a package whose `[host].glibc` is higher than the glibc of the system (`PANPAK-E2005`), with both
versions in the message, before anything is written; a system whose glibc cannot be read (musl, no `ldd`) is not refused by this check.
The stub resolves at SONAME granularity (names, not symbol versions
yet). **What the bundle carries** is the inventory of its tree: the names that a `DT_NEEDED` can ask for, in this order of precedence: (1) files and
links named `lib*.so*`, and the loader; (2) any other regular ELF file, by its file name (FreeCAD calls its modules `Mesh.so`, `Part.so`), and a link with
`.so` in its name that leads to a file of the tree; (3) the `DT_SONAME` of every ELF file when it differs from the file name. A later kind never replaces an earlier one, and
among files of one kind the path in alphabetical order wins. A reader that has only the index of the package (`panpak doctor` before the first start) knows names
only: it takes `lib*.so*`, the loader and any file with `.so` in its name. The resolution has two worlds:

- **world H** (no glibc in the bundle): a library that is in the bundle
  comes from the bundle, and one that is not comes from the host
  (`ldconfig -p`, fallback to scanning `/lib*` `/usr/lib*`). Four additions
  follow, decided from the files, never from a version number:
  1. *The host wins* (when the host has the library) for the glibc family
     (`ld-linux*`, `libc`, `libm`, `libdl`, `librt`, `libpthread`,
     `libresolv`, `libutil`, `libanl`, `libBrokenLocale`, `libmvec`,
     `libthread_db`, `libcidn`, `libnss_*`) and for the graphics stack that
     must match the driver (`libGL`, `libGLX`, `libGLdispatch`, `libOpenGL`,
     `libEGL`, `libGLESv1_CM`, `libGLESv2`, `libvulkan`, `libgbm`, `libdrm`,
     `libdrm_*`, `libglapi`, `libxcb-dri2`, `libxcb-dri3`, `libnvidia-*`) and for the Wayland client libraries (`libwayland-client`,
     `libwayland-egl`, `libwayland-cursor`), which the host's EGL calls into and which must be the same copy in the process
     (measured: a bundled 1.23 next to a newer host Mesa made `eglGetDisplay` fail).
     The list is a subset of the AppImage excludelist
     (<https://github.com/AppImageCommunity/pkg2appimage/blob/master/excludelist>):
     only the entries for which the system MUST win. The other entries of
     that list (X11, fonts, audio, GLib) are not exceptions: a package that
     wants the system copy of one of them says so with `prefer_host`.
  2. `libstdc++.so.6` and `libgcc_s.so.1`, when both the host and the bundle
     have them: the copy that offers every symbol version
     (`GLIBCXX_*`, `CXXABI_*`, `GCC_*`) that the entry, the executables next to
     it and the chosen libraries ask of it (`.gnu.version_r` against
     `.gnu.version_d`); if more than one does, the one with the newest
     versions; if none does, the one that offers more; the bundled one on a tie.
  3. *Atomic families*: all of a family comes from the bundle or all from the
     host, never a mix, because the libraries of a family are built against
     each other and a mix dies with missing symbols (observed: a bundled GLib
     older than the system `libgobject`, `g_free_sized` not found). If the
     bundle has every library of the family that the package needs, the bundle
     is used; otherwise, if the host has them all, the host; otherwise each
     library follows the general rule. The families are: Qt (`libQt5*`,
     `libQt6*`, `libKF5*`, `libKF6*`, with the core library in the bundle); GLib
     (`libglib-2.0`, `libgobject-2.0`, `libgio-2.0`, `libgmodule-2.0`,
     `libgthread-2.0`); GTK with the libraries whose types it shares (`libgtk-*`,
     `libgdk-*`, `libgdk_pixbuf-2.0`, `libatk-1.0`, `libatk-bridge-2.0`, `libatspi`,
     `libpango-1.0`, `libpangocairo-1.0`, `libpangoft2-1.0`, `libpangoxft-1.0`,
     `libcairo`, `libcairo-gobject`, `libepoxy`, `libgraphene-1.0`); GStreamer
     (`libgst*`); FFmpeg (`libavcodec`, `libavformat`, `libavutil`,
     `libswresample`, `libswscale`, `libavfilter`, `libavdevice`, `libpostproc`).
  4. *Symbol check*: after these rules the runtime reads the ELF files of the
     entry, the executables next to it, the chosen libraries, the loader and the
     system libraries that those need in turn, and looks for non-weak undefined
     symbols (with their version names) that none of them defines; symbols asked
     of a library that only modules need (§7.2) do not count. If there are some, it moves to
     its other copy (bundle to host, or host to bundle) every library whose other
     copy defines one of them, together with the rest of its atomic family (only
     if every member has that other copy), and checks again. A step counts only if
     it lowers the number of undefined symbols; otherwise it is undone and those
     libraries are not tried again. It stops when nothing is undefined, when no
     library can move, or after 8 steps. It never moves a library of exception 1,
     `libstdc++` or `libgcc_s` (exception 2), a library named in `prefer_host`,
     nor anything in world B. If symbols remain undefined,
     the app still installs and starts, the runtime prints a warning with the
     files and symbols at the first start after the resolution is computed, and
     `panpak doctor` shows the same detail.
  The cached decision is valid while these are unchanged: the verified root and
  the `[abi]` fields and entry of the package, the stub that runs, the kernel
  release, `/etc/ld.so.cache` (which `ldconfig` rewrites when a package manager
  updates a library), and every system file the decision looked at, also the
  libraries it did not choose and the loader (path, device, inode, size,
  modification and change time). A hit costs the key and one `stat` per system
  file, and runs no other process.
  The decision is made when the resolution is computed, at the first start of
  an installed package and again when the package, the kernel or the library
  inventory changes (it is cached in the store); it is not part of the package,
  so the format does not change. Before this rule the host won in world H.
- **world B** (glibc in the bundle): everything in the bundle comes
  from the bundle, loader included (`ld-linux` run with
  `--library-path`). The glibc family (loader+libc+libm+...) is
  atomic: mixing builds dies with `GLIBC_PRIVATE` or
  `version GLIBC_X.YY not found` (both observed in tests). Only
  `prefer_host` (e.g. libGL) and SONAMEs missing from the bundle come
  from the host.

For each decided SONAME the stub creates a symlink in `farms/<root>/`
(to host or bundle) and launches with that `LD_LIBRARY_PATH` (written relative to the program, section 7.1):
per-lib control despite the per-directory path. The loader (`ld-*`,
libc's own `DT_NEEDED`) is not part of the farm: it is the plan's
`loader`.

The decision closes over the transitive closure at a fixed point: the
`DT_NEEDED` of already-decided libs (read from the real files, host
or bundle) are decided with the same policy. Without this, bundle
libdirs in `LD_LIBRARY_PATH` would shadow system libs for transitives,
mixing versions (observed: newer host libgio + older bundle
libmount). Never a silent fallback: a missing library that a program needs stays a hard
error; one that only modules need is "not available" (§7.2).

Libs in `prefer_host` MUST never be substituted: if missing on the
host it is an error, never the bundle. `prefer_host` + included glibc
is a combination rejected by `panpak pack`.

The Qt/KF families (`libQt5*`, `libQt6*`, `libKF5*`, `libKF6*`) follow
the same atomicity as glibc: if the bundle covers ALL needs of the
family, the whole family comes from the bundle; otherwise, if the host
covers all of them, the whole family comes from the host. Only if
neither coverage is complete does it fall back to per-SONAME rules
(known mix risk: see NOTE v1 below).

### 7.2 The report of the modules [NORMATIVE]

No author declares a library optional. `panpak pack` reads the ELF files of the tree and writes `.panpak/modules`, a file of the tree (so the signed index covers
it) whose BLAKE3 hash, in 64 lowercase hex characters, is `[abi].modules`. A *program* is an executable ELF file with an interpreter whose name has no `.so`. The
*required* libraries are the names in the `DT_NEEDED` of the programs and, going through the libraries of the tree (found by file name or SONAME), of every file
that they reach. A library that is not required and that the tree does not provide is asked only by *modules*: plugins, extension modules and workbenches that are
loaded on demand.

The file is UTF-8 text. Its grammar (RFC 5234; `TAB` is %x09, `LF` is %x0A):

```
report   = header *program *needs *module
header   = "panpak modules " version LF          ; the form of the file, not of the package
version  = %x31-39 *DIGIT                        ; no leading zero; "1" is the only form defined
program  = "program" TAB field LF                ; a path of the index
needs    = "needs" TAB field LF                  ; a library name (a file name or a SONAME)
module   = "module" TAB field TAB count 1*3(TAB field) LF
count    = %x31-39 *DIGIT                        ; how many modules ask for the library
field    = 1*(%x20-7E / %x80-FF)                 ; UTF-8, no control character, no TAB
```

The first line is the version of the form. A Reader that knows the form MUST refuse a file whose first line is not exactly of this grammar; one that finds a version above
the ones it knows MUST refuse the package with "update panpak". A change to the grammar, for a field or a line, is a new version. Inside a group the lines are in ascending
byte order of their first field and no first field repeats; the three groups come in the order above. For a `module` line the paths after the count are in ascending byte
order without a repeat, `count` is at least their number, and they are paths of the index. A Reader MUST refuse a report that breaks any of these, and a file whose BLAKE3
hash is not the one in the manifest. A package that has an ELF file MUST have the report; one that has none has neither the file nor `[abi].modules`. A Writer MUST give
the same bytes for the same tree.

Resolution (§7) uses it: a library that is not required and that neither the system nor the tree provides is left out, and the app starts; the modules that ask for it do
not load, and `panpak inspect` lists the library as "not available on this system". A required library that nothing provides is an error where the app starts, as before.
There is no way for an author to turn this off or on.

NOTE v1: symbol-version comparison (`VERNEED` vs host) decides only between
the two copies of `libstdc++.so.6` and `libgcc_s.so.1`. For every other library
the §7 resolution applies at equal SONAME, and a possible incompatibility fails
the loader explicitly (fail-closed). `panpak doctor` reads the ELF files of an
installed app and reports the undefined symbols that none of the chosen libraries
defines, after the symbol check above. See also Appendix C.

### 7.1 The library path of the app [NORMATIVE]

The runtime puts the farm and the library folders of the package in `LD_LIBRARY_PATH` of the app. An absolute list reaches
every program that the app starts, system programs included (a shell, `zenity`, `flatpak`), and those load the package's
libraries instead of their own: `symbol lookup error`, `no version information available`. So the value is relative to the
program that is run:

```
LD_LIBRARY_PATH=/$ORIGIN/.panpak-lib-<h>/.farm:/$ORIGIN/.panpak-lib-<h>/lib:...
```

- `<h>` is the first 8 hex digits of the BLAKE3 hash of `app.id`. It is not derived from the root hash: the links below are part
  of the content that the root hash covers.
- glibc replaces `$ORIGIN` with the folder of the main program of the process, in `LD_LIBRARY_PATH` as well (measured on
  glibc 2.39, 2.41, 2.43 and 2.44). The entry becomes `//<folder>/.panpak-lib-<h>/...`.
- **In the package**: in every folder that holds a program there is a symbolic link `.panpak-lib-<h>` to the root of the tree
  (`.` in the root, `..` in `bin/`, `../..` two levels down), and the same link in the root, which is the mark by which the
  runtime knows that the package is made this way. A program is a regular file, with an execute bit, that is an ELF with a
  program interpreter (`PT_INTERP`) and has no `.so` in its name. `pack` refuses a package that has a program and lacks a link
  or has one with another target (`PANPAK-E0009`, with the `ln -s` commands); a package with no program needs none. `build` and
  `convert` make the links. The name `.farm` in the root of the tree is kept by the runtime.
- **In the tree**: the runtime makes `<tree>/.farm`, a link to `../../farms/<root>`, when it makes the tree (before the tree is
  made read-only; it is not part of the package). Before the app is started the runtime checks that the link and the farm folder
  are as they should be, and stops with `PANPAK-E2006` otherwise: with a farm that cannot be reached the loader skips the entry
  without a message and the app would start with other libraries than the ones chosen.
- **For a program of the host** the folder is `/usr/bin` or alike, where `.panpak-lib-<h>` does not exist, and the loader skips
  the entry: the program uses its own libraries. The cost is a failed `stat` and `open` for each entry and library
  (measured: +0.1 to +0.3 ms for a start of `grep` or `zenity` with 11 entries).
- **musl** does not expand `$ORIGIN` and takes a relative entry as a path under the current folder: a value without the slash in
  front loads a library from `./$ORIGIN/.panpak-lib-<h>/` when the folder holds one (measured). With the slash the entry is an
  absolute path in the root of the file system, where only the administrator writes, and the current folder does not matter.
- There is no absolute list of libraries for the app: a package whose entry is a program and that has no mark is refused
  (`PANPAK-E0009`, "make it again"), and a package with no program gets no `LD_LIBRARY_PATH` from the runtime.
  `--library-path` of the bundled loader (world B) is absolute: it is an argument of the loader, not an environment variable that
  other programs inherit.

### 7.3 The SBOM [NORMATIVE]

`panpak pack` writes `.panpak/sbom.cdx.json`, a file of the tree (so the signed index covers it), as a [CycloneDX](https://cyclonedx.org/) 1.6 JSON document. Its
BLAKE3 hash is `[sbom].hash` (§3.1.12), so the signature covers the document through the manifest, and a Reader that has the manifest can read the document alone: the
index gives the chunks of the file, and a request for those chunks (a range request, for a package that is served) is enough, with no download of the rest of the package.
A document larger than 8 MiB is refused by `pack` and by a Reader.

What the document holds depends on `[sbom].coverage`:

- **`build`**: the package was made by `panpak build` (it has `[provenance]`, §3.1.7). The components are the source (`vcs` or `distribution` reference, with the commit
  or the SHA-256 that the recipe pins), the build image, each dependency taken from the build image (its package and version), and the libraries of the tree.
- **`best-effort`**: the package was made by `pack` or `convert` from files, and the document says only what the files show: the libraries that the tree carries (an ELF
  file with a SONAME: its name, its path and its BLAKE3 hash) and, for a conversion, the file it was made from (name, version, SHA-256). It lists no version of a library
  that nothing signed names, and it is not a statement that nothing else is in the package.

A Writer MUST give the same bytes for the same input: no time, no random serial number, no name or version of the app (the tree is addressed by its content, so the same
files under two names are one tree, and a name in the file would make every release another tree; the manifest names the app), and the components in a fixed order (what the
package was made from, then the libraries in order of their path). The first lines of the document, up to `"version": 1,`, are fixed, and the `metadata` holds the properties `panpak:coverage` (the
coverage of the manifest) and `panpak:components` (how many components there are), so that a Reader can show a summary without parsing the document; a Reader MUST NOT
rely on that form for anything that matters, because the hash is what is signed. The names that Panpak adds to CycloneDX are properties with the prefix `panpak:`.

`panpak sbom [--export] <package | app id | URL>` writes the signed document. For an installed app it reads the file of the tree and checks it against the hash; for a package served
over HTTPS it reads the footer, the manifest, the signature block and the index, and then only the chunks of the SBOM, and checks the signature against the key that the
manifest names. That check shows that the document is whole and was signed by whoever holds that key: it does not make the key trusted, and the command says so.

**The document for a scanner.** The file of the package has no identity, no serial number and no time, so that it is the same bytes for the same files. A scanner usually wants
them, so `panpak sbom --export` writes the complete document: the same components, and `metadata.component` (the name, the version, and the id as the property `panpak:id`) taken
from the signed manifest, a `serialNumber` that is a function of the root of the tree and of `[sbom].hash` (the same package, the same number), and `metadata.timestamp` (the
time of the export, or `SOURCE_DATE_EPOCH` when it is set, so that an export can be reproduced). The export reads only the manifest and the SBOM (a range request for a URL), is
made at the time and never stored, and refuses a signed document that is not of the form that `pack` writes. This is the way to give the SBOM of a package to a scanner.

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
  + dirs `dri`, `shm`, and the pseudo-terminals: `ptmx` and `pts` (see below). Explicit fallback (with warning) to whole
  `/dev` if the kernel rejects per-file rules; privileged nodes stay
  protected by DAC/capabilities. From Landlock V5 the device ioctl right
  (`IoctlDev`) is granted only on `tty` (termios, window size) and on `dri` (the GPU);
  an ioctl on any other device the app opens is refused with `EACCES`, whole-`/dev` fallback
  included. Descriptors the app inherited (its standard streams) are not affected.
  *Pseudo-terminals.* Every app can open a pty (a terminal emulator, an editor or IDE with a terminal, a program that
  uses `openpty`), so there is no permission for it. The isolated view has a devpts of its own (a new instance, with its own `ptmx`),
  never the host's, so the ptys an app can see are its own and not the terminals of the user. `ptmx` and `pts` get read, write
  and the device ioctls (window size, session, job control); `TIOCSTI` and `TIOCLINUX` stay refused by the system call filter (§9.8).
- Read-only: the system caches of §9.9 (`/var/cache/fontconfig`), mounted into the view and given
  one Landlock rule each.
- TCP networking: denied by default (`network = false`). With
  `network = true` and no port listed, the app has the whole network: the
  runtime MUST NOT filter TCP, and MUST NOT refuse the start on a kernel
  without Landlock V4 (there is nothing to filter). With `network = true`
  and ports, only the manifest-allowlisted ports pass (`network_allow_tcp`
  for connect, `network_bind_tcp` for listen). Landlock filters the TCP
  port, not the address: a granted port is open to every address,
  loopback or not. The filter of ports needs Landlock V4 (kernel 6.7 or later). On kernels without
  Landlock V4 the network is not filterable: the runtime MUST refuse
  by default, and MAY start ONLY with `--network-unfiltered` from the
  CLI (never from the manifest), with a warning at every start; ports
  listed with `network = true` are always refused there. Landlock
  never filters UDP; in the isolated view (§9.2) an app that asks for no network has no
  network but the loopback interface.
- Landlock ABI: highest supported detected with HardRequirement (kernel 7.0:
  V8; kernel 7.2.7: V9). Minimum: V1 (kernel 5.13).
- Fail-closed self-test after `restrict`: `/dev/null` MUST open and
  creating a file in the tree MUST fail, otherwise hard error.
- If Landlock is not applicable: hard error, NEVER silent fallback.
- A seccomp filter is installed as the last step before the app is executed (§9.8); if the kernel cannot
  install it the start fails with `PANPAK-E3002`.

### 9.1 Private folders and environment [NORMATIVE]

Before the sandbox is applied, and after the manifest has been verified, the
runtime creates these folders with mode 0700 (an existing folder is kept; a symbolic link
or a file in its place is an error):

| Folder | Path |
|---|---|
| private home | `$XDG_DATA_HOME/panpak/userdata/<id>` (default `~/.local/share/...`): the only folder of the data of the app |
| config, data, state | `.config`, `.local/share`, `.local/state` below the private home |
| cache | `$XDG_CACHE_HOME/panpak/<id>` (default `~/.cache/...`); `tmp` below it is for large temporary files |

They are not inside the store. The private home is the home of the app. With the isolated view (§9.2) it is mounted at the
path of the real home folder, so that `$HOME` is the same path as for every other program and the app writes where it
expects to: `~/.config`, `~/.local/share`, `~/.local/state` and any other file or folder it makes in its home. The cache is
mounted as `~/.cache`. Without the view (a system where bubblewrap cannot run) `HOME` is the private home itself, the
cache is `$XDG_CACHE_HOME/panpak/<id>`, and the rules of the earlier format apply to the environment of the user (below,
"Without the view"). The runtime sets, for the app:

| Variable | Value |
|---|---|
| `HOME` | unchanged in the view; the private home without it |
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` | `~/.config`, `~/.local/share`, `~/.local/state` (the folders of the private home) |
| `XDG_CACHE_HOME` | `~/.cache` in the view (the cache folder); the cache folder without it |
| `TMPDIR`, `SQLITE_TMPDIR` | the folder `tmp` of the cache of the app, on disk, emptied when the last copy ends; in the view it is mounted at `/var/tmp` and `TMPDIR` is `/var/tmp`, a short path whatever the home is; without the view it is the folder itself; `/tmp` of the view is a small tmpfs (§9.2); the placeholder `${TMPDIR}` in `[launch]` means the same |
| `XDG_CONFIG_DIRS`, `XDG_DATA_DIRS` | the existing value, or the XDG default (`/etc/xdg`; `/usr/local/share:/usr/share`); without the view then the user's real config or data folder |
| `GTK_THEME` | see below |
| `SSH_AUTH_SOCK`, `GPG_AGENT_INFO`, `SSH_AGENT_PID` | removed |
| `DBUS_SESSION_BUS_ADDRESS`, `DBUS_SYSTEM_BUS_ADDRESS` | `disabled:`, an address no D-Bus library can use, unless `sockets_allow` lists that bus (a socket named `system_bus_socket` is the system bus, any other D-Bus socket the session bus); then unchanged |
| from `[launch]` | `env` values, and `args` before the user's arguments |

`XDG_RUNTIME_DIR` is not changed. Hiding the agent and bus variables is hygiene, not a barrier: below Landlock V9 an app can still open `$XDG_RUNTIME_DIR/bus` or an agent socket by path. Libdbus, GDBus and sd-bus do not look for the bus when the address is `disabled:` (tested with a private bus). The app can write only in the private home (with the cache),
the `rw` home paths and `/tmp`. Everything the isolated view mounts into the home (a granted folder, the tree of the
app, the environment below) needs a mount point, which `bubblewrap` makes in the private home as an empty folder or file;
those stay when the grant ends and are not data of the app. A folder of the user that is not granted is never there. There
is no migration from the folders of the earlier layout (`config`, `data`, `state`, `tmp` below the persistent root); they
are not read.

*The environment of the user.* The app sees the same fonts, themes, icons, cursors and toolkit settings as every other
program, at the real paths, inside its home. Each of these folders is mounted **read-only**, as it is (a bind mount of the
user's folder, `--ro-bind`), with any bubblewrap: nothing is copied and nothing depends on a version of bubblewrap or of the
kernel. A change the user makes meanwhile (another theme, a new font) shows in a running app; the app cannot change these
files (a write fails with "read-only file system"). For the entries that exist:

- `~/.config`: `fontconfig`, `gtk-3.0`, `gtk-4.0`, `qt5ct`, `qt6ct` (folders); `kdeglobals` and `Trolltech.conf` (files);
- `~/.local/share`: `fonts`, `themes`, `mime`, and each theme folder of `icons` except `hicolor` (folders);
- `~`: `.fonts`, and each theme folder of `.icons` except `hicolor` (folders).

A mounted folder shows what is in it now, files that are added or replaced included. A mounted *file* is the file that was there when
the app started: a program that replaces it by a rename (KDE writes `kdeglobals` that way) is not seen until the next start.

In those folders, `gtk-3.0/bookmarks`, `gtk-3.0/servers` and `gtk-4.0/bookmarks` are empty files (they list what the user has
been doing), and the user's `hicolor` icon theme (`~/.local/share/icons/hicolor`, `~/.icons/hicolor`) is not in the view, which is why the icon folders are mounted theme by theme. It is the folder where `install` puts the menu icons of the apps (§3.1.3), which the app has no use for, and it has no
`icon-theme.cache`, so Qt looks up each icon name file by file in every size folder: 40,247 failed `access()` calls and
about 100 ms more before the window of QOwnNotes appeared; mounting the whole `icons` folder with an empty `hicolor` was measured too and cost 170 ms (the user's `hicolor` cannot be hidden inside a folder that is mounted whole). The list of recent files is the app's own, in its private
home; the file dialog of the portal shows the real one. The user's `mimeapps.list` and `user-dirs.dirs` are not mounted:
the runtime writes the ones of the app (the opener, the granted folders) into the private config folder.

*Without the view.* `HOME` is the private home, the environment of the user is read from the real folders (Landlock
read-only rules for the same entries, and `hicolor` is not readable), found through `XDG_CONFIG_DIRS` and `XDG_DATA_DIRS`,
and fonts are found through a `fonts.conf` that the runtime writes at every start into
`$XDG_CONFIG_HOME/fontconfig/fonts.conf` (it includes, with `ignore_missing`, the real `fonts.conf` and `conf.d` of the user, and adds as `<dir>` the real
font folder and `~/.fonts`; it carries the marker `X-Panpak-Generated`, and a file without the marker is the user's). `panpak doctor` says so.

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
   and MUST NOT use it when it is writable by others. The view has: a new tmpfs as `/`, made read-only,
   with only these of the host put in it read-only: `/usr`, `/etc`, `/sys` (the folders Landlock lets the app read), `/nix/store` and
   `/gnu/store` when they exist, and the symbolic links at the top of the host's `/` (`/bin`, `/lib`, `/lib64`, and
   `/home` where the homes are in `/var/home`). Nothing else of the host's `/` is in the view: not `/var`
   (a new, empty tmpfs that holds only the links `/var/run` and `/var/lock` to the new `/run`), `/opt`, `/srv`,
   `/mnt`, `/media`, `/root`, `/boot`, `/snap`, nor the part of `/nix` and `/gnu` that is not the store, because a
   folder of the host can hold a socket that a Landlock below V9 does not stop the app from using. A path
   that the runtime must give to the app (a granted folder, an opened file) is mounted at its own place wherever it is. `/dev` is a new
   minimal one (the nodes of §9, a devpts of its own, and the GPU folder `dri` of the host); a new `/proc`; a new tmpfs for `/run`; `/tmp` a new tmpfs of the view with a size cap of 256 MiB (it is memory; `--size`), `/dev/shm` as in §9.4; the private home of the app (§9.1) mounted at `$HOME` (writable),
   with the cache of the app as `~/.cache`, the `home_paths` as declared, the environment of the user listed in §9.1 (read-only), the Xauthority file with `x11 = true`, a file opened with
   `panpak open` as granted, and from the store the tree, the library folder `farms/<root>`, the loader and
   the stub (read-only); a new tmpfs for `$XDG_RUNTIME_DIR`
   with only the sockets of the policy below and, with `permissions.gvfs`, the FUSE tree of GVfs (§3.1.5) as a bind mount; the folder in `/run` that holds the target of `/etc/resolv.conf` read-only (name resolution);
   the X11 socket of `$DISPLAY` only with `x11 = true`. The view uses its own user, PID and mount
   namespaces, and its own IPC namespace unless `x11 = true`. It shares the network namespace of the host when the
   manifest asks for network (`network = true`, `network_allow_tcp` or `network_bind_tcp`) and has a network
   namespace of its own, with only the loopback interface, when it does not; the resolver socket is
   in the view only in the first case. The app
   is in a new session and ends with the runtime. Inside, `TMPDIR` and `SQLITE_TMPDIR` are `/var/tmp` (§9.4).
2. *Landlock*, as in §9, applied inside the view when there is one. With Landlock V9 (path
   sockets) the sockets of the policy get a `ResolveUnix` rule and every other path socket is denied.
3. *None*: the runtime starts the app with Landlock alone, as before the view existed, and `install`,
   `update` and `doctor` MUST say that path sockets are not blocked.

On top of the view there is an optional fourth layer, the filtered session bus (§9.5). Without it the app
has no session bus, as before.

The sockets of the policy are the `sockets_allow` entries and these, when they exist: the Wayland
socket named by `WAYLAND_DISPLAY`, the journal sockets `/run/systemd/journal/socket` and
`/run/systemd/journal/dev-log`, the resolver socket `/run/systemd/resolve/io.systemd.Resolve`, and, with
`audio = true`, `$XDG_RUNTIME_DIR/pipewire-0` and `$XDG_RUNTIME_DIR/pulse/native`.
`sockets_allow` is accepted when at least one of Landlock V9 and the view is available.
The runtime reports the active layers (`panpak doctor`, `install`, `update`). Abstract Unix sockets are
not hidden by the view; they are blocked only by the Landlock scope, which `x11 = true` turns off.
The outer runtime verifies the package; the inner one, which Landlock then restricts, receives the
decisions of the outer one and does not verify the package again (THREAT-MODEL.md §3).

### 9.3 Opening addresses [NORMATIVE for the policy; the mechanism is INFORMATIVE]

An app (or a library in it: Qt `QDesktopServices`, GIO `g_app_info_launch_default_for_uri`, Electron
`shell.openExternal`) opens a link by running `xdg-open`, or by starting the default handler found in
`mimeapps.list`, or by calling the OpenURI portal over D-Bus. In the isolated view (§9.2) the runtime
MUST make these go to one channel, and MUST NOT start a browser inside the sandbox for them:

- The stub is `xdg-open` (also `gnome-open`, `kde-open`, `kde-open5`, `kde-open6`): a link of that name
  in a folder first in `PATH` and in `BROWSER`, and mounted over every `xdg-open` found in the `PATH` of
  the outer runtime outside the home folder, so that a full path is covered too.
- GIO: the private `mimeapps.list` (marker `X-Panpak-Generated`) names a private desktop entry whose
  `Exec` is that `xdg-open`, for `x-scheme-handler/http`, `https`, `mailto` and `text/html`.
- The OpenURI portal is not reachable: the session bus is not in the view.
- The outer runtime keeps running while the app runs. For each start it creates a Unix socket
  `$XDG_RUNTIME_DIR/panpak/<id>/open.<pid>.sock` (folder 0700, file 0600; one socket per start, so
  that copies of an app do not depend on each other), mounted in the view as
  `$XDG_RUNTIME_DIR/panpak/open.sock`, and removes it at the end. Sockets of runtimes that are gone are removed.
- The socket is served by a process of its own, the receiver (§9.7, "The resident stub in two pieces"),
  which serves one connection at a time with 2 s of read and write time; the request is
  the line `OPEN <address>` of at most 2056 bytes and the reply is `OK` or `ERR <reason>`.
  The request rate is at most 5 per second with a burst of 3 (a refused request counts); the rest is refused.
- The receiver checks the address and the outer runtime (the actor) MUST accept only an address that passes all of
  these, whatever the receiver checked:
  at most 2048 bytes of printable ASCII %x21-7E without `"`, `\`, `<`, `>`, `` ` ``; the scheme
  (compared in lowercase) is `http`, `https` or `mailto`, never `file`; an `http` or `https`
  address has `//` and a host; a `mailto` address has an address part or fields, and the fields (after `?`,
  separated by `&`) are only `to`, `cc`, `bcc`, `subject` and `body`, each once, compared as written
  (a field name is not percent-decoded), with no second `?`, no `#`, no `;`; every `%` is followed by two hex
  digits; `%00` and other control codes are refused, and line breaks (`%0A`, `%0D`) only in `body`.
- It then runs the `xdg-open` found in the `PATH` of the outer runtime at start, with the address as the only
  argument, no shell, no input and no output, in its own process group, with the environment of the outer
  runtime and nothing from the app. At most 4 such processes live at once. The process is detached from the
  runtime and from the app: it leads a session of its own (`setsid`), every descriptor above 2 is closed
  before it starts, its input and output are `/dev/null`, and nobody waits for it in the request (a thread
  reaps it). It lives on when the app and the outer runtime end, so a browser it starts stays open.
  The outer runtime uses little memory while it waits (about 3 MB of resident memory, 3 threads).
- The destination is one function (a `Target`); a later version MAY replace it with the OpenURI portal
  without changing the channel or the rules.

Without the isolated view the app runs with Landlock alone and nothing intercepts `xdg-open`.

### 9.4 Copies of the same app [NORMATIVE for the policy; the mechanism is INFORMATIVE]

In the isolated view, `/tmp` is a new tmpfs of each copy with a size cap (256 MiB: it is memory, and an app that fills it
gets "no space left on device" instead of taking the memory of the machine). It is not shared. What copies of one app need
to share is in two folders, which no other app sees: `TMPDIR`, the folder `tmp` of the cache of the app
(`$XDG_CACHE_HOME/panpak/<id>/tmp`, on disk; mounted at `/var/tmp` in the view, and `TMPDIR` is that path: Chromium makes its single-instance socket in `TMPDIR`, and a Unix socket path holds 107 bytes, so a path below a long home would stop the app. Without bubblewrap there is no view: `TMPDIR` is the folder itself, a long path, and an app that makes a socket there fails when the home is over about 60 bytes), for the large and the shared temporary files (the
local sockets and lock files of Qt and Electron use `TMPDIR`), and `/dev/shm`, the folder `/dev/shm/panpak-<uid>/<id>` of the system's own `/dev/shm`
(the runtime folder if that cannot be used: it is not there, or the folder is not ours), instead of the whole `/dev/shm`. It is on the file system of the
system's `/dev/shm`, with its size (Chromium fails with a small one, as in a container with 64 MB), and not in `$XDG_RUNTIME_DIR`, which is small and holds the sockets of the session.
While the app runs the outer stub looks at `/tmp` every two seconds; when less than 2 percent is free it says so once, in the log and in a notification of the desktop, in plain words. Each runtime holds a shared `flock` on `panpak/<id>/lock` while it runs. A runtime
that gets the exclusive lock (no copy is alive) first empties both folders without following links, and the last copy to end
empties `TMPDIR` again, so a large file that an app forgot does not stay on disk. This does not make two copies see each other's System V
IPC objects: that depends on the IPC namespace (§9.2). With `x11 = true` the namespace is shared and,
for example, a Qt single-instance guard works; without it each copy has its own and both start (known limit).

Process ids start again at 1 in every view, so a lock file that stores a process id and was left by a
copy that was killed would look alive to the next copy. A runtime that holds the exclusive lock (no copy is
alive) and the last run of the app did not end normally MUST therefore remove the stale lock files of the app before it
starts it, and MUST NOT remove them while another copy is alive. The runtime makes a mark `userdata/<id>.in-use`
(beside the app folder, so the app cannot see it; on disk, because the runtime folder is gone after a shutdown) at
every start and removes it when the last copy ends normally (exit status 0, or a signal that the runtime passed on);
without the mark at the next start nothing is searched. The search is made at two moments. At the start, as above. And when a copy
ends: the runtime that stays until the app is gone MUST then try to take the exclusive lock, which fails while another copy is alive; if it gets it, it
is the last copy and MUST search for stale lock files in the private folders and remove them *before* it removes the mark, whatever way the app
ended (a Qt application that gets `SIGTERM` or exits with status 0 does not always remove its `QLockFile`, and the next start would take the file for
a live copy). It removes the mark only after a normal end; after any other end the mark stays and the next start searches too. A copy that is not the last one touches
no lock file. The cost is paid at exit (0.4 to 3.7 ms on the 15 test apps), not at start. It recognises them by content, with no list in the manifest: a link
`SingletonLock` to `<host>-<pid>` (Chromium and Electron; the links `SingletonSocket` and `SingletonCookie`
in the same folder go too), a link `lock` to `<ip>:+<pid>` (Firefox), and a regular file of at most 512 bytes
whose name contains `lock` and whose lines are a process id, an application name and a host name (Qt
`QLockFile`, for example `lockfile` or `*.lock`). Only the private folders of the app are searched (`.config`,
`.local/share`, `.local/state` of the private home and the cache folder), to a depth of 3 below each, without entering the usual cache folders
(`Cache`, `Code Cache`, `GPUCache`, `node_modules` and similar). Never `xdg_dirs`, `home_paths` or documents. Links are not followed: a
link is removed itself and never its target, and every step is relative to the descriptor of the folder.
`panpak data unlock <id>` does the same by hand and refuses while a copy is alive; `panpak doctor` points to it
when it finds such files. A lock with a process id in a format not listed here is left alone: that is the
case for `data unlock` to be extended, or for the user to remove the file. Without a usable runtime folder
there is no lock and nothing is removed.

### 9.5 The filtered session bus [NORMATIVE for the policy; the mechanism is INFORMATIVE]

Without a bus the app has no portals (appearance, inhibit), no notifications and no tray icon. The runtime
gives it a filtered bus when all of these hold: the isolated view is used; the manifest does not list the session
bus in `sockets_allow` (then the app has the whole bus and nothing is filtered); a systemd user manager is
reachable on `$XDG_RUNTIME_DIR/bus`; the session bus socket exists; and `xdg-dbus-proxy` 0.1.5 or newer is at
`/usr/bin/xdg-dbus-proxy` or `/run/current-system/sw/bin/xdg-dbus-proxy` (never from `PATH`, as for bubblewrap).
When one is missing the app gets no bus and `doctor` says which. For diagnosis, `PANPAK_DBUS_LOG=<absolute file>` in the environment of the outer runtime makes the proxy log what it forwards and filters (the app cannot set it). A failure of the user's systemd or of the proxy
that is not a missing component MUST be printed at the start.

*Identity.* The outer runtime MUST move itself into a new transient scope named
`app-panpak.<id>-<n>.scope` (`StartTransientUnit` on the user bus, property `PIDs`, slice `app.slice`), where `<id>` is
`app.id` with each `-` written `\x2d` and `<n>` is eight lower-case hex digits chosen for this start, even when it was started
from a terminal that has its own scope. The portal reads the scope of the caller and parses the id with
`^app-(?:[[:alnum:]]+\-)?(.+?)(?:\-[[:alnum:]]*)(?:\.scope|\.slice)$`, so it knows the app as `panpak.<id>` if
`panpak.<id>.desktop` exists (§3.1.5); the runtime MUST NOT let the app call `org.freedesktop.host.portal.Registry`.
The id never equals the id of a Flatpak or of the distribution entry of the same upstream app.

*Order.* The portal sees the connection of the proxy, not the app's, so the outer runtime MUST be in a scope that gives the app
its identity before the proxy is started (a proxy outside such a scope makes the app known by the id of the terminal), and
bubblewrap, the app and everything it starts MUST start in the same scope, so that systemd-oomd and the user see one unit for
the whole app. Two cases. If the user unit of the outer runtime (the one `sd_pid_get_user_unit` returns: after `user@<uid>.service`,
the first component of the cgroup that is not a slice) is a **scope** (`.scope`) whose name the portal reads as `panpak.<id>`, no scope is made. A service or a slice never qualifies, even when
the portal of the machine would read it: a portal older than 1.21 may not read services and would see the app under the identity of
the unit's launcher, so the runtime makes its scope as it does from a terminal. The portal
reads a unit name with two expressions, the first for `.scope` and `.slice` (above) and, when it does not match, the second for services,
`^app-(?:[[:alnum:]]+\-)?(.+?)(?:@[[:alnum:]]*|\-autostart)?\.service$` (xdg-desktop-portal, `shared/xdp-app-info-host.c`,
`_xdp_app_info_host_parse_app_id_from_unit_name`, called from `get_app_from_pid`), and unescapes `\xHH` in the id; the runtime applies the
same rules by hand (`panpak-core/src/unitname.rs`) and takes the shortcut only for a scope whose id it reads as exactly `panpak.<id>`. So
the names `app-gnome-panpak.<id>-<n>.scope` (GNOME) and `app-panpak.<id>-<n>.scope` qualify; `app-gnome-panpak.<id>@<n>.service`, a slice,
anything else, or a name the portal would read differently, gets the scope of the paragraph above, with no error. Otherwise the scope above is made and the start
waits for systemd before bubblewrap is started (the proxy is started at once and the view is built while systemd works).
The proxy's folder is mounted in the view, `$XDG_RUNTIME_DIR/bus` there is a link into it, and the inner runtime MUST wait
(up to 10 s, before Landlock) until a connection to the proxy's socket succeeds, so an app never connects too early.
If the scope or the proxy fails the outer runtime leaves a file `failed` in the folder, the inner runtime stops waiting, removes
the link and the app gets no bus (`DBUS_SESSION_BUS_ADDRESS=disabled:`), as when there is no layer.

*Proxy.* One `xdg-dbus-proxy` per start, started by the outer runtime in the same scope with a clean environment, filtering
(`--filter`). Its socket is in a private folder (0700) of the runtime folder and is mounted in the view as
`$XDG_RUNTIME_DIR/bus`; `DBUS_SESSION_BUS_ADDRESS` points there. With Landlock V9 the socket has a `ResolveUnix` rule. The proxy ends when the
outer runtime ends (the runtime holds the read end of the pipe given with `--fd`, and the proxy is killed
when the runtime stops waiting for the app). The runtime also sets `GTK_USE_PORTAL=1`, and
`QT_QPA_PLATFORMTHEME=xdgdesktopportal` unless the manifest sets it in `[launch.env]`.

*Rules.* The portal is reached only through `--call` rules for named interfaces, never `--talk`:
`org.freedesktop.portal.Settings.*` and the signal `SettingChanged`, `org.freedesktop.portal.Inhibit.*` and its `StateChanged`,
`Request.*` with the signal `Response`, and `Session.*` with the signal `Closed`, each on its own object path
(`/org/freedesktop/portal/desktop`, `.../request/*`, `.../session/*`). The app may own and talk to its own `app.id`
(single instance across copies; since step 13p on its private bus). `notifications` and `tray` add the rules of §3.1.5.
Every other interface, method and name is refused, including `org.freedesktop.DBus.Properties`, `Introspectable`, every other portal interface
(Location, Background, Screenshot, ScreenCast, RemoteDesktop, Camera, Secret, Account, FileChooser, OpenURI and
the rest), `org.freedesktop.portal.Documents`, `org.freedesktop.secrets` and `org.freedesktop.systemd1`. The rules MUST
come from one function that the tests also use (`panpak dbus-rules <package>` prints them).

*The private bus (step 13p) [NORMATIVE for the policy].* The names of the app (its `app.id` and any name it takes) MUST NOT be on the host bus.
The app has a bus of its own: one router process per app, shared by all copies of that app, started by the first copy and ended after the last one
(1.5 s after the last copy detached; 10 s to wait for a first copy). Each copy has its own `xdg-dbus-proxy` in front of the host bus, as above, whose
listening socket is NOT in the view; the app's socket (`$XDG_RUNTIME_DIR/bus`) is the router's, which reads only the headers of the messages and passes each
one on unchanged to the proxy of the copy, or answers it from the state it keeps:

- `RequestName`, `ReleaseName`, `GetNameOwner`, `NameHasOwner`, `ListQueuedOwners`, `StartServiceByName`, `AddMatch`, `RemoveMatch` for names that are not the host's;
  delivery of calls, replies and signals between the connections of all copies (a second launch finds the first by the app's id);
- the identity: after `Hello` the router calls `org.freedesktop.host.portal.Registry.Register("panpak.<id>", {})` on that connection, as the first call, and
  keeps the reply from the app. A call of the app to that interface MUST be refused (`AccessDenied`). The portal accepts the identity only when
  `panpak.<id>.desktop` exists (portal 1.21 and later);
- `org.freedesktop.portal.OpenURI` goes to the address-opening channel (§9.3) and the router builds the method return and the `Response` (code 0 when the
  address was opened, 2 when it was refused or could not be opened, with the reason on the channel);
- with `permissions.gvfs`, the proxy of the host bus lets the names `org.gtk.vfs` and `org.gtk.vfs.*` through (`--talk=org.gtk.vfs.*`), and nothing else of GVfs is added; the router forwards the app's calls to them as it does for any host name that the proxy allows;
- with `permissions.mpris`, one alias on the host bus, taken with `DO_NOT_QUEUE`; the rules of the proxy allow `--own` for that one name only.

The router MUST NOT have privileges: no new privileges, no dump, `RLIMIT_NPROC` 0, Landlock with no rule, a seccomp allow list of the calls it needs
(sockets, poll, read, write, memory, exit), limits on messages (16 MiB), names (32 per connection), rules (64) and open requests (8). It does not open or connect
a path: the stub of each copy passes it the descriptors. The parser of the router is fuzzed (`fuzz_router`).

*Accessibility.* The runtime asks the host for the address of the accessibility bus (`org.a11y.Bus.GetAddress`) and starts a second proxy for it with
the rules of Flatpak's `flatpak_run_add_a11y_dbus_env`, without `NotifyListenersSync` and `NotifyListenersAsync` (the app registers and is read by the screen reader but
cannot read or command other applications); `AT_SPI_BUS_ADDRESS` points to it. If it cannot be made the variable is removed.

*Rules, since 13p.* The proxy rules no longer include `--own` or `--talk` for the app's own names. They add `--call` for
`org.freedesktop.host.portal.Registry.Register` (for the router only). `org.freedesktop.DBus.Properties` on the portal stays refused until the
file chooser (KL-7, docs/FILE-CHOOSER.md).

*Documents.* When `$XDG_RUNTIME_DIR/doc/by-app` exists the runtime mounts `$XDG_RUNTIME_DIR/doc/by-app/panpak.<id>` in the
view as `$XDG_RUNTIME_DIR/doc`, from the start, read-write. The Landlock rule for the runtime folder covers
documents added later. The app never gets the document portal on the bus and never gets another folder of `doc`: the outer runtime
grants a file with `Documents.AddFull` for the app id `panpak.<id>`.

The runtime reports the state of this layer in `doctor`.

### 9.6 Listing and removing apps [NORMATIVE for the policy; the mechanism is INFORMATIVE]

`panpak list [--json]` shows every app of the store: id, name and version of the installed package, the disk space of
its packages and unpacked trees, the space of its settings, data and cache, and a status (`ok`, `incomplete` with the
missing parts, or `data-only` for an id that has data and no installation). A tree shared with another app is counted
for both.

`panpak remove [--purge-data] [--yes] <app-id>` removes an installed app. The runtime MUST:

1. refuse while a copy of the app is alive (the lock of §9.4 is held), with `PANPAK-E2002` and no option to force it.
   It holds the exclusive lock until it ends. A start of an installed app MUST take the shared lock before it opens
   anything of `apps/<id>` and, once it has it, check that the app is still installed; if not it ends with `PANPAK-E2004`;
2. remove the launcher, the desktop entries `panpak.<id>.desktop` and `<id>.desktop` only when they carry the
   marker of §3.1.5 for this app, and the icon files `panpak.<id>.png` of the app (regular files only), and leave anything else (a file of the user, a link) alone and say so; then refresh the
   desktop database;
3. move `apps/<id>` into `removing/<id>.<n>/app` with one rename, after writing a journal (`panpak-removal 1`, the
   app id, the package file names of the app and the tree roots it used) beside it. From the rename on, an interrupted
   removal is finished by the next `panpak` command that uses the store, from the journal alone. A journal is checked
   name by name before anything is deleted: package names are plain file names ending in `.panpak`, roots are 64
   lower-case hex digits;
4. delete the packages of the app (those whose manifest has the id) and the trees and farms of its roots, except the
   roots that a package left in the store still uses;
5. delete the shared `/dev/shm` of the app and the folder for large temporary files (§9.4) and the mark "in use";
6. keep the settings, data and cache unless `--purge-data` is given, and print where they are and what they weigh. With
   `--purge-data` it MUST ask for a yes (or take `--yes`) before any removal. A data or cache folder that is a link or
   a file is not followed and not removed: the exit status is `PANPAK-E2003` and the message names it.

*Menu action.* The desktop entry of an app has the action `Uninstall` (`Actions=Uninstall;` and a group
`[Desktop Action Uninstall]`, with at least the names in English and Italian) whose `Exec` is `<store>/bin/panpak remove --ask <id>` (the stable copy of §6, never the path of the `panpak` that
happened to run `install`). If that file does not exist when the entry is written, the entry has no action.
`panpak remove --ask <id>` asks in a dialog (`zenity` or `kdialog`, from fixed folders, never from `PATH`) with the name of the
app, what is removed, that the settings and data are kept (with their size) and that a new install finds them; no command and no folder path (they are in the output of `panpak remove`); the texts are in the language of the system
(`po/<language>.po`, §3.1.4 of the translations). Only a yes starts the removal, which never deletes data. A `remove` that finds itself in the scope `app-*panpak.<id>-*.scope` of the app (the scope that the shell makes for the
action) first moves into a scope of its own, because the document portal takes the caller of such a scope for the app and refuses it to revoke the app's documents. It ends with a notification (`notify-send` or `gdbus`, if present). Without a window tool the question is a notification with two buttons (§6.10) that waits 5 minutes and is then withdrawn; with no answer nothing is removed. With neither it removes nothing, explains the
terminal command by notification and ends with `PANPAK-E5004`. `--ask` takes no other option.

The runtime and the shared blobs and chunks stay. Every step below the store, the data, the cache and the runtime
folder uses descriptors of folders and `O_NOFOLLOW`; a link met inside is removed itself and its target is never
entered; a walk does not cross to another file system and stops at 512 levels, leaving the rest and saying so.
`panpak remove` of an id that was half removed by hand (no launcher, no entry, no package) finishes the work without
error; an id with nothing at all is `PANPAK-E2004` ("not installed: nothing to remove"), and so is an id with only data left when
`--purge-data` is not given (the message names the data and the command that deletes it). A removal that succeeds prints
`removed <id> <version>: freed <size>`, where the size is the packages, the trees that no other app's package uses and, with
`--purge-data`, the data and cache; the shared blobs are not counted. It then lists what went (launcher, entry, icons, store
folder), what was kept and where the data is. Not done yet (step 8): the removal of unused entries of the resolution cache and of blobs and chunks that no tree
uses (garbage collection of the store). `panpak remove` does not touch the permission store or the
document portal of the desktop (see THREAT-MODEL.md, "Removal").

### 9.7 The resident stub in two pieces [NORMATIVE for the policy and the frames; the mechanism is INFORMATIVE]

The outer runtime is the one part of a start that stays alive and can run programs as the user, and it
used to read the bytes of the app itself. It is now two pieces:

- The **receiver** is a process of its own: the stub executed again with `--panpak-receiver`, with the
  listening socket on descriptor 3 and a socket to the actor on descriptor 4 and nothing else (every other
  descriptor is closed at the `exec`). Before it reads anything it MUST: set `PR_SET_DUMPABLE` to 0 and the
  parent-death signal to `SIGKILL`; set `RLIMIT_NOFILE` to 32, `RLIMIT_AS` to 128 MiB, `RLIMIT_CPU` to 600 s,
  `RLIMIT_CORE` and `RLIMIT_NPROC` to 0; apply a Landlock ruleset that handles every file system right, every
  network right and every scope the kernel has, with **no rule** (no path, no port); check that a file cannot be
  opened, a folder cannot be listed and (V4 and later) a TCP connection is refused; set `no_new_privs`; install a
  seccomp **allow list** (`accept4`, `poll`, `recvfrom`, `sendto`, `close`, `setsockopt`, `clock_gettime`,
  `mmap`, `munmap`, `mremap`, `brk`, `madvise`, `rt_sigprocmask`, `read`, `write`, `exit`, `exit_group`) under
  which any other call, any other architecture and x32 end the process. If any of this fails the receiver ends and the
  channel is off. It ends when the actor closes its side. It reads the request line, applies the shape check, the address check
  and the rate limit of §9.3, and passes a valid address on as a frame.
- The **actor** is the outer runtime. It reads frames from the receiver, checks each one again as if the
  receiver could not be trusted (the address check and its own rate limit), and acts. It never sees the app's bytes.
  A frame that is not a frame (bad magic or version, a length over the limit) ends the conversation.

A request frame is `"PKRQ"`, a version byte (1), a kind byte, a 16-bit length in big-endian order (at most 4096, 24580 for kind 2) and
that many bytes. Kind 1 is "open an address" (the payload is the address). Kind 2 is "show a file dialog": the payload is a token of
the router (4 bytes) and the call of the app (`OpenFile`, `SaveFile` or `SaveFiles` of `org.freedesktop.portal.FileChooser`) as a D-Bus message of at
most 24 KiB; the actor parses it again and keeps only the options it knows (docs/FILE-CHOOSER.md). Kind 3 (browser for a sign-in) is reserved and
answered `unsupported` until it is defined; every other kind is `unsupported`, and so is a kind 2 whose payload has no token. A response frame
is `"PKRS"`, the version, a status byte (0 done, 1 refused, 2 failed, 3 unsupported, 4 file dialog), a 16-bit length (at most 256, and at most
60000 for status 4) and printable ASCII text. The actor answers each request of kind 1 with one response, in order. The answer to a file dialog is
status 4, sent when the user has chosen (so out of order), and its text is `<token> <code> [<hex>]`: the token of the request, the code of the
`Response` (0 done, 1 the user closed the dialog, 2 anything else; the router tells the app 1 with `uris` = an empty list for every code but 0) and, for code 0, the `a{sv}` of the answer in D-Bus encoding as hexadecimal
digits (the document addresses, and `choices` and `current_filter` when the portal gave them). The router checks the dictionary again. No frame carries a descriptor.

### 9.8 System calls (seccomp) [NORMATIVE for the policy; the program is INFORMATIVE]

The inner runtime (and the runtime itself without the view) MUST install a seccomp filter in the last step before it
executes the app, after Landlock, with `no_new_privs` set, on all threads. Nothing runs between the filter and the `exec`.
The filter is a deny list: every system call passes except the ones below, which return an error number; the app is not killed.

- A call with an architecture other than the native one (the 32-bit ABIs) returns `EPERM`, and on x86-64 so does a number
  with the x32 bit (`0x40000000`).
- `clone`, `unshare` and `clone3`: a user namespace, or any other new namespace, cannot be created. `clone` and `unshare`
  return `EPERM` when the flags ask for one. `clone3` returns `ENOSYS` without condition (its flags are in memory that a
  filter cannot read); glibc and musl fall back to `clone` only on `ENOSYS`, so threads and `fork` keep working.
- `ENOSYS` is also the answer for `io_uring` and for the new mount API, so that a program that probes for them goes on
  without them.
- `ioctl` returns `EPERM` for `TIOCSTI` and `TIOCLINUX` (input injection into a terminal); `personality` returns `EPERM`
  unless the value is 0, 8, `0x20000`, `0x20008` or `0xffffffff` (the query).
- `socket` and `socketpair` return `EAFNOSUPPORT` unless the family (the first argument, as the kernel reads it, an int) is `AF_UNIX`, `AF_INET`, `AF_INET6` or
  `AF_NETLINK`, and for `AF_NETLINK` the protocol (the third argument, an int) must be `NETLINK_ROUTE` (the list of interfaces and routes that `getifaddrs` reads), or `NETLINK_KOBJECT_UEVENT` when the manifest says `permissions.devices = true`; otherwise `EPROTONOSUPPORT`. The other families (vsock, alg, bluetooth, packet, key, xdp, nfc, tipc, rds, can, ...) are a large kernel surface that no app of the desktop needs, and
  a program that probes for one is told that it is not supported. This is the one place where the filter is a list of what passes and not of what is refused.
- Calls that are left open on purpose: `seccomp` (Chromium and Firefox add their own filters), `landlock_*`,
  `memfd_create`, `modify_ldt` (Wine).

| Call | Returns | Why |
|---|---|---|
| `io_uring_setup` | ENOSYS | io_uring: a large, fast-changing kernel surface that bypasses the other filters of file and network calls |
| `io_uring_enter` | ENOSYS | io_uring |
| `io_uring_register` | ENOSYS | io_uring |
| `bpf` | EPERM | loads programs into the kernel |
| `userfaultfd` | EPERM | lets an attacker stop the kernel in the middle of a copy; a tool of race exploits |
| `perf_event_open` | EPERM | a large surface and a source of side-channel data |
| `keyctl` | EPERM | the kernel key ring is shared by processes of the user, outside the file system |
| `add_key` | EPERM | kernel key ring |
| `request_key` | EPERM | kernel key ring |
| `ptrace` | EPERM | reads and writes the memory of other processes of the user |
| `process_vm_readv` | EPERM | reads the memory of another process |
| `process_vm_writev` | EPERM | writes the memory of another process |
| `pidfd_getfd` | EPERM | takes a file descriptor out of another process |
| `kcmp` | EPERM | compares the descriptors of other processes (an information leak between processes) |
| `kexec_load` | EPERM | replaces the kernel |
| `kexec_file_load` | EPERM | replaces the kernel |
| `init_module` | EPERM | loads kernel code |
| `finit_module` | EPERM | loads kernel code |
| `delete_module` | EPERM | unloads kernel code |
| `syslog` | EPERM | reads the kernel log (addresses and hardware data) |
| `acct` | EPERM | process accounting writes a file as root |
| `swapon` | EPERM | machine-wide |
| `swapoff` | EPERM | machine-wide |
| `reboot` | EPERM | machine-wide |
| `mount` | EPERM | the view of the app is fixed before it starts |
| `umount2` | EPERM | mounts |
| `pivot_root` | EPERM | mounts |
| `chroot` | EPERM | a way to leave the paths that the view sets |
| `fsopen` | ENOSYS | new mount API |
| `fsconfig` | ENOSYS | new mount API |
| `fsmount` | ENOSYS | new mount API |
| `fspick` | ENOSYS | new mount API |
| `move_mount` | ENOSYS | new mount API |
| `open_tree` | ENOSYS | new mount API (also clones a mount tree) |
| `mount_setattr` | ENOSYS | new mount API |
| `setns` | EPERM | enters the namespaces of another process |
| `unshare` | EPERM when a new namespace is asked | a new user namespace gives a program the capabilities of its own that open the rest of the kernel |
| `clone` | EPERM when a new namespace is asked | the same, through clone |
| `clone3` | ENOSYS | its flags are in memory that a filter cannot read; ENOSYS makes the C library use clone, which is filtered |
| `open_by_handle_at` | EPERM | opens a file by a handle, outside every path rule |
| `ioctl` | EPERM for `TIOCSTI`, `TIOCLINUX` | TIOCSTI and TIOCLINUX put keystrokes into the terminal of the user |
| `socket` | EAFNOSUPPORT unless the family is local, IPv4, IPv6 or netlink (EPROTONOSUPPORT unless NETLINK_ROUTE, or NETLINK_KOBJECT_UEVENT with `permissions.devices`) | only the families that programs use: local, IPv4, IPv6, netlink; the others are kernel surface that nothing of the desktop needs |
| `socketpair` | EAFNOSUPPORT unless the family is local, IPv4, IPv6 or netlink | the same family list |
| `personality` | EPERM unless the value is a plain Linux one | READ_IMPLIES_EXEC, no randomisation, page zero: they weaken the memory protections |
| `iopl` | EPERM | direct access to the hardware ports |
| `ioperm` | EPERM | direct access to the hardware ports |
| `uselib` | EPERM | obsolete: loads a library by a path, outside the loader |
| `ustat` | EPERM | obsolete |
| `sysfs` | EPERM | obsolete |
| `_sysctl` | EPERM | obsolete and removed from the kernel |
| `nfsservctl` | EPERM | obsolete and removed from the kernel |

Without kernel support for seccomp filters, or on an architecture the filter does not cover, the start MUST fail
(`PANPAK-E3002`). A runtime MUST NOT offer a way to turn the filter off.

### 9.9 System caches and the Wayland socket [NORMATIVE for the policy; the mechanism is INFORMATIVE]

- `/var/cache/fontconfig`, if the host has it, is mounted read-only into the view and readable by Landlock. The view
  has an empty `/var`, and without the system font cache the first start of an app (whose private cache is empty)
  scans every font file.
- **Wayland security context.** If the app has a Wayland display (`WAYLAND_DISPLAY`) and the compositor offers
  `wp_security_context_manager_v1`, the outer runtime MUST register a context before the view is made: it binds a Unix socket
  `$XDG_RUNTIME_DIR/panpak/<id>/wayland-<pid>-<nanoseconds>` (the folder is 0700, the file 0600) and listens on it, makes a
  pipe, and sends `create_listener` with the socket and the read end of the pipe, `set_sandbox_engine("io.github.panpak")`,
  `set_app_id("panpak.<id>")`, `set_instance_id("<pid>-<nanoseconds>")` and `commit`, then waits for a round trip to learn of a
  protocol error. The new socket is mounted in the view at the path of the compositor's own socket, which is not mounted.
  The write end of the pipe is held by the outer runtime, which stays until the app ends; the socket file is removed then.
  Everything read from the compositor has a limit: 4096 bytes a message, 256 KiB in total, 1 s for the whole exchange.
- If the compositor does not offer the protocol (GNOME's Mutter does not, in the versions checked), the app gets the
  compositor's own socket as before, and `panpak doctor` says so. If it offers the protocol and the exchange fails,
  the app gets the normal socket and the runtime prints a warning.
- The audio permission gives the PipeWire and PulseAudio sockets of the session. Both servers
  treat playing and recording as the same client, so an app with `audio = true` can record from the microphone.
  `install` says so.

## Appendix A — test vectors [NORMATIVE]

Minimal valid package (1 file `bin/hi` = `hi\n`, 3 bytes; fake stub of
21 bytes `PANPAKTEST-VECTOR-v1\0`; fixed test key — NEVER use in
production). Verified by both the independent verifier
(`conformance/verify.py`) and the Rust side (`panpak doctor`): both
MUST accept it and report the same values.

- total size: 987 bytes
- stub `[0, 21)`: `50414e50414b544553542d564543544f522d763100`
  (`PANPAKTEST-VECTOR-v1\0`)
- chunk 0 `[21, 37)`, zstd level 3 of `hi\n`:
  `28b52ffd045819000068690a343d5092` (16 compressed bytes, 3 raw)
- compressed index `[37, 136)` (99 bytes):
  `28b52ffd0458b502008404010000000b8b60248fad7ac6dfac221b7e01a8b91c772421a15b387dd1fb2d6a94aee438150010000000030000000262696e01ed010000060062696e2f686900ed010000000001000400b8520a3561e54a0f800ce9caefff`
- raw index (98 bytes, whose BLAKE3 is the root_hash below):
  `010000000b8b60248fad7ac6dfac221b7e01a8b91c772421a15b387dd1fb2d6a94aee4381500000000000000100000000300000002000000030062696e01ed010000060062696e2f686900ed01000003000000000000000100000000000000000000`
  (n_chunks=1: hash=`0b8b6024…f7e01`, off=21, comp=16, raw=3;
  n_entries=2: dir `bin`, file `bin/hi` mode 0755 size 3 chunk [0], flags 0, no extension area)
- root_hash = `BLAKE3(raw index)` =
  `5c0359291daee4146c9b3046df3d012672fe3c832e59aab5f9d8580c33d6b0f6`
- manifest TOML `[136, 835)` (699 bytes):
  `[app]\nid = "org.example.tv"\nname = "TV"\nversion = "1.0"\nentry = "bin/hi"\n[format]\nmin_panpak = 1\ncritical = ["release.channel", "release.floor", "release.revision", "release.security", "runtime.stub_hash", "trust.recovery", "trust.root", "trust.threshold"]\ncompression = "zstd"\nchunking = "fastcdc:16384:65536:262144"\npacked_by = "conformance generator"\n[release]\nrevision = 1\nfloor = 0\nsecurity = 0\nchannel = "stable"\n[runtime]\nstub_hash = "503da793bbca6afccdd883c02e32c139ef3a589894325916eddc2bf67b87520c"\n[trust]\nroot = ["ed25519:783e43d53f1193914442b4346eed8e69d537a4786ee74af8c878230faa875c72"]\nthreshold = 1\nrecovery = "blake3:a6595f111fb45d19c482a93774d53e0d33464174135b8d3f62c70a0b295ee465"\n`
  (the key is the public half of the fixed test key of `conformance/gen.py`; `critical` names `runtime.stub_hash` and the keys of `[release]` and `[trust]` that the manifest uses, because `[runtime]`, `[release]` and `[trust]` are critical sections, §3.1.9; the recovery commitment is the BLAKE3 hash of the public key of a second fixed test key)
- signature block `[835, 907)` (72 bytes: one entry, type 1, critical, 64 bytes of
  Ed25519 over the labeled message of §3.0, that is
  `"panpak/package/v1\0" || 01000000 || 00000000 || root_hash || manifest_toml`):
  `01000100400000003a0062d15d117a7163e4348eb4d4ad6a5b4c6ef081a0b78aa27ac94587001e47c3a5eed0eac09e63c9bd5126d01ecc028062bfa8eae013da23b3bf75ce19a108`
- footer `[907, 987)` (80 bytes):
  `50414e50000100000100000000000000250000000000000063000000000000008800000000000000bb020000000000005c0359291daee4146c9b3046df3d012672fe3c832e59aab5f9d8580c33d6b0f6`
  (magic `PANP\0\1\0\0`, version=1, flags=0, index_off=37, index_len=99,
  manifest_off=136, manifest_len=699, root_hash=see above)

Invalid vectors: the conformance suite (`conformance/fixtures/`,
wrong magic/version/flags, incoherent layout, tampered
signature/sizes/root/chunk, a signature over the old unlabeled message, signature blocks (an unknown
entry that is critical or not, two signatures, no signature, flags that are not defined, an extra byte,
over 64 KiB, the old bare 64 bytes), non-canonical paths, plus `unknown-magic.bin`:
a file whose magic is not exactly the one defined in this section MUST be rejected) with expected
outcome in `conformance/fixtures/expected.json`.

## Appendix B — conformance file index [INFORMATIVE]

- `conformance/verify.py`: independent verifier (pure BLAKE3 in
  `conformance/b3.py`, Ed25519 via `cryptography`, zstd via CLI).
  Reads only this specification.
- `conformance/mkpanpak.py`: package builder for tests.
- `conformance/gen.py`: generates `conformance/fixtures/*.panpak` +
  `expected.json` (valid and invalid manifests and files, each with its expected error, including the legacy one with old magic and the fixtures for `xdg_dirs` and `desktop.schemes`).
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
(`VERNEED`) is used only to choose between two copies of `libstdc++.so.6` or
`libgcc_s.so.1` (§7).

## Appendix D — licensing, embedded licenses and patents [INFORMATIVE]

**Embedded license texts.** `panpak pack` adds NOTICE and the license texts
(`LICENSE-MIT`, `LICENSE-APACHE`, `THIRD-PARTY-LICENSES`) to every package as
ordinary regular files under `.panpak/licenses/`, because the runtime stub is
redistributed inside third-party packages. This uses the normal tree and index
rules of §4 and changes nothing in the file format, the manifest or the
verification rules. The top-level name `.panpak` is reserved for what `pack` generates: the license texts in `.panpak/licenses/` and the cleaned
icons in `.panpak/icons/<side>.png` (§3.1.3). Only `pack` writes there; a tree that already contains `.panpak` is refused, and `convert`
never writes it either. Readers show the texts with
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
cover use of the name "Panpak" (see `TRADEMARKS.md`).
