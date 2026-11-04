# Panpak — threat model

Status: v1 (runtime ABI 1). Empirically verified where stated (tests),
assumed where explicitly marked.

Every limit known today is listed in one place, §5 "Known limitations", with
its practical consequence and where it is described.

## 1. Attackers considered

- **A1 — malicious package**: `.panpak` built by the attacker, run by
  the victim. Goal: sandbox escape, persistence, data access.
- **A2 — compromised app**: legitimate app with an exploit (e.g. parser).
  Already running inside the sandbox. Goal: writes outside the allowed
  zones, reading other apps' data, privilege escalation.
- **A3 — hostile update**: package or key replacement. Goal: make the
  victim run code unauthorized by the original author.
- **A4 — network and kernel**: out of model (no network filtering, no
  seccomp, see §3 and §5).

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
| `install` overwrites a launcher or `.desktop` file of another program | A file is replaced only if it carries Panpak's marker for the same app (`# X-Panpak-App-Id:` in the launcher, `X-Panpak-App-Id=` in the `.desktop` file) or is one written by an older panpak. The check runs before the store is touched. Test: `foreign_files_are_not_overwritten` |
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
`ro` raised to `rw`) is not published without an explicit yes: an answer at a
terminal, or `--accept-new-permissions`. `--yes` never accepts them
(`PANPAK-E5004`; `PANPAK-E1003` when the answer is no). Installing a package over an
installed app follows the same rule.

### 2.3 User folders, links and copies of an app (tested in tests/view.py)

- **`xdg_dirs`.** The folder is found by `install` outside the sandbox, with a rigid parser, shown to the user
  and pinned. At start it is granted only if it is still the same, with no symbolic link in the way; a
  `user-dirs.dirs` changed later by anything the app can reach (it cannot write it) or by the user gives
  a warning and no access. The folder passes the same rules as `home_paths`. Listing the home folder
  is allowed only in the isolated view, where the home folder holds just the granted folders.
- **Opening addresses (new outward channel).** The app can ask the outer runtime to run the system `xdg-open`.
  The outer runtime is the only one that runs it, with its own environment, one argument and no shell. The
  rules are in PANPAK-FORMAT.md §9.3: `http`, `https`, `mailto` only, bounded length, 5 per second
  with a burst of 3, at most 4 processes alive.
  - *`file:` and paths* are refused: they would make an application outside the sandbox open a file the
    app cannot read.
  - *`mailto` and attachments.* Some mail clients take `attach=`, `attachment=` or similar fields in a
    `mailto:` address and attach that local file to the message. The attacker then reads a file of the user
    that Landlock denied (for example `~/.ssh/id_ed25519`), by sending a mail the user may only have to confirm.
    Only `to`, `cc`, `bcc`, `subject` and `body` are passed on, field names are compared as written, and
    a line break may only be in `body`, so a header cannot be added to a mail by a subject. Tested with
    `attach`, `Attach`, `%61ttach`, `x-attach`, `;attach` and `?x?attach`.
  - *Remaining exposure.* The app can make the user's browser or mail client open any web or mail address,
    including a page that tries to deceive the user, and can do it five times a second at first;
    the user sees the window. The address is not hidden from the user by the runtime.
  - *Detached.* The system `xdg-open` runs in a session of its own with no inherited descriptor (the job
    descriptor of the view is not close-on-exec, and is closed for it), so it holds no handle of the app and
    no signal of the app or the stub reaches it.
  - *Browsers started by a full path.* An app that starts `/usr/bin/firefox` itself, without `xdg-open`, is not
    intercepted: the browser runs inside the sandbox with the same limits as the app (no home, no sockets
    of the session), which usually makes it useless rather than dangerous. An `xdg-open` at a path that
    is not in the `PATH` of the outer runtime is not replaced either.
  - *Without the isolated view* nothing is intercepted; the app runs the system `xdg-open` inside Landlock.
- **Copies of the same app** share `/tmp` and `/dev/shm` of their own (not of the machine). The
  shared folder is 0700 in the runtime folder of the user: another user does not reach it, another app does not
  know its path or see it in the view. A copy can read and replace the files of another copy of the same
  app: they are the same app with the same rights. Without `x11 = true` two copies each have their own IPC
  namespace and a single-instance guard based on System V IPC does not see the other copy (known limit).

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
  services (D-Bus): this app is not isolated". An app that does not list a bus
  socket gets no bus, or, in the isolated view with a systemd user manager and
  `xdg-dbus-proxy`, the filtered bus below.

**The filtered session bus (PANPAK-FORMAT.md §9.5; tested in tests/dbus_filter.py).**
An app in the isolated view can have a session bus that goes through `xdg-dbus-proxy`, one per start.
What it can do: read the appearance settings and be told when they change; ask the desktop not to
sleep or lock (Inhibit); own and reach its own application id, so that copies of one app find each other
(single instance); and, with the permissions, show notifications, put a tray icon, own up to
8 names declared in the manifest. What it cannot do: everything else on the bus. The test calls every
method of the 30 interfaces that the real portal exports on a private bus (102 methods) through the
proxy with the product rules: 6 reach the portal (Settings, Inhibit), 8 with `notifications`,
the others are refused by the proxy; names of other services (`org.freedesktop.secrets`,
`org.freedesktop.systemd1`, the keyring, `ca.desrt.dconf`, the shell, the accessibility bus, `login1`,
`PolicyKit1`) are refused even though a service answers on each of them. The rules never use `--talk` for the
portal: `--talk` would open every interface. The same checks give the same result with the proxy 0.1.5 of
Ubuntu 24.04 (closed list and direct checks; the stub itself was run only with the system proxy, 0.1.7).
- *Identity.* The portal treats a host app as trusted (location is exact, background is
  always allowed, notifications are not asked, file choosers return real paths). The id it
  uses comes from the systemd scope of the caller, so the outer stub always makes its own scope,
  `app-panpak.<id>-<n>.scope`, even when started from a terminal; otherwise the app would be
  known by the id of the terminal (`app-gnome-org.gnome.Ptyxis-…`), with its permissions. The id is
  `panpak.<id>`, never the upstream id: an app converted from a Flatpak or an AppImage that has the
  Flathub id does not share the grants, documents or desktop entry of that Flatpak. The app can never call
  `org.freedesktop.host.portal.Registry.Register`, which would let it choose any id that has a desktop file.
- *Documents.* The app sees only the files granted to `panpak.<id>`, in `$XDG_RUNTIME_DIR/doc`, with the rights
  the outer stub gave each file, and the user can revoke them in the desktop settings. The app never has the document portal.
- *Addresses.* The portal's `OpenURI` is never given: the portal passes `mailto:a@b?attach=/home/user/.ssh/id_ed25519`
  to the mail program unchanged (tested in the study), and the proxy cannot look at arguments. Addresses
  go through the stub's channel (PANPAK-FORMAT.md §9.3).
- *Notification signals.* `NotificationClosed` of the older service is sent to everyone (measured on the GNOME Shell of
  the development machine: the message has no destination). `ActionInvoked` was not measured, because it needs a click;
  it is treated the same. Any client with a match rule would see them. Not forwarded.
- *Tray.* The watcher sees the app's object and, through `RegisteredStatusNotifierItems`, which other icons exist.
- *Attack surface.* The proxy parses the D-Bus protocol of an app that is not trusted, in C with GLib. It has
  been used by Flatpak for years, and it is a system package: Panpak does not ship it. The Rust client in the
  outer stub (`panpak-core::dbus`) parses the replies of the bus with strict limits and is fuzzed
  (`fuzz_dbus`, `fuzz_dbus_names`); it never reads from the app today, and was written to be able to.

  What the stub does today, and what it does not: it removes `SSH_AUTH_SOCK`,
  `GPG_AGENT_INFO` and `SSH_AGENT_PID` from the environment of every app, and it
  sets `DBUS_SESSION_BUS_ADDRESS` and `DBUS_SYSTEM_BUS_ADDRESS` to `disabled:` unless the manifest
  lists that bus. Libdbus, GDBus and sd-bus then report no bus (tested against a
  private bus; sd-bus would otherwise find `$XDG_RUNTIME_DIR/bus`). This is
  hygiene. An app that opens the socket by path still reaches the bus below
  Landlock V9. The system bus gets the same treatment: `DBUS_SYSTEM_BUS_ADDRESS`
  is `disabled:` unless `sockets_allow` lists `.../system_bus_socket`. Each address
  is kept only for the bus the manifest names.
  One function (`panpak_core::socket_isolation`) decides from the kernel, the isolated
  view and the permissions whether an app is isolated from local sockets, and reports
  which layers are active. `panpak doctor` and `install` show them ("isolation layers:
  ..."); `install` and `update` print "WARNING: on this kernel the sandbox cannot block
  local sockets (D-Bus, ssh-agent, gpg-agent): this app is only partially isolated" when
  no layer blocks path sockets, and a warning about abstract sockets when X11 access (or
  a kernel below Landlock V6) leaves them open.

- **Layers against local sockets.** Each layer works without the others, and the stub uses
  every layer the machine offers:

  | Layer | Needs | Blocks | Does not block |
  |---|---|---|---|
  | Isolated view (bubblewrap) | `/usr/bin/bwrap` that works (user namespaces allowed) | path sockets anywhere: the view holds only `/usr`, `/etc`, `/sys`, `/dev`, `/proc`, the Nix and Guix stores and the folders the app is granted, so the app sees only the sockets the policy names | abstract sockets (the network namespace is shared); a socket inside a folder the app is granted (the user's own choice) or inside `/usr`, `/etc` or `/dev` (root-owned system folders that hold none on the systems tested) |
  | Landlock V9 (Linux 7.1) | kernel | `connect()` to any path socket that the policy does not name, wherever it is | |
  | Landlock V6 scope | kernel 6.12 | abstract Unix sockets and signals outside the sandbox (off with `x11 = true`) | |
  | none | | | everything, as before: the warning above |

  The policy is the same for the two path layers: the sockets in `sockets_allow`, plus the
  sockets every app gets (the Wayland compositor, the journal, the resolver of systemd-resolved,
  and with `audio = true` PipeWire and PulseAudio). On Landlock V8 and below the view is the
  only layer for path sockets. The V9 rules were tested on Fedora 44 (Bluefin, kernel 7.2.7,
  SELinux enforcing): `tests/view.py` passes there, and the test on that kernel found that the
  rules for the sockets every app may reach were dropped on the musl build (fixed; the stub now
  prints a warning when it leaves any rule out).

  **The isolated view.** The stub runs `/usr/bin/bwrap` (a fixed path, never `PATH`, and it
  must not be writable by others) with: a new tmpfs as `/` (read-only) in which only `/usr`, `/etc` and `/sys` of the host (read-only), the Nix and Guix stores, the top-level links (`/bin`, `/lib*`, `/home` when it is a link) and `/dev` as it is are put; `/var` is a new empty tmpfs with the links `/var/run` and `/var/lock`; `/opt`, `/srv`, `/mnt`, `/media`, `/root`, `/boot` and `/snap` do not exist; a new `/proc`; new
  tmpfs for `/tmp` and `/run`; an empty tmpfs on `$HOME`, made read-only after the binds,
  with only these paths put back: the app folders (writable), the `home_paths` (as declared), the
  settings Landlock lets the app read (read-only, those that exist), with X11 the Xauthority file
  (read-only), a file opened with `panpak open` (as granted), and from the store only the tree of the
  app, its library folder (`farms/<root>`), the loader and the stub (read-only); a new tmpfs for
  the runtime folder holding only the allowed sockets; the resolver folder in `/run` read-only.
  The rest of the store (other trees, packages, blobs, the cache of other apps, pins) and the rest of
  the home folder do not exist in the view (tested with fake sockets in `~/.ssh` and `~/.gnupg`, a secret
  in `~/.config/gh` and the data folder of another app). With the home folder as a mount of its own the
  paths are resolved with `canonicalize` first. Inside, the session bus, the system bus, systemd of the user, gpg-agent and
  ssh-agent do not exist (tested with fake sockets in a private runtime folder).
  Why this set (defect found with real apps: a view with `/` read-only showed every socket under `/var`, `/opt`, `/srv`, such as the LXD or a database socket, and `connect()` to a path socket is not mediated by Landlock up to V8): Landlock only lets the app read `/usr`, `/etc`, `/sys`, `/proc`, `/run`, `/dev` and its own folders, so a folder outside that set was reachable only by `connect()`, never useful to read. `/usr` and `/etc` hold the libraries, certificates (`/etc/ssl`), fontconfig (`/etc/fonts`), `machine-id`, `localtime` and `/usr/share/zoneinfo` and locale data. `/sys` holds the GPU and input device data that Mesa and Qt read. `/var` has nothing the app may read (its caches, `/var/lib/dbus` and the certificate store of openSUSE are denied by Landlock anyway), but the links `/var/run` and `/var/lock` keep old paths such as `/var/run/dbus/system_bus_socket` leading to the new `/run`. `/opt` was in the first idea of the change and was left out for the same reason (Landlock never let the app read it). A test builds a fake host with sockets in `/var`, `/opt`, `/srv`, `/mnt`, `/media`, `/root`, `/boot`, `/snap` and Nix, scans the whole visible tree of the old shape of the view (finds them) and of the new view (finds none). Known cost: where the users come from SSSD or another daemon with a socket in `/var`, the app cannot ask it.
  **Stale lock files.** Because the view has its own PID namespace, the app is always process 2 and
  process ids repeat; a lock file with a process id left by a killed copy is never stale for the
  next one. The runtime removes such files when the per-app lock shows that no copy is alive and the mark
  `userdata/<id>.in-use` shows that the last run did not end normally (see PANPAK-FORMAT.md §9.4). The app can
  neither see nor reach the mark; deleting the files it can reach only makes its own next start skip the search. What this deletes is bounded: only files recognised by content, only in the app's
  own `config`, `data`, `state` and cache folders, at most 3 levels deep, no link followed (the link is
  removed, never its target), each unlink relative to the descriptor of its folder, so an app cannot make
  the runtime delete a file elsewhere by putting a link in its own folder. A random first process id was
  not used: it needs a capability in the user namespace, and the AppArmor profile that lets bubblewrap
  work on Ubuntu 24.04 and later removes the capabilities of its children.
  `--unshare-pid` hides the other processes of the user; `--unshare-ipc` is on unless the app
  has X11 access (MIT-SHM needs shared IPC); `--new-session` keeps the app out of the session
  of the terminal (no key injection with `TIOCSTI`; the cost is that Ctrl-C in the terminal does
  not reach an app started from it, but it ends the stub, which ends the app);
  `--die-with-parent`; `--unshare-net` when the manifest asks for no network (the app keeps a loopback
  interface and nothing else; path sockets, which the view mounts, do not depend on it).
  `TMPDIR` is `/tmp` there, a tmpfs of its own.
  Landlock is applied on top, inside the view, so the two layers add up: the view makes the
  folder read-only, Landlock decides what may be read at all (the user's secrets are not in the view at all; what is in it and Landlock does not allow, such as the tree of another path, gets `EACCES`).

  **Starting inside the view without checking the package twice.** The outer stub verifies the
  package as it always did (signature, pin, stub hash, index, tree), then starts bubblewrap
  with the inner stub (`--panpak-inner <fd>`). The decisions travel in a sealed memfd (no
  write, no resize, no further sealing) that the inner stub reads and closes before it starts
  the app. This is safe because the inner mode only adds restrictions to a process that already
  runs as the user: it applies Landlock and starts the program it is told to start, and the
  same user could start that program directly. Read from the code: the inner mode (`run_inner`) reads
  the sealed memfd and closes it, applies the same Landlock rules as the start without the view
  (`sandbox`, same function), sets the environment and executes the app. It creates no folder and
  writes no file. Its Landlock self-test opens an unnamed temporary file (`O_TMPFILE`) in the tree, which
  must fail and leaves nothing behind. Two differences from the start without the view, both in the job
  the outer stub makes: the runtime folder, a tmpfs of the view's own, is writable, and with X11 the
  Xauthority file is readable. Landlock keeps the existing fallback to the whole `/dev` (with a warning)
  when the kernel refuses the per-node rules; that is the same in both modes. The app cannot use it to escape: it would need to
  run the stub (outside its Landlock rules), and Landlock rules only accumulate. A tampered
  tree between the check and the start is possible with or without the view: the stub does not
  rehash the tree at every start. Checking again inside would cost about as much as the start
  without the view (15 ms, measured) and would prove nothing more.

  **Not available.** `doctor` says why and how to fix it: bubblewrap missing (install the
  `bubblewrap` package), user namespaces off (`user.max_user_namespaces`), AppArmor
  (`kernel.apparmor_restrict_unprivileged_userns=1` needs a profile for `/usr/bin/bwrap`, as
  Ubuntu ships). The stub then starts the app as before, with Landlock alone. The result of the
  quick test is kept in `$XDG_RUNTIME_DIR/panpak/view-probe`, bound to the inode and time of
  `bwrap` and to the kernel; only a success is kept. A wrong entry cannot weaken the sandbox
  (bubblewrap would fail to start the app).
- **Socket probe (measurement, off by default).** With `PANPAK_SOCKET_PROBE=<file>` the stub
  stays as a supervisor and logs the `connect`, `sendto`, `sendmsg`, `sendmmsg` and namespace
  calls of an app. It answers CONTINUE every time, so it protects nothing: an app can change what
  it asked for after the supervisor read it. It exists to measure what apps do and to design the
  isolation levels in docs/SOCKET-PROBE.md. A manifest cannot turn it on, and `PANPAK_*` names are
  refused in `launch.env`.
- **`gsettings` is run outside the sandbox.** The stub looks for it only at
  `/usr/bin/gsettings` and `/bin/gsettings`, never in `PATH`, so a program
  placed in a folder of the user's `PATH` is not run by the stub.
- **Network ports**: `network_allow_tcp` and `network_bind_tcp` use the Landlock
  network rules (ABI V4, kernel 6.7 or later). They filter by TCP port, not by
  address: a port opened for `127.0.0.1` is open for any address the app
  uses. UDP is not filtered by Landlock (see below for what the view adds). Checked with Joplin, whose local server on port
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
  which prints a warning at every start. **Landlock never filters UDP,
  in any ABI** (its rules cover TCP bind and connect only). What stops UDP is the isolated view: when
  the manifest asks for no network (`network` not true and no TCP port) the view gets a network
  namespace of its own, with only the loopback interface, so a datagram, a name lookup or any other
  protocol has nowhere to go (tested: `tests/view.py`, a listener on the host receives nothing). The
  resolver socket of systemd is not in the view of such an app, so the resolver cannot be used to look
  names up for it. **Without the view the limit is exact:** with `network = false`, Landlock refuses
  TCP `connect` and `bind`, and an app can still send UDP datagrams (to loopback, to the local network
  and to the Internet, DNS included) and use any non-TCP protocol the kernel offers; that is an
  exfiltration channel. An app that asks for network (`network = true` or a port) shares the network
  namespace of the host and has the resolver; UDP is open for it in both cases.
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
- **Path Unix sockets (ABI ≥ V9, or the isolated view)**: `sockets_allow`
  allowlist. Without V9 and without a usable view, the entry is refused if
  present. Up to and including V8, Landlock does not mediate `connect()` to
  filesystem sockets: only the isolated view hides them.
- **Per-ABI table** (best-effort detected with HardRequirement, never
  silent clamp; kernel 7.0: **V8**, kernel 7.2.7: **V9**):

| ABI | kernel | FS base | Refer | Truncate | Net TCP | Ioctl | sig/abstract scope | Unix path |
|---|---|---|---|---|---|---|---|---|
| V1 | 5.13 | ✓ | — | — | — | — | — | — |
| V2 | 5.19 | ✓ | ✓ | — | — | — | — | — |
| V3 | 6.2 | ✓ | ✓ | ✓ | — | — | — | — |
| V4 | 6.7 | ✓ | ✓ | ✓ | ✓ | — | — | — |
| V5/V6 | 6.10/6.12 | ✓ | ✓ | ✓ | ✓ | ✓ | (V6) ✓ | — |
| V7/V8 | newer | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — |
| V9 | 7.1 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |

  On ABI < V4 the sandbox starts ONLY with explicit
  `--network-unfiltered` (warning at every start): the network stays
  unfiltered. Without the flag, refusal.
- **No seccomp**: syscalls that are dangerous but legal for DAC (e.g.
  `ptrace` on same-uid processes outside the sandbox? No: Yama scope 1
  blocks non-children; `process_vm_readv` toward same-uid processes,
  keyrings, etc. remain).
- **`/proc`, `/sys`, `/run` readable**: process/host enumeration,
  readable system D-Bus. In the isolated view `/run` is a tmpfs with only the
  resolver folder, the journal sockets and the allowed sockets, and `/proc` shows
  only the processes of the app. Per-node `/dev` + dri/shm: GPU render nodes
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
  to V9 (kernel 7.0: V8, kernel 7.2.7: V9). Below 5.13 = hard error, never free run.
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

## 5. Known limitations

One list of what Panpak does not do or does not guarantee today. Each entry says what
the limit is, what it means in practice, and where the detail is. The entries in §3 and
§4 give the reasons; this section is the summary to read before relying on the sandbox.
Status of the project: pre-alpha.

**Sockets, buses and the desktop**

- **KL-1 X11 gives no isolation.** With `x11 = true` the app can read the keys and the
  screen of other windows of the same X server and inject input. The option also turns off
  the abstract-socket scope, so non-X11 abstract sockets are open too. In practice: use
  Wayland where possible and do not grant `x11` to an app you do not trust. `install` and
  every start print a warning. See §3 (X11 opt-in) and `PANPAK-FORMAT.md` §3.1.
- **KL-2 Copies of an app without X11 do not see each other's System V IPC.** Each copy has
  its own IPC namespace, so a single-instance guard that uses System V semaphores or
  shared memory does not find the other copy. In practice: such an app may open a second
  window instead of passing the request to the first. Copies share `/tmp` and `/dev/shm`,
  so guards based on files or sockets there work. See §2.3 and `CHANGELOG.md`.
- **KL-3 Landlock 8 and below has no rules for sockets by path.** `connect()` to a Unix
  socket is not mediated before ABI V9 (Linux 7.1). In practice: on these kernels only the
  isolated view keeps the session bus, ssh-agent, gpg-agent and the sockets of container
  managers out of reach, and the view is the only layer (§3, "Layers against local
  sockets"). The V9 rules were tested on one system (Fedora 44, kernel 7.2.7).
- **KL-4 Without bubblewrap the app runs with Landlock alone.** If `/usr/bin/bwrap` is
  missing, user namespaces are off, or AppArmor blocks it (Ubuntu needs a profile for
  `bwrap`), the view is not used. In practice: on a kernel below V9 the app can reach the
  session bus and agents through their sockets, `/proc`, `/sys` and `/run` are readable,
  `/tmp` is shared with every process of the user, and UDP is open (KL-15). `doctor` says which layers are
  active and why the view is missing; `install` and `update` warn. See §3 ("Not available").
- **KL-5 The bus is filtered only in the isolated view, and only with the needed system parts.** The filtered
  session bus (§3, "The filtered session bus") needs the isolated view, a systemd user manager, and
  `xdg-dbus-proxy` 0.1.5 or newer at a fixed path. Without one of them, or with a manifest that lists the
  bus socket in `sockets_allow`, the app has no bus (or the whole bus, the second case), so notifications, tray icons and the
  appearance portal are missing. Not mediated yet: the file chooser (KL-7), the clipboard, global menus. `doctor` says
  which part is missing. See `docs/COMPATIBILITY.md` (D-Bus summary), `PORTAL-DESIGN.md`, `docs/PORTALS-STUDY.md`.
- **KL-6 No keyring.** `org.freedesktop.secrets` is never given to an app, because it would
  expose every password of the user's keyring. In practice: apps that store a password
  there (Bitwarden and Obsidian ask for it in the tests) start without a keyring, and the
  feature that uses it is not available. See `docs/COMPATIBILITY.md` (D-Bus summary).
- **KL-7 The file chooser and the clipboard are not mediated.** They travel over Wayland
  or D-Bus, which the app can reach. In practice: the clipboard is not protected, and the app
  does not get the system (portal) file chooser; `panpak open` and `xdg_dirs` grant files and
  folders instead. See §4 and
  `PORTAL-DESIGN.md`.
- **KL-8 Opening addresses is a channel to the user's programs.** An app can make the user's
  browser or mail client open any `http`, `https` or `mailto` address, up to 5 a second at
  first. In practice: it can show a deceptive page; it cannot open files or attach them.
  A browser the app starts by its full path runs inside the sandbox and is usually
  useless. See §2.3 and `PANPAK-FORMAT.md` §9.3.

**Processes, files and the system**

- **KL-9 The internal sandbox of Electron and Chromium is not available.** Chromium needs
  user namespaces or a setuid helper for its own sandbox, and a user namespace cannot be
  nested in the view. In practice: Electron apps run in Panpak's sandbox only. Most start
  (Jitsi, Obsidian, Zulip and others, see the compatibility table), and balenaEtcher aborts
  on its SUID helper. See `docs/COMPATIBILITY.md` (Defects).
- **KL-10 No seccomp.** System calls that are legal for the user stay available, for example
  `process_vm_readv` toward processes of the same user outside the view and keyrings. The
  view hides other processes with its own PID namespace; without the view, Yama (ptrace scope 1)
  limits `ptrace` and nothing else stops these calls. See §3 ("No seccomp").
- **KL-11 `/proc`, `/sys` and `/run` are readable without the view.** The app can list
  processes and read the system bus socket directory. With the view `/proc` shows only the
  app, and `/run` holds only the allowed sockets. Render nodes of the GPU are shared with
  the whole desktop. See §3.
- **KL-12 Stale lock files in formats Panpak does not recognise.** After a kill or a crash
  the runtime removes the lock files it recognises by content (Chromium, Qt `QLockFile`,
  Firefox style). Any other format stays. In practice: an app with its own lock format may
  refuse to start after a crash; `panpak data unlock <id>` removes the recognised ones, and
  `panpak data clear <id>` removes all data. See §3 ("Stale lock files") and
  `PANPAK-FORMAT.md` §9.4.
- **KL-13 Shared `/tmp`.** Without the view, `/tmp` is the machine's `/tmp`: visible and
  writable by every process of the user, with the usual race and link attacks on
  predictable names. In the view it is shared only among copies of the same app. See §3.
- **KL-14 `rw` home paths are open to the whole user session.** An `rw` path from
  `home_paths` or `xdg_dirs` is readable and writable by the app and by every other process
  of the user. The list of refused places is incomplete by nature. `install` shows
  "Partial access to your home". See §2.2.

**Network**

- **KL-15 Landlock never filters UDP.** Landlock filters TCP `bind` and `connect` only. In the
  isolated view an app that asks for no network has a network namespace of its own with only the
  loopback interface, so UDP, DNS and every other protocol are cut. Without the view (KL-4), with
  `network = false` the app cannot connect by TCP but can still send UDP datagrams to loopback, the
  local network and the Internet, DNS included, and use any other non-TCP protocol: a way to send data
  out. An app that asks for network has UDP in both cases. See §3 ("TCP networking").
- **KL-16 Ports, not addresses; no ephemeral ports.** A rule for a port opens it for every
  address, and a rule cannot express `bind` on port 0 followed by a connect to the
  assigned port. In practice: apps that use a random local port (OpenShot thumbnails) do
  not work with a network allowlist. On kernels below V4 the network cannot be filtered
  at all (the stub refuses unless `--network-unfiltered`). See §3.

**Trust and the store**

- **KL-17 The first key is pinned without proof.** Trust on first use shows the
  fingerprint once; a hostile channel at that moment can supply the wrong key. In practice:
  compare the fingerprint with one published elsewhere. See §3 ("Initial TOFU").
- **KL-18 The local store is trusted.** The store is under the user's account, and a
  process of the same user outside the sandbox is out of the model. The tree is checked
  when it is built and not rehashed at every start, so a change between the check and the
  start is possible. See §1 and §3.
- **KL-19 The theme is read outside the sandbox.** At start the stub runs `gsettings`
  (found only at `/usr/bin` or `/bin`, at most 200 ms) to set the dark theme. In practice:
  the app does not follow a later change of theme. See §3.
- **KL-20 The socket probe protects nothing.** `PANPAK_SOCKET_PROBE` is a measurement mode,
  off by default, that logs the calls of an app. See §3 and `docs/SOCKET-PROBE.md`.

**Platform**

- **KL-21 Kernel and distribution.** Landlock needs Linux 5.13 or later; network rules need
  6.7, scopes 6.12, socket paths 7.1 (§3 table). Without Landlock the stub refuses to run.
  Alpine and other musl hosts have no host libc: only packages that carry glibc, or static
  programs, run there. The isolated view is tested on Ubuntu (AppArmor profile for `bwrap`)
  and Fedora (SELinux enforcing); other combinations are not tested. See §4 and
  `distro-matrix.md`.
- **KL-22 Conversion of AppImages has gaps.** Absolute symbolic links, libraries found
  through a directory that only `AppRun` adds to `LD_LIBRARY_PATH`, DwarFS images and
  images that rely on system libraries that are absent are not handled or are reported
  late. In practice: some AppImages do not convert or do not start. See
  `docs/COMPATIBILITY.md` (Defects).

- **KL-23 No input methods through the bus.** `org.freedesktop.portal.IBus`, `org.ibus` and `org.fcitx` are not given. In
  practice: apps in the view cannot use IBus or Fcitx, so typing Chinese, Japanese or Korean (and other languages
  that need an input method) does not work in them, except through what the toolkit does without the bus. The portal
  of IBus (`IBUS_USE_PORTAL=1`) is a candidate for later; it was not tried because the test
  setup has no input method engine.
- **KL-24 No accessibility bus.** `org.a11y.Bus` is not given: it hands out the address of a bus where every
  application exposes its widgets and where input can be sent, so one app could read and drive the others. In
  practice: screen readers and other assistive tools do not see the apps in the view. A permission with a bus of its
  own and a closed list is a possible later step.
- **KL-25 Tray icon names can collide.** Chromium and Electron ask for `org.freedesktop.StatusNotifierItem-<pid>-<n>`, and the
  `pid` is the one in the view (always 2). The proxy has no wildcard inside a name, so the runtime lists the names for
  pid 2 and the first three icons. Two such apps with a tray icon at the same time ask for the same name and the
  second does not get it (inferred, not measured; the second test app did not connect to the bus). Qt 6 registers with its unique name and is not
  affected.
- **KL-26 Actions of the older notification service are not delivered.** The signals that carry them are broadcasts
  (KL-5). In practice: notifications of apps that use `org.freedesktop.Notifications` (for example Qt's `showMessage`)
  are shown, but a click or an action button may not reach the app (which apps lose actions is to be measured with real apps). Apps that use the portal are not affected.
- **KL-27 The single-instance name is the upstream id.** The app owns its own `app.id` on the session bus, as GTK and KDE
  applications do for single instance. A Flatpak or a distribution package of the same program that runs on the same bus competes for the
  name, and a request from one can reach the other. Other programs cannot own or talk to that name unless the user runs them.
