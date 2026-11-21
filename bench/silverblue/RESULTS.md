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
| *(filled from the series: for example, opening a file by path outside the user folders; a graphics tablet in GIMP; a camera or capture device in Kdenlive; mounting a drive from Kdenlive; gvfs locations such as a network share)* | | `filesystem=host`, `device=all`, UDisks2, gvfs | open |

## Where Panpak loses (what the Flatpak gives and the conversion does not)

Said before the numbers, as they were found, each with its reason. Nothing was taken away to look more secure: these are functions for which Panpak has no way yet.

- **Every folder, by path (`--filesystem=host`).** All three Flatpaks have it. The conversions give the user folders where the app works, in read and write, and the file dialog of the portal; a file elsewhere by path is not reachable. For Kdenlive, a project that
  points at a clip in another folder is the case that shows it.
- **Graphics tablets and other devices (`--device=all`)** in GIMP and Kdenlive: Panpak has `permissions.devices` with the categories `gamepad` and `camera` (notices of plugging only); `input` and `usb` are names that are reserved and not implemented, so a tablet has no way in.
- **UDisks2 on the system bus** (Kdenlive mounting a drive), **gvfs**, and the **screenshot and file manager services** of the session that GIMP asks: no equivalent.
- **An app built on a base newer than the glibc of the machine is refused.** The Flatpak brings its runtime and its glibc; Panpak uses the glibc of the system and `panpak install` refuses a package that needs a newer one (`PANPAK-E2005`). The machines of the series have glibc 2.44 and it
  does not bite here; a reader with an older Fedora would meet it (a package built on an older base runs on more systems, and that is a choice of whoever builds the package).
- **The update of a conversion depends on how the AppImage was rebuilt.** A rebuilt AppImage shares fewer chunks with the previous one than a Flatpak shares objects (measured on the host: Kdenlive 26.08.2 to 26.08.3 downloads 127 MB of a 255 MB package, 5,901 chunks fetched and 11,757 reused); it is a property of the conversion, not of the format, and the table says it.
- *(more are added here, and each one that is a missing permission also goes in the table of holes above, as the series find them: the permissions that are not the same list on the two sides, the 3D that is off, the versions that differ, ...)*

## To understand later

- **Why the update of Kdenlive downloads 127 MB of a 255 MB package** (26.08.2 to 26.08.3, 5,901 chunks fetched and 11,757 reused, measured on the host). Is it how the AppImage was rebuilt (files that change in place shift the chunk boundaries), the chunking parameters, or the compression? Not explained yet: to be looked at before the format freeze, since the chunking parameters are one of the format gaps.

## Setup

*(machines, versions: `rpm-ostree status`, `ldd --version`, `gnome-shell --version`, `flatpak --version`, `panpak --version`, CPUs, memory, disk; 3D off on both; the date; the versions of the apps and what differs on purpose)*

## Bytes that came and space added

*(the installs table of `summarize`, both orders; the first, second, third and fourth app; what the runtime costs in each system and what is shared)*

## Time to the window

*(the table of `summarize`: first start after the install, after a reboot, following starts; n, median, minimum, maximum, quartiles, MAD; failed trials)*

## Updates

*(Kdenlive 26.08.2 to 26.08.3 and ONLYOFFICE 9.4.0 to 10.0.0: bytes that came, space, time, for both systems; GIMP: no previous version in both formats, written as such)*

## Permissions and operations

*(the permissions of each side as the system says them; the operations of `PROCEDURE.md` section 6 for each app and each system, and the Text Editor test against the one of Fedora: open from the file manager, save, save as, reopen from the recent files, and the second document of the same folder)*

## The sandbox of an app with a browser engine (ONLYOFFICE)

*(the table of `sandbox` for the two machines. A first reading on the host, 2026-11-21: in both, the editor and its six helpers are in one sandbox with a filter of system calls, no capability, `--no-sandbox` on every helper (the app turns off the Chromium sandbox itself), and the network and
the IPC namespace shared with the host; the Flatpak does not isolate the processes of the editor better than Panpak, and gives the whole home against three folders.)*

## The SBOM of a real build

*(the result of `verify_sbom_build.py` for each recipe: qBittorrent 5.3.0 and the Text Editor)*

## What the numbers do not say

*(the last section of `PROCEDURE.md`, with what the series showed)*
