# Desktop integration (step 7b): design

Status: design only, no code. Branch `desktop`, from main 0deab15.

Goal: an app installed with Panpak behaves in the menu like a native app. The right icon, actions that
show up at once, no reference to a path that can disappear, clear messages. The `.desktop` file stays thin
and stable, so that no one ever has to rewrite the entries of all users by hand.

## 0. What the code does today

Read from the code of 0deab15; nothing here was run for this document.

- `.desktop` generation is already a closed template (`desktop_file` in `panpak/src/main.rs`). The package
  contributes only the name, the window class and the URL schemes, each validated and escaped. The `.desktop`
  file of the original app is never copied. So the keys that must not come from a package (`Exec`, `TryExec`,
  `DBusActivatable`, `X-GNOME-Autostart-*`, a free `MimeType`) are already absent by construction. Section 3
  keeps this and makes it a tested rule.
- The entry is written at `install`; `update` rewrites it only if one exists or the app declares schemes.
- `Exec` points at the launcher in `~/.local/bin/<id>`. The launcher is a shell script that runs
  `<store>/runtimes/<abi>/panpak-stub run <store>/apps/<id>/current`. Both paths are in the store.
- The "Uninstall" action runs `installed_panpak()`, which is `current_exe()` of the `panpak` that ran
  `install`. This is defect 41: a `panpak` in a test folder or a build directory ends up in the entry.
- Files are written with a temporary file, fsync and rename (`write_replacing`); a runtime stub is installed
  the same way (`install_file_atomically`), which is why a running app does not give "Text file busy".
- There is no icon: no manifest field, no extraction, no `Icon=` line (defect 11).
- `remove` already prints what it removed, what it kept and the data it left; no app gives `PANPAK-E2004`.
  `list` already has a header, aligned columns and an empty-store message. Section 5 lists what is still
  missing.
- The scope shortcut (`unit_gives_id` in `panpak-stub/src/dbus_layer.rs`) accepts `.scope` and `.slice`
  with an optional one-word launcher. It does not accept `.service`.

## 1. Icon (defect 11)

### Source

The package. A new optional key in `[desktop]`:

```toml
[desktop]
icons = ["usr/share/icons/hicolor/256x256/apps/audacity.png", "usr/share/icons/hicolor/48x48/apps/audacity.png"]
```

- Paths are relative to the tree of the package, like `entry`; same path rules as `entry` (no `..`, no
  absolute path, no link in the way).
- At most 6 files. The size comes from the PNG header, never from the path or the file name.
- `panpak convert` fills it from the AppImage: `.DirIcon`, the root-level `<name>.png`, and the
  `usr/share/icons/hicolor/*/apps/*.png` files of the matching name. `--icon <file.png>` gives one by hand.
  `panpak pack` takes the key from `panpak.toml`.
- AppStream (`usr/share/metainfo/*.metainfo.xml`) is left out of this step; it can name an icon later
  through the same key. No XML is parsed by Panpak in this step.

### Where it goes

`~/.local/share/icons/hicolor/<W>x<H>/apps/panpak.<id>.png`, icon name `panpak.<id>`, so `Icon=panpak.<id>`.
The prefix keeps it apart from the icon of a Flatpak or distribution app with the same upstream id, and it
matches the `.desktop` file name. `install` and `update` write it with the same temporary file, fsync and
rename as the `.desktop` file; `update` removes the sizes the new version no longer has. `remove` deletes
exactly the `panpak.<id>.png` files in the size folders (never a recursive delete there) and says so.

The icon cache: `~/.local/share/icons/hicolor` has no `index.theme` of its own, so there is no cache to
rebuild; GTK reads the folder with the system `hicolor` theme and watches it. To be checked in the VM
(section 4); `gtk-update-icon-cache` is not run unless the test shows it is needed.

### Safety

The file is untrusted input that is decoded by host libraries outside the sandbox (gdk-pixbuf in GNOME Shell
and in file dialogs, Qt in KDE). Rules, applied by `install` and `update` before anything is written, and by
`pack` and `convert` so that a bad icon never gets into a package:

- Format: PNG only. The 8-byte signature must be exact.
- Chunks are walked with their CRC. Allowed: `IHDR` (first, once), `PLTE`, `tRNS`, `IDAT`, `IEND` (last, once,
  nothing after it), `gAMA`, `sRGB`, `pHYs`. Any other chunk makes the file be refused: this drops text,
  profile (`iCCP`), time and private chunks, which are where parser bugs and tracking data live.
- Size: file at most 256 KiB. Width and height each from 16 to 1024 (so at most 1 Mpx and 4 MiB decoded),
  square, 8-bit depth, colour types 0, 2, 3, 4, 6, not interlaced. The interlaced mode is refused because it
  is a second decoder path that no icon needs.
- The `IDAT` data is inflated with a bounded inflater (`flate2` is already a dependency of the CLI, the stub
  does not use this code) and must give exactly `height * (1 + row bytes)` bytes, no more and no less. A
  decompression bomb stops at that bound. We do not decode pixels; we only prove that a standard decoder
  will find the stream it expects.
- The file is written as read, after the check. Nothing is re-encoded: a re-encoder would be a second
  parser to trust.
- The destination is never a link and never followed (`O_NOFOLLOW`, rename into place). If `panpak.<id>.png`
  exists and is a link or not a regular file, install stops with `PANPAK-E2001`, as for the launcher.
- Fuzz target `fuzz_icon` on the validator (same corpus style as `fuzz_wayland`), plus unit tests for each rule
  above and a test that every valid PNG from a small corpus (several encoders) is accepted.

### SVG: decision

Chosen: refuse at install, and refuse in `pack`; `convert` tells the packager to supply a PNG.

- Reject (chosen). An SVG is an XML document with a script, external-reference and entity surface, and
  librsvg is a large parser that the host would run outside the sandbox. A package is signed by its
  publisher but not reviewed by us; the icon must not widen what an unreviewed package can reach.
- Clean it with a strict checker (discarded). It would be an XML parser written and fuzzed by us, and the
  list of safe elements and attributes would be an allow list that grows with every report from a user
  whose icon "does not show". The result still goes to librsvg.
- Convert to PNG at build time (discarded for this step). It needs a rasteriser in `pack` and `convert`: a
  dependency (`resvg`, or `rsvg-convert` from the host), and a host tool would make packages depend on what
  is installed, which breaks reproducible packages. It would also move the parsing of an untrusted SVG to
  the packager's machine, which is acceptable only for a tool that is itself reviewed. It stays open as a
  later `panpak convert --render-svg` that is off by default.

Cost of the choice: an app that ships only an SVG gets the generic icon unless the packager gives a PNG. Most
AppImages ship a PNG `.DirIcon`; `convert` will say clearly when one does not.

### No icon

`Icon=application-x-executable`, a name every icon theme has. We do not generate a letter icon (that would
be drawing code with no reviewer). `Icon=panpak.<id>` is written only when at least one icon file was
installed. If the file is later deleted by the user, the desktop shows its generic fallback, and the next
`update` writes it again.

### Tests and cost

Tests: the validator rules (accept and refuse); install writes, update replaces and removes old sizes,
remove deletes only ours; a link in the destination is refused; `convert` of the AppImage fixture finds its
icon; an SVG-only fixture gives the generic icon and a warning. Cost at start: zero (the icon is not read at
start). Cost at install: the validation of a file of at most 256 KiB, below 5 ms.

## 2. A stable `panpak` and a stable stub (defect 41)

### Choice

Three fixed paths in the per-user store, written by `install-runtime`:

```
<store>/bin/panpak            the CLI (copy), used by the menu action
<store>/bin/panpak-stub       a symbolic link to runtimes/<abi>/panpak-stub, the best ABI installed
<store>/runtimes/<abi>/       as today, one folder per ABI
```

- `install-runtime` from a release tarball already has both binaries; it installs them with
  `install_file_atomically`. From a bare stub (`install-runtime <stub> --allow-unsigned`) there is no `panpak`
  to copy, so it installs the stub only and `<store>/bin/panpak` stays as it was. `install` and `update` then
  copy the running `panpak` into `<store>/bin/panpak` when that file is missing, and never replace an
  existing one (a dev build run from a test folder must not overwrite the installed CLI). Replacing it is
  `install-runtime`'s job, and the only way the stable `panpak` changes.
- The launcher and the entry use only these paths: `Exec=... <store>/bin/panpak-stub run <store>/apps/<id>/current`
  in the launcher and `<store>/bin/panpak remove --ask <id>` in the action. Nothing refers to the folder
  where the user happened to run `panpak`.
- The link is switched with `symlink_atomic` (rename of a new link over the old one) after the new
  `runtimes/<abi>/panpak-stub` is in place, so it never points at a missing file.

### An update of the runtime while an app is open

- Each file is replaced by a rename of a complete, synced file. A running process keeps its open inode (the
  old file); the next start sees the new one. The "Text file busy" error only comes from writing into a
  mapped executable, which this never does (it is already tested for the stub).
- The `panpak` copy is replaced the same way. The menu action starts a new process each time.
- A start that overlaps the switch reads the link once, so it runs one stub or the other, never a mix.
- The old `runtimes/<abi>` folder is never deleted by `install-runtime`. Launchers written before this
  step point there and keep working; the store GC (step 8) must treat a launcher's runtime path as a
  reference. This is a constraint for step 8, noted there.

### Which ABI does an app run on

Today each launcher is written with the best ABI at the moment of `install` or `update`. With the link it
follows the best ABI installed. The stub already refuses an app whose `min_runtime` is above its ABI
(`check_runtime`), and a newer ABI runs older apps by the ABI promise. If this proves wrong for some app,
the pin is a one-line change in the launcher. Open question 1.

### Alternatives discarded

- Make the stub do `remove --ask`: the stub is the part that runs next to untrusted apps; it should not grow
  a removal path with dialogs.
- Put `panpak` in `~/.local/bin`: the user's `PATH` entry, may be shadowed or removed, and `panpak` is not
  an app launcher.
- Keep `current_exe()` but check that it is inside the store: a `panpak` that lives elsewhere would give an
  entry with no action, and defect 41 would be hidden, not fixed.

### Tests and cost

`install` from a test folder leaves no path outside the store in the launcher or the entry (a test that
greps both for the folder of the test binary). `install-runtime` twice while a fake app holds the old stub
open: no error, the old process keeps running, the next start uses the new stub. Cost at start: none (a
link in the same directory tree; one more `readlink` inside the kernel path walk, below 5 microseconds).

## 3. The thin, versioned `.desktop`

### Content

```
[Desktop Entry]
X-Panpak-Desktop-Version=1
X-Panpak-App-Id=<id>
Type=Application
Name=<from the package>
Exec="<home>/.local/bin/<id>" %U
Icon=panpak.<id>                   (or application-x-executable)
Terminal=false
Categories=Utility;
StartupWMClass=<from the package, if declared>
MimeType=x-scheme-handler/<scheme>;...   (only the declared URL schemes)
Actions=Uninstall;

[Desktop Action Uninstall]
Name=Uninstall
Name[de]=...  (as today)
Exec="<store>/bin/panpak" remove --ask <id>
```

- The two `X-Panpak-*` lines come right after the group header, so the stub reads the version from the first
  128 bytes.
- `Exec` is the launcher in `~/.local/bin` (it handles "open this one file"). The launcher itself gets a
  header `# X-Panpak-Launcher-Version: <n>` with the same rule as the entry, so the two stay thin and
  replaceable.
- The window class stays optional. Without it the desktop may not group windows under the entry; `convert`
  already warns about that.

### Which fields come from the package, which Panpak imposes

| Field | Source | Rule |
|---|---|---|
| `Name` | package | validated (no control characters, length), escaped as a Desktop Entry string |
| `StartupWMClass` | package | `valid_wm_class` |
| `MimeType` | package, only schemes | `check_scheme_name`; always `x-scheme-handler/<scheme>;` and nothing else |
| `Icon` | Panpak | `panpak.<id>` if an icon was installed, else `application-x-executable` |
| `Exec`, action `Exec` | Panpak | fixed paths of section 2, quoted by `desktop_exec_arg` |
| `Type`, `Terminal`, `Categories`, `Actions` | Panpak | fixed |
| everything else | nobody | not written |

Never written, whatever the package says: `TryExec`, `Path`, `DBusActivatable`, `X-GNOME-Autostart-*`,
`X-GNOME-Settings-Panel`, `Hidden`, `NoDisplay`, `OnlyShowIn`/`NotShowIn`, `Keywords`, any extra `Actions`, a
`MimeType` that is not a declared scheme, any key a future spec adds. The generator is a template with
holes, not a filter on a copied file, so a new key cannot get in by a mistake in a deny list.

Tests: a test that parses the output and checks that its set of keys is a subset of the table above; a
hostile package fixture (names with newlines and `Exec=` in them, a scheme with `;`, a `wm_class` with
`=`) gives either a refusal or a file that passes the same subset check; the existing escaping tests stay.

### Lazy update

The format will change (new keys, a different `Exec`). Instead of a migration that rewrites every entry at
`install-runtime`, the stub does it at the next start of the app:

1. The constant `DESKTOP_VERSION` is compiled into the code that generates the entry. That code moves from
   `panpak/src/main.rs` into `panpak-core` (`desktop.rs`), so the CLI and the stub share it.
2. At start, after the app is known and before the view is built, the outer stub reads the first 128 bytes
   of `$XDG_DATA_HOME/applications/panpak.<id>.desktop` and looks at `X-Panpak-Desktop-Version` and
   `X-Panpak-App-Id`.
3. Equal: nothing to do (this is the case almost always). Older: rewrite as in section 4. Newer than the
   code: leave it alone (an old stub must not undo a newer entry). Missing file: leave it (the user may have
   removed it; `install` makes it). The marker missing or the id different: not ours, leave it.
4. A failure to write is logged under `PANPAK_DEBUG` and never stops the start.

The stub has what it needs: the verified manifest (name, class, schemes), the store layout (the stable
paths of section 2) and the icon state (one `stat`, only in the rewrite case). Several starts at once are
safe: each writes its own temporary file and the rename is atomic; the content is the same.

Cost: the normal case is one `openat`, one `pread` of 128 bytes and one `close`, about 10 to 20
microseconds. Measure: a stage `desktop check` in `PANPAK_DEBUG`, and a new column in
`tests/startup_budget.py` with the entry current against no entry. Target: below 0.1 ms; the budget test
fails above 0.3 ms. The rewrite (rare, once per version bump) costs the fsync, a few ms, and is shown in the
same stage.

Alternatives discarded:

- Rewrite all entries at `install-runtime`: it would touch files of apps that are not being started, can
  fail half way, and needs the manifest of each app.
- Keep the version next to the app in the store (`apps/<id>/desktop-version`): one more file to keep in
  step; the entry itself is what the desktop reads, so it is the thing to check.
- Compare file content: needs the whole generation at every start.

## 4. Atomic write and the menu (defect 40)

Writing is already a temporary file in the same directory (hidden name, `O_EXCL|O_NOFOLLOW`), fsync of the
file and rename. To add: fsync of the directory after the rename, so the new entry also survives a crash.

Whether the menu notices is a question about GNOME Shell and cannot be answered from the code. What is known
from the sources, to be confirmed:

- GNOME Shell watches the `applications` folders through GIO (`GAppInfoMonitor`) and rebuilds its app list on
  a change. A rename into the folder is a `moved_to` event, which it handles.
- `update-desktop-database` only rebuilds `mimeinfo.cache` (URL schemes and MIME types). It does not matter
  for actions and icons.
- Hypothesis for defect 40: the Shell keeps its object for an app that is a favourite or that is running, and
  does not reload it when the file changes.

Experiments for the Bluefin VM (to ask for it when the code is ready; each one on a fresh install of a fake
app, with the file changed while the Shell runs):

1. Write by rename, as today. Does a new action or icon show in the overview, in the dash and in the
   right-click menu? Variants: the app is a favourite; the app is running; neither.
2. Same, plus a `utime` on the `applications` folder (a change event that does not depend on the rename).
3. Same, plus `update-desktop-database`.
4. Icon only: new `hicolor` file with and without `gtk-update-icon-cache`.
5. Same checks on the host with GNOME, in a second session of the real user, only if the VM result is
   unclear. Nothing is installed in the real home by us.

How to look: `Gio.DesktopAppInfo` from a script in the session for what the files say, and the Shell's own
view through a screenshot of the dash and the menu in the VM (no unsafe mode, no extension).

Decision rule: if (1) is enough, nothing is added. If only (2) or (3) helps, `install` and `update` do it. If
nothing helps for a favourite or running app, `install` and `update` print one line: "The menu may show the
new entry after the next login." That is honest and costs nothing; hacking the Shell is not an option.

## 5. `list` and `remove` messages (defect 39)

Already in place (from the code): `list` has a header, aligned columns and a line for an empty store; `remove`
prints what was removed, what was kept and the data it left; an unknown id gives `PANPAK-E2004` and an error
exit. To be confirmed by running them in the first test (not done for this document).

To change:

`remove` of an installed app:

```
removed <id> <version>: freed <size>
  launcher <path>
  desktop entry <path>
  icon <path> (one line, or "icons: n files")
  app files <path> (<size>)
kept: settings and data in <path> (<size>); delete them with `panpak remove --purge-data <id>`
the runtime and the shared blobs stay.
```

- `<version>` is the version of `current`; `<size>` is the sum of what was deleted, computed from the same
  facts that `list` prints (package and tree). The shared blobs are not counted and the text says so. Data
  and cache are shown as kept or deleted, never mixed into the freed size unless `--purge-data` deleted them.
- A removal with a problem still ends with `PANPAK-E2003` and a non-zero exit, as now.

`remove` of an app that is not installed (also the second run):

```
panpak: <id> is not installed: nothing to remove (it was removed already, or never installed). `panpak list` shows the installed apps.
```

with code `PANPAK-E2004`, exit 1, nothing on stdout, and no "removed" line. If only leftovers remain
(data of a removed app), the message says "no app, but data is left in <path>" and what removes it.

`list`:

- Empty: `no apps installed. Install one with `panpak install <file.panpak>`.`
- The explanation line of the columns goes to the end, after a blank line, so that the table is not
  mixed with prose; `--json` is unchanged.
- A status line for an app whose files are damaged names the next step.

Alternative discarded: an option to make a missing app exit 0 (`--if-installed`). A script can test
`panpak list --json`; the explicit error is the message the issue asks for.

Tests: the output of each case against a fixed isolated HOME (full text, not a substring); exit code of the
second `remove`; sizes computed equal the bytes on disk before and after.

Cost at start: none.

## 6. The scope the desktop made: all launchers

### Rule

Systemd's convention for desktop launchers (the one the XDG "Systemd and desktop environments" notes
describe):

```
app[-<launcher>]-<app id>[-<random>].scope
app[-<launcher>]-<app id>[@<random>].service
```

Components are separated by `-`; a `-` inside a component is written `\x2d` (systemd escape). `<app id>` for
us is `panpak.<id>`.

What matters is what the portal reads, because a wrong guess brings back the bug of step 7 (the portal
sees the terminal's identity). So the parser has two parts, and the shortcut needs both to agree:

1. The convention parse, by components, with the unescape: gives (launcher, id, random, kind). It accepts
   `.scope` (random after `-`) and `.service` (random after `@`), launcher present or not.
2. The portal's rule as of the version we know (the regular expression in `PORTALS-STUDY.md`). The shortcut
   is taken only when the portal reads the same id. For a `.service` name, or any shape the installed portal
   does not read, we do not take the shortcut and make our scope. First implementation task: read the portal
   source of the versions in Bluefin and on the CI runner to see whether `.service` is read at all. If it
   is not, the `.service` form is parsed, tested and documented, but never takes the shortcut.

The ambiguity of a launcher word is not a problem for us: every id here starts with `panpak.`, which has a
dot, so it can never be taken for a launcher word (letters and digits only).

If the name matches none of this, we fall back to our own scope without an error (as now). The cgroup name
is read from the leaf; a test also covers a leaf below the unit (a sub-cgroup) and decides whether to walk up.

### Tests

Name table, each with the expected answer for `panpak.org.example.app` and for an id with a dash
(`panpak.org.my-app`, escaped `panpak.org.my\x2dapp`):

- GNOME: `app-gnome-panpak.org.example.app-4242.scope` (yes).
- KDE: `app-org.kde.konsole@1a2b.service` (another app, no); an app with our id in the KDE form
  `app-panpak.org.example.app@1a2b.service` (parsed; shortcut only if the portal reads it).
- No launcher: `app-panpak.org.example.app-7.scope` (yes).
- Another app, another id, a prefix of ours (`panpak.org.example.application`), a suffix, a different
  launcher word, missing `app-`, `.slice`, an empty random, a random with a dash, an unescaped dash in the id,
  an id that includes `\x2d` where ours has a plain dash (and the reverse), and non-ASCII (all no).
- The existing tests stay; the table is shared with `tests/dbus_filter.py`, which runs the real systemd for
  the GNOME shape and for a hand-made name of the KDE shape (`systemd-run --user --unit=...`).
- Not tested here: a real KDE session (none is available). The KDE shapes come from the convention, not from
  a run; the document says so.

Cost: string work on one name at start, below 5 microseconds.

## 7. Order of work and questions

Order: (a) the generator moves to `panpak-core`, with the version, the key-set test and the stable paths;
(b) stable `panpak` and stub; (c) the icon validator, with fuzz; (d) install, update and remove of the icon;
(e) lazy update in the stub, with the budget numbers; (f) messages; (g) the scope parser; (h) the VM run for
section 4, which decides the last line of `install`.

Costs at start, together: one 128-byte read of the entry (target below 0.1 ms) and the name parse (below
5 microseconds). Nothing else runs at start.

Open questions:

1. The launcher follows the `bin/panpak-stub` link (best ABI) instead of the ABI it was written with. I
   recommend the link. The other choice keeps the old pin and makes every new ABI reach an app only at its
   next `update`.
2. The stable `panpak` is copied by `install` when missing. I recommend that. The other choice is to require
   `install-runtime` from a release first, which is stricter and makes the dev flow longer.
3. SVG-only apps get the generic icon. I recommend it for this step; `--render-svg` stays a later option.
