# Step 9b results: Panpak and Flatpak on Fedora Silverblue

Status: **not measured yet.** This page is the form that the results go in; the numbers come from `silverblue_bench.py summarize` over the four series of `PROCEDURE.md`, and nothing is written here
that the tables do not say. What a Panpak package is here, the base and the limits come first, because they decide how to read the numbers.

## What the Panpak packages are

**GIMP, Kdenlive and ONLYOFFICE on the Panpak side are conversions made by us** of the official AppImages of their authors (`panpak convert`), signed with one key, the key of the converter. They are **not packages made or signed by the
authors**, and `[origin]` of each says so (the file it was made from, its SHA-256, `signed_by_upstream = false`). Each AppImage was checked against the SHA-256 that its author publishes. A conversion carries what the AppImage carries, and what a
native package could save is not measured here. **qBittorrent is not a conversion**: it is built from the source with `panpak build` (libtorrent 2.1.2 like the Flatpak) and is in the series as the example of the build pipeline, not as a centre of the
comparison; it is the fourth app because it shares the KDE 6.11 runtime of Kdenlive, which is what shows the sharing of a Flatpak runtime.

## What a person receives with Silverblue (the base)

On Fedora Silverblue **many GNOME apps already arrive as Flatpaks of Fedora, with the runtime of Fedora**: Text Editor, Calculator, Calendar, Characters, Connections, Contacts, Decibels, Extensions, Logs, Loupe, Maps, NautilusPreviewer,
Papers, Snapshot, Weather, Disk Usage Analyzer (baobab), Clocks, Font Viewer and Fedora Media Writer, on `org.fedoraproject.Platform` 45. They are on both machines of the comparison (the Panpak machine is a clone) and they stay: they are part of what
a person has. Their space is the base (`base-<system>.txt` and the `base` line of the results), and the series are measured from there. The apps of the series come from Flathub for the Flatpak side, with the runtimes of Flathub (GNOME 51, KDE 6.11,
freedesktop 25.08), which are not the runtime of Fedora: nothing is shared with the apps that Silverblue brings, and the first app of each runtime pays the whole of it.

*(the base: space of the system installation of Flatpak, its apps and runtimes, the list with sizes)*

## Holes to close (principle 3)

A function that does not work with Panpak because the conversion lacks a permission that the Flatpak has is not only written under "Where Panpak loses": it is a hole to close, because security is never bought by removing features. Each entry names the exact operation that fails, the missing permission, and the app. The list is filled from the series (the operations of `PROCEDURE.md` section 6), nothing is written from memory.

| App | Operation that fails with Panpak | Permission of the Flatpak that Panpak lacks | Status |
|---|---|---|---|
| GIMP | The check for updates at every start (`https://www.gimp.org/gimp_versions.json`, loaded through GIO, which asks GVfs) fails with the error "loading of https://www.gimp.org/gimp_versions.json failed: Operation not supported", shown in a "GIMP Message" window at every start | talk to GVfs on the session bus (`org.gtk.vfs.*`; the Flatpak of GIMP has it, with `xdg-run/gvfs`) | open; seen on the control start on 2026-11-21 (Fedora 45, GNOME 51) |
| Kdenlive, ONLYOFFICE | The dark mode of the system is not followed, and the file dialogs of Qt do not go through the portal (the converter warns: "a Qt app without the platform theme plugin `platformthemes/libqxdgdesktopportal.so`") | the platform theme plugin that the KDE runtime of the Flatpak brings, built for its Qt | open; the converter's warning on 2026-11-21, to be confirmed by the operations (the dialog, the theme) |
| *(filled from the series: for example, opening a file by path outside the user folders; a graphics tablet in GIMP; a camera or capture device in Kdenlive; mounting a drive from Kdenlive; gvfs locations such as a network share)* | | `filesystem=host`, `device=all`, UDisks2, gvfs | open |

**Direction for the Qt theme plugin hole (to study, no code yet).** How to give a converted app the platform theme plugin that is right for **its** version of Qt: a plugin per Qt version that Panpak (or the package) brings,
chosen by the Qt that the converter finds in the tree (it already sees `libQt*` and the platform plugins), so that the app follows the dark mode and uses the portal for its dialogs without the app being rebuilt.

**The cache of the icons of a conversion is measured as it is.** The converter warns that the icon theme folder of the AppImage has no `icon-theme.cache` (Qt then looks up each icon file by file at the start; the message says about 0.1 s). The pipeline of
`panpak build` makes the cache with `gtk-update-icon-cache` in the build image; `convert` reads the tree in one pass from the squashfs and has no tool for it, and writing the format in the converter is not a small job. The conversions are measured without it, and the
number is in the notes of the start times.

**Direction for the GVfs hole (to study, no code yet).** Not opening the GVfs of the system to the app: that gives access to every file and to the shares of the person, a known weak point of the Flatpak. The direction
is a GVfs **inside the sandbox of the app, on its own private bus**, with only the parts of it that the app needs (for the check for updates of GIMP, the network part: `http` and `https`), so the app gets the
function and no more than the function. It keeps the function that the Flatpak gives and puts the protection in how it is made to work (principle 3).

## Where Panpak loses (what the Flatpak gives and the conversion does not)

Said before the numbers, as they were found, each with its reason. Nothing was taken away to look more secure: these are functions for which Panpak has no way yet.

- **Every folder, by path (`--filesystem=host`).** All three Flatpaks have it. The conversions give the user folders where the app works, in read and write, and the file dialog of the portal; a file elsewhere by path is not reachable. For Kdenlive, a project that
  points at a clip in another folder is the case that shows it.
- **Graphics tablets and other devices (`--device=all`)** in GIMP and Kdenlive: Panpak has `permissions.devices` with the categories `gamepad` and `camera` (notices of plugging only); `input` and `usb` are names that are reserved and not implemented, so a tablet has no way in.
- **UDisks2 on the system bus** (Kdenlive mounting a drive), **gvfs**, and the **screenshot and file manager services** of the session that GIMP asks: no equivalent.
- **An app built on a base newer than the glibc of the machine is refused.** The Flatpak brings its runtime and its glibc; Panpak uses the glibc of the system and `panpak install` refuses a package that needs a newer one (`PANPAK-E2005`). The machines of the series have glibc 2.44 and it
  does not bite here; a reader with an older Fedora would meet it (a package built on an older base runs on more systems, and that is a choice of whoever builds the package).
- **The dark mode of the system and the file dialogs through the portal, in Qt apps (Kdenlive, ONLYOFFICE).** The AppImages bring their own Qt without the platform theme plugin `libqxdgdesktopportal`, which in the Flatpak comes with
  the KDE runtime (built for its Qt): the app does not follow the dark mode of the system and its file dialogs do not use the portal (the converter says it for each conversion). The cause is in what the AppImage carries, and the loss is
  Panpak's to close (principle 3): the plugin must match the version of the Qt of the app.
- **The update of a conversion depends on how the AppImage was rebuilt.** A rebuilt AppImage shares fewer chunks with the previous one than a Flatpak shares objects (measured on the host: Kdenlive 26.08.2 to 26.08.3 downloads 127 MB of a 255 MB package, 5,901 chunks fetched and 11,757 reused); it is a property of the conversion, not of the format, and the table says it.
- *(more are added here, and each one that is a missing permission also goes in the table of holes above, as the series find them: the permissions that are not the same list on the two sides, the 3D that is off, the versions that differ, ...)*

## To understand later

- **Why the update of Kdenlive downloads 127 MB of a 255 MB package** (26.08.2 to 26.08.3, 5,901 chunks fetched and 11,757 reused, measured on the host). Is it how the AppImage was rebuilt (files that change in place shift the chunk boundaries), the chunking parameters, or the compression? Not explained yet: to be looked at before the format freeze, since the chunking parameters are one of the format gaps.

- **If the series confirm that GIMP and Kdenlive start more slowly with Panpak** (the control starts: GIMP 17.7 s against 12.4 s, Kdenlive 6.2 s against 4.5 s, one measure each), find the cause before the report (principle 6). The first hypothesis to check: GIMP waits for a time limit on GVfs, which is not there under Panpak
  (the check for updates through GVfs fails). Measure what the app does in the extra seconds (a trace of its system calls with times, and the time between the screen of start and the main window), with and without the check for updates.

## Setup

*(machines, versions: `rpm-ostree status`, `ldd --version`, `gnome-shell --version`, `flatpak --version`, `panpak --version`, CPUs, memory, disk; 3D off on both; the date; the versions of the apps and what differs on purpose)*

## What is measured as a window

The time is the time to the first frame of the **main window** of each app, found by the id of the app and by its title, with the same rule for both formats; never the screen of start (GIMP, Kdenlive) and never a dialog of the first
start (the legal notice of qBittorrent, the welcome and the quick settings of Kdenlive, the welcome of GIMP: written as already accepted before the first start, the same on both sides). The table of the main windows
is in `PROCEDURE.md` (section 3), the title of the window taken is in every line of the results, and a person watched each app start on the machine and confirmed that the time matches the complete window.

## Bytes that came and space added

*(the installs table of `summarize`, both orders; the first, second, third and fourth app; what the runtime costs in each system and what is shared)*

## Time to the window

*(the table of `summarize`: first start after the install, after a reboot, following starts; n, median, minimum, maximum, quartiles, MAD; failed trials)*

## Updates

*(Kdenlive 26.08.2 to 26.08.3 and ONLYOFFICE 9.4.0 to 10.0.0: bytes that came, space, time, for both systems; GIMP: no previous version in both formats, written as such)*

## Permissions and operations

*(the permissions of each side as the system says them; the operations of `PROCEDURE.md` section 6 for each app and each system, and the Text Editor test against the one of Fedora: open from the file manager, save, save as, reopen from the recent files, and the second document of the same folder)*

## Wayland and the desktop (a limit of GNOME, not of the formats)

On GNOME the Wayland compositor (Mutter) does not support security contexts. An app of either format gets the normal socket of the session and so has the same powers over the desktop (reading what other windows show, sending input) with Panpak and with Flatpak. `panpak doctor` says it as a note. It is a limit of GNOME that applies equally to the two sides and does not change the comparison of the sandboxes.

## Launcher and prerequisites (the runtime of Panpak)

*(whether the first `panpak install` installed the `panpak-stub` by itself (`runtime_installed`), and its space and time, next to the runtimes of Flatpak. The user should never have to run `install-runtime` by hand (step 10a).)*

## The sandbox of an app with a browser engine (ONLYOFFICE)

*(the table of `sandbox` for the two machines. A first reading on the host, 2026-11-21: in both, the editor and its six helpers are in one sandbox with a filter of system calls, no capability, `--no-sandbox` on every helper (the app turns off the Chromium sandbox itself), and the network and
the IPC namespace shared with the host; the Flatpak does not isolate the processes of the editor better than Panpak, and gives the whole home against three folders.)*

## The SBOM of a real build

*(the result of `verify_sbom_build.py` for each recipe: qBittorrent 5.3.0 and the Text Editor)*

## What the numbers do not say

*(the last section of `PROCEDURE.md`, with what the series showed)*
