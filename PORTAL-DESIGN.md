# D-Bus portal: design (NO code — architecture decision)

Context. The Panpak sandbox denies writes outside appdata+`/tmp` and
(with V9) could filter path Unix sockets. But on real kernels (here:
V8) `connect()` to filesystem sockets is **not mediated**: the app
reaches Wayland, PulseAudio/PipeWire and the **session D-Bus**
(`$XDG_RUNTIME_DIR/bus`) with no way for Landlock to stop it. The
session D-Bus is a wide surface: service activation, unlocked secrets,
media keys, notifications, and — critical for file-oriented apps — the
**portals** (`org.freedesktop.portal.*`: file picker, screenshot,
open-uri).

The file picker decides it: an editor/test viewer *must* open
user-chosen files, but not all of `$HOME`. Three options:

## (a) Direct access to the session bus

The app talks to the real bus (`$XDG_RUNTIME_DIR/bus`, reachable today
because `connect()` is not mediated and `/run` is read-only — reading
the socket path is enough).

- Security: **terrible**. Whoever holds the bus holds everything the
  session grants the user: at-spi (keylogging via accessibility),
  secrets (if unlocked), execution via D-Bus activation, reading every
  file through portals with no granular consent (portals assume a
  trusted *broker*, not the direct caller).
- Dependencies: none.
- Panpak complexity: zero (today's de-facto state).
- Landlock: no change (on V9 the bus socket would need allowlisting
  anyway, otherwise even this closes).
- Picker UX: 100% native (it is the real portal).
- Verdict: unacceptable as policy; today it is the hole documented in
  THREAT-MODEL.md ("D-Bus = wide surface").

## (b) xdg-dbus-proxy as a host dependency

`xdg-desktop-portal` + `xdg-dbus-proxy` installed on the host; the stub
spawns the proxy before the app with a policy that only lets
`org.freedesktop.portal.*` (+ trusted helpers) through and forwards the
rest to the real bus. The app sees a *filtered* bus on a private socket
in `/tmp` or appdata.

- Security: good — the same mechanism as Flatpak, reviewed for years;
  the proxy is a small stable boundary, and filtering is by interface
  name (allowlist), not by content.
- Dependencies: **heavy externals** — requires the portal stack on the
  host (absent on minimal/server/container/Alpine-musl systems). If
  missing, either degrade to (a) (unacceptable) or the app does not
  start (honest fail-closed, but fragile in distribution).
- Panpak complexity: medium — proxy spawn, lifecycle management (dies
  with the app), passing the address via a private
  `DBUS_SESSION_BUS_ADDRESS`, per-app policy file (generatable from
  the manifest).
- Landlock: the proxy socket lives under `/tmp` or appdata (already
  RW); the real bus stays reachable *in principle* — but without an
  advertised address the confined app cannot find it, unless it guesses
  it (`$XDG_RUNTIME_DIR/bus` is a known path!). It still needs (c) or
  V9 to close the hole: alone, (b) is security by obscurity.
- Picker UX: native (the host's real portals).
- Testability: high — compare the D-Bus calls the app sees (bus
  monitor on the private socket) with/without proxy; escape tests (an
  attempted `org.freedesktop.secrets` call must fail).

## (c) D-Bus proxy integrated in the runtime

A minimal broker inside `panpak-stub` (or an agnostic `panpak-proxy`
helper): exposes a filtered bus to the app and forwards to the real bus
only the interfaces the manifest allows (`[permissions.portal]`
already exists in the schema for exactly this: `files = "picker"`,
`camera = "ask"`).

- Security: potentially the best (no external dependencies, per-app
  policy declared in the manifest and verified with the signature),
  BUT the broker is new critical code: D-Bus parser, rule matching,
  management of passed file descriptors (`SCM_RIGHTS` for documents
  opened from the picker — the most delicate part: one wrongly passed
  fd reopens whole directories).
- Dependencies: zero external; significant internal cost (estimate:
  parser + router + non-bypass tests).
- Complexity: high. The D-Bus protocol is vast (object paths,
  variants, nested container types, `GetManagedObjects` expanding whole
  subtrees in a single reply — the classic incomplete-filter vector).
- Landlock: combines with V9 (`sockets_allow` only for the broker
  socket) and V6+ scopes (abstract). Without V9, as with (b), the real
  bus stays reachable to anyone knowing its path: mitigable only by
  hiding the address (weak) or moving the bus (impossible without
  session cooperation).
- Picker UX: native if forwarding is faithful; risk of small
  divergences (portal versions) to handle with integration tests.
- Testability: the highest of the three *at steady state* (unit tests
  on the matcher, fuzz on the D-Bus parser, escape e2e tests as in
  (b)), but it requires writing the code first.

## Synthetic comparison

| criterion | (a) direct | (b) xdg-dbus-proxy | (c) integrated proxy |
|---|---|---|---|
| real security | no | yes (if present) | yes (if correct) |
| host dependencies | none | portal stack | none |
| works everywhere (server, musl, container) | yes (too much) | no | yes |
| Panpak complexity | none | medium | high |
| closes the real bus without V9 | no | no (alone) | no (alone) |
| picker UX | native | native | native (to verify) |
| testability today | n/a | high | high (after implementation) |

Fundamental note common to (b) and (c): **while the real bus stays
reachable from the sandbox (missing V9 or missing rule), no proxy
offers guarantees, only hygiene**. Real closing requires V9 +
`sockets_allow` limited to the broker — to verify on kernel ≥ 6.??
with actual `ResolveUnix` support (here: V8, unavailable).

## What `panpak open` covers (implemented) and what stays with the broker

`panpak open <app> <file>` (and double click via launcher) solves the
"open THIS known file" case: at startup the runtime grants the sandbox
only that file (read; write only with explicit declaration).
The picker, when needed, runs outside the sandbox (host portal/picker)
and the result comes back as an explicit path.

Covers: opening known documents, attachments, files from
terminal/file manager. Tested: the granted file reads (even with
spaces/special characters), non-granted siblings/symlinks/hardlinks do
not; `--write` enables writing only on the granted file.

Left to the broker (c): everything born INSIDE the app — Save As,
app-invoked file picker, drag&drop, recent-files, whole-directory
access. Those flows still need D-Bus filtering (option (c) above);
`panpak open` does not intercept them and does not make them safe.

## Recommendation

**Main road: (c), with (b) as fallback where available.**
Reasons: Panpak aims to run everywhere including minimal and musl
hosts, where (b) cannot be installed; the per-app policy in the signed
manifest matches the rest of the format (declarative allowlists, never
silent fallbacks); non-bypass tests are the same in both cases.

Proposed sequence (outside this cycle):
1. Fuzzing + matcher tests on D-Bus specifics (no network code).
2. Minimal broker: only `org.freedesktop.portal.FileChooser` +
   `OpenURI`, rest denied; e2e with a test app (opens only files chosen
   in the picker).
3. `SCM_RIGHTS`: dedicated audit (one fd too many = broken sandbox).
4. When real V9 is available: close the system bus via
   `sockets_allow` and show the before/after with escape tests.

## What could be tested (once implemented)

- Escape: an app calling `org.freedesktop.secrets`, at-spi,
  `GetManagedObjects` on disallowed interfaces → all denied, logged.
- Picker: user-chosen file openable, non-chosen file (known path) not
  openable — including via passed fd (`SCM_RIGHTS` check).
- Missing host broker: on a system without portals, the app with
  `files = "picker"` must either use the integrated broker or fail
  explicitly (never silent direct access).
- Landlock regression: with V9, `strace -e connect` of the app must
  show no connections outside the broker socket.
