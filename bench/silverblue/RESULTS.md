# Step 9b results: Panpak and Flatpak on Fedora Silverblue

Status: **not measured yet.** This page is the form that the results go in; the numbers come from `silverblue_bench.py summarize` over the four series of `PROCEDURE.md`, and nothing is written here
that the tables do not say. The limits and the base come first, because they decide how to read the numbers.

## What a person receives with Silverblue (the base)

On Fedora Silverblue **many GNOME apps already arrive as Flatpaks of Fedora, with the runtime of Fedora**: Text Editor, Calculator, Calendar, Characters, Connections, Contacts, Decibels, Extensions, Logs, Loupe, Maps, NautilusPreviewer,
Papers, Snapshot, Weather, Disk Usage Analyzer (baobab), Clocks, Font Viewer and Fedora Media Writer, on `org.fedoraproject.Platform` 45. They are on both machines of the comparison (the Panpak machine is a clone) and they stay: they are part of what
a person has. Their space is the base (`base-<system>.txt` and the `base` line of the results), and the series are measured from there. The apps of the series come from Flathub for the Flatpak side, with the **GNOME 51 runtime of Flathub**, which is not
the runtime of Fedora: nothing is shared with the apps that Silverblue brings, and the first GNOME app pays the whole of its runtime. A person who only uses what Silverblue brings never needs either system for those apps; the comparison is about the apps
that a person adds.

*(the base: space of the system installation of Flatpak, its apps and runtimes, the list with sizes)*

## Limits of Panpak that this comparison shows

- **An app built on a base newer than the machine's glibc is refused.** The Panpak packages of GNOME Sudoku, GNOME Mahjongg and GNOME Text Editor are built in the image `debian-14` (glibc 2.43) and say so in `[host].glibc`; `panpak install` refuses them on a
  system with an older glibc (Fedora 43 has 2.42). The Flatpak brings the GNOME 51 runtime and its own glibc, so it installs on the older system. This is a limit of Panpak, which uses the glibc of the system on purpose (small packages, the drivers
  of the machine); a package built on an older base runs on more systems, and that is a choice of whoever builds the package. The machines of the series have glibc 2.44, so here it does not bite; it is said because a reader with an older Fedora would meet it.
- *(more limits are added here as the series find them: the permissions that are not the same list on the two sides, the 3D that is off, the versions that differ, ...)*

## Setup

*(machines, versions: `rpm-ostree status`, `ldd --version`, `gnome-shell --version`, `flatpak --version`, `panpak --version`, CPUs, memory, disk; 3D off on both; the date; the versions of the apps and what differs on purpose)*

## Bytes that came and space added

*(the installs table of `summarize`, both orders; the first, second and third app; what the second GNOME app costs in each system)*

## Time to the window

*(the table of `summarize`: first start after the install, after a reboot, following starts; n, median, minimum, maximum, quartiles, MAD; failed trials)*

## Permissions and operations of the Text Editor (no space, no time)

On the Flatpak machine the Text Editor is the one of Fedora that was already there; on the Panpak machine it is ours. The permissions as each system says them, and what a person can do with a document:

*(the two lists of permissions; the table of the operations of `PROCEDURE.md` section 6 for each system: open from the file manager, save, save as, reopen from the recent files, and the second document of the same folder)*

## The SBOM of a real build

*(the result of `verify_sbom_build.py` for each recipe)*

## What the numbers do not say

*(the last section of `PROCEDURE.md`, with what the series showed)*
