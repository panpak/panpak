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
notifications = false           # opt-in, §3.1.5: desktop notifications
tray = false                   # opt-in, §3.1.5: a tray icon
dbus_own = []                  # §3.1.5: extra names the app may own on the session bus (at most 8)

[[permissions.home_paths]]     # last resort, §3.1.2: a fixed path in the home folder
path = ".config/editor"        # relative to the home, no ".."
access = "rw"                  # "ro" or "rw"

[[permissions.xdg_dirs]]       # §3.1.4: a folder of user-dirs.dirs, found at install
name = "music"                 # music, documents, pictures, videos or download
access = "ro"                  # "ro" or "rw"

[desktop]                      # §3.1.4
schemes = ["myapp"]            # URL schemes the app opens (x-scheme-handler/<scheme>)
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
| `permissions.xdg_dirs` | At most 5 items, each `name` once, see §3.1.4 |
| `desktop.schemes` | At most 8 items, see §3.1.4 |
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
sandbox, in `$XDG_CONFIG_HOME/user-dirs.dirs` (`~/.config/user-dirs.dirs`). The parser is rigid.
The file is at most 16 KiB; a blank line, a line that starts with `#`, or a line
`XDG_<KEY>_DIR="<value>"` with `<KEY>` one of `DESKTOP`, `DOWNLOAD`, `TEMPLATES`, `PUBLICSHARE`,
`DOCUMENTS`, `MUSIC`, `PICTURES`, `VIDEOS` (each at most once) is accepted, at most 600 bytes
per line, with no control character; any other line makes the whole file unusable. `<value>` is
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
(§9.2) the home folder can be listed (Landlock `ReadDir` on it) and holds only the granted
folders and the parents of the private folders; without the view the listing right is never given.

**`desktop.schemes`.** URL schemes the app registers. Each is 1 to 32 bytes, starts with a
lowercase letter and has only lowercase letters, digits, `+`, `.` and `-`. These are refused:
`http`, `https`, `file`, `ftp`, `ftps`, `sftp`, `ssh`, `mailto`, `javascript`, `data`,
`about`, `blob`, `ws`, `wss`, `chrome`, `view-source`, `vbscript`, `ms-msdt`,
`search-ms`, `x-scheme-handler`. `install` MUST show the schemes, and writes them into the
desktop entry as `MimeType=x-scheme-handler/<scheme>;` with `Exec=` the launcher and `%U` (the
address arrives as an argument of the launcher). It then runs `update-desktop-database` on the
folder of the entry, when the tool exists at `/usr/bin` or `/bin`; a failure is only a note.

### 3.1.5 Bus permissions and the window class [NORMATIVE]

These fields are used only when the filtered session bus (§9.5) is active.

**`permissions.notifications`** (boolean). Lets the app show desktop notifications: the notification
portal (`org.freedesktop.portal.Notification`) and the methods `Notify`, `CloseNotification`,
`GetCapabilities` and `GetServerInformation` of `org.freedesktop.Notifications`. The signals of that
older service (`ActionInvoked`, `NotificationClosed`) are broadcasts that every client on the bus
can see; the runtime MUST NOT forward them (measured on GNOME Shell: `NotificationClosed` is sent with no destination). Actions
are delivered only by the portal.

**`permissions.tray`** (boolean). Lets the app put an icon in the notification area: it may talk to
`org.kde.StatusNotifierWatcher` and own the item names `org.freedesktop.StatusNotifierItem-2-N` and
`org.kde.StatusNotifierItem-2-N` for N from 1 to 3. 2 is the process id of the app in the view (§9.2).
The runtime MUST NOT own or let the app own any other name for this permission, and MUST NOT use a wildcard.

**`permissions.dbus_own`** (list of strings, at most 8, each at most once). Names the app may own
on the session bus besides its own `app.id`. Each is a D-Bus well-known name of at least three
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

**`desktop.wm_class`** (string, 1 to 100 bytes of `A-Z a-z 0-9 . _ + -`). The class the app sets on its windows
(Wayland `app_id` or X11 `WM_CLASS`). `install` and `update` write it as `StartupWMClass=` in the desktop entry,
which is how the desktop groups the windows under the entry. Without it the entry has no `StartupWMClass`.

The desktop entry is `panpak.<app.id>.desktop` (not `<app.id>.desktop`): the portals know the app as `panpak.<app.id>` (§9.5)
and look for the entry of that name, and the prefix keeps the entry apart from the Flatpak or distribution entry of an
app that has the same upstream id. `install` and `update` MUST remove `<app.id>.desktop` when panpak wrote it
(it has the `X-Panpak-App-Id` marker or the `Exec` line panpak writes) and MUST leave it when it did not.

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
  apps/<id>/xdg-dirs        # user folders fixed at install (§3.1.4)
```

Several starts can overlap on a store that does not have the tree yet. The runtime therefore takes an
exclusive `flock` on `trees/<root_hash>.lock` before it builds the tree, checks again whether the tree is
complete once it has the lock (a start that waited then just uses it), builds in `trees/<root_hash>.partial`
(a folder left by a start that was killed is removed first) and publishes it with one rename. The lock goes
with the process, so a builder that dies never blocks the others. Temporary blob files carry the process
number, since two trees can write the same blob at the same time.

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
     `libdrm_*`, `libglapi`, `libxcb-dri2`, `libxcb-dri3`, `libnvidia-*`).
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
     of an `allow_missing` library do not count. If there are some, it moves to
     its other copy (bundle to host, or host to bundle) every library whose other
     copy defines one of them, together with the rest of its atomic family (only
     if every member has that other copy), and checks again. A step counts only if
     it lowers the number of undefined symbols; otherwise it is undone and those
     libraries are not tried again. It stops when nothing is undefined, when no
     library can move, or after 8 steps. It never moves a library of exception 1,
     `libstdc++` or `libgcc_s` (exception 2), a library named in `prefer_host`
     or `allow_missing`, nor anything in world B. If symbols remain undefined,
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

NOTE v1: symbol-version comparison (`VERNEED` vs host) decides only between
the two copies of `libstdc++.so.6` and `libgcc_s.so.1`. For every other library
the §7 resolution applies at equal SONAME, and a possible incompatibility fails
the loader explicitly (fail-closed). `panpak doctor` reads the ELF files of an
installed app and reports the undefined symbols that none of the chosen libraries
defines, after the symbol check above. See also Appendix C.

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
  CLI (never from the manifest), with a warning at every start. Landlock
  never filters UDP; in the isolated view (§9.2) an app that asks for no network has no
  network but the loopback interface.
- Landlock ABI: highest supported detected with HardRequirement (kernel 7.0:
  V8; kernel 7.2.7: V9). Minimum: V1 (kernel 5.13).
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
   and MUST NOT use it when it is writable by others. The view has: a new tmpfs as `/`, made read-only,
   with only these of the host put in it read-only: `/usr`, `/etc`, `/sys` (the folders Landlock lets the app read), `/nix/store` and
   `/gnu/store` when they exist, and the symbolic links at the top of the host's `/` (`/bin`, `/lib`, `/lib64`, and
   `/home` where the homes are in `/var/home`). Nothing else of the host's `/` is in the view: not `/var`
   (a new, empty tmpfs that holds only the links `/var/run` and `/var/lock` to the new `/run`), `/opt`, `/srv`,
   `/mnt`, `/media`, `/root`, `/boot`, `/snap`, nor the part of `/nix` and `/gnu` that is not the store, because a
   folder of the host can hold a socket that a Landlock below V9 does not stop the app from using. A path
   that the runtime must give to the app (a granted folder, an opened file) is mounted at its own place wherever it is. `/dev` as
   it is; a new `/proc`; a new tmpfs for `/run`; `/tmp` and `/dev/shm` as in §9.4, or a new tmpfs for `/tmp` when the runtime folder is not usable; an empty tmpfs on `$HOME` (read-only once the
   binds are made) where only these are put back: the private folders of §9.1 and the `home_paths` as declared,
   the read-only settings listed in §9.1 that exist, the Xauthority file with `x11 = true`, a file opened with
   `panpak open` as granted, and from the store the tree, the library folder `farms/<root>`, the loader and
   the stub (read-only); a new tmpfs for `$XDG_RUNTIME_DIR`
   with only the sockets of the policy below; the folder in `/run` that holds the target of `/etc/resolv.conf` read-only (name resolution);
   the X11 socket of `$DISPLAY` only with `x11 = true`. The view uses its own user, PID and mount
   namespaces, and its own IPC namespace unless `x11 = true`. It shares the network namespace of the host when the
   manifest asks for network (`network = true`, `network_allow_tcp` or `network_bind_tcp`) and has a network
   namespace of its own, with only the loopback interface, when it does not; the resolver socket is
   in the view only in the first case. The app
   is in a new session and ends with the runtime. Inside, `TMPDIR` and `SQLITE_TMPDIR` are `/tmp`.
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
- The outer runtime serves one connection at a time with 2 s of read and write time; the request is
  the line `OPEN <address>` of at most 2056 bytes and the reply is `OK` or `ERR <reason>`.
  The request rate is at most 5 per second with a burst of 3 (a refused request counts); the rest is refused.
- The outer runtime MUST accept only an address that passes all of these, whatever the client checked:
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

In the isolated view, `/tmp` is the folder `$XDG_RUNTIME_DIR/panpak/<id>/tmp` (0700) and `/dev/shm` is
`$XDG_RUNTIME_DIR/panpak/<id>/shm`, instead of a new tmpfs and the `/dev/shm` of the machine. Copies of one app
that run at the same time share them and no other app sees them. Each runtime holds a shared `flock` on
`panpak/<id>/lock` while it runs. A runtime that gets the exclusive lock (no copy is alive) first empties
both folders without following links. This does not make two copies see each other's System V
IPC objects: that depends on the IPC namespace (§9.2). With `x11 = true` the namespace is shared and,
for example, a Qt single-instance guard works; without it each copy has its own and both start (known limit).

Process ids start again at 1 in every view, so a lock file that stores a process id and was left by a
copy that was killed would look alive to the next copy. A runtime that holds the exclusive lock (no copy is
alive) and the last run of the app did not end normally MUST therefore remove the stale lock files of the app before it
starts it, and MUST NOT remove them while another copy is alive. The runtime makes a mark `userdata/<id>.in-use`
(beside the app folder, so the app cannot see it; on disk, because the runtime folder is gone after a shutdown) at
every start and removes it when the last copy ends normally (exit status 0, or a signal that the runtime passed on);
without the mark at the next start nothing is searched. It recognises them by content, with no list in the manifest: a link
`SingletonLock` to `<host>-<pid>` (Chromium and Electron; the links `SingletonSocket` and `SingletonCookie`
in the same folder go too), a link `lock` to `<ip>:+<pid>` (Firefox), and a regular file of at most 512 bytes
whose name contains `lock` and whose lines are a process id, an application name and a host name (Qt
`QLockFile`, for example `lockfile` or `*.lock`). Only the private folders of the app are searched (`config`,
`data`, `state` and the cache folder), to a depth of 3 below each, without entering the usual cache folders
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
When one is missing the app gets no bus and `doctor` says which. A failure of the user's systemd or of the proxy
that is not a missing component MUST be printed at the start.

*Identity.* The outer runtime MUST move itself into a new transient scope named
`app-panpak.<id>-<n>.scope` (`StartTransientUnit` on the user bus, property `PIDs`, slice `app.slice`), where `<id>` is
`app.id` with each `-` written `\x2d` and `<n>` is eight lower-case hex digits chosen for this start, even when it was started
from a terminal that has its own scope. The portal reads the scope of the caller and parses the id with
`^app-(?:[[:alnum:]]+\-)?(.+?)(?:\-[[:alnum:]]*)(?:\.scope|\.slice)$`, so it knows the app as `panpak.<id>` if
`panpak.<id>.desktop` exists (§3.1.5); the runtime MUST NOT let the app call `org.freedesktop.host.portal.Registry`.
The id never equals the id of a Flatpak or of the distribution entry of the same upstream app.

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
(single instance across copies) and to the names of `dbus_own`. `notifications` and `tray` add the rules of §3.1.5.
Every other interface, method and name is refused, including `org.freedesktop.DBus.Properties`, `Introspectable`, every other portal interface
(Location, Background, Screenshot, ScreenCast, RemoteDesktop, Camera, Secret, Account, FileChooser, OpenURI and
the rest), `org.freedesktop.portal.Documents`, `org.freedesktop.secrets` and `org.freedesktop.systemd1`. The rules MUST
come from one function that the tests also use (`panpak dbus-rules <package>` prints them).

*Documents.* When `$XDG_RUNTIME_DIR/doc/by-app` exists the runtime mounts `$XDG_RUNTIME_DIR/doc/by-app/panpak.<id>` in the
view as `$XDG_RUNTIME_DIR/doc`, from the start, read-write. The Landlock rule for the runtime folder covers
documents added later. The app never gets the document portal on the bus and never gets another folder of `doc`: the outer runtime
grants a file with `Documents.AddFull` for the app id `panpak.<id>`.

The runtime reports the state of this layer in `doctor`.

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
cover use of the name "Panpak" (see `TRADEMARKS.md`).
