<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# The desktop's file dialog for sandboxed apps (step 11): design

Status: design, updated with the decisions of the owner (section 8) and with the table of permissions (section 9); no product code. Base: `main` after the
merge of the step 13p, the CI levels and cleanup-2. Source read: xdg-desktop-portal 1.20.3, 1.21.0 and 1.22.1 (`src/file-chooser.c`, `src/xdp-documents.c`,
`document-portal/document-portal.c`, `document-portal/document-portal-fuse.c`, `data/org.freedesktop.portal.Documents.xml`). Marks:
**[source]** read in the code, **[tested]** run in an earlier step (PORTALS-STUDY.md section 2, PRIVATE-BUS.md section 9), **[to test]** not yet run,
**[estimate]** a number not measured.

## 1. What is wrong today and what the design does

With the 13p the app has a bus of its own and no `Properties` on the portal (PRIVATE-BUS.md section 12), so GTK shows its own dialog, which opens and
saves only in the folders the app was granted. If the portal's dialog were reachable, the app would receive a path of the host that the view does not
contain (KL-7). The cause is in the portal: for a caller it treats as a host app, `FileChooser` returns the chosen `file://` URIs unchanged and registers
nothing in the document portal **[source: `file-chooser.c:131-135`, `if (xdp_app_info_is_host (...)) ruri = g_strdup (uris[i]); else ruri = xdp_register_document (...)`]**.
Panpak apps are host apps for the portal (the identity `panpak.<id>` comes from `Registry.Register`), so that registration, which Flatpak gets for free,
has to be done by Panpak.

Design in one line: the router of the app answers `FileChooser` calls itself, like it already answers `OpenURI`; the host-side work (the dialog, and the
sharing of the chosen files) is done by the actor in the outer stub, which re-checks; the app receives document-portal paths, which the view already
mounts (`doc/by-app/panpak.<id>` on `$XDG_RUNTIME_DIR/doc`, step 5 **[tested]**).

## 2. Flow of messages

```
app                    router (confined)              actor (outer stub, host)             portal / document portal
 | Properties.Get(FileChooser,version)
 |----------------------> answers 4 (or less) itself
 | FileChooser.OpenFile(parent, title, opts)
 |----------------------> checks shape, one dialog per copy,
 |                        method return: handle /request/<unique>/<token>
 |<---------------------- (same handle rules as OpenURI)
 |                        frame KIND_FILE_CHOOSER ------> re-checks, filters options
 |                                                        FileChooser.OpenFile(parent, title, opts') ---> dialog (user chooses)
 |                                                        <------ Response(0, uris=[file:///host/path ...])
 |                                                        for each uri: open on the host (section 4), Documents.AddFull / AddNamedFull
 |                                                        (app id panpak.<id>, permissions read[,write]) ---> doc ids
 |                        <------ frame result: [/run/user/<uid>/doc/<id>/<name> ...]
 | Request.Response(0, uris=[file:///run/user/<uid>/doc/<id>/<name>], choices, current_filter)
 |<---------------------- signal built by the router (sender: the portal's unique name, as for OpenURI)
```

Differences from forwarding the call to the portal through the proxy:

- the Response is unicast to whoever asked **[source: the portal emits it to the sender of the request]**; if the proxy forwarded the call, only the
  router would see it, and the router (which reads untrusted bytes) would then have to tell the actor which files to share. That makes the router the
  one who names the files, which is the wrong place (section 5). Here the actor itself is the requester, so it sees the user's choice first-hand.
- the options the app sends are not passed on as they are: the actor builds them (section 3).

`SaveFiles` (interface version 4: a folder, and names appended to it) follows the same path as `SaveFile`, with a directory document (section 6.3).

## 3. Answers to the questions

### 3.1 Document portal calls, permissions, app id, versions

| Case | Call (as a host caller) | Interface version | Permissions for `panpak.<id>` |
|---|---|---|---|
| existing file, read | `AddFull(fds=[fd], flags, "panpak.<id>", perms)` | 2 | `read` |
| existing file, writable | same, fd opened `O_RDWR` | 2 | `read`, `write` |
| new file, or overwrite (SaveFile) | `AddNamedFull(parent_fd, basename, flags, app, perms)` | 3 | `read`, `write` |
| folder (SelectDirectory, `directory=true`) | `AddFull` with flag 8 (export-directory), fd of the folder | 4 | `read`, `write` unless the app asked read-only |
| older document portal | `Add` / `AddNamed` and then `GrantPermissions` (what `xdp_register_document` does when `version < 2` or `< 3`) | 1 | as above |

Flags: `reuse_existing` (1) so that the same file gets the same document id each time, `persistent` (2) so that the grant survives a restart of the
portal and the session, `as-needed-by-app` (4) is NOT used (the portal's own code passes it only to skip files a Flatpak app can already reach; panpak
apps are checked by Landlock, not by the portal) **[source: `xdp-documents.c:129-134`, `document-portal.c:1021-1030`]**. All three portal trees read
announce interface version 5 (`document-portal.c:1649` in 1.22.1, `:1522` in 1.21.0, `:1514` in 1.20.3). Minimum versions per call: `AddFull` 2,
`AddNamedFull` 3, directories 4 (`Documents.xml`, "This method was added in version N"). The actor reads the `version` property first and falls back as
in the last row.

Differences from the portal's own sharing code, on purpose: the portal also gives `grant-permissions` (and `delete`, when asked). Panpak does NOT:
`grant-permissions` lets the holder share the document with another app, and the app has no way to call the document portal (the proxy refuses it),
but the permission would still sit in the user's permission store under a name the user cannot judge. `delete` only matters for documents the app created
itself, which Panpak does not create (the user's file stays the user's).

App id: `panpak.<id>`, the same as the identity registered by the router, so that the dialog, the document store and the `by-app` folder agree. The id
is valid for the document portal (`xdp_is_valid_app_id`) because it is a valid D-Bus name **[source: `document-portal.c:815`]**.

The caller of the document portal is the actor, a plain host process: its app id is empty, so no permission is recorded for it, and for host callers
the permission checks of the portal pass **[source: `document-store.c:77`, `if (xdp_app_info_is_host (app_info)) return DOCUMENT_PERMISSION_FLAGS_ALL`]**.
`Add*`, `GrantPermissions` and `List` are allowed only to host callers, which is why the app cannot do this for itself **[source: `document-portal.c:1109`, `1374`, `1500`]**.

### 3.2 Trust

What the app controls: that a dialog is shown, its title and accept label, filters, and "writable" (it can only lower it). What it does not control: which
file is chosen. The user does, in a window of the desktop. The chain:

1. The router checks the call: interface and member are `FileChooser.OpenFile`, `SaveFile` or `SaveFiles`; argument types are exact; at most one dialog
   per copy at a time (a second call is answered with `Response` 2 at once); sizes are bounded.
2. The router sends the actor a frame (`KIND_FILE_CHOOSER`, reserved in `frames.rs` today and answered "not done by this version"). The actor does not trust
   the frame: it parses it again, keeps only the option keys it knows (`accept_label`, `modal`, `multiple`, `directory`, `filters`, `current_filter`,
   `choices`, `current_name`), and drops the rest. `handle_token` is the actor's own. `current_folder` and `current_file` are kept only when they resolve inside
   a place the view shows (a granted folder, or a document path), so the dialog cannot be pointed at a folder for a reason other than the app already seeing it.
3. The actor is the requester of the portal, so the answer comes to it directly and no other process can inject a list of files. The actor checks that
   the answer is from the portal's current owner (the unique name from `GetNameOwner`, as the router does).
4. Each chosen path is opened by the actor (section 4) and shared; the actor builds the list of document paths itself.
5. The router only turns the actor's result into a signal. A compromised router can ask for a dialog (rate limited, the user sees it), refuse to deliver,
   or lie to its own app; it cannot name a file for the actor to share, because the actor takes the files from its own request.

The user sees a dialog shown by the desktop, not by the app, and its title is the one the app gave. That is the same limit as in Flatpak: an app can
label a dialog badly. The dialog is modal to the app window when the app gives its window (below).

Parent window: the app gives `x11:<xid>` or `wayland:<handle>` (from `xdg_exported`). The actor passes it through after checking the form (a short string with
one of the two prefixes); the desktop ignores a handle it does not know **[source: window-identifiers in the portal docs]**. No window handle means a dialog
without parent, which is allowed.

### 3.3 SaveFile

- New file: the dialog returns `file:///host/dir/new.txt`, which does not exist. The actor opens the folder (`O_PATH | O_DIRECTORY`) and calls
  `AddNamedFull(folder_fd, "new.txt", ...)`. The app sees `.../doc/<id>/new.txt`; the real file is created on the first write by the document portal's
  FUSE (`xdp_fuse_create`, `document-portal-fuse.c:2010`, needs `write`) **[source]**. If the app never writes, no file appears on the disk.
- Existing file chosen (overwrite): same call; `reuse_existing` returns the id of the document if there is one. The user confirmed the overwrite in the
  dialog (the backend asks). The app sees the current content and replaces it.
- Atomic save (write a temporary file and rename it over the document, as GTK's text editor and Qt do): the FUSE accepts a rename between the document name
  and a temporary name in the document's folder **[source: `xdp_fuse_rename`, `document-portal-fuse.c:2696`, "Source is (maybe) main file, destination is
  tempfile"]**. **[to test]** with Text Editor on both desktops: save, save as, save over an existing file, and the file on the disk after each.
- What stays on the disk: only the file the user named, in the folder the user picked. The folder is not shared (only the name).
- `SaveFiles` (a folder and several names): a directory document for the folder, names appended by the app inside it. The app can then create files in that
  folder, as the dialog said ("save these in this folder"); see 3.4 for what it can do with the rest of the folder.

### 3.4 Folders

`SelectDirectory` in GTK and `directory=true` in `OpenFile` return a folder. The actor shares it with `AddFull` and the export-directory flag (interface
version 4) and the permissions the app asked for (`read` by default for "open", `read` and `write` when the dialog was a "choose where to save"
or the app asked for it). The app gets `.../doc/<id>/<folder name>` and can read, create, rename and delete inside that folder, but not above it; the
FUSE checks `CHECK_CAN_WRITE | CHECK_IS_PHYSICAL_IF_DIR` for each operation **[source: `xdp_fuse_create`, `xdp_fuse_rename`]**.

Persistence: with the `persistent` flag the grant is written to the permission store (`org.freedesktop.impl.portal.PermissionStore`, table `documents`) and
`reuse_existing` returns the same id for the same folder, so the path an app saves in its configuration (qBittorrent keeps the download folder) is valid
at the next start. The view mounts `doc/by-app/panpak.<id>` at each start, and the Landlock rule on that folder covers documents added later
**[tested: PORTALS-STUDY.md section 2(b)]**. **[to test]** qBittorrent with a download folder on a FUSE path: many files, rename of partial files, and the
speed of a large transfer.

Revocation, in order of what a user does:

1. `panpak remove <app>` revokes every document of the app (section 3.5).
2. The user removes the grant in the desktop's settings where it lists "files" permissions of an app; those panels are written for Flatpak ids and may not
   list `panpak.<id>` **[to test]**; the Panpak app (step 10b) will list and revoke them; `panpak grants` is postponed.
3. The user moves or deletes the folder: the document stays in the store and the app gets errors; a later pick of another folder gives a new id.

### 3.5 Life cycle

- `panpak remove <app>`: the outer side calls `Documents.List("panpak.<id>")` (host only **[source: `document-portal.c:1500`]**) and, for each id,
  `RevokePermissions(id, "panpak.<id>", [read, write, grant-permissions, delete])`. This is the first step of the removal, before the files of the app are
  deleted, so a failure stops with a clear message and the removal can be run again. The user's own files are never touched.
- With `--purge-data`: after the revoke, a document that no other app holds is deleted from the store (`Documents.Delete(id)`; host callers have the
  `delete` right **[source: `document-store.c:77`]**). Whether another app holds it is read from the permission store (`Lookup`), so that a file the user also
  shared with another app keeps working for that app.
- Without the document portal (no FUSE, container, or no portal): the dialog is not offered, the router answers the properties as "no FileChooser", GTK and Qt
  use their own dialog, and the recipe's `xdg_dirs` is what the app can reach. `doctor` says so.
- Update: nothing changes. The id of the app is the same, the store is keyed by it, and a new version keeps its grants. A recipe that changes its `id` is a
  different app and has none.
- Two copies of an app: both have the same grants (same id). A document created by one copy is visible to the other.

### 3.6 Qt

Qt's `xdgdesktopportal` platform theme plugin (collected by `pack` since step 13a) calls `OpenFile` and `SaveFile` on the same object path and reads
`uris` from the `Response`, converting each to a local path **[from Qt's source; to test in the code phase]**. If it also reads `Properties` of the portal
(some versions check the interface version), the router answers it as it does for GTK. The expected result is identical: a path under
`$XDG_RUNTIME_DIR/doc/<id>/`. **[to test]**: qBittorrent opens a `.torrent` and picks the download folder on both desktops.

### 3.7 Properties on the portal

Since the 13p the router already answers `Properties.Get` and `GetAll` of `org.freedesktop.portal.OpenURI` itself (version 3; GTK does nothing on a link
without it, found on Bluefin), also for a call that goes to the unique name of the portal's owner, and every other `Properties` call goes to the host and
is refused. Step 11 adds `org.freedesktop.portal.FileChooser` to what the router answers (the version found by the actor, at most 4). GTK asks `org.freedesktop.DBus.Properties.Get(org.freedesktop.portal.FileChooser, version)` before it uses the portal (PRIVATE-BUS.md section 9). The
design asks the router to answer `Get` and `GetAll` for the `FileChooser` interface (the version the actor found, at most 4) and to refuse the other
interfaces, instead of reopening the rule `Properties.Get/GetAll` on the portal's object. Reason: the rule would pass the properties of every portal
interface to the app, and the answer for `FileChooser` has to come from the host side that actually serves the dialog (no document portal, no dialog).
If the code phase finds that a toolkit also needs the versions of other interfaces, the router answers those too, from a list, or the rule is reopened for
those interfaces. This differs from the first sentence of the brief ("reopen Properties.Get and GetAll"); the result for the user is the same.

### 3.8 Which recipes can drop `xdg_dirs`

| App | Without `xdg_dirs` | Comment |
|---|---|---|
| Text Editor | yes, for documents, desktop and downloads | files are chosen, saved or restored from `recent` through documents; **[to test]** the restore of the open files after a restart; opening a file from the command line or from the file manager is covered by 3.9 |
| qBittorrent | the `.torrent` yes; the download folder only after the user picks it once | the default `~/Downloads` is gone from the view, so the first start asks; or keep `xdg_dirs = download` |
| Ghostty | not applicable | a terminal needs the shell's view of the home, not the dialog |

Rule for `pack` (a note, not a failure): when a recipe asks for a user folder (`documents`, `desktop`, `pictures`, `videos`, `music`, `download`) with write
access, and the toolkit is GTK or Qt (read from the files, as `pack` does for the Qt plugins), print: `note: the file dialog of the desktop can give this
app the files the user chooses; consider removing xdg_dirs = [...] unless the app reads the folder on its own (indexer, project folder, file manager)`.
Install text for the user stays simple: "can open the files you choose" when only the dialog is used; "can read and write the folder X" when a folder is granted.

### 3.9 Files given at the start (Open with, double click, command line)

Approved scope of step 11. The same actor, the same sharing, before the app is started.

Which arguments are files. The outer stub looks at each argument after the app id:

- `file:///...` (percent-encoded, no host part other than empty or `localhost`): a file or folder; the URI form is kept in the replacement.
- An argument that does not start with `-` and names an existing file or folder (absolute, or relative to the directory the stub was started in; `~` is
  expanded by the shell, not here): a path; the path form is kept.
- Everything else is left alone: options (`--new-window`), `http:`, `https:`, `mailto:` and other addresses (they are not files; the opener channel
  handles addresses), words that match nothing. An option with a path inside (`--config=/home/x/f`) is left alone too: the stub cannot know it is a path.
  The desktop entry written by `install` passes the file field codes (`%U`) so that a double click or "Open with" arrives as plain arguments; **[to check]**
  the entry that `install` writes today.

What happens, in one batch before the app starts (one call, so the cost does not grow with the number of files):

| Argument | Sharing | The app receives |
|---|---|---|
| existing file | `AddFull` with the fd opened as in section 4; `read`, and `write` when the user can write the file | `/run/user/<uid>/doc/<id>/<name>` (or `file://` of it) |
| several files | one `AddFull` with all fds (at most 64; more are refused with a message) | one path each, same order |
| folder | `AddFull` with the export-directory flag, `read` and `write` when writable | `/run/user/<uid>/doc/<id>/<folder name>` |
| file that does not exist, folder exists | `AddNamedFull` (as SaveFile): the app creates the file on its first write, as `gnome-text-editor new.txt` does | document path of the new name |
| file that does not exist, folder does not | not shared; the argument is dropped with a message | nothing |
| symbolic link as the final component | refused as in 4.3; message: `the file you chose is a link: choose the original file` | nothing |
| file the user cannot read | not shared; message with the system error | nothing |

Failure policy: a failed argument is dropped and reported on standard error and, when a desktop session is running, as a notification (through the
opener's path, not the app's); the app still starts with the other arguments. An app that is started with no valid file is the same as a start with no
file. Nothing is shared for an argument the stub rejected.

Same rules as the dialog: `reuse_existing` and `persistent`, no `grant-permissions` or `delete`, the app id `panpak.<id>`, the checks of section 4. Before the app
runs there is no router and no app to trust: the arguments come from the user who started the program. The risk is only that a wrong argument shares the wrong
file; that is the user's command, as for any program. A program that starts Panpak apps with arguments it got from a web page or a message is the launcher's
problem, not this step's.

Second launch while the first copy runs (single instance across copies, 13p): the second stub shares its files in the same way and starts a second copy
that hands the paths to the first one over the app's bus; both copies have the same id, so the first copy can read the documents.

Cost: none without file arguments. With arguments, one `AddFull` round trip before the exec **[estimate: 1 to 5 ms for the batch]**.

## 4. Opening the file on the host

The actor receives a path from the portal and does not follow what it did not mean to follow:

1. Split into folder and name. Open the folder `O_PATH | O_DIRECTORY | O_CLOEXEC`.
2. Open the name relative to it with `O_NOFOLLOW`, `O_RDONLY` (or `O_RDWR` when `write` is wanted), `O_CLOEXEC`; `fstat` must say "regular file" (or a
   folder for directories). The document portal accepts a regular descriptor and checks that it points to the file it names (`validate_fd`:
   `fstatat(AT_SYMLINK_NOFOLLOW)` in the opened folder, same device and inode) **[source: `document-portal.c:487-545`]**; it also computes `writable` from the
   descriptor, so a file the user cannot write is not shared as writable.
3. If the final component is a symbolic link, the actor refuses (`ELOOP`) and the app gets `Response` 2 with no files. A link the user placed is rare in a
   dialog choice and a link the app placed (in a folder it may write) is the attack: the user sees `notes.txt` and the app has made it point to a file of
   the user's. A symbolic link in a parent folder is accepted only if the link is in a folder the app cannot write (the actor knows the app's write grants).
4. The descriptor goes to the document portal in the same call; the portal stores the folder's file handle and the name, and checks them again at each
   later access, so a swap after the choice is caught by the portal as well.

## 5. Threat model of this part

| Threat | Result |
|---|---|
| The app asks the portal for a path of its choice | not possible: the app only asks for a dialog; the actor shares what the dialog returned |
| A compromised router names a file to share | the router has no such message: the actor takes the paths from its own request to the portal |
| A compromised router asks for many dialogs | one dialog per copy at a time, 5 per minute per copy, each visible to the user |
| The app labels the dialog to mislead ("Save game", really a folder) | possible, same as Flatpak; limited to what the user chooses |
| A symbolic link made by the app in its own folder | final link refused (4.3); a link in a folder the app can write is refused |
| A swap of the file between the choice and the sharing | the descriptor is the file; the portal checks folder handle and name later |
| The app gets more than the user chose | `read`/`write` on the chosen file or folder only; no `grant-permissions`; a folder gives its subtree, as the dialog says |
| Grants outlive the app | `remove` revokes them; `--purge-data` deletes them (3.5) |
| Another local process calls the document portal | out of scope, same as any Flatpak desktop: the portal trusts the user's session |
| The portal's answer is not the portal's | the actor takes the owner's unique name from `GetNameOwner` and checks the sender of the `Response` |

Residual: a dialog the user confirms for a folder gives the app that folder for as long as the grant stays. The note in the install text and
the Panpak app (step 10b) are what the user has to see it.

## 6. Tests planned

In the lab (private bus, fake portal that answers the dialog with chosen paths, real document portal not needed): the shape checks of the router, the
filtering of options in the actor, one dialog per copy, the symlink refusal, the abandoned dialog (`Response` 1), the app that dies during the dialog, a
second copy at the same time. Fuzz: the frame of the file chooser (`fuzz_frames`), the arguments of the three methods in `fuzz_router`.

On the VMs (Bluefin and Fedora KDE, the user powers them on one at a time):

1. Text Editor: open a file with the dialog; save a new file; save as; save over an existing file (the file on the disk after each); close and start
   again and see the recent file work.
2. qBittorrent: open a `.torrent`; pick the download folder; start a download; restart the app and see the folder still there.
3. Files given at the start: `panpak run <app> file`, double click and "Open with" from the file manager (GNOME, KDE) for one file, several files, a
   folder, a file that does not exist yet (Text Editor), a link and a file in a folder the user cannot read; the second launch while the first copy runs.
4. `panpak remove` with the grants present: the list is empty afterwards and the user's files are intact; with `--purge-data`: the store has no entry for
   `panpak.<id>`.
5. Both dialogs are the desktop's own (GNOME and KDE), with the app window as parent.

## 7. Cost

Added to opening a file, besides the time the user spends in the dialog: the router's handling of three messages (about 50 microseconds each, measured in
the 13p), the frame to the actor and back (below 0.1 ms), the open of the file and the `AddFull` call, which is one round trip to the document portal plus the
invalidation of its FUSE cache **[estimate: 1 to 5 ms]**, and the first access through the FUSE instead of the disk **[estimate: tens of microseconds per
call, more for many small files]**. Measured in the code phase on both VMs: `AddFull` latency, open/read of a document against the same file on the disk,
and a large file transfer in a document folder.

## 8. Decisions of the owner (taken)

1. The actor, not the router, calls the portal (2 and 3.2). It costs one more host-side module; it keeps the router out of the decision about files.
2. `FileChooser` properties are answered by the router (3.7), not passed from the portal.
3. `grant-permissions` and `delete` are not given (3.1).
4. A final symbolic link is refused (4.3).

Answers (owner): 1 and 2 yes; 3 yes, with the message `the file you chose is a link: choose the original file` (user-facing text in the language of the
desktop is the job of step 10b; the English text is the source); 4 `panpak grants` is postponed (the grants will be shown in the Panpak app, step 10b);
5 file arguments at the start are part of step 11 (3.9). qBittorrent keeps `xdg_dirs = download`; Text Editor has no `xdg_dirs`; `pack` prints a note, not an error.
6 (added later) the table of permissions of section 9 is part of step 11, with `dbus_own` taken out of the install text.

## 9. One table of permissions (part of step 11)

### 9.1 Why

The words that tell the user what an app may do are written in three places today: `describe_permissions` (the text of `install` and `update`), `added_permissions` (what an
update adds) and the documents (PANPAK-FORMAT.md 3.1, the README). They drift, they are English only, they do not say how serious a permission is, and they are technical
("the PipeWire and PulseAudio sockets of your session"). One of the lines is wrong: `dbus_own` ("the app can own X on your session bus") has had no effect since the private
bus. Step 11 adds the permission that matters most to a user ("the files you choose"), so the wording is made once, here.

### 9.2 The table

A new module of the core, `panpak_core::permissions`, holds a table. Each row has a stable `id`, a level, an icon, a sentence in plain words (the message id for gettext),
a longer sentence for "what this means", and the function that says which rows a manifest turns on and with which values (ports, folder, access, path, scheme).
The result for a manifest is a list of items `{ id, level, icon, text, detail }` in the language of the user; nothing else formats a permission.

Levels (names for the user: Normal, Attention, High risk; in code `Normal`, `Attention`, `High`): **Normal** is what a well-behaved app does and costs the user nothing
in privacy; **Attention** gives the app something of the user's (data, a device, the network); **High risk** gives it a power over other programs or over the whole
session. The level can depend on the values (a folder read-only or read-write, a home path that is a secret).

Proposed rows (the owner decides the levels; the sentences are the English source of the translation):

| id | Level | Icon | Sentence |
|---|---|---|---|
| `files-chosen` | Normal | file | Open and save the files you choose in the file window of your desktop |
| `launch-files` | Normal | file | Open the files you start it with (Open with, double click, command line) |
| `notifications` | Normal | bell | Show notifications |
| `tray` | Normal | pin | Put an icon in the notification area |
| `media-keys` | Normal | music | Be controlled by the media keys of your desktop |
| `url-scheme` | Normal | link | Open the links that start with `{scheme}:` |
| `network-connect` | Attention | globe | Use the network: connect to the ports {ports} |
| `network-listen` | Attention | globe | Accept connections from the network on the ports {ports} |
| `audio` | Attention | microphone | Play sound and use the microphone |
| `folder-read` | Attention | folder | Read the files in your {folder} folder |
| `folder-write` | Attention | folder | Read, change and delete the files in your {folder} folder |
| `home-path-read` | Attention | home | Read {path} in your home folder |
| `home-path-write` | Attention, High risk for a secret place (`.ssh`, `.gnupg`, shell start files, `.local/bin`, autostart, user services) | home | Read, change and delete {path} in your home folder |
| `socket` | Attention | plug | Talk to the local service {socket} |
| `desktop-bus` | High risk | warning | Use the message bus of your whole desktop: it can talk to every service of your session |
| `x11` | High risk | window | See what the other apps show and what you type in them (it uses X11) |

`files-chosen` is shown for an app that gets the portal's file window (always, once step 11 is in); `launch-files` for an app whose menu entry takes files (a `%U`/`%F`
entry or a MIME type). Nothing is shown for what every app has (its own folders, the private bus, the sandbox itself) or for what has no effect (`dbus_own`). The
manifest keeps accepting `dbus_own` (old packages) and the format document says that it has no effect; it is no longer a permission that `update` asks about.

Icons: each row has the name of a symbolic icon of the freedesktop theme (for the Panpak app of step 10b) and a glyph for the terminal and the README (a Unicode
symbol; plain ASCII when the locale is not UTF-8 or `TERM` is `dumb`). The level has its own mark: `[ ]` Normal, `[!]` Attention, `[!!]` High risk in ASCII, and a colored dot
in a terminal that has colors (`NO_COLOR` is honored).

### 9.3 Where it is used

- `panpak install` and `update`: the list, High risk first, one line each (`mark icon sentence`), then the questions as now. For an update only what is new, or what went to a
  higher level, is listed ("This update asks for more"). The lines about `dbus_own` are gone.
- `panpak list`: a column PERMISSIONS with the highest level and the number of items (`Attention, 4`); `--json` gets `permissions: [{ "id", "level", "params" }]` (ids and values,
  not sentences: they do not change with the language).
- `panpak inspect <package or app id>` (new): verifies the package (as `doctor` does), says the name, version, key fingerprint and origin, lists the permissions with their `detail`
  sentence, and shows the layers of isolation; `--json` as above.
- README and QUICKSTART: a section "What an app can ask for" is the table rendered as Markdown by `panpak permissions --markdown`; a test fails when the README differs from the
  table, so the README cannot drift.

### 9.4 Translation with gettext

The message ids are the English sentences. Code marks them with `tr!("...")` and `trn!("...", "...", n)` (plural), which `xgettext --keyword=tr --keyword=trn:1,2` understands, so
translators use the usual tools (`msgmerge`, Poedit, Weblate). The files are `po/panpak.pot` (the template) and `po/it.po` (Italian); English needs no catalog.

No C library and no `.mo` files at run time: the runtime is a static musl program. A `build.rs` of the core reads the `.po` files and generates a table in the binary
(sorted ids, binary search; plural forms `nplurals=2; plural=(n != 1)` supported, any other plural rule falls back to English). The language is the first of `LANGUAGE`
(a list), `LC_ALL`, `LC_MESSAGES`, `LANG`; `it_IT.UTF-8` and `it` select Italian, anything unknown selects English. Placeholders are named (`{ports}`), and a test checks that a translation has
the same placeholders as its source.

Tests: every row of the table has a sentence in `po/panpak.pot` and in `po/it.po` (a missing Italian sentence fails the test); the template matches the sentences found in the source
(no external tool needed); the README section matches the table; `install` prints the same items in English and in Italian (`LANG=it_IT.UTF-8`); the level of each row follows the values.

Scope: the table, the words around it in `install`, `update`, `list` and `inspect`, and the names of the levels. The rest of the command line stays English until someone translates
it; the machinery is general, and a message is translated by marking it.

One point for the owner and for AGENTS.md: `po/it.po` holds Italian text, which the rule "nothing in Italian in the repository" does not allow. It is the one place meant to have it; the
check for Italian words must skip `po/*.po`.

### 9.5 Cost

No cost at run time worth saying: the table is a few kilobytes in the binary (about 3 KB for Italian), and `install` already prints these lines. The build does the parse of
two small files.

## 10. Order of the work

Two pull requests, in this order, so that each is small to review:

1. **Step 11a, the table of permissions** (section 9) with `install`, `update`, `list`, `inspect`, the README section and the Italian catalog. It changes no sandbox
   behavior. It comes first because the file window needs a sentence to show.
2. **Step 11b, the file window** (sections 1 to 8): the router answers the properties of `FileChooser`; the actor calls the portal and the document portal; the files given at the start;
   the revocation by `remove`; the tests of section 6; the VM tests (Bluefin and KDE), which the owner starts one at a time.
