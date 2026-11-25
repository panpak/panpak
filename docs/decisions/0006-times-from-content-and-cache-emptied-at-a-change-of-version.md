# 0006. Times from content, and the private cache emptied at each change of version

- Status: accepted
- Date: 2026-11-23
- Pull requests: #93

## Context
A tree had one time for everything (2000-01-01), which is wrong for programs that keep what they learned by a time. From the sources read: GIMP's `pluginrc`, fontconfig, Python `.pyc` with a time, QML `.qmlc`, GStreamer's registry and xdgmime compare for equality; KDE's ksycoca, the GTK icon cache and the Qt icon cache want a cache that is not older than its folder.

## Decision
A file has the time of its content (2000-01-01 plus up to 2^25 s, from its blob name); a folder the time of its list; the `icon-theme.cache` of a folder the latest time of its folder and the folders below it. The private cache folder of an app is emptied by `update`, `repair` and `rollback` when no copy runs, and by the first copy that starts on a tree other than the one the cache was made on.

## Alternatives that were not chosen
One time for the whole tree (the old rule), and keeping the caches across versions.

## Consequences
After an update the app makes its caches again (a GIMP update queries its plug-ins again: 43 plus 3 in the measure of 2026-11-23). Trees installed before the change keep their times until they are made again.

## Sources
PANPAK-FORMAT.md ("Times in the tree", §6.9); pull request #93.
