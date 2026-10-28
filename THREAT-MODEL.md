# Panpak — threat model

Status: v1 (runtime ABI 1). Empirically verified where stated (tests),
assumed where explicitly marked.

## 1. Attackers considered

- **A1 — malicious package**: `.panpak` built by the attacker, run by
  the victim. Goal: sandbox escape, persistence, data access.
- **A2 — compromised app**: legitimate app with an exploit (e.g. parser).
  Already running inside the sandbox. Goal: writes outside the allowed
  zones, reading other apps' data, privilege escalation.
- **A3 — hostile update**: package or key replacement. Goal: make the
  victim run code unauthorized by the original author.
- **A4 — network and kernel**: out of model (no network filtering, no
  seccomp, see §4).

The local store (`~/.local/share/panpak`) is considered **trusted**
(same uid, non-hostile fs). Same-uid competitors are out of model.

## 2. What the sandbox guarantees (tested)

- **No execution before verification** (spec §5): Ed25519 signature
  over `manifest || root_hash`, BLAKE3 hash of the stub bound to the
  signature (`[runtime].stub_hash`, verified at every run/install/
  update), index root-hash, per-chunk BLAKE3 hash before writing.
  Tampered file or replaced stub = refusal (tests + e2e).
  Launchers never run the package's stub: only the installed runtime
  (verified at install time) runs, and only verified apps.
- **Read-only tree**: Landlock r-x on the tree + `chmod a-w` on trees
  and blobs. An app cannot modify shared files (fork tests:
  write/truncate/create/rename/unlink/hardlink denied; blobs unchanged).
- **Confined writes** to the two private roots of the app, the `rw` home paths
  of its manifest and `/tmp` (fork tests and `tests/app_dirs.py`, see §2.2).
- **No silent fallback**: without Landlock the stub aborts;
  post-restrict self-test (`/dev/null` openable, create on tree denied).
- **TOFU + pin**: the key is self-signed (proves no identity); at the
  first `install` the fingerprint is shown and pinned, then enforced
  at every start (key change = refusal, tested).
- **Fail-closed ABI**: missing libs or incoherent B-world = error with
  a message, never a silently wrong lib (4-distro matrix).
- **Explicit file open** (`panpak open <app> <file>`, double click):
  the sandbox grants ONLY that file (read; write only with explicit
  `--write`), before startup, without widening anything else.
  The path is canonicalized (the rule covers the real target of
  symlinks); symlinks/hardlinks to NON-granted files stay denied,
  even in the same folder (fork tests: sibling, symlink, hardlink,
  create denied; names with spaces and special characters ok). The
  picker, if needed, runs outside the sandbox (host portal/picker).
- **Opt-in atomic save** (`--write-rename`, never default):
  editors saving via temp+rename (e.g. vim with backup, gedit) fail
  with the file-only rule (fail-closed: the temp is never created).
  The opt-in adds to the parent folder ONLY execute (traversal),
  create, rename and remove: NO read and NO listing (neighbors
  unreadable and unlistable, tested). Documented cost: neighbors stay
  overwritable and deletable (writing the temp requires WriteFile on
  the folder, which also covers them; deletion requires RemoveFile).
  Warning printed at every use. The "whole folder RW" alternative is
  NOT offered.
- **Temporary files**: the stub points `$TMPDIR` and `$SQLITE_TMPDIR` at the
  private `tmp` folder of the app. Without this, SQLite used `/var/tmp`
  (default-deny) and temp-file operations (FTS indexes) failed even with
  a writable DB. `/tmp` stays shared and writable as below; it will be
  narrowed in a later step.

### 2.1 Installation: paths, `.desktop` files and links (tested)

A signed package is attacker-controlled data, as in A1: the signature proves
who wrote the manifest, not that its text is harmless. Several fields become
file names or file content outside the store.

| Threat | Defence |
|---|---|
| `app.id` such as `../../x` or `/etc/x` makes `install`, `update`, `rollback`, `open` or the stub build a path outside the store, the launcher directory or the data directory | One grammar for `app.id` (spec §3.1.1), checked inside `panpak_core::verify`, so every command and the stub refuse it even when the package is signed (`PANPAK-E0006`). Ids given on the command line use the same check. Test: `hostile_manifest_fields_write_nothing`, conformance fixtures `invalid-id-*` |
| `app.name` or another text field with a line break adds keys such as `Exec=` to the `.desktop` file, or terminal escape sequences to the TOFU prompt | Text fields reject control, line-separator and bidirectional characters (`PANPAK-E0007`). The `.desktop` writer also escapes values as the Desktop Entry Specification requires and quotes the `Exec` path (spaces, `"`, `` ` ``, `$`, `\`, `%`). Test: `desktop_entry_values_cannot_start_a_line`, fixture `invalid-name-newline` |
| `app.entry`, `[abi]` library names or index paths with `..`, `/` or control characters reach `Command::new(tree.join(entry))` or `farm.join(name)` | Same field rules in `verify`; index paths and symlink targets reject control characters. Fixtures `invalid-entry-*`, `invalid-abi-name`, `invalid-index-control` |
| A symbolic link planted at `~/.local/bin/<id>` or `applications/<id>.desktop` makes `install` or `update` overwrite another file | The destination is checked with `lstat`; a link, a directory or a non-regular file is refused (`PANPAK-E2001`). The file is written to a temporary file in the same directory (O_EXCL, O_NOFOLLOW) and renamed over the destination, which replaces a link instead of following it. Test: `symlink_in_place_of_the_launcher_is_refused` |
| `install` overwrites a launcher or `.desktop` file of another program | A file is replaced only if it carries panpak's marker for the same app (`# X-Panpak-App-Id:` in the launcher, `X-Panpak-App-Id=` in the `.desktop` file) or is one written by an older panpak. The check runs before the store is touched. Test: `foreign_files_are_not_overwritten` |
| `panpak convert` copies strings from an untrusted AppImage (names, versions, file names) into `panpak.toml` or into a shell wrapper, so a quote or a newline adds a `[permissions]` section or shell code | The strings go through the TOML serializer and are single-quoted for the shell. Test: `convert_strings_cannot_add_keys_or_sections` |
| A launcher path with shell metacharacters (`$`, backtick, `'`) in `$HOME` runs code when the launcher starts | Paths are single-quoted in the generated launcher; control characters in them are refused |

Not covered: a same-uid process that swaps a destination between the check and
the rename (the home directory is trusted, see §1), and a parent directory
(for example `~/.local/bin`) that is itself a link, which users may set up on
purpose. The test and bench scripts are development tools; they refuse to run
with a HOME outside their work directory and never delete outside it
(`tests/isolation.py`).

### 2.2 Private folders, user settings and home paths (tested)

Each app has its own folders (spec §9.1), created with mode 0700 outside the store:
`$XDG_DATA_HOME/panpak/userdata/<id>/{config,data,state,tmp}` and
`$XDG_CACHE_HOME/panpak/<id>`. The stub points `XDG_CONFIG_HOME`,
`XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`, `TMPDIR` and
`SQLITE_TMPDIR` at them, and these are the only writable places besides
`/tmp` and the `rw` home paths of the manifest. `HOME` and `XDG_RUNTIME_DIR`
do not change. The old `~/.local/share/<id>` is not touched and not writable.
`tests/app_dirs.py` runs an app in an isolated HOME and checks that:

- writes through each XDG variable and `TMPDIR` land in the right folders;
- a write to `~/.config/<anything>` or to the home is denied;
- `~/.config/gtk-3.0` and `~/.local/share/icons` can be read and not written;
- `~/.config/gh` with a fake token file cannot be read, and `~/.config` cannot
  be listed;
- missing optional paths do not stop the start;
- `panpak data clear` deletes only the two roots, follows no link and refuses a
  root that is a link.

The user's settings the app may read are listed in spec §9.1. They are
read-only and only if they exist: never the whole config or data folder.
The theme files can reveal which software the user runs; they hold no secret.

`permissions.home_paths` (spec §3.1.2) is the last resort for apps that insist on
a fixed place in the home folder. The manifest validation refuses broad
folders and a list of places that run code at login or hold credentials. The
list is incomplete by nature: a path outside it is not thereby harmless, and
`install` shows the line "Partial access to your home" so the user can judge.
At start the stub refuses a path that is, or has above it, a symbolic link.
Landlock ties each rule to the folder opened when the rules are built, so
replacing a path by a link afterwards does not widen the access. An `rw` path is
readable and writable by the app and by every other process of the user.

An update that adds permissions (X11, network, a port, a socket, a home path or
`ro` raised to `rw`) is not published without a yes (`PANPAK-E1003`, or
`PANPAK-E5004` without a terminal and without `--yes`).

## 3. What the sandbox does NOT guarantee

**X11 opt-in (`[permissions] x11 = true`)**: default false. The opt-in
disables the abstract-socket scope (not the signal scope), so it also
reopens non-X11 abstract sockets: Landlock cannot limit it to the X
server alone. Explicit rule for `/tmp/.X11-unix`; `/tmp` is already
shared and RW, so no per-path X11 deny is promised by default.
Warning at install and at every start: X11 allows observing other apps'
input and screen. Proven with real `xdpyinfo` on Xvfb and XWayland:
default refused, opt-in works; signals to the parent still denied.

- **X11**: the X11 protocol does not isolate clients (keylogging,
  screenshots, input injection across windows of the same server).
  Verified: X11 socket reachable. On X11 a malicious Panpak app can spy
  on other windows. Real mitigation only on Wayland (compositor-level
  isolation).
- **Unix sockets and D-Bus (known way out of the sandbox).** Landlock
  filters `connect()` to a path socket only from ABI V9. On older kernels
  it does not, and the sandbox grants `/run` for reading, so the app
  reaches Wayland, PulseAudio/PipeWire and the session bus through
  `$XDG_RUNTIME_DIR/bus` with no permission asked. Checked on kernel
  7.0 (Landlock V8) with a test app in an isolated HOME and the real
  session bus: the app reads `NameHasOwner` for `org.freedesktop.secrets`
  (true), introspects `org.freedesktop.systemd1` and `org.freedesktop.secrets`,
  and a call to `org.freedesktop.systemd1.Manager.StartTransientUnit` reaches
  systemd, which only rejected the deliberately wrong arguments. Nothing was
  started and no secret was read. With a correct call, the user's systemd
  would start a unit outside the sandbox. `sockets_allow` cannot change this
  below V9 (the stub refuses it there). On V9 the default is the opposite: a
  path socket is denied unless `sockets_allow` lists it. A package that lists
  a bus socket makes `install` and `update` print "Full access to desktop
  services (D-Bus): this app is not isolated". `panpak doctor` prints a note
  on kernels below V9. A D-Bus filter is planned for phase 3.
- **Network ports**: `network_allow_tcp` and `network_bind_tcp` use the Landlock
  network rules (ABI V4, kernel 6.7 or later). They filter by TCP port, not by
  address: a port opened for `127.0.0.1` is open for any address the app
  uses. UDP is not filtered. Checked with Joplin, whose local server on port
  2658 needs both a connect and a bind rule.
- **Dark theme**: the stub reads `gsettings` once at start (at most 200 ms,
  before the sandbox) to set `GTK_THEME`. The app does not follow a later change.
  GTK3 apps cannot read dconf with a private config folder; the settings
  portal is planned to replace this.
- **Shared `/tmp`**: visible and writable by all user processes,
  inside and outside the sandbox. Predictable names = race/symlink
  attacks between apps. Use private subdirs or `O_NOFOLLOW|O_EXCL`.
- **TCP networking**: `network = false` (default) denies bind+connect
  (ABI ≥ V4); `network = true` + allowlist opens only the listed ports
  (e2e with local server: default deny, targeted allow, rest denied).
  Ephemeral ports (`bind` on port 0 + rebind, e.g. the OpenShot
  thumbnail server) canNOT be expressed with discrete allowlists:
  verified with a probe on ABI V8 (bind(0) denied by default and with
  fixed-port allowlists; a port-0 rule only unlocks bind(0), not the
  rebind to the assigned port) → no permission added. Filtering by
  address (loopback only) is not possible: Landlock filters ports, not
  addresses — the warning would stay mandatory even with a permission.
  On ABI < V4 the network is NOT filterable: `network = true` is always
  a hard error; `network = false` is default refusal, bypassable ONLY
  with `--network-unfiltered` from the CLI (never from the manifest),
  which prints a warning at every start. **UDP never filtered by any
  ABI, even with active rules** (Landlock filters cover only TCP
  bind/connect: an app can exfiltrate via UDP/DNS even with
  `network = false`; known permanent limit, not fixable without
  seccomp/netfilter).
- **The `panpak update` network**: HTTPS only (HTTP only to
  localhost/test with flag, never redirects); max 3 redirects without
  downgrade; timeouts; caps on manifest/index/file before allocating;
  reads limited to `expected+1`; incoherent Content-Range or wrong
  bodies = error; confined staging. Tested against a hostile server
  (loop, slowloris, huge body, 200, lying Content-Range, corrupt/
  truncated chunks).
- **Scope (ABI ≥ V6)**: signals and abstract sockets confined (tested:
  out-of-scope `kill` denied, abstract `connect()` denied). On ABI < V6
  not applicable (kernel too old).
- **Path Unix sockets (ABI ≥ V9)**: `sockets_allow` allowlist; on lower
  ABIs the entry is refused if present. Up to and including V8,
  `connect()` to filesystem sockets is NOT mediated (like X11/Wayland:
  works with no rules).
- **Per-ABI table** (best-effort detected with HardRequirement, never
  silent clamp; on kernel 7.0: **V8** — V9 declared by the crate but
  `ResolveUnix` unsupported):

| ABI | kernel | FS base | Refer | Truncate | Net TCP | Ioctl | sig/abstract scope | Unix path |
|---|---|---|---|---|---|---|---|---|
| V1 | 5.13 | ✓ | — | — | — | — | — | — |
| V2 | 5.19 | ✓ | ✓ | — | — | — | — | — |
| V3 | 6.2 | ✓ | ✓ | ✓ | — | — | — | — |
| V4 | 6.7 | ✓ | ✓ | ✓ | ✓ | — | — | — |
| V5/V6 | 6.10/6.12 | ✓ | ✓ | ✓ | ✓ | ✓ | (V6) ✓ | — |
| V7/V8 | newer | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — |
| V9 | future | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |

  On ABI < V4 the sandbox starts ONLY with explicit
  `--network-unfiltered` (warning at every start): the network stays
  unfiltered. Without the flag, refusal.
- **No seccomp**: syscalls that are dangerous but legal for DAC (e.g.
  `ptrace` on same-uid processes outside the sandbox? No: Yama scope 1
  blocks non-children; `process_vm_readv` toward same-uid processes,
  keyrings, etc. remain).
- **`/proc`, `/sys`, `/run` readable**: process/host enumeration,
  readable system D-Bus. Per-node `/dev` + dri/shm: GPU render nodes
  are shared with the whole desktop (standard for GUI apps).
- **`$HOME` denied writing but readable?** NO: `$HOME` is in no rule
  → default-deny, neither read nor write. But
  `~/.local/share/<id>` is inside `$HOME` and is allowed (a more
  specific rule? Landlock merges per-object: access is allowed if AT
  LEAST one rule covers it — verify: path inside appdata covered by
  an RW rule → ok even if the `$HOME` prefix has no rules. Tested:
  data writes work.) Reading `~/.ssh`, `~/.gnupg`, wallets: DENIED
  (default-deny). Good, but fragile: every new broad RW rule reopens.
- **Local denial of service**: chunks up to 1 MiB, index up to 256
  MiB, manifest 1 MiB (anti-zip-bomb caps); beyond = refusal, not OOM.
- **Initial TOFU**: the first pin is not authenticated. Whoever
  distributes the first package over a hostile channel can pin the
  wrong key. Mitigation: out-of-band fingerprint comparison (first 16
  hex shown).

## 4. Known requirements and limits

- **Minimum kernel: 5.13** (Landlock ABI V1). Best-effort detected up
  to V9 (on kernel 7.0: V9). Below 5.13 = hard error, never free run.
- **Alpine/musl**: no host libc → only packages with glibc in the
  bundle (B-world, tested) or static binaries; no `ldconfig`
  (fallback to dir scan). `prefer_host` with included glibc rejected by
  pack.
- **Wayland untested with a dedicated compositor** (only a real GNOME
  session, native backend OK); real GPU OK (renderD128), no explicit
  Vulkan test, non-persistent shader cache (`~/.cache` denied).
- **Clipboard, notifications, portal file/camera**: not mediated
  (they travel via Wayland/D-Bus); file-picker = access to files
  chosen BY THE USER, by design, not sandboxable at this level.
  `panpak open` covers the "open this known file" case (explicit
  per-file grant); the "the app asks the user which file to open/save"
  case stays with the future broker (see PORTAL-DESIGN.md).
- **Rollback**: `apps/<id>/previous` + `panpak rollback` (atomic swap).
- **Key succession**: supported and verified against the pin
  (`rotation_old` + `rotation_sig`, spec §3.2); without a valid
  succession, key change = manual reinstall.
