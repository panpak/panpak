# Socket probe and socket isolation

## The probe (measurement only)

`PANPAK_SOCKET_PROBE=<log file>` makes `panpak-stub` log what an app does with
sockets. It is a way to MEASURE. It checks nothing and blocks nothing, and it is
never on unless the user sets the variable for one run (a manifest cannot).

```
PANPAK_SOCKET_PROBE=/path/to/log.jsonl ~/.local/bin/org.example.app
```

How it works (x86-64, Linux 5.6 or later):

- The stub stays alive as the supervisor (`PR_SET_CHILD_SUBREAPER`) and runs the app as
  its child (`PR_SET_PDEATHSIG`). The log file is opened before Landlock is applied.
- A seccomp filter (`SECCOMP_FILTER_FLAG_NEW_LISTENER`) sends these calls to the supervisor:
  `connect`, `sendmsg`, `sendmmsg`, `clone3`, `sendto` with an address, and `clone` or
  `unshare` with a `CLONE_NEW*` flag. The supervisor reads the arguments from the
  memory of the app, writes a line, and answers CONTINUE every time: the kernel runs the
  call exactly as the app asked. CONTINUE is not safe against an app that changes its
  memory, descriptors or namespaces after the read, which is why this mode is no
  security feature (see "Why not CONTINUE" below).
- The supervisor forwards `SIGINT`, `SIGTERM`, `SIGHUP`, `SIGQUIT`, `SIGUSR1` and `SIGUSR2`
  to the app, waits for every process that descends from it, and exits with its code
  (128 + n if a signal ended it).
- Other architectures than x86-64 are not filtered; with the variable set the stub refuses to start there.

Log lines (one JSON object per call):

```
{"t":1793209955827,"tid":729115,"syscall":"connect","family":"AF_UNIX","path":"/run/x"}
{"t":...,"tid":...,"syscall":"connect","family":"AF_INET","addr":"127.0.0.53","port":53}
{"t":...,"tid":...,"syscall":"sendmsg","family":"connected","control":true,"scm_rights":true,"fds":2}
{"t":...,"tid":...,"syscall":"unshare","flags":"0x10000000","newuser":true}
{"summary":{"connect:AF_UNIX:addr":13,"sendmsg:noaddr":8,...}}
```

An abstract Unix address is written `@name`. `sendmsg` and `sendmmsg` with no address and no
control data are only counted in the summary; `sendto` with no address is filtered out in
the kernel and not counted. The summary is written again every two seconds while the counts
change, so a log of a killed app still ends with one (the last counts).

## What four apps do (30 seconds of use, Xvfb, isolated HOME)

Joplin 3.7.21 (open notes, new note), Simplenote 2.27.1 (login form), QOwnNotes 26.10.9 (first-run
wizard, open notes), Kdenlive 24.12.3 (new project). Network was allowed on ports 80 and 443.

| App | connect (UNIX / INET / INET6 / UNSPEC) | sendto with address | sendmsg without address (of which with SCM_RIGHTS) | sendmmsg | namespace calls |
|---|---|---|---|---|---|
| Joplin | 13 / 17 / 15 / 14 | 9 AF_NETLINK | 186 (178) | 8 | `unshare(CLONE_NEWUSER)` once |
| Simplenote | 16 / 18 / 12 / 11 | 6 AF_NETLINK | 54 (40) | 8 | none |
| QOwnNotes | 3 / 6 / 3 / 2 | 2 AF_NETLINK | 0 | 4 | none |
| Kdenlive | 18 / 0 / 0 / 0 | none | 0 | 0 | none |

`AF_UNSPEC` connects reset a datagram socket. No app called `sendto`, `sendmsg` or `sendmmsg` with
an `AF_INET`, `AF_INET6` or `AF_UNIX` address. DNS over UDP goes through `connect` and then a send
without address.

`AF_UNIX` addresses contacted:

| App | Addresses |
|---|---|
| Joplin | `@/tmp/.X11-unix/X<n>` (abstract, 7), `/var/run/nscd/socket` (6) |
| Simplenote | `@/tmp/.X11-unix/X<n>` (abstract, 12), `/var/run/nscd/socket` (4) |
| QOwnNotes | `/var/run/nscd/socket` (2), `@/tmp/.X11-unix/X<n>` (1) |
| Kdenlive | `/run/pipewire/pipewire-0` (3), `<runtime>/pipewire-0` (3), `<runtime>/pulse/native` (3), `/var/run/pulse/native` (3), `/var/run/nscd/socket` (2), three KIO worker sockets in `<runtime>` (1 each), `@/tmp/.X11-unix/X<n>` (1) |

None of the four apps asked for the session bus, an agent or the system bus (their variables are
hidden). Joplin sent descriptors with `SCM_RIGHTS` in 178 calls (223 descriptors) and Simplenote in 40
(50 descriptors), with no address: these are the Chromium IPC channels between its own processes.

Cost to the time of the first window (8 starts each, Xvfb; the window title is matched):

| App | without probe | with probe |
|---|---|---|
| Joplin | median 1.71 s | median 1.75 s |
| Simplenote | median 1.09 s | median 1.11 s |
| QOwnNotes | median 0.28 s | median 0.28 s |
| Kdenlive | median 0.44 s | median 0.43 s |

A notification costs about 7 µs in a micro-benchmark on the same machine (9 µs with a descriptor
copy and a memory read), so the 255 notifications of Joplin cost a few milliseconds.

## Why not CONTINUE

A supervisor that reads the address and then answers CONTINUE lets the kernel read it again. The
app can change the address, the descriptor or its mount namespace in between. Test with
`bwrap --unshare-user --bind B A` as the app's view, the same path string `A/sock` and two
servers:

- the app connects by itself: the kernel resolves `A/sock` in the app's namespace and reaches
  server B;
- the supervisor takes the app's socket with `pidfd_getfd` and connects it in its own namespace:
  the connection reaches server A.

So a broker that decides and connects must do both in its own namespace, read the path once,
refuse a relative path, and never answer CONTINUE for a call that carries an address. Landlock
also denies `mount`, `pivot_root` and `umount` to a process that is already restricted, which
removes the app's own way to build such a view (`mount` and `chroot` inside a new user
namespace failed with EPERM under the stub on the test machine; an AppArmor profile that denies
capabilities in unprivileged user namespaces was active there too, so the two causes were not
told apart).

## Socket isolation levels

The policy comes from the manifest (`sockets_allow` and the permissions that imply a socket:
Wayland always, X11, PulseAudio and PipeWire when asked). Four levels apply it, the first that
works on the machine is used:

1. **Landlock ABI 9** (kernel with `LANDLOCK_ACCESS_FS_RESOLVE_UNIX`): rules on the listed
   sockets. No process, no cost. Implemented.
2. **A view made with bubblewrap**: `bwrap --unshare-user` builds a mount namespace with a new
   `/run` and `/tmp` where only the listed sockets are bound; inside the view the stub applies
   Landlock and runs the app. Not implemented; measured below.
3. **A seccomp supervisor** (the design of the previous report): connects are made by the
   supervisor on a copy of the app's socket. Not implemented.
4. **None**: the current behaviour, with the warning from `install`, `update` and `doctor`.

### Level 2 measured on Ubuntu 26.04 (kernel 7.0, Landlock V8)

- `bwrap` 0.11.1 is installed, `/etc/apparmor.d/bwrap-userns-restrict` exists, and
  `kernel.apparmor_restrict_unprivileged_userns` is 1. As an ordinary user `bwrap --unshare-user`
  works; `unshare -Urm` fails with "write failed /proc/self/uid_map".
- View: `--unshare-user --bind / / --dev-bind /dev /dev --proc /proc --tmpfs /tmp --tmpfs /run`,
  then `--ro-bind /run/systemd/resolve ...` (DNS), the socket of the display and the sockets of
  the permissions. Inside it, with the stub applying Landlock: the session bus, the system bus,
  `systemd/private`, `gnupg/S.gpg-agent`, `S.gpg-agent.ssh`, `openssh_agent`, `pulse/native` and
  `pipewire-0` do not exist; a bound Wayland-like socket and an X11-like socket connect; writes to
  the home and reads of `~/.config/gh` give EACCES (Landlock, not a read-only mount); `/tmp` shows
  only `.X11-unix`.
- Order: the view must be made before Landlock (a restricted process cannot mount), so the
  sequence is the outer stub, `bwrap`, then the stub again inside the view (it checks the package
  again, about 15 ms) which applies Landlock and runs the app. `bwrap` cannot run inside the
  Landlock sandbox.
- Cost: `bwrap` adds a median 7.1 ms to `/bin/true` and 5.5 ms around the stub (40 starts each:
  8.4 ms against 1.3 ms; 21.6 ms against 16.1 ms).
- Joplin starts in the view and opens, creates and edits a note; QOwnNotes starts and runs its
  wizard and opens notes. With `pulse` and `pipewire-0` added, `pw-cli info 0` works and a
  connection to `pulse/native` succeeds; without them both fail.
- Not hidden by the view: abstract Unix sockets (the network namespace is shared; X11 uses one),
  and the system bus is only hidden because `/run` is a new tmpfs.

### Apps that need a runtime folder

Kdenlive makes KIO worker sockets in `$XDG_RUNTIME_DIR`. With the real `/run/user/<uid>` the
sandbox does not let an app write there. A view needs its own writable runtime folder
(a tmpfs with the allowed sockets in it), which is also the natural place for them.
