# Changelog

## Unreleased

- **`panpak convert` unpacks on disk, and counts before it writes.** The tree went into `$TMPDIR` (`/tmp`, which is in memory on many machines), so a large or hostile AppImage could freeze the machine. Now it is unpacked in `panpak/convert/` of the cache folder (`$XDG_CACHE_HOME`, or `~/.cache`), made private; a cache folder that is in memory (tmpfs, ramfs) is refused before anything is made there; and the index of the squashfs is read first: over 8 GiB in all, 8 GiB for a file or 1,000,000 entries, `convert` stops with nothing written, not even the work folder. `tests/convert_workdir.py` (a `TMPDIR` that does not exist, a cache in memory, a sparse file of 9 GiB); `tests/convert_links.py` now puts its secret in the work folder.
- **P2-09: `panpak rollback` checks that `previous` is a record of the same app.** It swapped the two links after looking at the tree of `previous` only; a `previous` that named the record of another app left the app with a record that it could not start. The record is verified and `check_identity` is applied, as the start does for `current`. `tests/rollback_identity.py`.
- **`panpak convert`: P2-03 and P2-08 (second security pass).** Each has a test that failed before the fix.
  P2-03: two links that are each inside the tree and leave it together (`d/e/l -> ..`, `app.desktop -> d/e/l/../../secret`) made `convert` read a file of the host as the desktop file; its lines (`Name`, `Exec`, `Version`, ...) went into the manifest of the package. After the unpacking every link is followed again as the file system does (`treewriter::resolves_inside`) and one that leaves the tree is taken out and listed with the others that point outside. `tests/convert_links.py` runs the real `convert` on such an AppImage.
  Separate fix: `--help`, `-h`, `--version` and a command that is wrong or incomplete write nothing. `panpak convert --help` took `--help` for the AppImage and made the personal key (`~/.config/panpak/secret.key`) before it said that `--id` was missing; `build` of a folder that does not exist installed the runtime first. `--help` and `-h` (alone, or after a command) now print the usage and stop before anything is read or written, and `convert` and `build` check their arguments before the key is made. `tests/no_side_effects.py` runs 14 commands, each in a new home, and the home must stay empty.
  P2-08: the unpacking had no limit. Now at most 8 GiB for the tree (what `pack` takes), 8 GiB for a file and 1,000,000 entries; a file is read only up to what is left, so an endless one stops at once.
- **`panpak build`: P2-01, P2-02, P2-05, P2-06, P2-07 (second security pass).** Each has a test that failed before the fix.
  P2-01: a source tarball with a link and then a file under it (`src/link -> /some/folder`, `src/link/x`) wrote outside the source folder; the folders above an entry are made one at a time and a link or a file on the way stops the unpacking.
  P2-02: `collect` copied into the output tree through links that the build had left in it (`usr/share/glib-2.0 -> <folder of the user>`), so files of the image landed in that folder; the copies of the toolkit, the libraries, the licenses, the loader cache and the manifest now go through `ensure_dirs`, `copy_file` and `write_file`, which make folders one at a time, refuse a link as a folder and replace a link where a file goes (never writing through it).
  P2-05: the commands of the recipe no longer inherit the environment of `panpak build` (`--clearenv`). The steps that fetch still use the host network: documented in THREAT-MODEL.md.
  P2-06: `build.packages` names start with a letter or a digit, so none is an option of `apt-get` (`--allow-unauthenticated`).
  P2-07: `--new-session` in the build sandbox (no keystrokes into the terminal of the user). Not reproduced here: `dev.tty.legacy_tiocsti` is 0.
- **`permissions.devices`: the uevent socket, for the apps that say they need it.** Since the netlink filter of #34 an app cannot open `NETLINK_KOBJECT_UEVENT`, so it does not hear of devices that are plugged in or removed. A package that sets `permissions.devices = true` gets that one protocol (the rest of the filter is the same); it is a new row of the table, level Attention: "Be told when devices are plugged in or removed (game pads, USB devices, cameras, keyboards)". Default false. Seven apps of the harness asked for the socket (Bitwarden, Jitsi, Joplin, Kdenlive, Obsidian, PCSX2, Zulip): a package of theirs declares it if it wants the notices. The permission does not open any device file. `seccomp::build_with(uevent)`, a test of the program, an end-to-end probe in `tests/hardening.py`.
- **P2-04: `panpak gc` keeps what it cannot read the links of.** A `current`, `previous` or `pending` that is there and cannot be read (an absolute link of a store of an earlier version, a file or a folder in its place, a read error) used to count as "no link": the record, the tree and the farm of the app were collected, at the first `gc`, `update`, `apply` or `remove`. Now the app is kept whole and the report says why; a folder of `apps/` whose name is not an app id keeps every record. `tests/gc_unreadable.py`.
- **Seccomp: netlink only for NETLINK_ROUTE; `--write-rename` says when it is not used.** `socket(AF_NETLINK, ..., protocol)` passes only for NETLINK_ROUTE; the others return `EPROTONOSUPPORT`. Seven apps of the harness (Bitwarden, Jitsi, Joplin, Kdenlive, Obsidian, PCSX2, Zulip) ask for NETLINK_KOBJECT_UEVENT (libudev: device hot-plug); they start as before and no longer hear of devices that come and go. With the document portal the stub does not use `--write` and `--write-rename` (the file is a document and the portal lets the app save by replacing it) and says so; the duplicate warning of the CLI is gone (the stub warns when it opens the folder). QUICKSTART corrected: with the portal the app gets the rights that the user has on the file.
- **Security review PP-01 to PP-07** (an independent review of cfd31a5). Each defect has a test that failed before the fix.
  PP-01: a record is named `<id>@<escaped version>@<whole root>.rec` and the start refuses a record whose name is not what its content gives, or that another app's link names (before, `org.a-1` at `2` and `org.a` at `1-2` shared a name and a package made of the same files could replace the record of another app, with its own permissions and environment).
  PP-02: the fonts.conf, user-dirs.dirs and opener files that the outer stub writes in the folders of the app never go through a link the app planted (`safefs::write_generated`).
  PP-03: `update` and `repair` check the layout of the remote footer before any range is asked for; an empty manifest or index is refused; an empty range is an error, not "no limit".
  PP-07: `--write-rename` is refused for the home, for a folder that holds it, and for a folder that holds or lies in a place that starts programs (`~/.config`, `~/.local`, `~/.ssh`, shell start files, autostart); the warning says the grant covers the subfolders.
  PP-04 and PP-05: a link in a folder that the user gave the app through the document portal with write is not followed (the host reads those folders from the portal); the walk of a path to share is one `O_NOFOLLOW` step per name, so a folder swapped for a link in between is not followed.
  PP-06: `socket` and `socketpair` return `EAFNOSUPPORT` unless the family is local, IPv4, IPv6 or netlink.
  Records of an earlier store (the old names) are refused at the start: reinstall.
- **Measurement: memory of two apps that run at the same time.** `bench/shared_memory.py` and `docs/bench-data/shared-memory*.csv`: two releases of QOwnNotes running together take 90.8 MiB of PSS in one panpak store, 118.8 MiB as two unpacked copies and 148.5 MiB as two AppImages (the second app adds 23, 54 and 69 MiB); one app alone costs 3.1 MiB more in the isolated view than unpacked. Limits in `docs/BENCHMARKS.md`.
- **A copied store brings its launcher and menu entry along; no absolute links are read.** Every command that opens the store (`list`, `doctor`, `update`, `apply`, `rollback`, `repair`, `remove`, `install`, `gc`, ...; not the end-of-run job nor the start-time apply) reads the launcher in `~/.local/bin` of each installed app (one small file) and, when it names another place for the store, writes the launcher and the menu entry again and says so; only files that panpak wrote. The launcher is a script, not a link, so it stays absolute. `durable::resolve_link` no longer reads an absolute link (a store of an earlier version): the error says to reinstall the app. `tests/store_move.py` covers the copied home: after one command the launcher starts the app and the entry names the copy.
- **Power-loss proofs (end of step 8).** `docs/POWER-LOSS.md`: 1768 points of `dm-log-writes` (every flush and FUA point of 15 steps of install, update, apply, rollback and remove, and 200 random points) and 30 `virsh destroy` cycles, all OK, with the
  limits. `tests/power_loss_dmlog.py` is confirmed on a VM and fixed (parsing of `replay-log --verbose`, a server for `update --stage`, `--max-flush`); `tests/power_loss_packs.py` makes its packages.
- **Step 8e: garbage collection by reachability from the records.** `panpak gc [--dry-run]` keeps the records that `current`, `previous` and `pending` name, the trees and farms of their roots (a farm lives while any
  record with its root lives) and the blobs that a kept tree links, and takes out the rest: records, trees, farms, blobs, work folders of dead processes, `*.panpak` of the old layout, ABI cache entries of a gone farm or older than
  30 days, download folders idle for 7 days, runtimes that nothing names. **No clock decides about work in progress**: `locks/.store` is a two-mode lock; install, update, stage, apply, repair, rollback, remove and the making of a
  tree hold it shared, `gc` takes it exclusive without waiting and, when it cannot, says it was left for the next time. An app that is busy or runs keeps everything. It runs after an apply, an update or a removal, and when the last
  copy ends. Test points `gc.*` (33 points, 70 stops) and a test that runs a collection in the middle of a slow install (a pause point); `tests/gc.py`; PANPAK-FORMAT.md 6.8.
- **The links of the store are relative.** `current`, `previous`, `pending` and the links of a farm to a library of the bundle name their target relative to their own folder (`../../packages/<record>`, `../../trees/<root>/...`), like `.farm` and `bin/panpak-stub` already did, so a store that is copied to another path (another home) works there. `panpak_core::durable::{relative_to, resolve_link}`; `tests/store_move.py` (install, update, a staged update, the home copied: list, doctor, start, rollback in the copy, the original untouched; an absolute `current` is refused). (The launcher and the menu entry stay absolute: see the entry above.)
- **Work at the end of the last copy; HTTPS with an authority of the user; the notice through our D-Bus client.** The outer stub now stays while any installed app runs; when the last copy ends it starts, detached
  at nice 19 and idle I/O, `panpak background --exit <id>`: the staged update is applied, the files found damaged are repaired (with the network), and the daily job runs if it is due. A start no longer repairs
  anything and never waits for the network: it applies a staged update when no copy runs (the fallback), names damaged files with `panpak repair <id>` on standard error and starts the app. The notice of a security
  update goes through the notification portal with the D-Bus client of panpak (no `gdbus`). `network.ca_file` in `config.toml` replaces the built-in authorities for downloads. `tests/exit_work.py` runs the
  downloads over HTTPS with a test authority and a test server and checks the exit work. Not changed: the farm stays `farms/<root>` (see the PR).
- **Step 8d: a daily background job.** The stub starts `panpak background <app id>` (detached, nice 19, idle I/O, 20 s after the start) when `apps/<id>/update-state` says it is due: it stages a new
  version when the update URL has one (the version is applied at a start with no copy running, as in 8c) with a backoff from an hour to a day after a failure, and it reads part of the installed files against
  the signed index by a budget of megabytes a day (a rotating cursor), notes damage, repairs it when no copy runs (or at the next start with none) and shows it in `panpak list` and `panpak doctor`.
  `panpak config` shows and sets `background.updates` (on) and `background.verify_mb` (64; 0 is off) in `~/.config/panpak/config.toml`. The mechanism for a security update is in place without a format: the
  staging asks `record::security_mark` (always no for now) and the job shows one notice through the notification portal. `PANPAK_NO_BACKGROUND` keeps the stub from starting the job.
  `tests/background.py`; PANPAK-FORMAT.md 6.7; THREAT-MODEL.md. Also prepared, not run: `tests/power_loss_dmlog.py` (writes recorded by `dm-log-writes` in a VM and the disk rebuilt at every flush and at random points).
- **Step 8c: an update can be staged and is applied at the next start.** `panpak update --stage <app id>` downloads the missing chunks, writes the record and makes the tree and the resolution, and names the record
  with `apps/<id>/pending`; `current` does not move. `panpak apply <app id>` (and the update that `panpak update` makes when the user is there) moves `current` after checking again the signature, the pin
  (a key succession is written as two lines in `pinned-key` for the time of the switch) and the tree, only when no copy of the app runs, and only when the version asks for no new permission that the user has not
  accepted. The stub runs `panpak apply --at-start` at the start of an app that has a staged update (one `lstat` when none). A version that asks for new permissions is prepared and waits for the user;
  `panpak list` shows `update <v> is ready` or `waits for you`. `panpak update` while a copy runs stages and says so instead of failing. New test points `stage.*` and `apply.*` (29 points, 66 stops in `tests/failpoints.py`,
  with the start that applies and an update that replaces the key); `tests/store_flow.py` covers a staged update with a running copy and one that waits for permissions. PANPAK-FORMAT.md 6.6; THREAT-MODEL.md.
- **Step 8b: locks, test points, and the order of the writes.** One lock per app (`<store>/locks/<id>.lock`) for `install`, `update`, `rollback`, `repair` and `remove`: two never change an app at once,
  a command waits for the other up to 60 s (`PANPAK-E2008`); `update` lets the lock go while the chunks come and checks `current` again before it publishes. 26 named test points
  (`panpak_core::failpoint!`, only in a build with the feature `failpoints`; `PANPAK_FAILPOINT=<name>` stops the process there with `abort`): `tests/failpoints.py` stops every command at every point
  (46 stops) and checks that the store is whole and that the command run again finishes; CI fails if a release binary holds a point (`scripts/check-no-failpoints.sh`). `update.url` and the pin of the
  user folders are written like the record (synced, then renamed). PANPAK-FORMAT.md 6.4 and 6.5; THREAT-MODEL.md says what the store guarantees about the installed files and what it does not.
- **Step 8a: one copy of an app in the store, and the app is ready when `install` ends.** The store no longer keeps the package: `install` makes the tree (with
  the farm and the cache of the resolution, by `panpak-stub prepare`, the code a start uses) and keeps a record of the version (`packages/<id>-<version>-<root>.rec`: the
  signed index, manifest and signature, with the magic `PANR`; PANPAK-FORMAT.md 6.3). A record is never a package (`PANPAK-E0010` for `install`, `update` and the other readers; the stub
  runs one only from `packages/`). A start whose files are gone says `PANPAK-E2007`; `panpak repair <app id>` makes them right from the update URL (or `--from <package>`),
  `panpak doctor <app id>` compares the tree with the signed index. `update` downloads only the chunks the installed version does not have and builds the new tree from its
  files (the reused blobs are checked against the index); `rollback` checks that the files of the previous version are there. Made durable in an order that keeps a link from
  being on disk before what it leads to (6.2): `fsync` of exactly the files and folders of Panpak, on several threads (no `syncfs`); folders one after the other took 4.4 s,
  on 16 threads 15 to 78 ms. The tree is made on up to 8 threads, large files by parts, and a file whose blob is in the store (named by its chunk hashes) is only linked.
  A blob is visible in `blobs/` only when it is on disk (written into a work folder, synced, then renamed, then `blobs/` synced), so a blob that is there is linked by any later install
  without a new sync and with no rule that depends on the clock; `tests/blob_visibility.py` kills the start between the writing and the naming (with `strace`, no hook in the program).
  Measured (ext4, this machine, 40 runs of each): Text Editor first install 0.44 to 0.48 s, median 0.46 s (the first version of this code, with a sync after every folder: 4 s); Ghostty
  installed second, with 77% of its 2,491 files already in the store: 0.24 to 0.31 s, median 0.29 s, 8 runs of 10 under 0.3 s; `install` used to take 0.08 s and leave the work to the first start (0.6 s). A tree that a killed start
  left read-only in `<root>.partial` could not be removed and stopped the next start: found by the interruption test, fixed. `panpak remove` also takes `<root>.lock`, `<root>.partial` and the temporary file of a record that a kill left (found by the interruption test on the CI machine).
  Tests: `tests/store_flow.py`, `tests/interrupt_8a.py` (install and update killed at 148 moments), unit tests of the stub and the core. The garbage collection of old trees, blobs and
  cache entries is step 8e.
- **Two stale tests.** `tests/abi_cache.py` expected a change of the output of `ldconfig` to invalidate the warm cache of the resolution; the key is the identity of `/etc/ld.so.cache` since the cache was made, and a hit runs no other process, so the test now checks that. The test of `SQLITE_TMPDIR` (`sqlite_tmpdir.py`) expected the old layout of the private folders, a `SQLITE_TMPDIR` that the user's value could override and no isolated view; `tests/app_dirs.py` and `tests/view.py` check the variable today, so it is removed.
- **Step 8, defect 60: the library path of an app is valid only for its own programs.** The stub put the farm and the library folders of the package in `LD_LIBRARY_PATH` as
  absolute paths, and every program that the app started inherited it: a system program loaded the package's libraries instead of its own (`zenity` failed with `symbol lookup error`
  under qBittorrent's path; `grep` and `flatpak` on Fedora 45). The value is now `/$ORIGIN/.panpak-lib-<h>/.farm:/$ORIGIN/.panpak-lib-<h>/lib:...`: glibc replaces `$ORIGIN` with the
  folder of the program, and each folder of the package that holds a program has a link `.panpak-lib-<h>` to the root of the tree, so the path leads into the tree for the app's programs
  and nowhere for the host's. No ELF file is rewritten and the hard links between the trees of different apps are kept. `build` and `convert` make the links, `pack` refuses a program
  without one (`PANPAK-E0009`, with the commands), the runtime makes `<tree>/.farm` and checks it before the exec (`PANPAK-E2006`), and the app harness checks every run that only the
  programs of the app, and what they start, hold the variable (the outer stub and bubblewrap must not) and that the value is the relative one. The slash in front keeps the value harmless
  on musl. Packages made before keep the absolute list. Measured on the three source packages and ten AppImages: the loaded libraries are the same files as before, no start-time change,
  `zenity` started by qBittorrent works; the study of the alternatives (RUNPATH in every ELF, RPATH in the executables, `ld.so --library-path`) is in the reports of the work. PANPAK-FORMAT.md section 7.1.
- **Step 11c, third part: the CI keeps the patched Debian packages.** The rebuild of GTK 4 with the dmabuf patch was 841 s of the 930 s of the Text Editor harness run on a cold runner. `apps-harness` now restores `patched/<id>/` from the Actions cache (key from the content of `image.rs`, `sandbox.rs` and the patches), checks it against its `SHA256SUMS` and removes what does not match; only main and the weekly run write it, a pull request only reads; the weekly run builds cold and compares the root hash of the new packages with the cache. With the cache the patch phase took 2.4 s and the run 96 s (on a machine, from a restored folder). `panpak build` no longer leaves `out/` (a second copy) in the folder. New: `tests/apps/patched_cache.py` with its tests.
- **Step 11c, 51 (b): another app with the same id.** While an app with the same id (or window class) is installed another way (a Flatpak, a distribution package), the menu entry of the Panpak app is named `<Name> (Panpak)`; with no other app it is unchanged. `install` and `update` say so in one line, and the check is made again at every start of the app (about 25 microseconds, in the step that reads the version of the entry), so the entry follows the other app coming and going. The icon is not changed (a mark on it was left out: it needs the pixels of a package icon to be decoded). The reference hashes file is now generated by `scripts/repro-build.sh --update` (date, sizes, hashes), the date follows one rule, `scripts/code-epoch.sh`, shared with `panpak/build.rs`, and the `repro` job checks the file against a fresh run.
- **Step 11c, second part: the stable `panpak` follows the build, not only the version.** `panpak --build-epoch` prints the date of the code of a build (`SOURCE_DATE_EPOCH`, else the date of the last commit that touched the code). `install` and `update` replace `<store>/bin/panpak` when the running one has a higher version, or the same version and a later date; never downwards. `scripts/repro-build.sh` sets `SOURCE_DATE_EPOCH` the same way, so the reproducible build keeps one date per code state.
- **Step 11c, first part.** `panpak install` updates `<store>/bin/panpak` when it says an older version than the running one (one rename, under a lock of the folder), and never replaces one that is the same version, newer, a link, or does not say its version. `panpak build` prints the time of each phase (`time image`, `patch`, `source`, `zig`, `compile`, `collect`, `pack`) and the app harness keeps those lines in its log. Two tests of the lock files (`safefs`, `stalelocks`), which failed now and then, run alone in a process: a lock taken with `flock(2)` belongs to the open file description, so a child forked by another test keeps it until it calls `exec`.
- **The file dialog of the desktop for apps (step 11).** Where the desktop has the document portal, an app that asks for a file gets the dialog of the desktop: the
  router of the app serves `OpenFile`, `SaveFile` and `SaveFiles` of the portal, the outer stub shows the dialog as a client of the session bus, opens what the user chose
  (a final link is refused: "the file you chose is a link: choose the original file") and shares it with the app through the document portal (`read`, and `write` where the user
  can write; never `grant-permissions` or `delete`). The app gets `$XDG_RUNTIME_DIR/doc/<id>/<name>` paths and never a path of the host, so Text Editor no longer needs the
  `documents` folder. One dialog at a time per copy and five a minute. Files given at the start (Open with, a double click, the command line) are shared the same way before the app
  runs. `panpak remove` takes the grants back (`--purge-data` also deletes the documents that no other app holds). `panpak doctor` says whether it is served; `panpak pack` notes a GTK or
  Qt app that asks for a user folder with `rw`. New frame kind and fuzz target (`fuzz_chooser`), the lab scenario D with the real portal and a fake portal for the unit tests.
  The menu entry now lists the file types of the original `.desktop` file (`[desktop] mime_types`, copied by `convert` and `build`), so the app appears in "Open with" for them.
  Found on the VM and fixed: the Save of Text Editor on GNOME was refused because the parent window handle of GNOME has punctuation (the app then crashed: GTK does not survive an answer with no `uris`); a dialog that gives no file is now answered "cancelled" with `uris` = []; a link the user put in a folder the app cannot write is followed to an ordinary file instead of refused, with the place it points to in the message when it is refused.
  What the app can already reach (under a folder it can write) is given as the real address, not as a document; a second dialog waits for the first (one may wait); `--panpak-open` is not added when the files are shared
  as documents. The menu action "Uninstall" works (it was refused by the document portal because the desktop starts it in a unit that carries the name of the app: a scope on GNOME, a service on KDE; `remove` now always moves into a scope of its own, unless it is already in a `panpak-remove-` one), asks in the language of the system, and shows no paths or commands.
  `panpak remove` also takes the entry out of `mimeapps.list` (where the desktop writes the default app the user chose) and refreshes the type cache, so a double click on a text file goes back to the native app.
  Design and what differs from it: docs/FILE-CHOOSER.md.
- **`user-dirs.dirs` is read line by line.** A key that this version does not know (`XDG_PROJECTS_DIR` of xdg-user-dirs 0.19, on Fedora 45 and later) or a line that is not understood is skipped and no longer makes the whole file unusable, so the Downloads and Documents folders of an app are granted on a current system; an unknown key is never granted.
- **The table of permissions, corrected.** High risk is the whole home folder (also read-only) and the secret places (passwords, keys, browser and keyring data, startup files); a
  sub-folder of the home is Attention, read or write. The sentences about what an app cannot do are shown only where this system keeps them (without the isolated view the
  claims about the network, other apps and the desktop services are shorter or left out). New rows: the files you choose in the dialog, and the files you start the app with.
- **One table of permissions** (`panpak_core::permissions`): the words, the level (Normal, Attention, High risk), the icon and the longer sentence of each permission are written once, and `install`, `update`, `list`, the new `panpak inspect` and the README use them. The summary says "Full isolation" or "Partial isolation" and why; the list also says what the app cannot do; an update lists only the permissions that are new or higher, and says that without a yes the current version stays. Every home path is High risk. The line about `dbus_own` is gone (it has had no effect since the private bus). The text is translated with gettext catalogs in `po/` (English, Italian): a language is added with a `po/<language>.po` file and no code; the catalogs are compiled into the program. `panpak list` has a PERMISSIONS column and `--json` has `permissions`.

- **A simpler Quick start.** `./build.sh` prepares Zig (the pinned wheel, hashes checked), adds the Rust target and builds, and says what is missing if something is. `panpak build` and `panpak install` install the runtime that comes with `panpak` (the file `panpak-stub` beside it) when there is none or it is older, with one line; `install-runtime` stays for a signed release or another build. The first `panpak build` or `convert` without a key makes a personal one in `~/.config/panpak/secret.key` (mode 0600) and says so with its fingerprint. README, QUICKSTART.md and docs/REQUIREMENTS.md follow.

- **`install` is about 25 ms faster** (96 to 71 ms for a 44 MB package): the icons are written without a sync of the file and of the folder each (14 syncs of about 2 ms were the difference with the 38 ms of before the desktop integration); the package copy and its sync are what remains.
- **Zig is taken from `ziglang==0.17.0.post0`**: 0.17.0 was yanked on PyPI because of a partial upload (wheels missing), and the post release is the complete one. The Zig executable is byte for byte the same, and the build is reproducible with the same hashes.

- **Symbolic links that point outside the package (defect 57).** `convert` leaves out a link of the AppImage whose target is absolute or climbs above the root, lists them in a note, and stops with a clear error when the entry or a library that a program of the tree asks for is such a link. `pack` and `build` refuse such a link and name it, with the way to correct it. The app harness no longer makes `fixed-` copies of AppImages (GIMP and MuseScore convert and start as they are).
- **`panpak doctor <package>` counts the libraries the package carries** before the first start (the tree is not on the disk then; it called every library of the package missing).
- **The CI in three levels** (docs/CI.md): a pull request runs the fast level (build, tests, clippy, the private bus; 8 job-minutes instead of 34 to 63); main, the label `full-ci`, the manual and the weekly runs add fuzz, reproducibility and the distro containers. Every job has a time limit, and a test checks that every action is pinned to a full commit.

- **The private bus works with xdg-dbus-proxy 0.1.5** (Debian 12, Ubuntu 24.04), found when the tests first ran for real in CI. The router talked to the proxy with the whole handshake in one write and
  mixed its own messages between the app's, and 0.1.5 closes a connection in both cases: the app had no bus. The handshake goes one line at a time and every message to the host side gets the next serial
  (replies get the sender's number back). `tests/privbus.py` runs in CI against a systemd user manager started for the runner; A, B, C, F11, F22 and F23 are required there.

- **README rewritten** around what Panpak is for (a secure and fast format for desktop and command-line apps, good for atomic distributions); the requirements moved to
  `docs/REQUIREMENTS.md`; every number has a command and a date in `docs/BENCHMARKS.md`, which now also holds the sources of the comparison table and
  `bench/shared_chunks.py`. The benchmark data of updates and starts were measured again on the final build of step 13p (the packages are larger by the icon caches).

- **A private bus for each app (step 13p).** The names of an app (its id and any name it takes) are on a bus of its own, in a router process shared by all copies of
  that app (started by the first, ended after the last), and never on the host bus. A native program with the same D-Bus name no longer meets the copy (KL-31 closed:
  Ghostty, F22 and F23). The router registers the identity `panpak.<id>` with the portal itself (the app cannot), answers `OpenURI` through the address-opening channel
  and tells the app what happened (the exit status of `xdg-open`, with the reason), and gives a media player one alias on the host bus (`permissions.mpris`). It has no
  privileges (Landlock, seccomp, limits) and is fuzzed. `dbus_own` has no effect any more. The accessibility bus is filtered like Flatpak's, without
  `NotifyListenersSync` and `NotifyListenersAsync` (KL-24). `Properties` on the portal stays refused until the file chooser (step 11). `tests/privbus.py`, `docs/PRIVATE-BUS.md`.
- **GTK 4.18 on KDE Plasma 6.8 (defect 54).** The Debian 13 build image rebuilds GTK 4 once per image version with a one-line patch (bind `zwp_linux_dmabuf_v1` at version 5 at most, as
  GTK 4.20 does); the patch is in `panpak.lock` (`patches`) and in `[provenance]`. Text Editor and Ghostty built on it start on Plasma 6.8.2. A lock without `patches` builds on
  unpatched packages; `panpak build --update-lock` adds it.
- **Links tell the truth (defect 55).** The address-opening channel waits up to 4 s for `xdg-open` and answers with its exit status and the reason; on Bluefin the only browser is a Flatpak, found through `XDG_DATA_DIRS`, which the opener takes from the launch, the user's systemd manager or the defaults with the folders of Flatpak and Snap.
- **The router ends under a lock** so that a copy started at that moment always gets a bus (`tests/privbus.py`, scenario E).
- **`pack` warns when `[desktop] wm_class` is missing** (the task bar showed a gear or another program's icon, defect 56).

- **Icon caches.** `panpak build` makes `icon-theme.cache` for every icon theme folder of the package (`gtk-update-icon-cache`), and `pack` warns about a theme folder with icons and no cache. Qt looked each icon up file by file without it: qBittorrent from source made 49,000 failed lookups at the start (0.415 s; now 0.21 s).
- **One time for every entry of a tree in the store** (2000-01-01T00:00:00Z): the store made each entry with the time of its creation, which made a cache file older than its folder, and Qt ignores such a cache (PANPAK-FORMAT section 6).
- **Downloads choose the address like Happy Eyeballs** (IPv6 and IPv4 in turn, 250 ms apart, 4 s per address) for `build` and `update`; an unreachable IPv6 address no longer stalls a download.

- **`panpak init` and `panpak build`: packages from source.** A recipe (`panpak.toml` with `[source]`, `[build]`, `[collect]`) and `panpak.lock` (the build image: its digest and the date of the package snapshot,
  changed only by `panpak build --update-lock`). `build` makes the image (a Debian root file system unpacked from the registry by digest, plus the recipe's packages from the snapshot) without root, fetches the source at a pinned commit or hash,
  builds in bubblewrap with no network, collects the libraries (read from the ELF files, never with `ldd`), the plugins of GTK 4 or Qt 6 and the data, makes the icons, writes the manifest and packs. A recipe states the memory it needs
  (`build.memory`, `jobs`): the build checks the memory that is available, lowers the number of jobs if it must, runs in a systemd scope with a memory cap, and measures the peak. A linked Qt module whose plugin is missing stops the build.
  Dependencies taken from the image are declared (`[[build.image_deps]]`) and shown at install. See docs/SOURCE-BUILD.md.
- **`[env]` in the manifest** (variables that point the app at its own tree, from a fixed list, relative to `${PANPAK_ROOT}`) replaces the shell script as the entry; `pack` warns about a script entry. New `[provenance]` table.
- **Faster start.** The stub is hashed in pieces of 64 KiB (1.4 ms instead of 5 ms), and the answer of `gsettings` is collected after the Wayland, D-Bus and shared-folder work. With the pty always available the fixed cost is 25.3 ms (24.6 ms before).

- **Apps can open a pty (terminals, IDEs).** The isolated view now has a devpts of its own and a minimal `/dev` of bubblewrap (null, zero, full, random, urandom, tty,
  the GPU folder `dri`) instead of the host's `/dev`; Landlock grants `ptmx` and `pts` read, write and the device ioctls. Before, `open("/dev/ptmx")` failed with `EACCES` and
  no terminal emulator could start a shell. The ptys an app sees are its own, never the user's terminals; `TIOCSTI` stays refused. No permission is needed. `tests/pty.py`.
- **The Wayland client libraries come from the host.** `libwayland-client`, `libwayland-egl` and `libwayland-cursor` join the graphics stack that the system always wins (a bundled copy next to the host's
  Mesa made EGL fail: Ghostty built from source had no OpenGL context).
- **`pack` writes `glibc_min`** (the highest `GLIBC_x.y` the files ask for) and `install` and `update` refuse a package that needs a newer glibc than the system has, with both versions (`PANPAK-E2005`).
- **`pack` warns about a Qt tree without the plugins Qt loads at run time:** the `xdgdesktopportal` platform theme (dark mode, portal dialogs) and, with `libQt6Svg`, the SVG image format and icon engine plugins (tray icons).

- **A lock file left by an app that stops on a signal is removed when the last copy ends.** qBittorrent (a Qt `QLockFile`) stopped with `SIGTERM` left its `lockfile`, and the next start did not
  start because process ids start again at 1 in the view. The runtime that stays now searches for the stale lock files of the private folders when the last copy ends, before it removes the mark "in use" (cost at
  exit, 0.4 to 3.7 ms on the 15 test apps; none at start). A copy that is not the last touches nothing. `tests/stale_lock_exit.py`.

- **Desktop integration (step 7b).** Apps get their icon in the menu: `[desktop] icons` lists up to 6 PNG files of the package; `install`
  and `update` check them (PNG only, a short list of chunks, 16 to 1024 pixels, at most 256 KiB, image data of exactly the promised size;
  SVG is not accepted) and write them as `~/.local/share/icons/hicolor/<S>x<S>/apps/panpak.<id>.png`; `remove` deletes them. `panpak
  convert` takes the icons from the AppImage (`.DirIcon`, the root file, the `hicolor` sizes) and has `--icon <file.png>`; an app with no usable
  PNG gets the generic icon and a warning. New crate `panpak-icon`, fuzz target `fuzz_icon`.
- **Icons are cleaned by `pack` and `convert`.** Real PNG files carry text, profile, gamma and other chunks that `install` does not accept. `pack` and `convert` now
  keep only `IHDR`, `PLTE`, `tRNS`, `IDAT` and `IEND` (byte for byte; the pixels and the transparency are not touched), print the chunks they removed from each icon, and
  `install` stays strict. `pack` puts the cleaned icons in the package as generated entries `.panpak/icons/<side>.png` (the signed manifest points there); `convert` reads the icons
  (links followed only inside the tree) and hands them to `pack` in memory: it writes no icon file. `.panpak/` is the folder that only `pack` writes.
- **`convert` no longer writes through links of the AppImage.** The unpacked tree is a private `mkdtemp` folder, and every write to it (files, folders, links, the wrapper
  scripts and `panpak.toml`) goes through folder descriptors that do not follow links. Before, an AppImage with `bin -> /somewhere` or `panpak.toml -> /some/file` could make
  `convert` write outside its folder.
- **The user's `hicolor` icon theme is not in the view of the app.** The menu icons that `install` writes there made Qt apps start 60 to 110 ms later (no `icon-theme.cache`: 40,000 failed lookups). The app still reads the
  other theme folders of `~/.local/share/icons` and `~/.icons`.
- **A stable `panpak` and a stable stub.** The store has `bin/panpak-stub` (a link to the best runtime) and `bin/panpak`. Launchers run the stub
  through the link, so a new runtime reaches every app at its next start, and the menu action "Uninstall" runs `bin/panpak`: no launcher or
  entry names the folder from which `panpak` was run any more. `install-runtime` writes both with a rename; `install` copies `panpak` when there is none and warns when the one in
  the store is older. `panpak --version` prints the version.
- **The menu entry is thin and versioned.** The entry is made from a fixed template (the keys it may have are tested) and carries
  `X-Panpak-Desktop-Version`. When an app starts and its entry or launcher is older, the runtime rewrites both (temporary file, `fsync`, `rename`); when
  there is nothing to do it costs 0.04 ms.
- **`panpak remove` and `panpak list` say more.** `removed <id> <version>: freed <size>`; a second `remove` (or an unknown id) says "not installed:
  nothing to remove" with `PANPAK-E2004`; `list` of an empty store says how to install.
- **The menu entry and GNOME Shell.** Tested on GNOME Shell 50: a new or changed entry (and its icon) shows about 5 seconds after the file changes, with nothing but the
  `rename` panpak already does; an app that was already started from the Shell in the session keeps its old entry until the next login. `install` and `update` say so when
  they replace an entry with a different one.
- **The desktop's own scope is recognised for every launcher.** The shortcut that skips the scope of the app follows the portal's rule for scopes
  (`app-gnome-panpak.<id>-<n>.scope`, `app-panpak.<id>-<n>.scope`), with the systemd escape; the unit is the one `sd_pid_get_user_unit` finds, so a process in a sub-cgroup counts.
  A service (`app-...@<n>.service`) does not get the shortcut: a portal older than 1.21 may not read services, so the runtime makes its own scope, as from a terminal.
  `tests/dbus_filter.py` runs the three shapes against the real portal.
- **`install` and `update` warn about unknown fields.** A signed package that has manifest keys this panpak does not know is still installed (they are ignored), with a
  warning that says to update panpak (`panpak install-runtime`).
- **A key that this panpak does not know** in a `panpak.toml` given to `pack` or `convert` is still refused, now with `PANPAK-E0008` and a message that says it is a
  misspelling or a field of a newer format, and how to get a newer panpak (`panpak install-runtime`).

- **Hardening of the app process.** Every app now starts under a seccomp filter (a deny list, installed last, with
  `no_new_privs`; it returns errors, it does not kill): `io_uring`, `bpf`, `userfaultfd`, `perf_event_open`, the key ring,
  `ptrace` and `process_vm_*`, the mount calls, `setns`, new namespaces through `clone` and `unshare` (user namespaces too), `clone3` (`ENOSYS`),
  terminal input injection, odd `personality` values and the non-native ABIs are refused; the table with the reasons is
  PANPAK-FORMAT.md §9.8. New error code `PANPAK-E3002` (no seccomp). **Programs that need a user namespace of their own (Chromium and
  Electron sandboxes, WebKitGTK, `bwrap`, `flatpak`) cannot run in the view.** The device ioctl right (Landlock V5) is kept only on `/dev/tty` and `/dev/dri`.
  `/var/cache/fontconfig` is read-only in the view: the first start with an empty private font cache went from 446 ms to 48 ms with `fc-match`.
- **Wayland security context.** With a compositor that offers `wp_security_context_v1` (KWin, Sway and the other wlroots compositors),
  the app gets a socket of its own that the compositor knows as sandboxed (engine `io.github.panpak`, app id `panpak.<id>`). GNOME's compositor does not offer it:
  the app keeps the normal socket and `panpak doctor` says so. The client is part of `panpak-core` (no new dependencies) with the fuzz target `fuzz_wayland`.
- **The open-address channel is read by a receiver with no rights.** The resident stub is now two pieces (PANPAK-FORMAT.md §9.7): a receiver process
  (Landlock with no rule, a seccomp allow list, limits) reads what the app sends and passes validated requests as frames to the actor, which checks them again.
  Fuzz target `fuzz_receiver`.
- **The `audio` permission says that it includes the microphone** in the text of `install` and `update`; it was already so, and it is a known limit (KL-30).
- **Faster start.** The fixed cost of a start (measured against running the program directly) fell from 40 ms to 25 ms without the filtered session bus and from 56 ms
  to 38 ms with it (26 ms when the desktop started the app in a scope that already names it, as the GNOME menu does: then no scope is made): the `gsettings` queries, the systemd scope and the proxy overlap the work of the start, the inner stub waits for the proxy's socket, and the version of the proxy is remembered between starts. The app and the proxy are always in a scope that gives the app its identity.
  `PANPAK_DEBUG` prints where the milliseconds go. `tests/startup_budget.py` measures it and runs in the CI as an advisory step.

- **`panpak list` and `panpak remove`.** `list [--json]` shows the installed apps (id, name, version, disk space of the
  app and of its data, status, including incomplete installs). `remove [--purge-data] [--yes] <id>` takes away the launcher,
  the desktop entries panpak wrote, the packages, the trees and the shared `/tmp` of the app; the settings and data stay
  unless `--purge-data` is given and confirmed. It refuses while a copy is running (`PANPAK-E2002`), never follows a link,
  moves the app folder aside with one rename and is finished by the next command if it is interrupted
  (PANPAK-FORMAT.md §9.6, THREAT-MODEL.md §2.4). The desktop entry gets the action "Uninstall" (`panpak remove --ask <id>`: a dialog with zenity or kdialog, a notification at the end). A start waits for a removal in progress and ends with `PANPAK-E2004` when the app is gone. New error codes `PANPAK-E2002` to `E2004`; the fuzz targets `fuzz_dbus`,
  `fuzz_dbus_names` and the new `fuzz_journal` run in the CI smoke job.

- **A filtered session bus for apps in the isolated view.** With `xdg-dbus-proxy` (0.1.5 or newer), a systemd user
  manager and the view, an app gets a session bus that reaches only: the appearance and inhibit portals, its
  own `app.id` (single instance across copies), and, if the manifest asks, notifications, a tray icon and up to 8
  declared names (`[permissions] notifications`, `tray`, `dbus_own`). Every other interface and name is refused.
  The outer stub moves itself into a systemd scope `app-panpak.<id>-<n>.scope` so that the portal knows the app as
  `panpak.<id>`, and the folder `doc/by-app/panpak.<id>` of the document portal is in the view. Without the parts the
  app has no bus, as before; `panpak doctor` reports it. See PANPAK-FORMAT.md §3.1.5 and §9.5, THREAT-MODEL.md §3 and KL-5, KL-23 to KL-27.
- **The desktop entry is `panpak.<id>.desktop`** (it was `<id>.desktop`) and carries `StartupWMClass` when the manifest has
  `[desktop] wm_class`. `install` and `update` remove the entry of the old name when panpak wrote it. After an update,
  the launcher of an app is found under the new name in menus and the dock.
- **Quiet start.** The lines about `allow_missing`, libraries taken from another copy and undefined symbols are no longer printed
  at every start; they appear with `PANPAK_DEBUG` set. `install` lists the excluded libraries once and `doctor <package>` checks the symbols.
- `install-runtime` replaces the runtime by a rename (open apps are not disturbed) and takes the flags before or after the file.
- In the isolated view, writable folders get the Landlock `Refer` right, so a rename between folders works (Qt local sockets).
- New command `panpak dbus-rules <package>` prints the arguments the stub gives the proxy.
- A small D-Bus client for the outer stub in `panpak-core` (no new dependencies), with the fuzz targets `fuzz_dbus` and `fuzz_dbus_names`.
- **No network means no network in the isolated view.** An app whose manifest asks for no network (`network` not
  true, no TCP port) runs in a network namespace of its own with only the loopback interface, so it can no longer
  send UDP datagrams or look names up; before, Landlock stopped TCP only and UDP went out. The resolver socket
  of systemd is in the view only for apps that ask for network. Without bubblewrap nothing changes: UDP is still
  open (see THREAT-MODEL.md).
- **Fix: Landlock V9 rules were dropped on the musl build.** The rules for the sockets every app may reach
  (journal, Wayland, resolver, audio) and for `sockets_allow` were left out without a message, because
  `landlock::PathFd` opens files with the standard library, which drops `O_PATH` on musl. On a V9 kernel the
  journal socket was refused. Every rule now opens its path with `O_PATH`; a rule left out for any reason
  other than a missing path prints a warning, the fallbacks for `/dev` say why they were used, and
  `panpak doctor` lists rule paths that cannot be opened.
- **Fix: sockets outside `/run`, `/tmp` and the home folder.** The root of the isolated view is a new tmpfs with
  only `/usr`, `/etc`, `/sys`, `/dev`, `/proc`, the Nix and Guix stores and the top-level links, so a socket
  in `/var`, `/opt`, `/srv` or the folder of a container manager is not in the view.
- **Fix: stale lock files in the view.** The app is always process 2 in its own PID namespace, so lock files
  with a process id (Chromium `SingletonLock`, Qt `QLockFile`, Firefox `lock`) never expired. The runtime now
  removes the ones it recognises by content, in the app's private folders only, when no copy is alive and the
  mark `userdata/<id>.in-use` shows that the last run did not end normally. New command
  `panpak data unlock <id>`.
- **Fix: `panpak convert`.** The manifest is validated with the rules of `install` before it is signed;
  a `DT_NEEDED` entry with a path is not a library name; the string table limit follows the file instead of a
  fixed 1 MiB; the loader symbols are no longer reported undefined when `ldconfig` is not in `PATH`.
- **Fix: processes started after a fork.** Between `fork` and `exec` the opener and the socket probe call
  only async-signal-safe functions, so a start made while other threads allocate cannot hang.
- **Documentation.** `SECURITY.md` uses GitHub's private vulnerability reporting; `THREAT-MODEL.md` has one
  list of known limitations; `docs/BENCHMARKS.md` and the scripts in `bench/` give reproducible measurements.

- **Fix: first starts at the same time.** Two starts that overlapped on a store without the tree could end with
  `PANPAK-E2000: No such file or directory`, because they built in the same `.partial` folder. The tree is now
  built under an `flock`, in a folder apart, and published with one rename; the others wait and then use it.
  The problem was in the first version of the stub, so it exists in every earlier release.

- **User folders: `[[permissions.xdg_dirs]]`.** An app can ask for the music, documents,
  pictures, videos or download folder of the user, `ro` or `rw`. `install` and `update` find the
  folder in `~/.config/user-dirs.dirs` with a strict parser, refuse unsafe places, show it
  (`music: ~/Music (rw)`) and pin it; at start the folder is granted only if it still matches. The app gets a
  private `user-dirs.dirs`. In the isolated view the home folder can be listed.
- **Opening links from the isolated view.** `xdg-open` (and GIO, Qt and Electron through it) in the view
  asks the outer stub over a private socket, which passes on `http`, `https` and `mailto` addresses only
  (with only the plain `mailto` fields), at most 5 per second. The stub now stays running while the app runs.
- **URL schemes: `[desktop] schemes`.** The `.desktop` file declares `x-scheme-handler/<scheme>` and runs the
  launcher with `%U`.
- **Copies of one app** share a `/tmp` and a `/dev/shm` of their own in the isolated view; a start with no
  copy alive empties them. Two copies still do not see each other's System V IPC without `x11`.

- **Private folders for each app.** The stub gives every app its own
  config, data, state, tmp and cache folders (mode 0700, outside the store) and
  points `XDG_*`, `TMPDIR` and `SQLITE_TMPDIR` at them. Writes go only there,
  to `/tmp` and to the `home_paths` of the manifest. The app can read the
  user's theme, font and icon settings and nothing else of the real config.
  `~/.local/share/<id>` is no longer used and is not migrated: move the files
  by hand with `panpak data path <id>`. New commands `panpak data path|clear`.
- **Manifest: `[launch]` and `permissions.home_paths`.** Extra start arguments
  and environment (with placeholders for the private folders), and fixed home
  paths as a last resort. `install` shows the permissions; `update` asks before
  it adds any. A package that asks for the D-Bus socket gets a warning.
- **Local sockets.** The stub removes `SSH_AUTH_SOCK`, `GPG_AGENT_INFO` and
  `SSH_AGENT_PID` and sets `DBUS_SESSION_BUS_ADDRESS` and `DBUS_SYSTEM_BUS_ADDRESS` to `disabled:` (each unless the
  manifest lists that bus). `doctor` shows whether the kernel can isolate
  an app from local sockets, and `install` and `update` warn when it cannot.
  `install` over an installed app asks before it adds permissions.
  `--yes` no longer accepts new permissions: use a terminal answer or the new
  `--accept-new-permissions`. `gsettings` is looked for only in `/usr/bin` and
  `/bin`.
- **Socket probe** (`PANPAK_SOCKET_PROBE=<log>`), a measurement mode, never on by default:
  the stub supervises the app and logs its socket and namespace calls. It blocks nothing.
  `DBUS_SYSTEM_BUS_ADDRESS` is hidden like the session bus. See docs/SOCKET-PROBE.md.
- **Isolated view (bubblewrap).** If `/usr/bin/bwrap` works, the app runs in a view with `/` read-only,
  an empty home folder (only the app folders, `home_paths`, the readable settings, the tree and the stub
  come back), a private `/tmp` (`TMPDIR=/tmp`, which also fixes the long-path crash of Simplenote), a `/run`
  and a runtime folder that hold only the allowed sockets (Wayland, journal, resolver, the `sockets_allow` list),
  its own process list and, without X11, its own IPC namespace. Inside it the session and system buses,
  systemd of the user, gpg-agent and ssh-agent do not exist. Landlock is applied on top. Without bubblewrap
  the start is as before. New manifest key `permissions.audio` (PipeWire and PulseAudio sockets).
  `socket_isolation` now reports the active layers; `doctor`, `install` and `update` show them, and `doctor`
  says why the view is unavailable and how to fix it. `sockets_allow` also works on kernels below Landlock V9
  when the view is available. Landlock V9 gets `ResolveUnix` rules for the same sockets (tested on Fedora 44,
  kernel 7.2.7; see the fix below).
- **Libraries: the bundle first.** When a library is in the package, the package copy is used, except for
  glibc and the graphics stack (which must match the system and the driver) and for `libstdc++` and
  `libgcc_s` (the copy with every symbol version the app needs). A library that is not in the package comes
  from the system, as before. This fixes `symbol lookup error` with a system library of the same name and a
  different API (Audacity and `libSoundTouch.so.1`). `panpak doctor` checks, for an installed app, that the
  symbols it needs are defined by the chosen libraries. The format does not change. See PANPAK-FORMAT.md §7.
- **Libraries: families and a symbol check.** GLib, GTK (with GDK, Pango, Cairo and the like), GStreamer and FFmpeg move
  whole, as Qt already did: all from the package or all from the system. After the rules, the runtime looks for symbols
  that no chosen library defines and, if it finds some, takes the library (with its family) from its other copy, until
  nothing is missing or nothing can change (at most 8 steps). If some remain, the app still starts, with a warning, and
  `panpak doctor` lists them. This fixes QOwnNotes, whose bundled GLib was older than the system libraries it also loads.
- **Resolution cache and speed.** The cache of the library resolution is now keyed by the package root and its `[abi]`
  fields, the stub, the kernel, `/etc/ld.so.cache` and the identity of every system library the decision looked at (also
  the ones not chosen) and of the loader, so an update of `libglib` or `libcrypto` makes the next start resolve again.
  A hit no longer hashes the package or runs `ldconfig`. The ELF files are read once each with `pread`, only the sections
  that matter, on several threads: the resolution of Kdenlive (449 libraries) takes about 40-140 ms instead of 1.2-1.4 s.
- **GTK3 dark theme** from `gsettings` at start (at most 200 ms).
- **Security: paths and text from the manifest.** `app.id` has one strict
  grammar (spec §3.1.1) and every manifest field that becomes a path, a file
  name, a `.desktop` line or terminal text is checked in `verify`, so a signed
  package cannot make `panpak` or the stub write outside the store (new codes
  `PANPAK-E0006`, `PANPAK-E0007`). `rollback` and `open` check the id they are
  given. Index paths and symlink targets reject control characters.
- **Security: launchers and `.desktop` files.** Written through a temporary
  file and a rename, never through a symbolic link, and only over a file that
  Panpak wrote for the same app (`PANPAK-E2001`). Values are escaped as the
  Desktop Entry Specification requires and the `Exec` path is quoted.
  `panpak convert` escapes the strings it copies from an AppImage.
- **Test and bench scripts** refuse to run when HOME is not inside their work
  directory and never delete outside it; `bench/campaign.py` no longer copies
  into `~/Applications`.

## 0.1.0

First version. Format v1, runtime ABI 1.

- **Package format**: `.panpak` file (`.pan` accepted as an alias; a file is
  identified by its magic and signature, never by its extension) with a static
  executable stub, deduplicated zstd chunks, a signed index, a signed manifest
  and an 80-byte footer. Specification in `PANPAK-FORMAT.md`.
- **Verification before execution**: Ed25519 signature over manifest and root
  hash, BLAKE3 hash of the stub bound to the signature, per-chunk BLAKE3
  check before writing. Typed `PANPAK-Exxxx` error codes.
- **Sandbox**: Landlock confinement with no silent fallback, read-only
  trees, private SQLite temporary directory, optional network and device
  allowlists.
- **Trust model**: key shown at install and pinned (TOFU), signed key
  rotation, fail-closed ABI check of the host libraries.
- **CLI** (`panpak`): `keygen`, `pack`, `convert` (AppImage), `install`,
  `install-runtime`, `update`, `rollback`, `open`, `doctor`, `licenses`,
  `release`, `dump-index`, `dump-manifest`.
- **Updates**: incremental updates over HTTPS with range requests, reusing
  the chunks already present.
- **Desktop integration**: launchers and `.desktop` entries, the MIME type
  `application/x-panpak` for the packages (`panpak desktop-integration
  install|remove`); explicit file open for double click.
- **Quality**: conformance suite with an independent Python verifier, fuzz
  targets for every parser (corpus seeds generated by `fuzz/gen-seeds.py`),
  reproducible builds (`scripts/repro-build.sh`, `Dockerfile.repro`),
  SPDX headers, cargo-deny and gitleaks in CI.
