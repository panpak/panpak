# A private D-Bus bus for each app (step 13p): study, prototype and design

Phase 1 of step 13p. No code in the product. The prototype is outside the repository (a local work folder, plus two small
changes to the stub on a local branch that is not merged). The decision is the owner's.

Status of the evidence, so that nothing is read as more than it is:

- **Measured on this machine** (Ubuntu 26.04, kernel 7.0, GNOME session; every D-Bus and Wayland test on a private bus and a
  headless Sway, never on the session of the user): sections 2, 4, 5 and 6.
- **Measured on two real desktops** (VMs: Bluefin with GNOME on Fedora 44, kernel 7.2.11; Fedora 45 KDE with Plasma, kernel 7.2.15; apps run in an isolated HOME on the
  VM's own session): section 9, with checks by eye by the owner.
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
player parts), `router3.py` with `routerlib.py` (one router for each app, shared by its copies), rs/router.rs (Rust
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

On the two VMs (Fedora with `dbus-broker`) `dbus-daemon` is also installed, so the bridge variant would run there; it is dropped for the
reason in 3.1, not for availability.

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
lists `['gnome-text-editor']` (in the lab, and on the real GNOME session). **Qt needs more**: qBittorrent did not register until the
router also answered `NameHasOwner("org.a11y.Bus")` (Qt asks whether the name exists first) and the properties of `org.a11y.Status`
(`IsEnabled`, `ScreenReaderEnabled`); then it was listed on KDE. The prototype answered "enabled" for both, which is only a test;
the product has to ask the real bus and pass on the changes (`PropertiesChanged` when a screen reader starts), or apps started before the
screen reader would never register. What the app does not get: the ability to register an event listener for keys or to see other
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

Risks 9 and 10, found on the VMs:

9. **Qt applications need more than `GetAddress`** for accessibility (section 5.2): they ask whether the name `org.a11y.Bus`
   exists and read `org.a11y.Status`. The router must answer both from the real bus and follow changes (`PropertiesChanged`
   when a screen reader starts). Only a fixed "enabled" answer was tried.
10. **The portal does not know the app on Fedora** (section 9, found with the notification test): independent of the private bus,
    but the identity `panpak.<id>` is the base of notifications, permissions and the document portal.

## 9. Results on real desktops

Method: the kit (binaries of the prototype, packages, `vm_tests.py`) copied to the VM; the parts `env`, `lab` (A, B, C on a private
bus), `session` (the three apps on the VM's own session), `a11y`; then checks by eye by the owner, one at a time. Router: `router3.py`
(shared) for the apps, `router2.py` for the accessibility, links and media parts. The VMs went back to a clean state.

| Check | GNOME (Bluefin, Fedora 44) | KDE (Fedora 45, Plasma) |
|---|---|---|
| Bus on the host | `dbus-broker`; `dbus-daemon` binary also installed; `xdg-dbus-proxy` 0.1.9; portal 1.22.1; Landlock V9 | the same (`dbus-broker`, `dbus-daemon` installed, proxy 0.1.9, portal 1.22.1-2, Landlock V9) |
| A, B, C (stub and sandbox of the kit) | before `ok ok ok` / squatter answers / native started; after `error error error` / `ServiceUnknown`, 0 received / not started | the same numbers |
| Text Editor | alive after 14 s with and without the router; id `org.gnome.TextEditor` owned on the real bus (`:1.140`) before, none after; by eye: window, correct icon, light and dark follow | **crashes with and without the router** (SIGSEGV in `libgtk-4`, from a Wayland event at display open; also with the security context off) |
| qBittorrent | alive; tray item registered with the watcher, properties readable, `Activate` answered, with and without the router; by eye: tray icon and menu | alive; tray items registered; by eye: tray icon right, click shows or hides the window, right click opens the menu |
| Ghostty | alive with the router; by eye: window with a shell, new window with `Ctrl+Shift+N` in the same instance | **crashes** (same SIGSEGV; stack of Text Editor only) |
| Wayland security context | none: Mutter has no protocol (the stub says so) | used; the sandboxed app sees 61 globals against 68 on the full socket; hidden: `org_kde_kwin_fake_input`, `org_kde_plasma_window_management`, `zkde_screencast_unstable_v1`, `ext_data_control_manager_v1`, `kde_lockscreen_overlay_v1`, `org_kde_plasma_activation_feedback` and the security context manager |
| Accessibility, GTK | applications on the AT-SPI bus: without the router the app is absent; with it `gnome-text-editor` is listed. By eye with Orca: text and buttons are spoken; menus were silent in the first run and spoken in a second run with the same setup | not run (Text Editor crashes) |
| Accessibility, Qt | not run | with only `GetAddress` served: the app does not appear. After also answering `NameHasOwner("org.a11y.Bus")` and the status properties: `qBittorrent` is listed |
| Notification through the portal | the backend gets `AddNotification` with an empty `app_id`, with and without the router; no notification shown | the same: empty `app_id` |
| Links | the shim `xdg-open` in the view answers 0; the address reaches the system `xdg-open` (seen on the bus); no browser tab (with an isolated HOME and with the real data folders). The same `xdg-open` run directly opens Firefox | not run |
| Media player widget (MPRIS alias) | not run on the desktop (lab only) | not run |

What this means:

1. **The private bus works on both desktops for what could be run** (names, tray, portal calls, accessibility for GTK and Qt).
   Tray callbacks from the real GNOME and KDE hosts pass through, with the unique name kept.
2. **Four findings that are not caused by the private bus** (the same without the router), to be handled apart:
   - the portal 1.22.1 of Fedora 44 and 45 reports an empty `app_id` for the panpak scope (the portal of Ubuntu 26.04 reported `panpak.<id>`);
     notifications are accepted and then dropped;
   - GTK 4.18 apps from the Debian 13 image crash at display open on Fedora 45 KDE;
   - the application icon in the task bar is a gear or another app's icon for qBittorrent (GNOME and KDE) and Ghostty (GNOME), because the
     desktop entry has no `StartupWMClass` in the test packages; Text Editor is right (F24);
   - the opener channel returns 0 and nothing opens on the Fedora GNOME VM (F29 stays open; cause not found).
3. **Not run:** MPRIS on a real shell, Orca on KDE (no GTK app runs there), notifications with a correct `app_id`, a real Plasma tray with
   Plasma's own icons for the alias, the 25 AppImages on the prototype.

## 11. Phase 1b: one agent for the app, against router + proxy

The question: instead of our router in front of one `xdg-dbus-proxy` for each copy (design A), one process of ours for each app that also does
the filtering towards the session bus, with one connection to the host for each copy (design B, "the agent"). The agent would also hold the receiver
of the opener channel and, later, the file chooser, and would be the one that registers the identity with the portal. `xdg-dbus-proxy` stays in the
tests as the judge.

Prototype for the numbers: the Rust skeleton of section 6 got an `--agent` mode: no proxy, one connection to the host bus for each app connection,
and a filter for the `--call` and `--broadcast` rules of the product (method calls checked against name, method and path pattern; broadcast
signals against interface and path; replies and unicast signals pass). It is a skeleton: no descriptors, no `SEE` policy, no match-rule validation.
The shared router (`router3.py`) got a "direct" mode (it connects to the host bus itself) to test the connections and the identity. All of it on a
private lab bus or on the two VMs; nothing was run on the bus of the owner.

### 11.1 Measure 1: what the proxy costs

Fixed cost of a start (script that exits at once, the same stub, 40 runs for each variant in blocks of ten, medians; "over direct" is the
difference to the same script started directly in the same wrapper):

| Variant | From a terminal (the stub makes its scope) | From the menu (inside an `app-gnome-...` scope) |
|---|---|---|
| no bus layer at all | 25.4 ms (+24.6 over direct) | 34.6 ms (+15.2) |
| layer with `xdg-dbus-proxy` | 38.6 ms (+37.8) | 35.0 ms (+15.6) |
| layer with the Rust router + proxy | 39.0 ms (+38.2) | 34.9 ms (+15.4) |
| layer with the Rust agent (no proxy) | 37.5 ms (+36.7) | 34.6 ms (+15.2) |

(The menu column includes about 10 ms of `systemd-run` that wraps every variant; the differences are what counts.) Reading: **the proxy costs
about 0.4 ms from the menu** (its start is hidden behind the setup of the view) and the whole layer 13 ms from a terminal, which is the
transient scope, not the proxy. The three variants are inside the noise of a run (about 1.5 ms between repeats of the same variant: the earlier
section 6 run gave +0.3 for the router, this one +0.4). Neither design changes the cost of a start.

Memory of the layer for one running copy (proportional set size; resident size in brackets): proxy 0.48 MB (6.0 MB); router + proxy 1.03 MB
(router 0.59 + proxy 0.44); agent 0.52 MB (0.52 MB). With three copies: proxy alone 3 x 0.48; router + proxy 0.59 + 3 x 0.44 = 1.9 MB; agent 0.52 plus
a thread and a socket for each copy (not measured, thousands of bytes).

Latency of a call through the layer (`Ping`, 1000 calls on one connection, three runs, medians): no layer 22 to 24 us; proxy 54 to 58 us (+32);
agent 33 to 39 us (+11 to +16); router + proxy 69 to 76 us (+47 to +52); Python router + proxy 119 to 125 us. The agent number is for calls to the
bus itself, which the skeleton does not filter, so it is a floor.

### 11.2 Measure 2: connections, scopes, identity

The portal knows a caller by the cgroup of the process that owns the connection. The two designs were run with the real portal and the fake backend,
with a probe that asks the portal (`Inhibit`, which makes a request and an answer) and listens to the `Response`:

Two copies, the first started first and ended before the second asks the portal (8 s later):

| | A: shared router + a proxy for each copy | B: shared agent, one connection to the host for each copy |
|---|---|---|
| processes of the layer while copy 2 waits | the router and the proxy of copy 2 | the agent (it lives in the scope of copy 1) |
| the portal's `app_id` for copy 1 and for copy 2 | `panpak.<id>` for both | `panpak.<id>` for both |
| request path of copy 2 | `.../request/1_8/c2`, equal to the path the copy builds from its unique name | the same |
| `Response` delivered to the right copy | yes (copy 1 and copy 2) | yes |

In B the agent stays inside the scope that copy 1 made; when copy 1 ends, the scope stays alive because the agent is in it, which is why the portal
still knows copy 2. That is a detail to settle in the product: the agent should have a scope of its own (named after the app) so that its life does not
depend on the first copy. In A, each copy's own scope is what the portal reads, as today.

With the real file chooser of Text Editor on the GNOME VM (design A, shared router; two independent copies started with `--standalone`):

1. **The filter has to let the app read the portal's properties.** First try, with only the `FileChooser.*` rule: the dialog that opened was GTK's own,
   and no `OpenFile` reached the bus. GTK asks the portal for the version of the interface first (`org.freedesktop.DBus.Properties.Get` on the portal's
   object); the filter refused it and GTK fell back. With `Properties.Get` and `GetAll` allowed on `/org/freedesktop/portal/desktop`, GTK used the
   portal. This goes into the rule list the day the file chooser (KL-7, step 11) is given.
2. Copy 1 (`:1.115`): `Ctrl+O` opened the system dialog (GNOME's, with its sidebar); `OpenFile` was seen on the bus from `:1.115`; `Esc`: the `Response`
   arrived at `request/1_115/gtk...` and the editor was usable again.
3. Copy 1 closed (process group ended). Copy 2 (`:1.132`): `Ctrl+O` opened the dialog; a file was chosen; the `Response` (code 0, `uris`) arrived at
   `request/1_132/gtk...`, to copy 2, and the dialog closed. So the portal still knew copy 2 and the answer went to the right copy after the first copy was gone.
4. The file did **not** appear in the editor. The portal answered with a real path (`file:///var/home/<user>/...`) because it takes the app for a host
   application; the view does not contain that path. That is KL-7 and the reason for the file chooser of step 11, not an effect of the bus; the agent or the
   router would have to hand the files over through the document portal or bind them.
5. The system dialog is not attached to the editor (it covers it): the app has no parent window handle to give. Noted for step 11.

The same test on design B (agent) was not repeated on the VM; the lab test above (`Inhibit`, same request path rule) covers its identity logic.

### 11.3 Measure 3: accessibility without opening a window on the other apps

How Flatpak does it (source of Flatpak 1.16.1, `common/flatpak-run-dbus.c`, function `flatpak_run_add_a11y_dbus_args`):

1. It does **not** put `org.a11y.Bus` on the filtered session bus at all. It asks the real session bus for the address of the AT-SPI bus (or takes
   `AT_SPI_BUS_ADDRESS`), starts a second `xdg-dbus-proxy` on that bus with `--filter --sloppy-names`, binds its socket into the sandbox
   (`/run/flatpak/at-spi-bus`) and sets `AT_SPI_BUS_ADDRESS` in the app's environment. Toolkits then skip the question to the session bus.
2. The rules are two broadcasts (`org.a11y.atspi.Registry.EventListenerRegistered` and `EventListenerDeregistered`, path
   `/org/a11y/atspi/registry`) and seven calls to the name `org.a11y.atspi.Registry`: `Socket.Embed` and `Socket.Unembed` (path
   `/org/a11y/atspi/accessible/root`), `Registry.GetRegisteredEvents` (path `/org/a11y/atspi/registry`), and under
   `/org/a11y/atspi/registry/deviceeventcontroller`: `GetKeystrokeListeners`, `GetDeviceEventListeners`, `NotifyListenersSync`,
   `NotifyListenersAsync`. For an app that is not a sub-sandbox it adds `--call=<id>.Sandboxed.*=org.a11y.atspi.Socket.Embedded`.
3. Nothing else: no call on the root object of the desktop, no listener registration, no call to other applications.

This replaces the idea of section 5.2 (the router answering `GetAddress`, `NameHasOwner` and the status properties); that was needed only because the
app was not told where the bus is. With the variable, **Qt works without any answer on the session bus**: qBittorrent (KDE VM) was listed on the
AT-SPI bus with `AT_SPI_BUS_ADDRESS` alone. The stub already sets the environment of the view (`DBUS_SESSION_BUS_ADDRESS`), so this is one more variable
and one more socket in the same folder; the outer stub (which already talks to the session bus to make the scope) asks for the address and starts the proxy.

Proposed rule for the agent or the second proxy: the Flatpak list, **minus `NotifyListenersSync` and `NotifyListenersAsync`** unless a toolkit is
found that needs them (they let an app tell the listeners, a screen reader for example, that a key was pressed: an app could make a screen reader react
to keys that were never pressed), and **plus** nothing. Tested with the Flatpak list (all of it), a GTK app (Text Editor, lab) and a Qt app
(qBittorrent, KDE VM), a screen-reader-like client on the host (Atspi) and a probe inside the sandbox on the filtered bus:

| What | Result |
|---|---|
| the host client lists the apps | Text Editor listed, with its window and its role (`frame`); qBittorrent listed |
| the probe calls `GetRegisteredEvents` | allowed (empty list) |
| the probe calls `GetKeystrokeListeners` | allowed (empty list; it would show which tools listen to keys) |
| the probe calls `GetChildren` on the desktop root (it would list all applications) | refused |
| the probe registers for events (`RegisterEvent`) | refused |
| the probe calls `GetChildren` on every connection `:1.1` to `:1.79` (other apps) | refused, 79 of 79, in the lab and on KDE |
| the probe calls `ListNames` | allowed: it shows a few unique names (`:1.1`, `:1.3`) and `org.a11y.atspi.Registry`, but the connections cannot be called |

So: the app can register and be read; it cannot read or command the other apps. What it can learn is that other connections exist and which listeners
are registered. Orca on the GNOME VM, six runs (owner's ears), all with Text Editor:

| Run | Setup | Typed text | Menu entries |
|---|---|---|---|
| 1 | filtered a11y proxy, address served on the session bus | spoken | silent |
| 2 | the same | spoken | spoken |
| 3 | filtered, `AT_SPI_BUS_ADDRESS` only | spoken | silent |
| 4 | **unfiltered** a11y proxy, `AT_SPI_BUS_ADDRESS` only | spoken | spoken |
| 5 | filtered, `AT_SPI_BUS_ADDRESS` only, proxy log on | spoken | spoken |

Run 5's proxy log (6,593 lines) has **no denied and no hidden message**: 79 messages "filtered in" and none out, with Orca reading the menus. So no call that
Orca or the toolkit needs was refused in the one run that was logged, and the same filtered setup was silent in runs 1 and 3 and spoken in 2 and 5. The
unfiltered run is one run. **The silence of the menus is not explained by the filter**; it looks like Orca's first contact with a freshly started app
(it fixed itself in 2 of 4 filtered runs, with nothing changed). It stays an open point: phase 2 should repeat the filtered and the unfiltered setup several times
each, restarting Orca between runs, and log the proxy in all of them.

### 11.4 Investigation 4: empty `app_id` on Fedora, `xdg-desktop-portal` 1.22.1

Cause, from the source (tags 1.20.3, 1.21.0, 1.22.1; function `get_app_from_pid` in `src/xdp-app-info-host.c`):

- Up to 1.20 the portal took the application id from the name of the systemd scope of the caller
  (`app[-<launcher>]-<id>-<random>.scope`, `_xdp_app_info_host_parse_app_id_from_unit_name`) and returned it, whatever the id was.
- From **1.21.0** ("Improve the heuristic to detect the App ID for host apps", #1595) it accepts the id from the scope **only if a file
  `<id>.desktop` exists** for it (`g_desktop_app_info_new (<id>.desktop)` in the data folders of the **portal's** process); otherwise the id is empty.
  A host application registered with `org.freedesktop.host.portal.Registry.Register` (since 1.19.4) goes through the same rule
  (`XDP_APP_INFO_FLAG_REQUIRE_GAPPINFO`).

So the empty id was **an effect of my test, not a regression for users**: the test installed into an isolated HOME, so `panpak.<id>.desktop` was in a
folder the portal does not read. Verified on the KDE VM (portal 1.22.1-2, `AddNotification` with the backend watched): entry only in the isolated HOME:
`app_id ""`; the same entry also in the real data folder (what a real install writes): `panpak.org.panpaktest.NotifyProbe`, with and without the router.
On Ubuntu 26.04 (portal 1.21.1) the lab had always worked because it points the portal's data folder at the lab's home. The panpak scope name
(`app-panpak.<id>-<token>.scope`) is read correctly by the 1.22 regular expression.

`Register`, tested from a host process on the KDE VM (portal 1.22.1-2): at path `/org/freedesktop/portal/desktop`, interface
`org.freedesktop.host.portal.Registry`, same bus name as the other portals (not a name of its own). Without a `.desktop` file: `Register` fails
("App info not found"), `app_id ""`. With the file: `Register("panpak.<id>", {})` is accepted, and the next `AddNotification` of the same connection arrives
with `app_id "panpak.<id>"`, even from a process that is **not** in an `app-...scope` (without `Register` it is `""`). A connection can register once, and only
before its first portal call ("Registered too late"). Consequences:

- It works on 1.20, 1.21 and 1.22 (the interface is there since 1.19.4), but it cannot replace the `.desktop` file: `panpak install` already writes
  `panpak.<id>.desktop` into the user's data folder; on a system with a portal older than 1.19.4 only the scope name works.
- It makes the identity independent of how the scope is named or whether the copy is in one (menu, terminal, service), which is the fragile part of
  `docs/DESKTOP-INTEGRATION.md`.
- **It must be called only by us.** A call from the app could claim any id that has a `.desktop` file (another app's) and take its portal permissions.
  In design B the agent calls it first on each connection and never forwards the app's own call. In design A the proxy rule has to allow the call (it is
  the same connection), so the router must drop the call from the app and inject its own, which puts the protection in the router instead of in a rule.
  The router already reads the header of every message; the extra code is a few lines, and it is the kind of rule that needs its own test.
- The call is not in the closed list that `tests/dbus_filter.py` checks (it expects it refused for a proxy started with the product rules); the test would
  change to "refused for the app, done by the stub".

### 11.5 Investigation 5: GTK 4.18 of the Debian image crashes on Fedora 45 KDE

**Cause found, from the sources of GTK and KWin.** Reproduced outside the sandbox (the tree's `gnome-text-editor` started by hand with the stub's library
path): `SIGSEGV`, so not the sandbox, not the security context, not the bus.

- **Crash site.** Core dump: `libgtk-4.so.1 + 0x47d730`, called by `ffi_call`/`wl_closure_invoke` of the host `libwayland-client` 1.26.0 while GTK opens the
  display. The disassembly of the tree's library at that offset is `linux_dmabuf_tranche_done` of `gdk/wayland/gdkdmabuf-wayland.c`:
  `g_ptr_array_add (info->pending_dmabuf_formats->tranches, info->pending_tranche)`. The fault is the read of `pending_dmabuf_formats->tranches`
  (`info + 0x30`, then `+ 0x8`): `pending_dmabuf_formats` is `NULL`. It is only set in `linux_dmabuf_main_device`.
- **Why no `main_device`.** `WAYLAND_DEBUG=1` shows `format_table`, `tranche_target_device`, `tranche_formats`, `tranche_flags`, `tranche_done` and no
  `main_device`. KWin 6.8 (`src/wayland/linuxdmabufv1clientbuffer.cpp`, `LinuxDmaBufV1FeedbackPrivate::send`) sends `main_device` **only if the client's
  object is older than version 6**: `if (resource->version() < ZWP_LINUX_BUFFER_PARAMS_V1_SET_SAMPLING_DEVICE_SINCE_VERSION) send_main_device(...)`. In
  `linux-dmabuf` version 6 `main_device` is replaced by sampling devices in the tranches.
- **Why GTK 4.18 gets version 6.** In GTK 4.18.6 (`gdk/wayland/gdkdisplay-wayland.c`, line 370) the global is bound with the **server's** version:
  `wl_registry_bind (..., &zwp_linux_dmabuf_v1_interface, version)`, and its handlers only know up to version 5. KWin 6.8 advertises version 6 (inferred from the KWin source and from the missing `main_device`; the number itself is to be read on the VM with `wayland-info`).
- **Fixed upstream, but not in 4.18.** Commit `e2d0ac2691` "wayland: Add interface version defines" (Matthias Clasen, 2025-03-31) binds with
  `MIN (version, LINUX_DMABUF_VERSION)` (5). It is in 4.19.0, so in **4.20.0** (2025-08-29) and after; 4.24.1 (the Fedora 45 host) has it. 4.18.6 (2025-06-09)
  is the last 4.18 release and the fix was not backported. No issue in the GTK tracker names `tranche_done` or the dmabuf feedback (searches on
  `tranche_done`, `dmabuf feedback`, `main_device`: none; the only match, #8224, is about sampling feedback support). The `NULL` dereference in
  `linux_dmabuf_tranche_done` itself is still unguarded in 4.24.1 and in `main`: a compositor that sends a tranche without `main_device` crashes any GTK
  that binds a version it does not handle; with the cap that no longer happens.
- **Does it depend on the virtual GPU?** From the code, no: the version KWin advertises and the condition for `main_device` do not look at the GPU. It was seen on
  a virtio GPU; a run with virtio with and without 3D is asked for below as a confirmation, not as the explanation. (Not done: the VM's hardware is changed by the
  owner.)
- **Other things tried:** `GDK_DISABLE=dmabuf` (and with `vulkan`) and `GSK_RENDERER=cairo` do not help (the feedback object is created while the display is
  opened). Only the host's `libgtk-4.so.1` (4.24.1) in front of the tree's libraries fails at once, `undefined symbol: pango_font_description_set_width`: it needs the host's Pango.

**Options** (GTK apps built from source on the Debian 13 image):

| Option | What it is | For | Against |
|---|---|---|---|
| 1. Patch GTK in the image | Rebuild `libgtk-4-1` 4.18.6 from the Debian source package with one change: `MIN (version, 5)` at line 370 (and, as a second line of defence, a `NULL` check in `linux_dmabuf_tranche_done`). The package carries the patched library | Smallest change in behaviour; the app stays on the GTK it was tested with; the patch is the upstream one | A GTK build for every GTK recipe (heavy; time and memory not measured), a patch to keep while the base is 4.18, and the library stops being the distribution's |
| 2. A newer GTK built in the image | GTK 4.20.x or later from source (or from a newer Debian suite) | The fix and others come with it; no patch of ours | Needs newer GLib, Pango, libadwaita and others than Debian 13 has: a stack, not one library; the app is tested on another GTK than the one the recipe was written for |
| 3. A newer base for GTK recipes | Build GTK apps on a Debian suite that already ships GTK 4.20+ (testing) | No build of GTK by us | glibc of the base goes up (`glibc_min` rises, the 2.39 line of 13a); the snapshot discipline holds, but the package then does not run on older hosts |
| 4. A compatibility step in the runtime | Cap the version of `zwp_linux_dmabuf_v1` for apps that need it: a small preloaded library (as `nochown.so` is for the build) that wraps `wl_registry_bind`, or the same cap in a Wayland proxy | No rebuild; works for every GTK 4.18 app, also converted AppImages | `LD_PRELOAD` is forbidden for apps by the `[env]` rules; a Wayland proxy is a large new component; both touch every app to fix one toolkit |
| 5. Do nothing now | List it as a known limit: GTK 4.18 apps crash on compositors that advertise `linux-dmabuf` version 6 (KWin 6.8 and later) | Free | Plasma 6.8 is the current KDE; Ghostty and Text Editor would not start there |

Recommendation for the image: **option 1 for the 13b images** (the patch is one line and exists upstream), with option 2 or 3 as the way out when the base moves to
a Debian suite with GTK 4.20 or later. Option 4 only if converted AppImages with GTK 4.18 turn out to need the same (not seen). The GTK stack can come from the
host only as a whole; the "graphics family" of `SYSTEM_WINS` (Wayland, Mesa) should not grow to GTK.

### 11.6 Comparison

| | A: router + `xdg-dbus-proxy` for each copy | B: one agent |
|---|---|---|
| Processes for an app with N copies | 1 router + N proxies (+ 1 proxy for the accessibility bus for each copy) | 1 agent (+ the accessibility filter inside it) |
| Hops for a message | app, router, proxy, bus: 3 | app, agent, bus: 2 |
| Fixed cost of a start | menu 34.9 ms, terminal 39.0 ms (inside the noise of the proxy-only 35.0 / 38.6) | menu 34.6 ms, terminal 37.5 ms |
| Memory, 1 copy / 3 copies (PSS) | 1.03 MB / 1.9 MB | 0.52 MB / about 0.55 MB |
| Latency of a call (floor) | +47 to +52 us | +11 to +16 us |
| Host requirement | `xdg-dbus-proxy` 0.1.5 or newer at a fixed path (as today; KL-5) | none |
| Code of ours (estimate) | router: 1,000 to 1,400 lines of Rust; the filter stays in a program that is used and tested by others (3,269 lines of C plus 433 of command line) | router 1,000 to 1,400 + the filter, all of the policy of the proxy that we need: policy levels, visibility of names (`ListNames`, `NameOwnerChanged`, `GetNameOwner`), reply tracking by serial, validation of `AddMatch`, `StartServiceByName` for talk names: 2,000 to 2,800 lines more |
| Where the permissions are decided | the proxy (as today) | our code |
| Effect of a bug in the new code | the router loses or delays messages, or lets a message for a local name go local: an availability or logic bug; what reaches the host is still filtered by the proxy | a hole in the filter is a hole in the sandbox |
| Identity with the portal | scope name, as today; `Register` possible with a drop-and-inject rule in the router | `Register` natural (the agent is the only caller) |
| Opener receiver and future file chooser | separate processes as now (the receiver can be moved into the router) | in the same process |
| Tests | the existing closed list works unchanged; new tests for the router | the closed list and the direct checks of `tests/dbus_filter.py` run through the agent too (the judge) |

The judge, run on the skeleton (`tests/dbus_filter.py` with `PANPAK_TEST_PROXY` pointing at the agent): the closed list (102 portal methods of 30
interfaces, with and without the optional permissions: 6 and 8 allowed) gives **the same allowed and refused methods as `xdg-dbus-proxy`**, the control
calls to the other services (secrets, systemd, the shell, the keyring, dconf, the login manager...) are refused alike, and `SettingChanged` crosses. The one
difference is the wanted one: a name the app asks to own is granted (on its private bus), where the proxy refused it.

### 11.7 Recommendation

**Keep design A for step 13p (router + `xdg-dbus-proxy`), build the router so that the filter can move into it later, and take the agent as a
second step only if a reason appears that the numbers do not show today.**

Reasons:

1. **The numbers do not separate the designs**: fixed cost, memory and latency differ by less than the noise of a run or by a few tens of microseconds.
   Nothing about speed or size argues for the agent.
2. **The difference is in code and in risk.** The agent adds 2,000 to 2,800 lines of filter on the security boundary, in place of a program that Flatpak
   uses and others test. The judge shows that the agent can reach the same answers on the cases we have, which is a strong argument that it is *possible*,
   not that it is *as safe*: the proxy has years of cases that the closed list does not. A first release is the wrong time to carry that.
3. **What the agent would add is available in A**: the identity with `Register` (router drops the app's call and injects its own: a few lines and a
   test), the accessibility socket (started by the outer stub, as Flatpak does), and the opener receiver (moved into the router if the number of processes
   matters; each is a process of ours with no rights).
4. **What the agent would remove is the dependency on `xdg-dbus-proxy`** (KL-5: a host package at a fixed path, version 0.1.5 or newer). On the two Fedora
   VMs and on Ubuntu it is installed and current (0.1.7 and 0.1.9), so today it is a cost on paper. The day a target system has no package for it, or an
   audit asks for one policy engine in one language, the agent becomes the work to do, behind the same router and the same judge test.

What I would build in A, in this order, with the findings of this study: the router with names and delivery between copies; `AT_SPI_BUS_ADDRESS` with a
second filtered proxy started by the outer stub (Flatpak's list without the two `NotifyListeners*` calls until a toolkit needs them); `Register` injected by
the router with the app's own call dropped; links through the opener; the media alias; the router as the first user of `tests/dbus_filter.py` run as judge for
every later change. The small corrections found on the way are noted, not made: the opener that returns 0 without opening anything on Bluefin, and
`StartupWMClass` in the desktop entries of the test packages.

## 10. What the owner decides (phase 1; see 11.7 for phase 1b)

1. Router of our own, shared by the copies, in Rust (recommended), or a `dbus-daemon` with a bridge (not recommended).
2. Links through the opener channel (recommended) or through the portal.
3. Media players with an alias carrying the id (recommended) or with no support.
4. Accessibility with a filtered proxy on the AT-SPI bus, started by the outer stub, as a requirement of the first release.
5. The text change at install (risk 4) and the effect on `dbus_own`.
6. Order of work for the product: (a) the router with names, delivery between copies and pass-through, behind the existing
   test `tests/dbus_filter.py` rewritten for the new rule, with the fuzz targets; (b) the accessibility proxy; (c) links;
   (d) the media alias; (e) the install text and docs; (f) the full harness on the 25 AppImages and the three source builds, on
   GNOME and KDE.

## 12. Decision: no `Properties` on the portal until step 11

The rule list does not allow `org.freedesktop.DBus.Properties.Get` and `GetAll` on `/org/freedesktop/portal/desktop`. Section 9 showed that with them
GTK finds the `FileChooser` interface and shows the portal's dialog, and the file it returns is a path of the host that the sandbox cannot read (KL-7).
Until the file chooser of step 11 is given (with the document portal), the user must get a file dialog that works, so GTK keeps its own dialog,
which opens and saves in the folders the app was granted.

What did need it, found on Bluefin: a click on a link in GTK's About dialog did nothing, because GTK reads the `version` property of
`org.freedesktop.portal.OpenURI` before it uses the portal, and without it falls back to a launcher that finds nothing in the view. So the router answers
`Properties.Get(OpenURI, version)` and `GetAll(OpenURI)` itself (version 3) and passes every other `Properties` call to the host, where the rules refuse it.
That is the narrowest rule: it does not reach the file chooser. What does not need `Properties`: the settings (`Settings.Read`, `ReadAll`,
`SettingChanged`), notifications and inhibit are calls of their own interface. Nothing narrower than the pair `Get` and `GetAll` exists for the
FileChooser version: the rule cannot name an interface in the arguments, so allowing one is allowing the file chooser. The complete rule set for the
file chooser comes with step 11.

## 13. Results of the code phase (step 13p)

What runs (tests in `tests/privbus.py`, `tests/dbus_filter.py`, unit tests of `panpak-core::dbus`, `fuzz_router`):

- A, B, C, F11, F22, F23 (names are the app's own and never on the host bus; a native program with the same name is not reached; F22 and F23 closed);
  S (a second launch finds the first through the one router of the app); I and R (identity `panpak.<id>` for each copy, also after the first copy ended; the
  app cannot register one); L (OpenURI answered with the real outcome of `xdg-open`; the version of OpenURI read as GTK reads it); M (media alias); X
  (accessibility: the app is read by a screen reader, `GetChildren` on the root and `RegisterEvent` are refused); R1 to R4 (copies started together share one
  router, a copy killed with SIGKILL does not take it down, it ends with the last copy, also when that one is killed); E (a copy that starts while the router
  decides to end gets a bus: the router ends only under the lock of the app's folder and the test holds that window open for 1.5 s; the same test fails with
  `may_end` always true).
- Real desktops. Bluefin (GNOME): the file dialog is GTK's own and opens and saves in the granted folder; links from Ghostty and from the About dialog of Text
  Editor open Firefox (a Flatpak: `xdg-open` needs `XDG_DATA_DIRS`, which the opener now supplies, defect 55); Orca, 5 rounds with Orca restarted each time, lists the
  sandboxed Text Editor in 0.00 to 0.02 s with 6 nodes and the window title, the same numbers as the Flatpak Text Editor read directly (5 of 5 rounds each; speech was not
  checked by ear). Fedora KDE (Plasma 6.8.2): Text Editor and Ghostty built on the patched GTK (4.18.6+ds-2+panpak1) start and stay open, the task bar shows the
  right icon (with the real desktop entry in place), and links open Firefox (defect 54).
- GTK patch: `panpak/data/patches/gtk4-dmabuf-v5.patch`, applied by rebuilding the `gtk4` source package of the snapshot once per image version (about 10 minutes, 6 GB
  peak). Two independent builds gave the same hash for the packages that go into images (`libgtk-4-1`, `libgtk-4-common`, `libgtk-4-dev`, `libgtk-4-bin`,
  `gir1.2-gtk-4.0`, `libgtk-4-media-gstreamer`); only `gtk-4-examples`, which no image installs, differed. The text for the upstream report is
  not in the repository (it names versions of the author's machines); the cause is in section 10.

Measures. Machine: the author's workstation, release build, medians; every comparison with the prototype or with `main` was run in the same series.

| What | Method | Study (prototype) | First product build | Product now |
|---|---|---|---|---|
| Round trip of a call through the layer, bus daemon `Ping` over a raw socket (no client library) | the bench of the study (`lat.py`: 1000 pings, three alternating runs), the product measured by the same messages from a probe inside the sandbox (`tests/privbus.py`, mode `rawlat`) | prototype router and proxy 66 to 70 us, proxy alone 56, no layer 22 (same series) | 170 to 240 us | 77 to 83 us (+55 to 60 us over no layer, +10 over the prototype) |
| Portal call through the layer (`Settings.ReadOne`, Python fake backend, GDBus client) | `tests/privbus.py`, mode `lat`, 2000 calls | no layer 486 to 506 us | 843 to 959 us | 650 to 700 us (+150 to 200 us) |
| Memory of the router | `Pss` and `Rss` of `/proc/<pid>/smaps_rollup` (the router was run dumpable only for this measure) | PSS 0.55 MB (1.03 for router and proxy together) | RSS 2.3 MB | PSS 0.9 to 1.15 MB, RSS 2.3 MB, anonymous 0.38 MB |
| Memory of one proxy | same | PSS 0.48 MB | RSS 6.3 MB | PSS 0.48 MB, RSS 6.0 MB |
| Fixed cost, from a terminal (the stub makes its own scope), D-Bus layer | `tests/startup_budget.py --dbus --runs 30`, two rounds, `main` and the branch alternating | 36 to 37 ms (another day) | 41.7 ms | `main` 40.5 to 41.0 ms, branch 41.0 to 41.8 ms (+0.5 to 0.8) |
| Fixed cost, from the menu (the launcher is in a scope the portal reads as the app: `systemd-run --user --scope --unit app-panpak.org.example.budget-<n>.scope` around the script) | same, inside that scope | 34.6 to 35.0 ms (study setup) | not measured | `main` 26.9 to 27.3 ms, branch 27.5 to 27.6 ms (+0.3 to 0.6); no layer 25 ms |

Where the differences come from:

- Memory. RSS counts every page of a process that is in memory, including the pages of files it maps and shares with other processes. The proxy maps GLib and
  the other libraries: 6 MB of RSS, of which only 0.48 MB is its own (PSS divides shared pages by the number of processes that share them). The router is the stub
  itself (3.5 MB of static program, shared with the stub and the receiver): 2.3 MB of RSS, 0.38 MB of its own memory (anonymous), and a PSS of 0.9 to 1.15 MB that
  depends on how many other processes have the stub mapped at that moment. The prototype was a smaller binary. The router stays at 0.38 MB of its own memory
  with a read buffer of 64 KiB; two copies of an app share one router, so the cost of a second copy is one proxy (0.48 MB PSS).
- Round trip. Profiling the first build with `strace` (the router run under it, dumpable for the measure) showed two system calls for every message beyond the
  minimum of `poll`, `recvmsg` and `sendmsg`: a map and an unmap of 64 KiB, the buffer made again for each read, with the page faults of zeroing it. One buffer
  for the life of the router removed them; the sequence is now `poll`, `recvmsg`, `sendmsg` for each message in each direction. `perf` is not allowed here
  (`perf_event_paranoid` is 4), so the rest was not profiled in more detail; the remaining +10 us over the prototype is the parse of the header, the
  state machine and the allocation of the message, and the seccomp filter of the client in the sandbox.
- Start. The stages printed by `PANPAK_DEBUG` are the same in `main` and in the branch up to the proxy being ready (20.5 ms against 20.7 ms: the router adds 0.2 ms, because
  its start is done by the worker thread in parallel with the proxy). The rest was in the exit: the stub waited for the thread that serves the router's requests and killed the
  two proxies one after the other (2 to 3 ms). They are now stopped together and the thread is not waited for. The 36 to 37 ms of the study from a terminal are not
  reproduced today by either `main` or the branch (40.5 to 41.8 ms): the difference is the machine on the day, not the layer. The budget of 45 ms with the layer is
  unchanged.
- The router stays alive 1.5 s after the last copy; it no longer holds the caller's error output open (a script that read it waited 1.5 s; found by the budget test).

### xdg-dbus-proxy 0.1.5

Found by the CI, which runs the tests for real on Ubuntu 24.04 (xdg-dbus-proxy 0.1.5, portal 1.18.4); the development machines had 0.1.7. Two behaviors of the older proxy broke the router
(the app had a bus that closed at once): the handshake (`AUTH`, `NEGOTIATE_UNIX_FD`, `BEGIN`) must come one line at a time, and the serials of the messages on a connection must grow
("Versions older than 0.1.6 required monotonically increasing serials", `flatpak-proxy.c`). The router's own messages (the identity, the owner of the portal) used a range of their own and so came
between the app's. Now every message that goes to the host side is numbered by the router, in order, and a reply gets the number of its sender back; the fuzz target and a unit test check that the
serials grow. On that runner all the scenarios of `tests/privbus.py` pass; the closed list of `tests/dbus_filter.py` does not run there (the 1.18 portal with the lab's fake backend exports no
Settings).
