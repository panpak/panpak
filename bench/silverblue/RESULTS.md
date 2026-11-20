# Step 9b results: Panpak and Flatpak on Fedora Silverblue

Status: **not measured yet.** This page is the form that the results go in; the numbers come from `silverblue_bench.py summarize` over the four series of `PROCEDURE.md`, and nothing is written here
that the tables do not say. The limits come first, because they decide how to read the numbers.

## Limits of Panpak that this comparison shows

- **An app built on a base newer than the machine's glibc is refused.** The Panpak package of GNOME Text Editor 51.0 is built in the image `debian-14` (glibc 2.43) and says so in `[host].glibc`; `panpak install` refuses it on a
  system with an older glibc (Fedora 43 has 2.42). The Flatpak of the same version brings the GNOME 51 runtime and its own glibc, so it installs on the older system. This is a limit of Panpak, which uses the glibc of the system
  on purpose (small packages, the drivers of the machine); a package built on an older base runs on more systems, and that is a choice of whoever builds the package.
- *(more limits are added here as the series find them: the permissions that are not the same list on the two sides, the virtual GPU, the versions that differ, ...)*

## Setup

*(machines, versions: `rpm-ostree status`, `ldd --version`, `gnome-shell --version`, `flatpak --version`, `panpak --version`, CPUs, memory, disk; the date; the versions of the apps and what differs on purpose)*

## Bytes that came and space added

*(the installs table of `summarize`, both orders; the first app and the second)*

## Time to the window

*(the table of `summarize`: first start after the install, after a reboot, following starts; n, median, minimum, maximum, quartiles, MAD; failed trials)*

## What the numbers do not say

*(the section 8 of `PROCEDURE.md`, with what the series showed)*
