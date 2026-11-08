# A private D-Bus bus for each app (step 13p): study, prototype and design

Phase 1 of step 13p. No code in the product. The prototype is outside the repository (a local work folder, plus two small
changes to the stub on a local branch that is not merged). The decision is the owner's.

Status of the evidence, so that nothing is read as more than it is:

- **Measured on this machine** (Ubuntu 26.04, kernel 7.0, GNOME session; every D-Bus and Wayland test on a private bus and a
  headless Sway, never on the session of the user): sections 2, 4, 5 and 6.
- **Not run yet**: the tests on a real GNOME desktop (the VM with Bluefin) and on KDE Plasma (the Fedora KDE VM), including the
  Wayland security context on KWin. Section 8 lists exactly what they will check; the results go there.
- The router of the prototype is Python (it shows what is needed and that it works). The cost numbers come from a Rust skeleton
  of the same shape (150 lines, no file descriptors, no delivery between copies). A full Rust router is not written.

## 1. Summary and recommendation

**Recommendation.** Build the private bus as one small router process per app, written in Rust with the code we already have
(`wire.rs`), *not* as a `dbus-daemon` plus a bridge. The router:

1. gives the app a bus of its own, shared by all copies of the app; the app owns its id and any other name there;
2. does not read or filter anything it passes on: every name it does not serve itself goes, unchanged, to an
   `xdg-dbus-proxy` that the copy still has, with the same rules as today (so the portal list, the identity `panpak.<id>`
   and the closed list stay exactly as they are);
3. reads only message headers, and the body of eight calls to the bus itself (names and match rules);
4. serves itself a few things that must not go to the real bus: the app's names, the accessibility address, links
   (OpenURI), and the media player name (as an alias with a fixed prefix).

What the prototype showed, in numbers:

| | Today | With the private bus (prototype) |
|---|---|---|
| A: app calls a native process that owns a name | all 3 calls delivered | refused, native saw 0 calls |
| B: app owns the name of a closed native app, outside client calls it | squatter answers | `ServiceUnknown` outside, squatter got 0 messages |
| C: app calls an activatable native name | native started outside the sandbox | refused, nothing started |
| F22: Ghostty copy with a native Ghostty running | copy becomes a remote instance of the native one | two separate processes, two windows |
| F23: native `ghostty` command with the copy running | not testable on the old stack: the copy failed to register first (F11) | two separate processes, two windows |
| F11: app that owns its id | fails when the id is not granted | owns it on its own bus |
| Cost at start (fixed cost) | 36.2 ms (from a terminal) | **+0.3 ms** with the Rust skeleton |
| Memory per app (bus layer, PSS) | 0.48 MB (the proxy) | 1.0 MB (+0.54 MB) |
| Round trip of a call | 57 us | 72 us (+15 us) |

The first start time has room: the Rust router reports "ready" at once and the real proxy starts at the same time
(section 6).

Two things the study found that change the design of the idea as it was written:

- **A bridge on one side of a private daemon cannot work for portals** (section 3). The portal builds the path of a request
  from the unique name of the *caller*, and the app builds the path it listens on from *its own* unique name. If the app sits
  on a private bus its name is not the one the portal sees. A router that keeps the app's own unique name (one connection to
  the real proxy for each app connection) has no such problem and needs no rewriting.
- **The router must be shared by the copies of an app** (section 5). With one router for each start, two copies of Text Editor
  became two processes and two windows; with one shared router the second copy found the first (1 process, 1 window), as today.
  The real proxy, however, must stay one for each copy, because the portal knows a caller by the cgroup of its connection.

## 2. The idea, and what the tests of the prototype show

Prototype files (not in the repository): `router2.py` (one router for each start, with the accessibility, link and media
player parts), `router3.py` with `routerlib.py` (one router for each app, shared by its copies), `rs/router.rs` (Rust
skeleton for the cost numbers), and the test scripts. The stub changes on the local branch `privbus-proto` are two:
`PANPAK_PROTO_PROXY` makes the stub start a given program in place of `xdg-dbus-proxy` (same command line), and the Landlock
list of sockets allows one more socket for the accessibility part.

How the router sits on the line (one copy):

```
app --(its private bus socket)--> router --(one connection per app connection)--> xdg-dbus-proxy (same rules as today) --> real session bus
                                    |  names the app owns, delivery between copies, OpenURI, accessibility address, media alias
                                    +-- lives once per app; the proxy lives once per copy, in the copy's scope
```

Tests, all on a private bus, with the real stub and the real sandbox unless said otherwise:

| Test | Without | With |
|---|---|---|
| A, `squat` package calls a name owned by a fake native password manager (`Ping`, `GetSecret`, `Activate`) | `ok ok ok`, native saw 3 calls | `error error error`, native saw 0 |
| B, package owns the name of an app that is not running; an outside client calls it | owner is the package; client gets `squatter-reply`; package received `Introspect`, `GetEntry` | no owner on the real bus; outside client gets `ServiceUnknown`; package received nothing |
| C, activatable native app, package calls its name | native started by the bus, 3 calls | refused, native not started |
| F22, native Ghostty first, then the Panpak copy (`--gtk-single-instance=true`) | copy exits (remote instance of the native process); the owner of the name is the native one | copy keeps running: 2 windows, one under each launcher |
| F23, Panpak copy first, then the native `ghostty` | the copy could not register the name (F11, `ApplicationRegisterFailed`) so the order was not testable | native opens its own window under the native process; copy runs: 2 windows |
| F11, Ghostty copy and Text Editor register their ids | `ServiceUnknown` for Ghostty (the package of that test does not grant the id) | registers; Text Editor `RequestName org.gnome.TextEditor` is served by the router |
| Text Editor, qBittorrent: window, light and dark switch, tray registration | window at 2.15 s and 0.82 s; dark and light followed; the watcher was reached | the same, to the decisecond |
| `tests/dbus_filter.py` (the closed list of 102 portal methods, identity, notification permission, names, tray, single instance, theme change) | passes | passes unchanged |
| Two launches of Text Editor | 1 process, 1 window | one router for each start: 2 processes, 2 windows; shared router: 1 process, 1 window |
| Links: `OpenURI` with `https:`, `mailto:`, `mailto:` with `?attach=`, `file:` | (not reachable) | served by the router: `Response` 0, 0, 2, 2 (the policy of `openurl.rs`) |
| Media player: app owns `org.mpris.MediaPlayer2.vlc` | (not allowed) | the real bus shows only `org.mpris.MediaPlayer2.panpak.<id>`, owned by the app's connection; a shell-like client reads `Identity`; a second name the app asks for stays private |
| Accessibility: a client lists the applications on an AT-SPI bus | `[]` | `['gnome-text-editor']` |

One caution on `tests/dbus_filter.py`: it passes with the router too, but it is not sensitive enough. Its "declared names
refused" part was written for a bus that refuses names (with the router such names are granted locally), and its "single
instance" part passed with one router for each start, where two launches of Text Editor gave two processes. The test has to
be rewritten for the new rules (a name of the app is never visible on the real bus; two launches give one process), not just run again.

## 3. Question 2: the bridge, and why it must keep the unique name

### 3.1 What the portal needs

The portal returns the handle of a request as `/org/freedesktop/portal/desktop/request/<caller unique name with . as _>/<token>`
and sends `Response` there. GTK, libportal and Qt build or check that path from the unique name *they* see. Measured (the real
portal in the lab, a client connected through the prototype):

```
app unique name as the app sees it:     :1.7
request handle returned by the portal:  /org/freedesktop/portal/desktop/request/1_7/tok1
path built from the app's own name:     /org/freedesktop/portal/desktop/request/1_7/tok1
```

With a private `dbus-daemon` and a bridge connection to the real bus, the app is `:1.1` on its bus and the bridge is `:1.88` on
the real one: the paths differ and the bridge would have to rewrite the paths of `Request` and `Session` objects in calls,
replies and signals. That is a filter that understands portal objects, which is what the owner wanted to avoid. In the router
design the app's connection *is* the connection of the real proxy (the `Hello` reply is passed on), so nothing is rewritten.
This was not tested end to end with a bridge on a private daemon; the argument is the handle above and the rule of the portal.

### 3.2 What the router reads

For each message from the app it reads the header (type, serial, destination, interface, member, reply serial; a few hundred
bytes) and then:

| Message | What the router does | Reads the body? |
|---|---|---|
| call to `org.freedesktop.DBus`: `Hello` | passes it on; learns the unique name from the reply | no |
| `RequestName`, `ReleaseName` | answers itself (name table of the app) | yes: `(su)`, `(s)` |
| `GetNameOwner`, `NameHasOwner` | answers itself if the name is a name of the app; else passes it on | yes: `(s)` |
| `AddMatch`, `RemoveMatch` | notes the rule for delivery between copies; passes it on | yes: `(s)` |
| any other call to the bus daemon (`ListNames`, `GetConnectionUnixUser`...) | passes it on | no |
| call, reply or signal to a name or unique name of the app (another copy) | delivers it to that connection, with the sender field set | no |
| signal without destination | delivers to matching rules of the other copies; passes it on | arguments only for `argN` rules |
| `OpenURI` | answers itself (section 5.3) | yes: two strings and `handle_token` |
| `org.a11y.Bus.GetAddress` | answers itself (section 5.2) | no |
| everything else | passes it on unchanged (bytes and file descriptors) | no |

It never decides whether a call is allowed. A name that is not the app's goes to the proxy, which decides with the rules of
today (`proxy_args`). The router has no list of names to keep: an app owns any name on its own bus, because it is its own bus.
The only list is in the proxy, where it already is. (One exception on purpose: the alias of the media player, section 5.1.)

### 3.3 Interfaces, directions and who calls whom

| Interface or name | Direction | In the new design |
|---|---|---|
| `org.freedesktop.portal.Settings` (`Read`, `ReadOne`, `ReadAll`) | app to portal | passes through to the proxy rule (as today) |
| `...Settings.SettingChanged` | portal to app, broadcast | passes through; the app followed light and dark switches |
| `...Request.*`, `Request.Response` | app to portal; portal to app (signal) | passes through; the path matches because the unique name is kept |
| `...Session.*`, `Session.Closed` | both | passes through |
| `...Inhibit.*`, `Inhibit.StateChanged` | app to portal; portal to app | passes through |
| `...Notification.*` | app to portal | passes through with the notifications permission; the portal sees `panpak.<id>` |
| `org.freedesktop.Notifications` (`Notify`, `CloseNotification`, ...) | app to the old service | passes through with the permission. Its signals (`ActionInvoked`, `NotificationClosed`) are not delivered today and are not after |
| `org.kde.StatusNotifierWatcher.RegisterStatusNotifierItem` | app to watcher | passes through with the tray permission; the item is the app's unique name |
| `org.kde.StatusNotifierItem`, `com.canonical.dbusmenu` on the app's unique name | host (shell or panel) to app | the host calls the unique name of the app's connection on the real bus; the proxy passes it to the router, the router to the app. Tested with a fake host (lab); the real GNOME and KDE hosts are in section 8 |
| `org.mpris.MediaPlayer2.*` | host to app | an alias with the id, section 5.1 |
| `org.freedesktop.portal.OpenURI` | app to portal | served by the router, to the opener, section 5.3 |
| `org.a11y.Bus` and the AT-SPI bus | app to a11y bus | section 5.2 |
| `org.gtk.Application`, `org.gtk.Actions`, `org.freedesktop.Application` | app to app (copies) | local to the router |
| `org.freedesktop.portal.FileChooser`, `Documents` and others | app to portal | not given today (KL-7); no change. The document portal is used by the outer stub, not by the app |
| `ca.desrt.dconf`, `org.gtk.vfs`, secrets, the shell | app to services | refused as today |

Calls from the real bus *to the app* are only the ones to its unique name (replies, `Response`, tray and menu callbacks) and,
for the media alias, to the alias. A process of the user cannot call the app by an id any more, which is the point of B.

### 3.4 Where the router runs, and how it is fuzzed

- **Process.** The router is the stub started again with a flag (`--panpak-busrouter`), as the receiver of step 7 is
  (`--panpak-receiver`). Landlock: it may create its own socket and its control socket in the app's runtime folder, and
  connect to the sockets of the proxies of the copies; nothing else. Seccomp with a short list; no network; no new privileges;
  small memory limits. It does not hold the user's session bus socket: the proxy does, as today. If an attacker took the
  router over, the gain would be what the app already has through the proxy.
- **Parser.** `wire.rs` already parses messages with limits and has the fuzz target `fuzz_dbus`. The router needs the header
  walk (a subset), the eight body readers and a re-marshaller for the sender field. New fuzz targets: header walk on random
  bytes; the router state machine on a sequence of messages from two clients (names, queues, match rules, disconnections),
  checked against an invariant: nothing for a name that is not the app's reaches the proxy as a local delivery, and nothing
  local reaches the proxy.
- **Descriptors.** Passed through as received (`SCM_RIGHTS`), counted against the header field; a message that announces
  more descriptors than arrived is dropped. The Python prototype passes them; the Rust skeleton does not yet.

## 4. Question 1: who makes the bus

| | Host `dbus-daemon` + bridge | `dbus-broker` | Our router (recommended) |
|---|---|---|---|
| Installed | Ubuntu and Debian yes (this machine: 1.16.2); Fedora: to be checked in the VMs (section 8) | Fedora, Arch; not here | in the stub, nothing to install |
| Can be launched for one app without root or a launcher | yes (`--config-file`, `--nofork`) | not practically: it needs a controller socket and a launcher (`dbus-broker-launch`) | yes |
| Start cost | 2.3 ms to the first accepted connection (median of 30; p90 3.1) | not measured | 0.84 ms to "ready" (the real proxy starts at the same time) |
| Memory | 4.1 MB resident | not measured | 0.57 MB resident |
| Keeps the app's unique name | no: needs a bridge that rewrites portal paths (3.1) | no | yes |
| Code to keep | config files, a bridge of about the size of this router, plus the daemon as a dependency | the same | about 1,000 to 1,400 lines of Rust (the Python prototype with all parts is 430 lines; the Rust skeleton with names only is 150) |
| Security surface | a C daemon of the host, written for trust between peers, in front of an untrusted app | the same | our parser, fuzzed, in a process with no rights |
| Activation of services (case C) | must be switched off by the config (no service directories) | the same | none: there is nothing to activate |

Alternatives discarded: a private `dbus-daemon` with a per-name bridge (unique name problem, extra daemon on every host);
`dbus-broker` (cannot be started for one app); a router that also filters (option 2 of the old design: it is not needed because
the proxy keeps filtering).

Life cycle. The first copy that starts the app starts the router; the router listens on a socket in
`$XDG_RUNTIME_DIR/panpak/<id>/`. Each copy asks it (control socket) to listen on a socket of that copy and to use that copy's
proxy as upstream. A copy that ends closes its control connection and the router closes that socket. When no copy is left the
router ends after a short grace time. The first start is chosen with a lock file, so two copies started together do not make
two routers. This was run: the second copy of Text Editor was delivered to the first through the router (`DescribeAll`,
`CommandLine`), and the listeners were removed and the router ended when the copies ended. A copy that dies badly is
detected by the closed control connection (the proxy of that copy ends by itself when the stub is gone, as it does now). The
router has to run in its own scope, not in the scope of the first copy (in the prototype it did, which keeps that scope alive
while other copies need the router); this is a detail for the implementation.

## 5. Question 3: the special cases

### 5.1 Media players (MPRIS)

The shell and the media keys find players by the name `org.mpris.MediaPlayer2.*` on the real bus. Tested: when the app owns
`org.mpris.MediaPlayer2.<anything>` on its private bus and has the `mpris` permission, the router also asks the real bus (through the
proxy of that copy) for **one** name, `org.mpris.MediaPlayer2.panpak.<id with dots as underscores>`; the proxy gets
`--own=` for exactly that name. A shell-like client read `Identity` at the alias and got the app's answer; the name the app
chose (`...vlc`) is not on the real bus. B is not reopened: a package can only take the name that carries its own id, never the
name of another player. (The id is the package id; the real name of a native player is different by construction, and a
second package with the same id is the same app.) The permission already exists in the list as "not yet available".

### 5.2 Accessibility (defect 53, requirement)

Chain tested in the lab with the real AT-SPI bus launcher: the router answers `org.a11y.Bus.GetAddress` with the address of a
second socket in the copy's proxy folder; that socket is an `xdg-dbus-proxy` on the real accessibility bus with the rules
that Flatpak uses (Flatpak's rule set, recalled from its source, not checked against it here): `--filter --sloppy-names` and
`--call` for `org.a11y.atspi.Socket.Embed`, `Unembed`, `Registry.GetRegisteredEvents` and four methods of
`DeviceEventController` (`GetKeystrokeListeners`, `GetDeviceEventListeners`, `NotifyListenersSync`, `NotifyListenersAsync`).
Result: before, the app logs "Unable to acquire the address of the accessibility bus" and a client sees `[]`; after, the client
lists `['gnome-text-editor']`. What the app does not get: the ability to register an event listener for keys or to see other
applications' widgets through the registry (those calls are not in the rules). The test checked that the app appears; it did not run a screen reader and did not
try the refused calls one by one. Orca on the GNOME VM and a refusal check are in section 8.

In the product the *outer stub* asks the real bus for the address (it already talks to the bus to make the scope) and starts
the proxy; the router stays without access to the session bus. The prototype let the router do it for speed; it is a
shortcut, not the design.

### 5.3 Links (defect 50, F29): one road

**Recommendation: the opener channel is the one road; the router answers `OpenURI` and hands the address to it.** Reasons:
the policy is in one place, already written, fuzzed and rate limited (`openurl.rs`: `http`, `https`, `mailto` without a query;
never `file:`); an app that runs `xdg-open` and an app that calls the portal (GTK4 with `GTK_USE_PORTAL=1`) end up in the same
function; the portal is not asked, so its own handling of `mailto:` with `attach` (see `docs/PORTALS-STUDY.md`) is not in the
path; and it works the same on every desktop. Tested in the prototype: `OpenURI` for four addresses gives the policy's answer
and a `Response` signal at the right path (code 0 for the two allowed, 2 for the two refused); the call to the real `xdg-open`
was not made in the prototype (it wrote a log line). Cost: the router reads the options of `OpenURI` to find `handle_token`
(one bounded parse, about 40 lines, to fuzz). Not covered: `OpenFile` and `OpenDirectory` (they carry descriptors); they stay
refused. The alternative, forwarding `OpenURI` to the real portal after a check in the router, was discarded because the app
could still reach the portal's own handler logic and the two roads would stay.

### 5.4 The tray on GNOME and KDE

Registration and callbacks work by the unique name of the app's connection. With the router that name is kept, so what works
today (qBittorrent registered with the watcher; with a fake host, item and menu were read and activated, with two apps at
once) keeps working. In the lab with the router: the registration reached the watcher. Not yet tested with the real hosts:
the GNOME AppIndicator extension and Plasma. Section 8.

## 6. Question 4: cost

Fixed cost of a start (a script that exits at once, from a terminal; 24 runs for each variant in blocks of six, the proxy
version cache warm in each block; direct start of the same script 0.8 ms):

| Variant | Median | p90 | Over baseline |
|---|---|---|---|
| launcher with `xdg-dbus-proxy` (today) | 37.0 ms | 41.1 | 0 |
| launcher with the Rust skeleton router + proxy | 37.3 ms | 42.4 | **+0.3 ms** |
| launcher with the Python router + proxy | 48.3 ms | 51.5 | +11.3 ms (interpreter start; not a number for the product) |

Why it is not slower: today the app waits until the proxy listens (about 3.4 ms after the scope). With the router the
router listens at once (stage "proxy ready" at 20.8 ms against 25.0 ms in the same run) and the real proxy starts at the same
time; the router connects to the proxy only when the first app connection arrives, so the proxy's start is hidden behind the
rest of the start of the view. The first connection of an app, if it came at once, would wait for the proxy: time from
the start of the layer to the first `Hello` reply, median of 30: proxy alone 2.56 ms, Rust router + proxy 3.01 ms (+0.45 ms).
An app does not connect that early (inferred from the stages of the start, not measured with an app). The target of +3 ms has room of about 2.5 ms for the missing parts (descriptors, delivery,
control socket for the shared router).

Memory per app, bus layer, proportional set size: proxy alone 0.48 MB; Rust skeleton + proxy 1.0 MB (router 0.57 MB); Python
router + proxy 8.3 MB. The shared router is one per app, so a second copy adds only its listener. Round trip of a call through
the layer (`Ping` to the bus daemon, 1000 calls): proxy 57 us, Rust router + proxy 72 us, Python 114 us.

If a step turns out slower in the product: the cheapest place to recover is the two ms of the proxy version check
(`proxy_status_cached` already avoids it); the next is starting the router before the scope is made (it needs nothing from it).

## 7. F22 and F23

Both are closed in both orders by construction: the app's id lives on its own bus, so nothing on the real bus carries it and
nothing outside can reach it. The lab showed it with Ghostty and a native Ghostty on a private bus (section 2). The old stack
could not show F23 in the lab: with the copy started first it failed to register the name (F11). F23 stays a prediction of
section 4 of `docs/SOURCE-BUILD.md`, now closed in the other order's test.

## 8. Risks, open points and what the VMs must check

Risks of the design:

1. **Code of our own in the path of every D-Bus message of an app.** Mitigation: it passes bytes through and reads headers;
   fuzz targets; no rights; the proxy keeps filtering. A bug that loses messages breaks apps, so the first release needs
   the full harness (25 apps, 3 source builds).
2. **Delivery between copies is a small bus daemon** (names, queues, match rules, signals). The prototype has about 250 lines of it.
   Match rules are needed for `NameOwnerChanged` (GApplication) and `arg0` rules; the subset must be written from what the
   real apps send, and the harness logs those rules to find them.
3. **Shared router life cycle** (section 4): lock, grace time, scope. Race tests needed (two copies at once, a copy killed).
4. **`dbus_own` loses its meaning for the app's own names** (they are free on a private bus). It stays for MPRIS (alias)
   and may be reused as "names to publish". The install text must change: today it says "the app can own X on your session
   bus"; it becomes "the app has a bus of its own; it can reach: portals, notifications, tray" — shorter and more honest.
5. **Apps that need a name on the real bus** (a service that other programs call by name: for example a browser extension talking
   to a password manager by its name). They stop working inside the sandbox. This is the aim of B, but it is a visible change; a
   publish permission with a reason is possible later. Not designed here.
6. **Descriptors and large messages** in the router; not exercised in the Rust skeleton.
7. **GApplication details**: `NameOwnerChanged` for names owned locally, `Introspect` of objects, `org.gtk.Notifications`
   do not leave the private bus. Text Editor, qBittorrent and Ghostty ran; apps with more D-Bus use (the other 25 AppImages) have
   not been run on the prototype.
8. **A local desktop service that expects the app id on the real bus**, for example notifications that call back by
   `org.freedesktop.Application.ActivateAction` on the app id. Today's callbacks do not work either (section 3.3); after the
   change a click starts the launcher, and single instance among copies does the rest. To check on GNOME and KDE.

VM checklist (the owner turns each VM on; one at a time; back to the clean snapshot afterwards). The kit (binaries of the
prototype, the packages, `vm_tests.py`) is prepared; it runs in an isolated HOME on the VM's own session:

- **GNOME (Bluefin VM).** `env`: versions, whether `dbus-daemon` is installed on a Fedora-based system with `dbus-broker`,
  Landlock level. `lab`: A, B, C with the kit's stub on a private bus. `session`: Text Editor, qBittorrent, Ghostty with and without
  the router: alive after 14 s, no registration error, the app id absent from the session bus, portal calls seen, tray item
  registered with the watcher and a host-style `Activate` on it. `a11y`: the accessibility list with the real AT-SPI bus.
  By eye (the owner): window visible, tray icon present and clickable if the AppIndicator extension is on, a notification, Orca
  speaking in the Text Editor window.
- **KDE (Fedora KDE VM).** The same, plus: the Wayland security context on KWin (the stub's "Wayland context" stage, the window
  appears and the app does not get the compositor's privileged protocols), the Plasma tray (native) and Plasma's media widget with
  the MPRIS alias.

Results of both VMs are added to section 9 when run.

## 9. Results on real desktops

Not run yet.

## 10. What the owner decides

1. Router of our own, shared by the copies, in Rust (recommended), or a `dbus-daemon` with a bridge (not recommended).
2. Links through the opener channel (recommended) or through the portal.
3. Media players with an alias carrying the id (recommended) or with no support.
4. Accessibility with a filtered proxy on the AT-SPI bus, started by the outer stub, as a requirement of the first release.
5. The text change at install (risk 4) and the effect on `dbus_own`.
6. Order of work for the product: (a) the router with names, delivery between copies and pass-through, behind the existing
   test `tests/dbus_filter.py` rewritten for the new rule, with the fuzz targets; (b) the accessibility proxy; (c) links;
   (d) the media alias; (e) the install text and docs; (f) the full harness on the 25 AppImages and the three source builds, on
   GNOME and KDE.
