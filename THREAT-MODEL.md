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
- **Confined writes** to `~/.local/share/<id>` and `/tmp` (fork tests).
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
- **SQLite temp**: the stub points `$SQLITE_TMPDIR` (unless the user
  set it) at a private writable `<appdata>/tmp`. Without this, SQLite
  used `/var/tmp` (default-deny) and temp-file operations (FTS
  indexes) failed even with a writable DB. Only SQLite reads this
  variable; `/tmp` stays shared as above.

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
- **Unix sockets**: `connect()` is not mediated by Landlock V1..V9
  (only `open()` is). The app reaches Wayland, PulseAudio/PipeWire and
  the session D-Bus if the sockets are where it looks for them. D-Bus
  = wide surface (service activation, secrets if unlocked, etc.).
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
