# Panpak — threat model

Status: v1 (runtime ABI 1), written for the outside review of the format and the trust layer
(`PANPAK-FORMAT.md` §0.1 says what to read first; §1.1 here is the table of the attackers of the
trust layer). Empirically verified where stated (tests), assumed where explicitly marked.

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
- **A4 — network and kernel**: out of model (no network filtering; the kernel
  is trusted except for the system calls that the seccomp filter of §2.5 refuses,
  see §3 and §5).

### 1.1 The trust layer: who may sign, and what each attacker can do

A3 (hostile update) is the attacker of the trust layer (`PANPAK-FORMAT.md` §3.0 and §3.2). This table is what a reviewer should try to make false. The keys are the **root**
(signs packages; on the machine of the author or a token) and the **recovery key** (24 words on paper; only its commitment is in a package).

| Attacker | Can | Cannot | Covered by |
|---|---|---|---|
| Whoever serves the package (a mirror, a CDN, a compromised distribution point) | freeze an app at an old version, hide a new one, serve a package that is not for you | make an installed app run code that its author did not sign; change the permissions of a signed version; replay an older version below the floor or a fork of one revision | signature on a labeled message that covers the manifest and the root of the index; the pin; the revision, the floor and the history (§3.1.10) |
| A network attacker at an update | the same as above | the same as above | the same, and HTTPS for the downloads |
| A network attacker, or a hostile page, at the **first** install | supply a package of their own key | make it look confirmed if the author published the domain file and the person (or script) is online and asks it | the domain confirmation (§3.2.1), `--expect-key`; **not** covered when the author has no file, the check is off, or the attacker controls the web server of the domain (KL-17) |
| A thief of the root key | sign what the key signs: a package, or a succession to a key of the thief that keeps the commitment; the pin of every client that updates moves to the thief | change the recovery commitment; stop the author from taking the app back with the 24 words | succession rules: by the root, the commitment cannot change; by the recovery, it wins over the root (§3.2 rule 4); a visible notice at every succession |
| A thief of the root key, **before** the author notices | reach the clients that update first, with no expiry to bound the window | | **not covered**: the mitigation is custody (a token, `--sign-command`); a delegation with an expiry is designed for the beta and only its names are reserved |
| A thief of the recovery words | take the app back from the author (the same power as the author's own recovery) | | **not covered**: on paper, offline; lost together with the root they are lost for good |
| The domain of the author contradicts, or is made to | refuse a first install or a succession (a denial of service by whoever controls the domain file) | accept a package that the checks of §3.0 and §3.2 refuse | `--despite-domain-mismatch`, which still asks whom to trust; a contradiction is never silent |
| A look-alike domain | host a package whose `[trust].domain` names itself and confirm its own key | pass as the real domain: the name of the domain is the main text of the prompt, on a line of its own and in Punycode if it is not ASCII, but a person who does not read it is not protected | **not covered** (KL-17) |
| A coerced or malicious author | sign anything | | **not covered**: no human review, no transparency log (the signature block reserves a type for a proof; none is implemented) |
| The same user, outside the sandbox | rewrite the store, the pin, the floor | | out of model (KL-18) |

What the design gives up on purpose: no clock (no expiry, no delay before a recovered root is accepted: a clock is something to attack), no threshold of keys (the field is
reserved at 1), no list of serials, no catalog that can block an app (a catalog may only put it in protected mode, later). Those are listed in the design notes as discarded,
with the reason.

The local store (`~/.local/share/panpak`) is considered **trusted**
(same uid, non-hostile fs). Same-uid competitors are out of model.

## 2. What the sandbox guarantees (tested)

- **No execution before verification** (spec §5): Ed25519 signature
  over a labeled message (footer `version` and `flags`, `root_hash`, manifest; spec §3.0), BLAKE3 hash of the stub bound to the
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
- **Nothing written is executed** (W^X): the rights on every folder the app can write lack
  Execute unless the package asks for `exec_written` (PANPAK-FORMAT.md §3.1.8). An app cannot
  download a program and run it, nor build one and run it. `mmap(PROT_EXEC)` is not covered by
  Landlock and stays possible. Tests: `a_program_the_app_wrote_is_not_executed_unless_exec_written_is_asked_for`, `tests/app_dirs.py`.
- **No silent fallback**: without Landlock the stub aborts;
  post-restrict self-test (`/dev/null` openable, create on tree denied).
- **TOFU + pin**: the key is self-signed (proves no identity); at the
  first `install` the fingerprint is shown and pinned, then enforced
  at every start (key change = refusal, tested).
- **Fail-closed ABI**: missing libs or incoherent B-world = error with
  a message, never a silently wrong lib (4-distro matrix).
- **Explicit file open** (`panpak open <app> <file>`, double click):
  the sandbox grants ONLY that file, for reading and writing (a person
  who opens a file with an app means to work on it: principle 3 of
  docs/PRINCIPLES.md; `--read-only` takes away only the write of that grant, not a read-write folder right that the
  manifest already gives the app: with such a right the file can still be overwritten), before startup,
  without widening anything else: a program that saves by a temporary
  file and a rename in the folder fails (fail-closed, tested), and the
  folder is never opened but by `--write-rename`. Since 2026-11-23 the
  write is the default; before it was opt-in with `--write`, and a save
  with Ctrl+S ended in "Save as".
  The path is canonicalized (the rule covers the real target of
  symlinks); symlinks/hardlinks to NON-granted files stay denied,
  even in the same folder (fork tests: sibling, symlink, hardlink,
  create denied; names with spaces and special characters ok). The
  picker, if needed, runs outside the sandbox (host portal/picker).
- **Opt-in atomic save** (`--write-rename`, never default, and only used without the document portal: with it the file is a document and the portal lets the app save by replacing it):
  editors saving via temp+rename (e.g. vim with backup, gedit) fail
  with the file-only rule (fail-closed: the temp is never created).
  The opt-in adds to the parent folder ONLY execute (traversal),
  create, rename and remove: NO read and NO listing (neighbors
  unreadable and unlistable, tested). Documented cost: neighbors stay
  overwritable and deletable (writing the temp requires WriteFile on
  the folder, which also covers them; deletion requires RemoveFile). A Landlock rule on a
  folder covers everything below it, so the grant reaches the subfolders too: it is refused for
  the home, for a folder that holds it, and for a folder that holds or lies in a place that starts
  programs later (`~/.config/autostart`, `~/.local/bin`, `~/.ssh`, the shell start files; the
  list is `STARTUP_PLACES` in the stub).
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
| `panpak convert --permissions <file>` reads a file that the converter gives and adds its `[permissions]` to the manifest, so a conversion can ask for any permission; `--wm-class` and `--update-url` carry one value each into the manifest | The converter is trusted for what it declares, as an author is, and the permissions are shown at install as for any package (an update that asks for more waits for the user). Only `[permissions]` tables are read (another table is refused), each value goes through the checks of `pack`, `--wm-class` must be a plain window class and `--update-url` an https address. Tests: `the_permissions_option_declares_what_the_converter_gives_the_app_and_reads_nothing_else`, `the_wm_class_option_sets_the_window_class_that_the_desktop_groups_by`, `a_conversion_can_name_where_it_is_updated_from_and_follow_an_earlier_one` (`convert_icons.rs`) |
| `panpak convert --append <path>=<file>` changes a file of the program in the tree (adds text at its end), so the converter decides a setting for everyone who installs the conversion | The converter is trusted for what it signs, as for the files that it takes from the AppImage; the option can only add at most 4 KiB of text to a regular file that the AppImage already has, with no link on the way and a path that stays inside the tree (never a new file, never a link, never a folder), and the report of the conversion says that it was done and why. The change is in the signed index like any file. Tests: `the_append_option_adds_a_setting_at_the_end_of_a_file_of_the_app_and_nothing_else` (`convert_icons.rs`) and `append_to_tree_file_adds_the_text_keeps_the_mode_and_follows_no_link` |
| `panpak convert` unpacks an AppImage into a folder and writes into it (wrappers, `panpak.toml`); a link in the AppImage (`usr -> /some/folder`, `bin -> ..`, `panpak.toml -> ~/.bashrc`) makes a write land outside the folder | The unpacked tree is a private `mkdtemp` folder (0700, removed on every way out). Every write to it goes through folder descriptors that never follow a link (`panpak_core::safefs`, `panpak/src/treewriter.rs`): folders are made name by name, files with `O_EXCL | O_NOFOLLOW`, links with `symlinkat`, and `panpak.toml` and the wrappers replace what is there instead of being written through it. What `convert` then reads by path (the desktop file, the icons, the entry) cannot be led out of the folder by two links that are each inside it and leave together (`d/e/l -> ..`, `x -> d/e/l/../../secret`): every link is followed again as the file system does (`resolves_inside`) and one that leaves is not taken, and `convert` says so. Icons are not written to the tree at all: they are read (links followed only inside the tree) and handed to `pack` in memory, and `pack` puts them in the package under `.panpak/icons/`. Tests: `tests/convert_links.py` (two links that leave together), `a_chain_of_links_is_followed_the_way_the_file_system_does`, `a_link_in_the_way_never_lets_a_write_leave_the_tree`, `a_usr_link_to_a_folder_outside_the_tree_is_never_written_through_or_read`, `a_bin_link_to_a_folder_outside_the_tree_stops_the_wrapper_from_being_written` |
| A small AppImage decompresses to hundreds of gigabytes (or millions of files) and fills the disk, or the memory when the temporary folder is a tmpfs | `convert` counts before it writes: the index of the squashfs gives the number of entries and the size of every file, and over 8 GiB for the whole tree (what `pack` takes), 8 GiB for one file or 1,000,000 entries it stops with nothing written (`Limits::check_index`). It unpacks in `panpak/convert/` of the cache folder (`$XDG_CACHE_HOME`, or `~/.cache`), on disk, never in `$TMPDIR` or `/tmp`, and refuses a cache folder that is in memory (tmpfs, ramfs). While it writes, the read of a file is cut at what is left. Tests: `tests/convert_workdir.py`, `the_index_is_checked_before_anything_is_written`, `a_file_larger_than_the_limit_stops_the_unpacking`, `the_whole_tree_and_the_number_of_entries_have_limits` |
| An app stopped by a signal leaves a lock file that names a process id; in the next view process ids start again at 1, so the file looks alive and the app does not start (denial of use after a logout or a `kill`) | The last copy to end removes the stale lock files of the private folders at exit, before the mark "in use" (PANPAK-FORMAT.md §9.4); the next start searches again if the mark stayed (a hard kill of the runtime itself). A copy that is not the last touches nothing. Test: `tests/stale_lock_exit.py` |
| A launcher path with shell metacharacters (`$`, backtick, `'`) in `$HOME` runs code when the launcher starts | Paths are single-quoted in the generated launcher; control characters in them are refused |

| A package supplies an icon that a host library (gdk-pixbuf, Qt, librsvg) decodes outside the sandbox, in the shell, a dock or a file dialog, and a parser bug runs code or an image bomb exhausts memory | The icon is checked by `panpak-icon` in `pack`, `convert`, `install` and `update` (PANPAK-FORMAT.md §3.1.3): PNG only (SVG is refused), a short list of chunks with right checksums (`IHDR`, `PLTE`, `tRNS`, `IDAT`, `IEND`; `pack` and `convert` remove every other chunk with one deterministic function and say which, `install` accepts only a file that is already clean), a square image of 16 to 1024 pixels, 8 bits, not interlaced, a file of at most 256 KiB, and image data that inflates, with a bound, to exactly the size the header says. The file is written as it was read: nothing is re-encoded. Fuzz target `fuzz_icon`; tests: `the_header_rules`, `only_listed_chunks_are_accepted`, `the_image_data_has_exactly_the_promised_size` |
| A package makes the menu entry carry a key that starts a program or changes the desktop (`Exec`, `TryExec`, `DBusActivatable`, `X-GNOME-Autostart-*`) | The entry is made from a template; the package gives only the name, the window class and the URL schemes, each checked. A test lists the keys the writer may emit (`only_the_allowed_keys_are_ever_written`) and feeds it hostile values |
| The lazy update of the entry at the start of an app overwrites a file that is not panpak's, or follows a link | The stub opens the entry with `O_NOFOLLOW`, rewrites it only when it is a regular file with the marker for this app, writes through a temporary file and `rename`, never creates a missing entry, and a failure never stops the start. Tests: `what_is_not_ours_is_left_alone`, `a_launcher_that_is_not_ours_stays_but_the_entry_is_updated` |

Not covered: a same-uid process that swaps a destination between the check and
the rename (the home directory is trusted, see §1), and a parent directory
(for example `~/.local/bin`) that is itself a link, which users may set up on
purpose. The test and bench scripts are development tools; they refuse to run
with a HOME outside their work directory and never delete outside it
(`tests/isolation.py`).

### 2.2 Private folders, user settings and home paths (tested)

Each app has its own home (spec §9.1), created with mode 0700 outside the store:
`$XDG_DATA_HOME/panpak/userdata/<id>/{.config,.local/share,.local/state}` and the cache
`$XDG_CACHE_HOME/panpak/<id>`. In the isolated view the private home is mounted at the path of
the real home and the cache as `~/.cache`; `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME` name
the folders there, and these are the only writable places besides `/tmp` (a tmpfs of the view with a size cap of 256 MiB) and the `rw` home paths of the manifest. Without
the view `HOME` is the private home. The old `~/.local/share/<id>` is not touched and not writable. What the app
writes in its home is its own data: it cannot reach the real home, so a program it starts finds only that. The user's fonts,
themes, icons and toolkit settings are mounted into it read-only (§9.1): the app cannot change them. A file the app writes in its home may
be a program (a script, a binary): the view does not stop it from being run (§3, W^X is a separate decision).
`tests/app_dirs.py` runs an app in an isolated HOME and checks that:

- writes through each XDG variable and `TMPDIR` land in the right folders, and a file written anywhere in `~` lands in the private home;
- nothing the app writes reaches the real home: `~/.config/<anything>` and `~/stray` are in the private home only;
- `~/.config/gtk-3.0` and the theme folders of `~/.local/share/icons` can be read and not written, and the real files are unchanged; `gtk-3.0/bookmarks` is empty and `~/.local/share/icons/hicolor` (where the menu icons go) is not in the view;
- `~/.config/gh` with a fake token file cannot be read, and `~/.config` lists only what is in the private home;
- missing optional paths do not stop the start;
- `panpak data clear` deletes only the two roots, follows no link and refuses a
  root that is a link.

The user's folders are mounted as they are, read-only: there is no copy of them in the private home (the mount points are
empty), no upper layer, and so no memory an app could fill by writing there. A file the user changes while a running app has it open
may be seen half changed, as for any program that reads a file that is being written. The mounts show what is in the folder
of the user at the time, so what is in them is as private as the folder: the settings of the theme can reveal which software the user
runs and hold no secret (`gtk-3.0/bookmarks` and `servers`, which list what the user has been doing, are empty files).

`permissions.home_paths` (spec §3.1.2) is the last resort for apps that insist on
a fixed place in the home folder. The manifest validation refuses broad
folders and a list of places that run code at login or hold credentials. The
list is incomplete by nature: a path outside it is not thereby harmless, and
`install` lists the access at the High risk level, and the summary says "Partial isolation", so the user can judge.
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

- **`xdg_dirs`.** The folder is found by `install` outside the sandbox, with a parser that takes only the lines it understands and never grants a key it does not know, shown to the user
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

### 2.4 Removal (tested in panpak/tests/remove.rs)

The folders that `panpak remove` deletes in (the store, `userdata`, the cache, the runtime folder) can be written
by the user and by any program that runs as the user, including another app whose permissions give it a `home_paths`
entry. A link put in place of a folder must not make the command delete something else.

- Below the first folder every step is `openat`, `fstatat`, `unlinkat` or `renameat2` on a descriptor of the folder
  that was opened before, with `O_NOFOLLOW`; no path is built from strings. The folder a walk enters is the one that
  was looked at (same device and inode), a link met inside is removed itself, and a walk does not cross to another file
  system and stops at 512 levels (leaving the rest in place and saying so).
- A data or cache folder that is a link is not followed and not removed; the command ends with `PANPAK-E2003` naming it.
  A link in place of `apps/<id>`, a tree or a farm is moved or unlinked as a link, never entered. Tested for each place the
  command touches (`a_link_in_place_of_any_folder_never_deletes_outside`).
- The folder of the app is moved to `removing/` by one rename before anything shared is deleted, with a journal. If the
  process is killed, the next command finishes from the journal, which is parsed strictly (package names are plain file names,
  roots are 64 hex digits) so that a damaged or planted journal cannot name a path outside the store. A fuzz target
  (`fuzz_journal`) covers the parser. An install of the same id first finishes any interrupted removal.
- A removal cannot run while a copy of the app is alive (the lock of the copies); the lock is held for the whole removal.
  A start takes the shared lock of the copies before it reads anything of `apps/<id>` and checks again, once it has the
  lock, that the app is still installed: a start that comes in during a removal waits, then ends with `PANPAK-E2004` and
  does not run the app (tests/remove_race.py). Without bubblewrap the lock goes on to the app itself, so a copy is still seen.
- A launcher or desktop entry that panpak did not write (no marker), or that is a link, is left alone and listed.

The menu action `remove --ask` runs with the rights of the user from an entry that panpak wrote with the absolute path of
its own binary; it uses `zenity`, `kdialog`, `notify-send` or `gdbus` only from `/usr/bin`, `/bin` and `/run/current-system/sw/bin`,
passes the app name (which comes from the package) as one argument without a shell (and, without those tools, as text of a notification of the user's own session, to the notification service of the desktop and, if it is not there, to the portal, with the characters that start markup written as entities and no control characters; the buttons are the only way to answer, an answer from the service counts only if the signal is sent by the owner of the name `org.freedesktop.Notifications` and carries the number of that notification (the number is small and another program of the session can guess it, but cannot write another sender), and a question is withdrawn after 5 minutes on the service and 1 minute on the portal, which is asked only from a scope of an app, because it refuses other callers in silence: a late click does nothing), removes markup characters and control
characters from it, and never deletes data. Another program that can write the user's `~/.local/share/applications` could
point an action elsewhere, but it could do the same with the launcher entry itself.

What `remove` does not do: it does not delete entries of the desktop's permission store (`org.freedesktop.impl.portal.PermissionStore`)
or the document portal for `panpak.<id>`. Measured on a desktop that ran the test apps: the stores had no entry for any
`panpak.*` app (`documents` empty; `notifications` only for snap apps), and the folders `doc/by-app/panpak.<id>` of the document portal
are virtual (they exist for any name). Entries will exist once an app is granted a document (step 5b); `remove` should
then call `PermissionStore.DeletePermission` for the app and revoke its documents. Until then there is nothing to delete.

### 2.5 Hardening of the app process (tested in tests/hardening.py and the unit tests)

- **What the store guarantees about the installed files, and what it does not.** (PANPAK-FORMAT.md 6.2 to 6.5.)
  *Guaranteed, and tested:* the signature and the pin are checked at every start; a version is never named (`current`, a tree, a blob) before what it leads to is on disk,
  so a crash or a power loss leaves the app on a whole version (`tests/interrupt_8a.py`, `tests/blob_visibility.py`, `tests/failpoints.py`: every step of every command stopped);
  a blob in `blobs/` was complete and on disk when it got its name; two commands never change one app at once (the lock of the app); `install`, `update` and `repair` check every blob they reuse
  against the signed index, large or small (the file of a tree is a hard link to the blob, so a tree would inherit a damaged or altered file; this replaces the rule of step 8d that `install` did not hash a blob that was in the store, and costs one read of the file); `panpak doctor <app id>` compares the whole tree with the signed index; a start whose files are gone stops with a message and a command (`PANPAK-E2007`).
  *Not guaranteed:* that the files are what they were at install. a start does not hash the tree, because **a program
  that can write the store of the user can change an installed tree** (and the launchers, the records, `panpak` itself): the signature cannot protect against what runs as the user outside the sandbox.
  The sandboxed app cannot write the store (the tree is read-only to it, and the folders outside its own are not granted). Bit rot is not guaranteed against either, except as below.
  *Built in step 8d (`PANPAK-FORMAT.md` 6.7):* the daily background job of each app also reads part of the installed files against the signed index: a budget of megabytes per day
  (`background.verify_mb`, 64 by default, 0 turns it off), going round all the files in the order of the index over the days (the cursor is in `update-state`). What it finds is noted there, shown by
  `panpak list` and `panpak doctor`, and repaired from the chunks of the update URL when no copy of the app runs (right away, or when the last copy ends; a start never waits for the network). It finds silent damage; it does not
  stop a program that is running as the user from changing a file after the check (or from writing `update-state`), and it is not integrity protection against such a program. Limits that remain:
  a repair from the update URL needs the URL to serve the installed version (when it has moved on, `panpak repair --from <package>` or an update is the way), and only the installed version is
  read, not `previous`.
- **A staged update is applied only as the user would have applied it.** The background job (step 8d) and `panpak update --stage` download and prepare a version; they do not move `current`. At the start of the app the stub applies it only when the checks of `PANPAK-FORMAT.md` 6.6 hold again at that moment (signature, pin or a succession signed by the pin, a complete tree), no copy of the app runs, and the version asks for no permission that the installed one lacks: a version that asks for more waits until the user accepts the permissions (`panpak update --accept-new-permissions`), however long that takes. A staging that was tampered with (a link to another record, a record that does not verify, a key that is not the pin) is refused at apply, not trusted from the staging. What it does not do: it does not protect against a program running as the user that replaces `current` or the pin itself (see above).
- **Test points are not in a release.** The names, the message and the variable that arms a point exist only in test builds; CI fails if a release binary holds one (`scripts/check-no-failpoints.sh`).
- **The record of an installed version is not a package.** The store keeps the signed index and manifest of each installed version in a file with
  its own magic (`PANPAK-FORMAT.md` §6.3), not the package. A reader of packages refuses it (`PANPAK-E0010`), and the stub runs one only when its
  path resolves inside `<store>/packages/`, so a record that is copied out of the store cannot be installed, and a file put in the store cannot be
  taken for a package. Tested: `panpak-core` unit tests, `tests/store_flow.py`. What it does not do: a program that can write the user's store
  (the app cannot: it has no access to it) could replace a tree; the signature is checked at every start, the tree is not hashed at every start
  (`panpak doctor <app id>` compares it with the signed index, `panpak repair` makes it right).
- **The library path reaches only the app's own programs.** `LD_LIBRARY_PATH` of the app is `/$ORIGIN/.panpak-lib-<h>/...`
  (`PANPAK-FORMAT.md` §7.1), not a list of paths of the store. A system program that the app starts inherits the text but
  finds no such folder next to itself, and uses its own libraries (before this, it loaded the package's: a defect, and a way
  for a package to change what a host program runs). Tested: the harness reads the environment of every process of a run and
  fails when the outer stub, bubblewrap or a helper holds the variable, or when the app holds a path of the store; unit tests;
  checked by hand on glibc 2.39, 2.41, 2.43 and 2.44 and on musl (the slash in front of `$ORIGIN` keeps a folder of the current
  directory from being used there). It is a rule about names, not a barrier: a program of the package can still set its own
  `LD_LIBRARY_PATH` or `LD_PRELOAD` for what it starts, as it could before, inside the same sandbox.
- **Seccomp deny list.** The inner stub installs a filter as the last step before the exec, after Landlock, with
  `no_new_privs`, on all threads; nothing runs between the filter and the exec (every start goes through one function,
  `exec_filtered`). It returns an error number, it does not kill. It refuses `io_uring_*` (`ENOSYS`), `bpf`,
  `userfaultfd`, `perf_event_open`, the kernel key ring, `ptrace`, `process_vm_*`, `pidfd_getfd`, `kcmp`, kernel modules and
  `kexec`, the mount calls old and new (`ENOSYS` for the new ones), `chroot`, `setns`, `open_by_handle_at`,
  `syslog`, `reboot` and the like; `unshare` and `clone` with any new namespace (user namespaces first of all); `clone3`
  (`ENOSYS`, so that the C library goes back to `clone`); `ioctl` `TIOCSTI` and `TIOCLINUX`; `personality` other than plain Linux;
  every non-native architecture and x32. The full table with the reason of each entry is `PANPAK-FORMAT.md` §9.8. Tested:
  the program against an interpreter for every entry; the filter loaded in a child process of the real kernel; the same calls from an app
  started by the real launcher (ENOSYS or EPERM as documented, threads, fork and exec still work). Costs about 0.1 ms
  at start and tens of nanoseconds a call. `panpak doctor` reports kernel support; without it the app does not start.
- **Device ioctl.** From Landlock V5 the app can do an ioctl only on `/dev/tty`, `/dev/dri` and the pseudo-terminals `ptmx` and `pts` (and on descriptors it
  inherited, such as its terminal). `/dev/null`, `/dev/zero`, `/dev/urandom` and the other nodes refuse it with `EACCES`
  (tested per device).
- **Variables that point at the tree (`[env]`).** The manifest can set a short list of variables, with values made of `${PANPAK_ROOT}` and a relative path inside the tree
  (PANPAK-FORMAT.md §3.1.6). The list leaves out everything that changes how code is loaded from outside the tree (`LD_*`) or that the runtime protects; `pack`, `install` and the runtime check
  each value, and `pack` refuses a path that is not in the tree. What a package can load is therefore what its signed files contain. It replaces the shell script that a source build used to need as its entry.
- **Builds from source (`panpak build`).** The build runs in a user namespace of bubblewrap with one user id, no network (except the first run of a recipe that says `network = "fetch"`), the source
  read-only, and a memory cap. The commands of the recipe get an empty environment (only the variables the build sets; none of the user's) and their own terminal session (`--clearenv`, `--new-session`).
  The steps that fetch (`apt-get`, and the first Zig build with `network = "fetch"`) run with the network of the host, so a recipe must be trusted for those steps; a package name that starts with `-` is refused.
  What the build leaves in its output and what a source tarball unpacks is never followed: a link that would take a write out of the tree or of the source folder stops the build.
  It is a build sandbox, not a security boundary against a malicious build script: a recipe is code, and the commit or hash it pins is what the author vouches for.
  The package records where the source and the image came from (`[provenance]`), and the libraries taken from the image instead of the pinned source are declared one by one.
- **Pseudo-terminals.** Every app may open a pty (terminals, IDEs and editors with a terminal need one), so it is not a permission. What makes it safe is
  that the view has a devpts of its own: a pty of the app is not a terminal of the user, and the app cannot read the keys the user types in another terminal
  or write into one. `TIOCSTI` and `TIOCLINUX` are refused by seccomp. A shell on a pty is still inside the same view, Landlock and seccomp as the app.
  A pty gives no new authority; what it removes is a reason why a whole class of apps could not run (`tests/pty.py`: shell, window size, job control, a full-screen program).
- **Wayland socket with a security context.** When the compositor supports `wp_security_context_v1` the app is given a
  socket of its own that the compositor knows is sandboxed (engine `io.github.panpak`, app id `panpak.<id>`), instead of the user's own
  socket. What a sandboxed client may do is the compositor's choice (screen capture, virtual input and global shortcuts are the usual
  refusals). Tested with a fake compositor, with the real Mutter (which does not offer the protocol: the app keeps the normal
  socket and `doctor` says so), and a parser fuzzed on its own (`fuzz_wayland`).
- **The resident stub in two pieces.** What reads the bytes of the app (the open-address channel) is a receiver process with no rights:
  Landlock with no rule, a seccomp allow list of 17 calls, `no_new_privs`, 32 files, 128 MiB, no new process. It passes
  validated requests to the actor (the outer stub) as frames, which the actor checks again and answers; a frame
  that is not a frame ends the conversation. Taking over the receiver gives an attacker two sockets it already had. Fuzz target
  `fuzz_receiver`; tests: the receiver runs, serves, and shows its confinement in `/proc`; the actor against a hostile receiver.
- **System font cache.** `/var/cache/fontconfig` is read-only in the view, which makes the first start with an empty private
  font cache 9 times faster (446 ms to 48 ms with `fc-match`); read-only, so an app cannot poison the cache of other apps.

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
  a bus socket makes `install` and `update` list "Use the message bus of your whole
  desktop" at the High risk level, and the summary says "Partial isolation". An app that does not list a bus
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
  `app-panpak.<id>-<n>.scope`, even when started from a terminal (or reuses the scope the desktop gave the app, when its name already gives `panpak.<id>`), and starts the proxy only after the scope exists (the portal sees the
  cgroup of the proxy; tests/dbus_filter.py fails if the order is wrong); otherwise the app would be
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
  `network = true` **without ports is the whole network** (as `--share=network`
  in Flatpak): TCP is not filtered, any port of any address, the loopback and the
  local network included, and the start does not need Landlock V4. The permission
  screen says "Can use the network without limits." An app with such a permission can
  send any data it can read to any server, and can reach services that listen on the
  loopback of the host; the answer is to list the ports (the filter then stays), or not
  to give the permission. **The change reaches apps that are already installed:** the stub of
  the installed runtime does the filtering, so an app that was installed with
  `network = true` and no ports (and could use only UDP until now) gets TCP as soon as the runtime
  is updated, with no new question: its signed manifest did not change, and the row the person
  accepted ("Use the network") is the same. An update of the app that goes from listed ports to no
  ports is a new row and is asked about. The listed ports of an app with ports are a real limit only for TCP:
  the screen says it once, in a row under the rows of the ports: other traffic, such as UDP, is not limited.
  Ephemeral ports (`bind` on port 0 + rebind, e.g. the OpenShot
  thumbnail server) canNOT be expressed with discrete allowlists:
  verified with a probe on ABI V8 (bind(0) denied by default and with
  fixed-port allowlists; a port-0 rule only unlocks bind(0), not the
  rebind to the assigned port) → no permission added. Filtering by
  address (loopback only) is not possible: Landlock filters ports, not
  addresses — the warning would stay mandatory even with a permission.
  On ABI < V4 the network is NOT filterable: `network = true` with ports is always
  a hard error (`network = true` without ports has no filter and starts);
  `network = false` is default refusal, bypassable ONLY
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
  truncated chunks). The chunks of an update are asked for by ranges: neighbours with up to 16 KiB between them
  are one request (those bytes are dropped, never used; a chunk is checked by its hash when it is used), at most 4 MiB for each response and at most 6
  requests at once, so a server cannot make the client take more than 6 x 4 MiB in memory, and at most 16 KiB more than asked for each range. A server that answers 429 or 503 is
  waited for as it asks (`Retry-After`, in seconds or as a date, up to 300 s; a longer wait ends the update with the time it asked, and what came is kept), and the rest of
  the download goes one request at a time; a range that is refused six times ends the update. Tested with a local server that answers busy (`panpak/src/ranges.rs`).
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
- **The seccomp filter is a deny list.** It refuses the calls of §2.5 and lets everything else through, so a
  call that is not on the list and is dangerous (a new one, or one that nobody listed) stays open; a kernel
  bug in a call that remains is not stopped. The filter removes the large surfaces that exploits start from
  (`io_uring`, `bpf`, `userfaultfd`, user namespaces); it is not a complete system call policy. See KL-10.
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
- **Initial TOFU**: the first pin is authenticated only if the author
  published a domain file and the client could ask for it (§1.1,
  `PANPAK-FORMAT.md` §3.2.1). Otherwise whoever distributes the first
  package over a hostile channel can pin the wrong key. Mitigation:
  the domain confirmation, or out-of-band comparison of the key (shown whenever the
  domain does not confirm it).

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
- **GVfs** (spec §3.1.5, `permissions.gvfs`): an app that declares it can talk to the file services of the system (`org.gtk.vfs.*` on the host session bus) and reach the sockets of `gvfsd`. Through them it can read and
  change any file that the user can, **also outside its sandbox**, as an app of Flatpak with `--talk-name=org.gtk.vfs.*` can: GVfs does the work in a process of the session that Landlock does not confine. The permission is
  therefore shown as a high risk, the weight of the whole home folder, with that sentence on the screen of the permissions, and an update that adds it asks again. Without it nothing changes: the names are closed
  as before. It opens no more than Flatpak does (no owned name; the FUSE tree `$XDG_RUNTIME_DIR/gvfs` is opened for reading and writing, as `--filesystem=xdg-run/gvfs` does). The app then sees the network mounts of the person (`sftp://`, `smb://`, shares and the other mounts that `gvfsd-fuse` shows) also as paths of files, not only through GIO: the file access that the sentence of the permission already names, in a second form, so the weight does not change. **A share that is stuck can stop the app that reads it** (a read of the FUSE tree waits for `gvfsd-fuse`, and `gvfsd-fuse` for the server), and a stuck `gvfsd-fuse` could delay the **start** of an app that has the permission, because bubblewrap makes a `stat` of the mount that waits for the daemon (measured: `stat` and the bind wait and `SIGKILL` does not end them; Flatpak has the same limit). So the runtime reads `/proc/self/mountinfo` to know that the mount is there, never resolves it in the process that starts the app, and tests that it answers with a `stat` in a child process that it waits for at most half a second: a stuck or a dead daemon (its mount is left behind) means the app starts without the tree. What is left: the daemon that stops answering in the few milliseconds between that test and bubblewrap's `stat`, and a mount that is replaced by a link between the reading of `mountinfo` and the bind (only the user's own processes outside the sandbox, or an app that makes gvfsd do it, which already has the weight of the whole home folder, can do that: an app in the view has a runtime folder of its own, the folder is the user's, and Landlock does not allow mounts). One more exception to the rule that a link inside a folder that the app can write is not trusted (the file dialog and the files given at the start): the two folders of GVfs, `$XDG_RUNTIME_DIR/gvfsd` and the FUSE tree, are added to the writable folders after that list is made, so a link that the app puts there is followed if the person chooses it. It stays inside the weight of the permission. What it does not do: it does not give the app the host's mounts without the permission, and a system without GVfs
  gives it nothing.
- **Rollback**: `apps/<id>/previous` + `panpak rollback` (atomic swap).
- **Patches** (spec §4.1): an update can download, for a file whose chunks all changed, a zstd frame that uses the installed file as its dictionary, instead of the chunks.
  The risk is the decompressor: it reads bytes that come from the network, with a second input (the installed file) and a window larger than the one of a chunk. What
  is done about it: there is no list of patches that nobody signed: the hash of each patch is in the *signed index* of the new version, next to the file it rebuilds, so a
  server cannot change, add or swap a patch, only keep it back (and then the client downloads the chunks, as it does for a server that does not answer); the client checks that hash **before**
  it gives a byte of the patch to the decompressor, so only data with a signed hash is decompressed; the file it starts from is the installed file and its hash is signed too (the
  `base` field), so the dictionary is never data of the server; the window is limited to 2^28 (the decoder is told), the output is limited to the size that the index gives for
  the file and a frame that ends before or after it is refused, and the memory is checked first (twice the two files, and a margin), so a patch cannot take more memory than the
  chunks of the same file can; and the rebuilt file is checked chunk by chunk against the index as any file is, so a patch cannot produce a file that the index does not describe.
  What remains: a flaw in the zstd decoder that a *signed* patch could hit; the author signs it, so it is the author (A3 with the key) who could, and the same decoder reads
  every chunk already. The cost is 76 bytes in the index for each file that has a patch, and a second file on the server.
- **Kept versions and the outcome of starts** (spec §6.10): a version that fails to start twice in a row makes the client ask the person whether to go back to the last version
  that worked; it never goes back by itself, because an attacker who can make an app fail (a hostile update that crashes, a file that is damaged) must not be able to choose the version that
  runs. The question names the app and the versions only, so what it says is not data of the app. The notification "the app did not start" is built the same way (the name, and the reason in Panpak's own words: an error code, a crash or a stop, or, for a start that Panpak stopped before the app ran, the first line of Panpak's own error with the paths taken out, only for a plain start (a start with a flag of the stub that starts with `--panpak-`, such as `--panpak-open` with a file, is not told this way, because the error text of such a start can hold words of a file name that the person chose); nothing the app prints; the name is the id of the app when its record does not verify). A word that holds a `/` is replaced by `...`, and the brackets and quotes around it and the punctuation after it stay. The notification has the hint `transient` and one button that opens the log; the button counts only as an `ActionInvoked` from the owner of the name of the service for our number (the same check as the questions); the process that shows it is started by the stub, outside the sandbox. The log (`apps/<id>/last-start.log`, mode 0600, the last 16 KiB of the standard error of the failed start) is written by the stub, not by the app, in a folder that the app cannot write; it can hold personal data (paths, names, parts of documents), so it is readable by the user only and is never sent anywhere by Panpak (a backup tool of the user will copy it, like the rest of the store); it begins with a line of Panpak's, so that its first bytes are not the app's choice (this does not decide the type of the file where `xdg-open` looks at the content: see KL-8), and it is created new, with no link followed; the app cannot read it back (it is outside the view). The standard error is copied at once to where it went; the capture never closes the app. Errors inside the view (Landlock, seccomp, the set-up of bubblewrap) are not reported this way: they end with the status 1 and are told as the app closing right after starting, with the standard error in the log, and counted as a failed start (a limit: the inner part cannot tell the outer one yet why it stopped). The notification that follows "Go back" is plain (no action, sent as "Panpak", so another program of the session can imitate its text but not trigger anything): the name of the app (chosen by its author, sent as the title, which is plain text in the specification, and written in the body with the characters of markup as entities) and the versions, and on a failure the first line of Panpak's own error: the home of the person and the store are taken out by name (also when they hold a space), then every word that holds a `/` (a path, a `file://` address, a relative path) is replaced by `...`, because a path of the store holds the name of the user; without control characters, at most 200 characters of text (counted before the entities); it can hold the id and the version of the signed manifest, never anything that the app prints. A rollback does not go below the floor of the author (§3.1.10), so a version that
  the author withdrew for a serious problem does not come back this way. The outcome of the starts (counters in a file of the store) never leaves the computer. What is not solved: the
  data of the app (its settings) can have been changed by the newer version in a way the older one cannot read; the client says so (the line of `panpak rollback`, the documents) and does not
  try to undo it.
- **A version below the floor of its author (protected mode, spec §3.1.10)**: it is not blocked. It runs with **no network but its own loopback**: in
  the isolated view this is the network namespace of an app with no network permission, so no TCP, UDP (DNS and QUIC too), IPv6 or other protocol
  leaves it (tested with datagrams that a socket of the test must not receive); **without the view** the filter of the system calls makes every IPv4 and
  IPv6 socket fail, the loopback of the app included (tested end to end with the view masked). It does not run programs that it wrote, and it
  cannot open addresses (`xdg-open` and OpenURI, tested through the filtered bus too), which would otherwise carry data out in the browser of the person; it fails, and the person is told in one line or one notification, so that a link that does nothing is not a mystery. **Without the view the app can run the `xdg-open` of the system itself: the addresses are not stopped there** (the network is, by the filter of the system calls: tested with the view masked). What it does **not** take away: the
  sockets the manifest asks for (D-Bus, portals, the display), what the app can already read and write, and the network of a host service that the app
  reaches through such a socket. The floor must have been seen by this machine (an update check, an install, an update that was refused): a machine that never
  saw it runs the version as usual. The person can turn the internet back on for one app; the programs it wrote stay refused. Nobody is told to type a
  command: the notification has a button (only when an update is ready), the windows of the system show the permissions and the risk, and a program of the
  terminal asks where it is, once for each fixed version that is ready (after an answer, even a "no", the next starts have one short line until a new version is ready; the record is `apps/<id>/protected-asked`).
- **Keys and succession** (spec §3.2): the root key signs the packages; the recovery key (24 words of BIP-39 on paper, only its commitment in the package, from the first release)
  takes the app back. A chain of at most 8 successions, each signed by the key that was and bound to the app id, moves the pin; a succession of the root cannot change
  the recovery commitment, and one of the recovery key wins over any of the root, so a thief of the signing key can move the pin (he signs what the key is for) but only until
  the author uses the words. No delay is imposed (a clock is something to attack). The first install has to say whom it trusts (the domain of the author confirms the key, or `--expect-key`, or `--trust-first-key`;
  `--yes` alone is refused unless the domain confirms; a domain that contradicts refuses it, §1.1), a new root is asked of the domain too, and updates are accepted from the author's own source only (`source=author` in `apps/<id>/trust`). Without a valid chain a key change =
  remove and install again.

## 5. Known limitations

One list of what Panpak does not do or does not guarantee today. Each entry says what
the limit is, what it means in practice, and where the detail is. The entries in §3 and
§4 give the reasons; this section is the summary to read before relying on the sandbox.
Status of the project: pre-beta.

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
  appearance portal are missing. Since 13p the app also needs the router (a process of the stub itself, no extra package). Not mediated yet: the clipboard (KL-7), global menus. `doctor` says
  which part is missing. See `docs/COMPATIBILITY.md` (D-Bus summary), `PORTAL-DESIGN.md`, `docs/PORTALS-STUDY.md`.
- **KL-31 An app could talk to a native app that has the same D-Bus name (closed in step 13p).** The proxy rules `--own=NAME` and `--talk=NAME` could not
  tell the app's own name from the same name owned outside the sandbox (found with Ghostty: F22 and F23). Since 13p the names of an app are on a bus of its
  own, in a router process shared by the copies of that app, and none is on the host bus: the tests `tests/privbus.py` (A, B, C, F11, F22, F23) and
  `tests/dbus_filter.py` check that. What remains: the router reads the app's bytes and has no privileges (Landlock with no rule, seccomp allow list,
  `RLIMIT_NPROC` 0, fuzzed parser); a flaw in it gives the attacker the router, not the user's session. The media alias of `permissions.mpris` is the one name
  an app has on the host bus. The design is in `docs/PRIVATE-BUS.md`.
- **KL-6 No keyring.** `org.freedesktop.secrets` is never given to an app, because it would
  expose every password of the user's keyring. In practice: apps that store a password
  there (Bitwarden and Obsidian ask for it in the tests) start without a keyring, and the
  feature that uses it is not available. See `docs/COMPATIBILITY.md` (D-Bus summary).
- **KL-7 The clipboard is not mediated (the file chooser is, since step 11).** The clipboard travels
  over Wayland, which the app can reach: it is not protected. The file chooser of the desktop is given
  through the router and the outer stub (docs/FILE-CHOOSER.md): the app gets the files the user
  chooses as document-portal paths, never a path of the host. Residual: a dialog the user confirms for a
  folder gives the app that folder until the grant is taken back (`panpak remove` does it); an app can
  label a dialog badly, as in Flatpak; the dialog of the desktop stays open when the app closes its
  request. Without the document portal the app keeps its toolkit's own dialog.
- **KL-8 Opening addresses is a channel to the user's programs.** An app can make the user's
  browser or mail client open any `http`, `https` or `mailto` address, up to 5 a second at
  first. In practice: it can show a deceptive page; it cannot open files or attach them.
  A browser the app starts by its full path runs inside the sandbox and is usually
  useless. See §2.3 and `PANPAK-FORMAT.md` §9.3.
  The button "Show details" of the notification of a failed start is a second, smaller channel of
  the same kind: after a click of the person, `xdg-open` opens `last-start.log`, a file whose text
  after Panpak's first line is the app's. On GNOME 51 the type comes from the name of the file
  (`xdg-open` calls `gio open`; `gio info` gives `text/x-log` for text, HTML, PDF and binary
  content; measured on 2026-11-26), so the program does not depend on the content. On a desktop
  where `xdg-open` looks at the content (not measured on KDE or on any other desktop) an app can
  write text that makes its log open in another program of the person, such as a browser or a PDF
  viewer, which then does what it does with that text. The first line of Panpak's does not stop
  that, and protected mode does not cover this click (it limits the address opener, not this
  button). It needs the failed start and the click of the person.

**Processes, files and the system**

- **KL-9 The internal sandbox of Electron and Chromium is not available.** Chromium needs
  user namespaces or a setuid helper for its own sandbox, and a user namespace cannot be
  nested in the view. In practice: Electron apps run in Panpak's sandbox only. Most start
  (Jitsi, Obsidian, Zulip and others, see the compatibility table), and balenaEtcher aborts
  on its SUID helper. The seccomp filter now refuses `unshare` and `clone` with `CLONE_NEWUSER` too, so no program
  in the view can make a user namespace, with or without the view (WebKitGTK's own bubblewrap, `flatpak` and
  `bwrap` run by an app fail the same way). See `docs/COMPATIBILITY.md` (Defects).
  The libraries of the host that start their own programs in bubblewrap fail for the same reason: the filter of
  Panpak refuses the namespace flags that bubblewrap needs (`clone` and `unshare`, `EPERM`), and a process that Landlock has
  closed could not `mount` or `pivot_root` either (kernel documentation, Landlock), so bubblewrap cannot nest in the view.
  What follows was read in the public sources of those libraries (their development branches, WebKit `main` and gnome-desktop
  `master`, on 2026-11-27; not the versions of a distribution) and was not run for this text: the web processes of WebKitGTK do
  not start in the view (WebKit tests bubblewrap only in a container it recognises, otherwise uses it, and when it fails the
  launch fails), and the thumbnail helpers of gnome-desktop do not run in builds with bubblewrap support (the command is wrapped in
  `bwrap`, with no test and no fallback), so no thumbnail is made. Apps that need these are not supported yet, and
  `docs/COMPATIBILITY.md` says so. glycin is a different case, see KL-32.
- **KL-32 Image decoding by the host's glycin runs without glycin's own sandbox in the view.** glycin (the image loading
  library of GNOME) starts each image loader in bubblewrap. In the view the `clone` with seven namespaces that bubblewrap makes is
  refused with `EPERM`. This was seen on 2026-11-27 on the Fedora VM, once, on one Electron app, under `strace`, **with a test
  build of the filter (not `main`)**; with `main` that app stopped earlier, on the SUID helper of Chromium, so glycin was never
  reached. `main` is expected to refuse the same way, because its filter refuses every namespace flag on `clone` (test
  `namespaces_refused_through_clone_and_unshare`). After the refusal the loader of glycin started directly. What glycin does
  then was read in the published source of two versions, not run: in `glycin` 3.0.1 (`src/util.rs:81-100`, `src/sandbox.rs:627-685`
  for the test of bubblewrap, `src/sandbox.rs:487-507` for the launch) and in `glycin-core` 4.0.5 (`src/util.rs:141-180`,
  `src/sandbox.rs:652-731`, `src/sandbox.rs:521-541`), when its test finds that bubblewrap is blocked glycin goes on without its
  own sandbox, prints a warning on its standard error (not seen in the run above), and starts the loader with a clean
  environment and a signal on the death of its parent, with no memory limit (`set_memory_limit` is called only in the bubblewrap
  path) and no seccomp filter (used only in that path). In `glycin` 2.0.1 (`src/util.rs:41-48`) there is no such test: bubblewrap
  is always used outside Flatpak, so there the loader does not start in the view. No other version was read.
  A loader that is taken over by a hostile image has every right of the app: its private home, the folders, sockets, devices and
  buses the app was given, and the network if the app has it. It does not get more than the app, because the view, Landlock and
  Panpak's filter stay around it. Flatpak gives the loader a sub-sandbox of its own (`flatpak-spawn --sandbox`: read in the source
  of glycin, not checked in Flatpak's), so on this point Panpak is weaker than Flatpak for every app that uses the host's glycin,
  not only for browsers. Not measured yet: the processes of a loader in the view; the statements about glycin and Flatpak come from
  the source of glycin.
- **KL-10 Seccomp is a deny list.** (Except `socket` and `socketpair`: only the families local, IPv4, IPv6 and netlink pass, and of netlink only NETLINK_ROUTE; the rest return `EAFNOSUPPORT` or `EPROTONOSUPPORT`. Without the uevent socket an app does not hear of devices that come and go (a game pad plugged in while the app runs, a camera or a microphone in a call): `permissions.devices = true` gives it that socket, and nothing else of netlink.) System calls that are not on the list of `PANPAK-FORMAT.md` §9.8 stay
  available, and a kernel bug in one of them is not stopped. In particular `memfd_create`, `seccomp` and `landlock_*`
  are open on purpose, and `modify_ldt` (Wine) on x86-64. The view hides other processes with its own PID
  namespace; without the view Yama (ptrace scope 1) and the filter (`ptrace`, `process_vm_*`, `pidfd_getfd` refused)
  keep the app from other processes of the user. A runtime or a manifest cannot turn the filter off.
- **KL-29 The Wayland security context needs the compositor.** GNOME's Mutter does not offer
  `wp_security_context_v1` (checked in its source and on a running session); there the app gets the user's own
  Wayland socket, with the powers of any Wayland client of the session. KWin 6.7 and the wlroots family (Sway, Hyprland,
  labwc, river, niri) offer it. The protocol names the application but the compositor chooses what to refuse.
  `panpak doctor` says which case the session is.
- **KL-30 `audio` includes the microphone.** The PipeWire and PulseAudio sockets give an app both playback and
  capture; their permission models do not split the two for a client that connects to the socket (only the camera has a
  portal), and a split would need a second socket and rules in the user's own configuration. The permission text of
  `install` says "play sounds and record from the microphone".
- **KL-11 `/proc`, `/sys` and `/run` are readable without the view.** The app can list
  processes and read the system bus socket directory. With the view `/proc` shows only the
  app, and `/run` holds only the allowed sockets. In the view `/proc/sys` is also a
  read-only mount of its own, for every app: the sysctl files of the network namespace of
  the view cannot be written (a write gives `EROFS`; before this, `ip_forward` of a network
  namespace that the app made for itself was writable by a process that is the administrator
  of that namespace, as seen on a test build), and the app cannot remount it, because
  Landlock and the seccomp filter refuse `mount`. Render nodes of the GPU are shared with
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
  the access as High risk, with the summary "Partial isolation". See §2.2.

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
  at all: an app with no network is refused unless `--network-unfiltered`, an app with ports is refused
  even with it, and an app with `network = true` and no ports starts, because it asks for no filter. See §3.

**Trust and the store**

- **KL-17 The first key is pinned without proof, unless the author's domain confirms it.**
  Trust on first use has no proof of identity in the package. Where the author
  names a domain and publishes `https://<domain>/.well-known/panpak.toml`, the client asks for it before
  the first pin and at every succession, and a domain that contradicts stops the install
  (§1.1, `PANPAK-FORMAT.md` §3.2.1). What stays open: an author with no file, an offline
  machine or one with the check turned off (the key is then the only proof, and it is
  shown); a look-alike domain (the package names its own); a domain whose web server is
  taken over, which confirms the thief's key as it would the author's; and a person who
  does not read the domain. The request tells the server of the domain that an address
  installs one of its apps (said once, before the first request). In practice: install
  from the address of the author, read the domain that is shown on a line of its own (in the `xn--` form if it is not plain ASCII), and compare
  the key with one published elsewhere when it is not confirmed.
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
- **KL-22 Conversion of AppImages has gaps.** Libraries found
  through a directory that only `AppRun` adds to `LD_LIBRARY_PATH`, DwarFS images and
  images that rely on system libraries that are absent are not handled or are reported
  late. In practice: some AppImages do not convert or do not start. See
  `docs/COMPATIBILITY.md` (Defects). A symbolic link that points outside the AppImage is left out of the package and listed (it would point at the machine that
  made the AppImage, and the runtime refuses it).

- **KL-23 No input methods through the bus.** `org.freedesktop.portal.IBus`, `org.ibus` and `org.fcitx` are not given. In
  practice: apps in the view cannot use IBus or Fcitx, so typing Chinese, Japanese or Korean (and other languages
  that need an input method) does not work in them, except through what the toolkit does without the bus. The portal
  of IBus (`IBUS_USE_PORTAL=1`) is a candidate for later; it was not tried because the test
  setup has no input method engine.
- **KL-24 The accessibility bus is filtered, not given.** The app reaches `org.a11y.Bus` only through a second proxy with the rules of Flatpak's
  `flatpak_run_add_a11y_dbus_env` minus `NotifyListenersSync` and `NotifyListenersAsync`: it registers and is read by a screen reader, and it cannot read or
  command other applications (test X: `GetChildren` on the root and `RegisterEvent` are refused). Orca on GNOME lists and reads a sandboxed Text Editor as it reads the Flatpak one (5 rounds each, same tree). Without `xdg-dbus-proxy` or without the accessibility bus the variable `AT_SPI_BUS_ADDRESS` is removed and the app is not visible to
  assistive tools.
- **KL-25 Tray icons are registered with the unique name.** The runtime owns no `StatusNotifierItem-<pid>-<n>` name
  (the pid is the one in the view, always 2, so two apps would compete for it). Chromium (Zulip 5.13.2) and Qt 6
  (qBittorrent 5.3.0), with every such name refused, register the icon with the unique name of their connection, and a
  test host read the item and activated it through the filter, with both apps running at once. Measured on a private bus
  with a fake watcher and host, not on GNOME Shell with the AppIndicator extension (which GNOME needs for any tray icon).
  An app or a host that insists on a well-known item name does not get an icon.
- **KL-26 Actions of the older notification service are not delivered.** `ActionInvoked` and `NotificationClosed` of
  `org.freedesktop.Notifications` are broadcasts in every released GNOME Shell (46.0, 48.0, 49.0 and 50.1:
  `js/ui/notificationDaemon.js` calls `this._dbusImpl.emit_signal('NotificationClosed', ...)` and
  `emit_signal('ActionInvoked', ...)` without a destination, around lines 296 to 306) and in KDE Plasma
  (`libnotificationmanager/server.cpp` lines 70, 83 and 94 emit the adaptor signals, which QtDBus sends without a
  destination). Only the development branch of GNOME Shell (after 50.1) sends them to the caller. A filter that forwarded
  these signals would forward every client's notification events, so they are not forwarded. In practice: notifications
  of apps that use `org.freedesktop.Notifications` directly (libnotify, so Electron and most Qt apps through
  KNotification) are shown, but a click or an action button does not reach the app. Apps that use the portal are not
  affected, and GTK apps use it because the layer sets `GTK_USE_PORTAL=1`. Which apps lose actions is derived from the
  toolkit, not measured with each app. The questions of Panpak itself are not affected: the process that asks runs outside the sandbox, on the user's own bus, and receives these broadcasts (measured on GNOME Shell 51 on 2026-11-25: `notify-send -w` got the button and the click on the body, from a terminal and from a process started by the menu). The button "Update now" of the notice of protected mode counts the same way as the questions: only a signal sent by the owner of the name of the service, for the number that `Notify` returned (a test sends the same signal from another client and nothing happens).
- **KL-27 The single-instance name is the upstream id, on the app's own bus.** The app owns its `app.id` on its private bus
  (step 13p), as GTK and KDE applications do for single instance, so the copies of the app find each other through the router. A Flatpak or a
  distribution package of the same program on the host bus no longer competes for the name (before 13p it could, see KL-31). The portals know the
  app as `panpak.<app.id>`, which is not the upstream id.
- **KL-28 No Wayland activation token for a second start.** When the user starts an app that is already running, the
  second copy hands its request to the first one (for example through the single-instance socket of the toolkit) and
  exits. A compositor lets the first copy raise its window only with an activation token (`xdg_activation_v1`) from the
  second one; probably the token does not reach the first copy (not verified). Observed with qBittorrent 5.3.0 on GNOME
  (Wayland): no second copy opens, but the existing window does not come to the front. A fix needs the runtime to pass
  `XDG_ACTIVATION_TOKEN`/`DESKTOP_STARTUP_ID` from the launcher to the app and the app to use it; not done.
