# Portals and D-Bus: study and decision

Status: study, no code. Branch `portal-study`. Measured on Ubuntu 26.04.1 (GNOME,
xdg-desktop-portal 1.21.1, xdg-desktop-portal-gtk 1.15.3, xdg-dbus-proxy 0.1.7,
bubblewrap 0.11.1, systemd 259, kernel 7.0, Landlock ABI 8).

Today the isolated view hides the session bus. That keeps the app away from every D-Bus
service, but it also takes away portals (file chooser, settings, open URI), notifications,
tray icons, media keys, keyrings and the D-Bus single-instance mechanism of GTK
(`GApplication`) and KDE (`KDBusService`). This document answers five questions about how
panpak could give some of that back. It recommends what to do before the beta and what to do after.

Every statement is marked:

- **[tested]**: run on the machine above, with the prototype described in §5.
- **[source]**: read in the source code, with file and line (xdg-desktop-portal tag `1.21.1`,
  libsecret tag `0.21.7`).
- **[inferred]**: a conclusion that was not run.

## Summary of the decision

1. **Identity.** Every start runs in a systemd scope named `app-panpak-<id>-<n>.scope`. A
   desktop file `<id>.desktop` must exist, and `panpak install` already writes one. The portal
   then knows the app as a host app with that id. Panpak MUST NOT pretend to be Flatpak. It also
   MUST NOT let the app call `org.freedesktop.host.portal.Registry.Register`.
2. **Bus filter.** Use the system `xdg-dbus-proxy` with one proxy per start, inside the scope.
   It is an optional dependency, the same as bubblewrap. Without it the app gets no bus, as it
   does today. A filter of our own in Rust is not worth it before the beta.
3. **Files chosen after the start.** Mount the app's folder of the document portal
   (`$XDG_RUNTIME_DIR/doc/by-app/<id>`) in the view from the start. The outer stub then grants
   files with `Documents.AddFull`. The file chooser goes through the outer stub, which turns the
   real path into a document. This keeps the Landlock rules fixed at start (tested).
4. **Instances.** Keep one sandbox per start. Allow the app to own its own id on the bus.
   GTK and KDE single instance then works across sandboxes (tested). The SysV case without X11
   stays a known limit.
5. **Before the beta:** scope, proxy with Settings, Inhibit and own id, document folder in the view,
   `xdg-open` kept on the outer stub. **After the beta:** file chooser, notifications, tray
   icons, MPRIS and secrets, each behind a permission.

## 1. Identity of the app towards the portals

### How xdg-desktop-portal finds the caller [source]

For each D-Bus message, the portal asks the bus for the sender's PID and opens a pidfd. It then
tries the app kinds in this order:

1. Flatpak: reads `/.flatpak-info` in the root of the caller (`xdp-app-info-flatpak.c`, keys
   `Application.name` and `Instance.*`, lines 39-49 and 201-207). The instance also refers to
   Flatpak's own runtime files (bwrap information).
2. Snap: the cgroup and the snap environment.
3. Linyaps.
4. Host: anything else (`xdp-app-info.c:320-366`).

For a host app, the id comes from the systemd unit of the caller. `sd_pid_get_user_unit()` must
return a name that starts with `app-`. The id is parsed from
`app[-<launcher>]-<ApplicationID>-<RANDOM>.scope` (regular expression at
`xdp-app-info-host.c:130`). The id is kept only if `<ApplicationID>.desktop` exists
(`get_app_from_pid`, `xdp-app-info-host.c:163-195`). Otherwise the id is empty.

`org.freedesktop.host.portal.Registry.Register(app_id, options)` exists in this version
(interface version 1, seen with `gdbus introspect` [tested]). It lets a host connection choose its
id, and it must be the first portal call on that connection. It is refused for Flatpak and Snap
callers (`registry.c:60-143`). The portal does not check the id: any host process can claim any
id that has a desktop file.

The portal treats host apps as unconfined. Some permissions are given without asking:

- Location: exact accuracy (`location.c:371`).
- Background: always yes (`background.c:911`).
- Notifications: no permission check (`notification.c:1076`).
- File chooser and screenshot: the real path, never a document (`file-chooser.c:131`,
  `screenshot.c:125`).

### What was tested

- A prototype app ran in a view like panpak's, behind a per-start `xdg-dbus-proxy`. The proxy and
  the view were started with `systemd-run --user --scope --unit=app-panpak-org.example.Study-<n>.scope`.
  The portal, in debug mode, logged `Adding XdpAppInfo: XdpAppInfoHost app 'org.example.Study'`.
  **[tested]**
- The same app started without its own scope inherited the scope of the terminal
  (`app-ghostty-surface-transient-<n>.scope`). The portal logged `app ''`. **[tested]**
- The portal sees the proxy's connection, not the app: the app's process is in another PID
  namespace. One upstream connection per client is enough, because the proxy runs in the same
  scope. **[tested]** (the id above came through the proxy)
- GNOME Shell already starts apps in scopes named `app-gnome-<id>-<n>.scope`. Units seen on this
  machine: `app-gnome-org.flameshot.Flameshot-<n>.scope`, `app-gnome-update\x2dnotifier-<n>.scope`.
  **[tested]**

### Paths compared

| Path | Verdict | Why |
|---|---|---|
| A scope `app-panpak-<id>-<n>.scope` per start, made by the outer stub | **Chosen** | It is the documented host path and works with GNOME's own launchers. The id comes from panpak, not from the app. It needs systemd (user manager) and `<id>.desktop` (already written). |
| `Registry.Register` sent by panpak on each upstream connection | Fallback without systemd | `xdg-dbus-proxy` cannot add a message; it needs a filter of our own (§3). |
| `Registry.Register` allowed to the app | **Refused** | The app would choose any id, for example `org.gnome.Nautilus`, and get its stored permissions. |
| A fake `/.flatpak-info` in the view | **Refused** | See below. |

**Why faking Flatpak is refused.**

- It lies to the desktop: the app would show up as a Flatpak app in settings and permission
  dialogs.
- The portal reads Flatpak's instance files and runtime paths. Panpak would have to fake a format
  it does not own, and that format changes between Flatpak releases.
- libsecret and other libraries change behavior when they see `/.flatpak-info`
  (`secret-backend.c:156`), so the app would break in ways that depend on Flatpak internals.
- Panpak ids can be the same as Flathub ids. Grants of the real Flatpak and of the panpak app
  would mix.

**Consequences.**

- Starting from a terminal or a file manager must still give the right id. So the stub MUST make
  its own scope unless it already runs in `app-*-<id>-*.scope` with its own id. Otherwise a
  terminal whose desktop file exists (for example `app-gnome-org.gnome.Ptyxis-<n>.scope`) would
  lend its id, and the portal permissions that go with it, to the app. **[inferred from source]**
- The id is shared with any host app that has the same desktop id, including a Flatpak with the
  same id. Converted packages that reuse the upstream id share portal permissions and documents with
  the Flatpak of that id. **[inferred from source]** Open risk, see §7.

**Cost:** 1–2 days. Two tasks:

- Ask the user manager for a transient scope with `StartTransientUnit` (D-Bus to the user's
  systemd), or run `systemd-run --user --scope`, and move the stub into it.
- Detect when the stub is already in a correctly named scope.

## 2. Choosing files and files granted after the start

A host app gets the real path from `FileChooser` [source, `file-chooser.c:131-134`]. In the
view that path does not exist, and Landlock rules cannot be added after `restrict_self`.

### (b) Document portal, mounted at start [tested]

The outer side, as a host process, calls `Documents.AddFull(fds, flags, app_id, permissions)`.
`Add*` and `GrantPermissions` are allowed only to host callers (`document-portal.c:971`, `1119`,
`1224`, `1335`, `1375`, `1434`).

The document then appears in `$XDG_RUNTIME_DIR/doc/by-app/<id>/<doc-id>/<name>` (FUSE).

What was run:

- The view mounts `doc/by-app/org.example.Study` at `$XDG_RUNTIME_DIR/doc`, as Flatpak does.
- The app applies Landlock while that folder is still empty: `docs_at_start=` (empty). Read and
  execute are given on `/usr`, `/etc` and `/proc`, and all rights on the document folder and `/tmp`.
- The outer side then adds two files: one with `read`, one with `read`+`write`.
- The app, already restricted, lists both documents.
  - It reads both: `read_chosen.txt=the file the user chose`.
  - It cannot write the read-only one (`write_chosen.txt=err13`, refused by the document portal).
  - It writes the other one (`write_editable.txt=ok`), and the write reaches the real file.
  - A path outside the rules is refused by Landlock (`read_outside=err13`).
- The Landlock rule on the folder covers documents added later, so the rules fixed at the first
  start keep working.
- Persistent documents (`flags` 1|2) are still there at the next start
  (`docs_at_start=c5ee171b`).

What it exposes:

- Only the files granted to that id.
- The real path is not shown.
- Rights are per file, set by whoever grants.

It needs the document portal and FUSE. Both are in xdg-desktop-portal, so the same holds on KDE.
On a system without FUSE or without the document portal, nothing appears [inferred].

### (a) File chooser served by the outer stub [inferred]

`xdg-dbus-proxy` can only allow or deny a call. It cannot turn the real path in a `FileChooser`
response into a document. Two ways to do it:

- **(a1)** The outer stub answers `org.freedesktop.portal.FileChooser` itself, on the bus the app
  sees. Either the stub owns a name on that bus, or it runs a small private bus in the view with
  the stub as its only service. The stub calls the real portal as the app (same scope, same id),
  then adds each chosen file with (b) and returns the document paths.
- **(a2)** A filter of our own between the app and the bus rewrites the `Response` signal of
  `FileChooser` requests.

Either way the app only sees files the user chose in a dialog of the desktop. GNOME and KDE behave
the same, because the conversion happens in the portal front-end, not in the back-end
[source, `file-chooser.c`].

Risks:

- A second implementation of part of a portal interface. It must follow the portal's request and
  response protocol, with handle tokens and `Request` objects.
- A bug could hand out a path instead of a document.

Cost:

- (a1): 5–8 days, with tests and fuzzing of the request parser.
- (a2): see §3.

### (c) Other routes considered

- **Bind-mounting the chosen file into the running view** with `mount --bind` in the view's mount
  namespace. It needs `setns` into the user namespace of the view and a path rule Landlock cannot
  add. **Refused.**
- **Restarting the app with a new grant**, as `panpak open` does. Correct, but not usable for a
  "save as" dialog. **Kept only for `panpak open`.**
- **Letting the app see the real home.** **Refused:** it undoes the view.

**Recommendation:** (b) before the beta (1 day: mount, Landlock rule, test). (a1) after the beta.
The current `panpak open` keeps working as it is.

## 3. Filtering D-Bus

### xdg-dbus-proxy

Packaged versions, from repology.org (October 2026):

| Distribution | Version |
|---|---|
| Ubuntu 26.04 | 0.1.7 (installed here) |
| Ubuntu 24.04 | 0.1.5 |
| Debian 13 | 0.1.6 |
| Fedora 42 | 0.1.7 |
| Fedora 43 and 44 (updates) | 0.1.9 |
| Arch | 0.1.9 |
| nixpkgs 25.11 | 0.1.7 |
| nixpkgs 26.05 and unstable | 0.1.9 |

It is a dependency of Flatpak, so it is present wherever Flatpak is installed: Fedora Atomic and
Bluefin ship Flatpak [inferred]. On NixOS it is in the store, but not in `PATH` unless a package
that needs it is installed [inferred].

What the prototype showed **[tested]**:

- Rules on name, interface, member and object path work: `--call`, `--broadcast`, `--own` and
  `--talk`.
- `Settings.ReadOne` and `OpenURI` passed.
- `Registry.Register`, `org.freedesktop.Notifications`, `org.freedesktop.secrets` and
  `org.freedesktop.portal.Documents` were refused.
- `ListNames` showed only the portal and the bus.
- Signals the portal sends to the app (the `Request.Response` of `OpenURI`, needed by GTK's
  `UriLauncher`) passed with `--broadcast=...=org.freedesktop.portal.Request.*@/org/freedesktop/portal/desktop/request/*`.

It cannot look at arguments. This matters for `OpenURI`:

- The portal passed `mailto:a@b.org?attach=/home/user/.ssh/id_ed25519` unchanged to the mail
  handler **[tested]**.
- It did not open `file:///etc/passwd` (`open-uri.c:445`, **[tested]**).

So opening addresses MUST stay on the outer stub's channel, with its rules (PANPAK-FORMAT.md
§9.3). The stub's `Target` can later call `OpenURI` as the app; `OpenURI` itself is not opened to
the app.

| | System `xdg-dbus-proxy` | Bundled `xdg-dbus-proxy` | Filter of our own (Rust) |
|---|---|---|---|
| Availability | Ubuntu, Debian, Fedora, Arch: a package (Flatpak dependency); NixOS: needs a package that pulls it in | Always | Always |
| Licence and build | LGPL-2.1+, C and GLib, from the distribution | Ship a separate executable with GLib (static GLib is awkward under the LGPL); a second build chain | Ours |
| Argument checks (`mailto`, `FileChooser` rewrite, `Register` injection) | No | No | Yes |
| Attack surface | The full D-Bus wire protocol, the authentication handshake, file descriptor passing; years of use by Flatpak | Same, plus our updates | The same protocol, new code: needs a fuzzed parser of messages and the authentication handshake |
| Cost | 3–4 days: find it (fixed paths, as for bwrap), rules from the manifest, lifetime tied to the stub, socket in the view, Landlock V9 rule | 5–7 days | 10–15 days, plus fuzzing and review |

**Recommendation:** the system `xdg-dbus-proxy`, found at fixed paths (`/usr/bin`,
`/run/current-system/sw/bin`) as for bwrap, and treated as an optional layer. Without it the
behavior is today's: no bus. Revisit a filter of our own only if (a2) or a system without systemd
becomes a priority.

### Minimum rules per feature

The portal object path is always `/org/freedesktop/portal/desktop`.

| Feature | Rules | Exposes | When |
|---|---|---|---|
| Settings (dark theme, accent, contrast) | `--call=org.freedesktop.portal.Desktop=org.freedesktop.portal.Settings.*@/org/freedesktop/portal/desktop`, `--broadcast=...=org.freedesktop.portal.Settings.SettingChanged@...`, `Properties.Get` and `GetAll` | Appearance settings | Before beta, everyone **[tested]** |
| OpenURI | Not through the proxy. The stub channel, later with the portal as the destination | — | Kept as is |
| FileChooser | Through the outer stub, (a1) | Files the user picks | After beta, by default |
| Inhibit (stop sleep or idle during playback) | `--call=...=org.freedesktop.portal.Inhibit.*@...`, `Request.*` and `Session.*` broadcasts | That the app is active | Before beta, everyone |
| Notification portal | `--call=...=org.freedesktop.portal.Notification.*@...` | Text and icons on screen. No permission check for host apps (`notification.c:1076`) | After beta, a permission |
| `org.freedesktop.Notifications` (older apps) | `--talk=org.freedesktop.Notifications` | The same, plus server information. Actions are signals to all clients | After beta, the same permission |
| Tray icons (StatusNotifierItem) | `--own=org.kde.StatusNotifierItem-*`, `--talk=org.kde.StatusNotifierWatcher` | An icon and a menu. The watcher sees the app's object | After beta, a permission |
| Media keys (MPRIS) | `--own=org.mpris.MediaPlayer2.<name>` and `.<name>.*` | Now-playing information to the shell | After beta, a permission listing `<name>` |
| Single instance (GTK) | `--own=<id>`, `--talk=<id>` (name = application id) | Only to copies of the same app | Before beta, everyone **[tested]** |
| Single instance (KDE) | `--own=org.kde.<name>` or the reverse-DNS name from the organization domain | Same | After beta, the manifest declares the name |
| Secrets | See below | — | After beta |

Rules that MUST never be given:

- `org.freedesktop.portal.Location`, `Background` and `Screenshot`: allowed without asking for a host app.
- `org.freedesktop.host.portal.Registry`.
- `org.freedesktop.portal.Documents`: the outer stub grants documents, the app does not.
- `org.freedesktop.secrets`.
- `org.freedesktop.systemd1`.
- `--talk` to any name not in the list.

### Keyrings

- **`org.freedesktop.secrets`** gives the app every secret of the user's unlocked keyrings
  [inferred from the Secret Service API]. **Never.**
- **The Secret portal** (`org.freedesktop.portal.Secret.RetrieveSecret`) gives a secret per app id,
  generated by the back-end. The front-end passes the app id (`secret.c:125-175`).
  - libsecret uses it only when `/.flatpak-info` exists or `SNAP_NAME` is set
    (`secret-backend.c:156`, `secret-file-backend.c:583-600`).
  - Otherwise libsecret uses the Secret Service, or, with `SECRET_BACKEND=file`, a password from
    `SECRET_FILE_TEST_PASSWORD` or a TPM.
  - So an app that uses libsecret directly finds no keyring in panpak and fails or falls back to
    its own storage [inferred].
  - Faking Snap or Flatpak is refused (§1).
  - The id-less case is dangerous: all host apps without an id would share the secret of the
    empty id [inferred from source].
- **Options after the beta:**
  - (1) no keyring, and document it;
  - (2) a small Secret Service of our own in the outer stub that stores only this app's items, in
    a file encrypted with a key from the Secret portal (5–8 days);
  - (3) contribute to libsecret a way for host sandboxes to use the portal backend (upstream work,
    time not under our control).
- **Recommendation:** (1) before the beta; (2) or (3) after.

## 4. Instance model

**Single instance through D-Bus works across sandboxes [tested].** Two starts of a GTK app
(`Gtk.Application`, id `org.example.Study`), each in its own view with its own proxy
(`--own=org.example.Study --talk=org.example.Study`):

- The second start found the first: `is_remote=True`.
- It passed its command line over, and the first printed `primary_got=['second']`.
- The second exited with status 0.

PID and IPC namespaces were separate.

| | One sandbox per start (today) | One sandbox per app and session |
|---|---|---|
| D-Bus proxy | One per start; the portal sees each start with the same id; no shared state | One per app |
| Landlock fixed at the first start | Each start gets its own rules (a `panpak open` grant affects only that start) | Later starts cannot get new file rights; (b) solves this for files given through documents, not for `panpak open` paths |
| Single instance GTK/KDE | Works with `--own`/`--talk` of the id **[tested]** | Works |
| QOwnNotes (System V shared memory) without X11 | Two copies start (known limit) | One copy |
| Return from a login in the browser (URL scheme) | The new start finds the first copy through D-Bus (GTK and KDE) and passes the URL; System V guards only with X11 | Always reaches the open copy |
| Cost | 0 | About 2 weeks (see the xdg-dirs report): supervisor in the view, lifetime, signals, refusing to join when version or permissions changed |
| Risks | — | One crash ends every copy; a control socket into the view; harder to reason about |

**Recommendation:** keep one sandbox per start. D-Bus single instance covers GTK and KDE.
(b) covers files granted after the start. The remaining gap is apps that use System V guards
without X11. Revisit if more of those appear.

## 5. Prototype

Built in a study folder outside the repository, with an isolated `HOME`.

**Private session, not the user's.**

- A private `dbus-daemon`, a private runtime folder, Xvfb, and a clean environment (no
  `WAYLAND_DISPLAY`, no real bus).
- `xdg-desktop-portal`, `xdg-desktop-portal-gtk` (back-end chosen with `portals.conf`),
  `xdg-document-portal` and `xdg-permission-store`, started on that bus.
- Settings come from a private dconf.
- A handler desktop file that only writes the address it gets to a log.

Nothing ran in the user's session apart from transient user scopes, which end with their processes.

**Launch.**

- `systemd-run --user --scope --unit=app-panpak-<id>-<n>.scope` runs the launcher.
- The launcher starts `xdg-dbus-proxy` with the rules of §3, then bubblewrap: empty home, private
  `/tmp`, runtime folder with only the proxied bus, and the document folder of the app.

Results:

| Check | Result |
|---|---|
| Portal Settings through the proxy, after `gsettings set ... color-scheme prefer-dark` | `(1,)` (dark) **[tested]** |
| libadwaita in the view | `adw_dark=True`, `system_supports_color_schemes=True` **[tested]** |
| GTK 4 on X11 alone | Does not read the Settings portal (`gtk-interface-color-scheme` stayed default until libadwaita set it); GTK 4 reads it on Wayland [inferred] |
| `Gtk.UriLauncher` with `GDK_DEBUG=portals` | `https://example.org/from-the-view` reached the handler through `OpenURI` **[tested]** |
| Custom scheme (`panpakstudy:`) | The portal showed its "Open With…" dialog (no stored choice); `http`, `https`, `mailto`, `ftp`, `webcal` and `calendar` skip it (`open-uri.c:479-500`) **[tested]** |
| Denied by the proxy | `Registry.Register`, `Notifications`, `secrets`, `Documents` **[tested]** |
| Portal identity | `org.example.Study` with the scope, `''` without **[tested]** |
| Documents granted after Landlock | Read, write per grant, rest denied (§2) **[tested]** |
| Single instance across two views | Yes (§4) **[tested]** |
| (a1) file chooser through the stub | Not built: §2 [inferred] |

## 6. Costs

| Part | Days | Before beta |
|---|---|---|
| Scope per start, own-scope detection | 1–2 | yes |
| `xdg-dbus-proxy` integration: find, rules, lifetime, view, Landlock V9 rule, tests | 3–4 | yes |
| Document folder in the view and Landlock rule; grant API in the stub | 1 | yes |
| Single instance: `--own`/`--talk` of the id; manifest field for extra names (KDE) | 1–2 | id: yes; extra names: after |
| Stub opener with the OpenURI portal as `Target` (rules unchanged) | 1–2 | after |
| File chooser through the stub (a1) | 5–8 | after |
| Notifications, tray, MPRIS: rules plus manifest permissions plus install text | 2–3 | after |
| Secrets: Secret Service of our own per app (option 2) | 5–8 | after |
| Filter of our own in Rust instead of `xdg-dbus-proxy` | 10–15 | not planned |
| One sandbox per app and session | about 10 | not planned |

Before the beta: about 7–9 days.

## 7. Open risks

- **Ids shared with Flatpak and other host apps.** Portal permissions and documents are keyed by
  app id. A converted package that keeps the upstream id (for example the Flathub id) shares them
  with the Flatpak of that id. A policy is needed: own namespace or accept the sharing.
- **Host apps are trusted by the portal.** Any portal interface opened by mistake gives
  unconfined behavior: Location, Background, real paths. The allow-list must be closed, and
  tested against everything `gdbus introspect` lists.
- **Without systemd** (or with a user manager that refuses transient units) the id is empty.
  Portals that need an id fail. Registering it needs a filter of our own.
- **xdg-dbus-proxy absent or old** (Ubuntu 24.04 has 0.1.5). The rule syntax used here exists in
  0.1.5 [inferred from the changelog, not tested]. When it is absent, the layer is off and the app
  has no bus.
- **Document portal without FUSE.** Some containers and minimal systems have no FUSE. Documents
  then do not appear and the chooser path (a1) cannot work.
- **Landlock V9 and the proxy socket.** The socket must be in the allowed list. Not testable here
  (ABI 8).
- **Notifications and `Notification.ActionInvoked` broadcasts** from `org.freedesktop.Notifications`
  reach every client that subscribes. To be checked before giving that interface.
- **KDE.** Nothing was run on KDE. The front-end behavior is the same by construction; the
  back-end dialogs and the `KDBusService` names were not tested.
- **GTK 3 apps** use the portal only with `GTK_USE_PORTAL=1` or inside Flatpak [inferred]. Panpak
  would set that variable when the proxy is active.

## 8. Changes to THREAT-MODEL.md and PANPAK-FORMAT.md

**THREAT-MODEL.md**

- New channel: the session bus through a filtering proxy, with the closed list of §3. What each
  rule exposes, from the table in §3.
- The portal treats the app as a trusted host app. Name the interfaces never given and why
  (Location, Background, Screenshot, Registry, Documents, secrets).
- The id comes from the scope made by the outer stub. Explain the terminal-scope case and why the
  stub always makes its own scope.
- Documents: the app sees only the files granted to its id. A grant survives restarts when it is
  persistent, and the user can revoke it in the desktop's settings.
- `mailto` attachments: why `OpenURI` is never given directly (tested above).
- Single instance across copies: copies of the same app can send each other command lines over
  the bus. Other apps cannot own or talk to that name.

**PANPAK-FORMAT.md**

- §9.2: a fourth layer, the bus proxy. Fixed paths, optional, reported by `doctor`, `install`
  and `update`.
- New §9.5: the scope name `app-panpak-<id>-<n>.scope`, the rules always given, the document
  folder mount and the grant rules.
- §3.1: new permissions under `[permissions]`. Proposed shape:

  ```toml
  [permissions]
  notifications = true
  tray = true
  mpris = ["audacity"]
  dbus_own = ["org.kde.kdenlive"]
  ```

  Names validated like app ids, with no wildcards from the manifest. Each permission is shown at
  install and counted as new at update, like the existing ones.
- §9.3: the opener's destination MAY be the OpenURI portal; the address rules stay the same.

## 9. Decision

Decided after the study: panpak gives the desktop an identity of its own, not the id of the
package. The study (§1) offered two ways; this is **solution 1**.

**Choice.** The systemd unit, the `.desktop` file and the id that the portals see all use a
prefixed id, `panpak.<id>`, where `<id>` is the app id of the manifest:

| Where | Name |
|---|---|
| Scope | `app-panpak-panpak.<id>-<n>.scope` (portal id is `panpak.<id>`) |
| Desktop file | `panpak.<id>.desktop` |
| Documents folder | `$XDG_RUNTIME_DIR/doc/by-app/panpak.<id>` |
| D-Bus name owned by the app (single instance) | whatever the app asks, limited by the proxy rules; not the prefixed id |

The prefixed id is a valid D-Bus name (at least two elements, each element made of letters,
digits, `_` and `-`, none starting with a digit; the app id rules of PANPAK-FORMAT.md §3.1.1
already give that) and the portal accepts it (`^app-(?:[[:alnum:]]+\-)?(.+?)(?:\-[[:alnum:]]*)(?:\.scope|\.slice)$`,
`xdp-app-info-host.c:130`). The final form is open until step 5 checks that nothing in the
portals, GNOME or the proxy rules objects to a first element without a top-level domain.

**Reasons.**

1. *Permissions of a Flatpak.* Portal permissions and documents are stored per app id. With the
   package id, a converted package whose id is the Flathub id of an app would inherit what the
   user granted to that Flatpak (location, background, notifications, documents). With a prefix,
   nothing is shared.
2. *Not claimable from Flathub.* Flathub ids are reverse-DNS names of a domain the author
   controls. A first element `panpak` is not a top-level domain, so no Flathub app can have an id
   in this space. This is an inference from Flathub's naming rules, to be confirmed.
3. *No duplicates in the menu.* A `<id>.desktop` from panpak next to the Flatpak's `<id>.desktop`
   would show the app twice, with one of them shadowing the other.

**Windows.** GNOME matches a window to a desktop file by the application id of the window
(Wayland `app_id`, X11 `WM_CLASS`), compared with the name of the desktop file or with
`StartupWMClass`. The app sets its own identity, not ours, so the desktop file MUST carry
`StartupWMClass=<what the app sets>`. Where that value comes from (the manifest, or measured by
`convert`) is open.

**To verify in step 5 (nothing is implemented now).**

- GNOME on Wayland and on X11: a window of an app started from the prefixed desktop file is
  grouped under it (one icon in the dash, the right name and icon in the overview, the alt-tab
  list and the window title bar), with `StartupWMClass` set and, as a control, unset.
- The same for apps whose `app_id` or `WM_CLASS` differs from their `.desktop` name (many
  Electron and Qt apps), and for apps that set no class at all.
- That the portal gives the prefixed id to the app (as in §5) when the stub makes its own scope,
  and an empty id when it does not.
- That documents, permissions and the permission store entries are separate between a Flatpak and a
  package with the same upstream id.
- That the name `app-panpak-panpak.<id>-<n>.scope` (or the form we settle on) is parsed back to
  the right id, including ids with `-` and `_` (unit-name escaping).
- That a desktop that is not GNOME (KDE) also groups the windows; not tested in the study.
- Effects on `panpak install` / `update` / `remove`: a desktop file of the new name, and the
  removal of the old one for packages installed before.
