# 0007. No app runs that the user did not open

- Status: accepted
- Date: 2026-11-23
- Pull requests: none (a decision of the owner, taken in the conversation of that day); the related work is #92 (the conversion of GIMP), #93 (times and caches) and #105 (no `.pyc` files)

## Context
The first start of GIMP is slow because it queries every plug-in once to build its registry (`pluginrc`). A registry prepared in advance worked: **4.5 s for the first start, one trial made by hand** (not a series). The first start without it, on the Panpak machine (VM on an external Samsung T7 SSD over USB, LUKS, ext4, guest store btrfs): 15.6 to 15.8 s (three first starts, medians of the file `results/gimp-p1p3/panpak.jsonl`, the night of 2026-11-23 to 24, after TRIM; the first start after an install in the same file took 19.6 s); and 15.7 to 16.0 s in series 3 of the comparison with Flatpak (`results/s3-panpak-full/panpak.jsonl`, 2026-11-21, before TRIM). Flatpak's first start: 9.2 and 9.3 s (medians of series 1 and 2, 2026-11-22).

## Decision
No app runs unless the person opened it. The first start is made fast only with what is the same for everybody, in the pipeline that makes the package. The proposal to GIMP (a registry that does not depend on the language, so that it can be made at build time) is a contribution to GIMP, not something that Panpak waits for.

## Alternatives that were not chosen
1. Preparing the registry in the background at install and at updates: the app would run without the person having opened it.
2. Preparing the registry for every language, by whoever publishes: it complicates the work of the publisher.
3. English only: whoever uses another language would gain nothing.
4. Waiting for a change in GIMP: it does not depend on us.

## Consequences
The first start of GIMP stays slower than Flatpak's (15.6 to 16.0 s on Panpak against 9.2 and 9.3 s; the files named above and `risultati-9b.md`). The menu entry stays a closed template that never carries `DBusActivatable` or `X-GNOME-Autostart-*` (THREAT-MODEL.md; PANPAK-FORMAT.md §3.1.5), which is the same rule seen from the other side.

## Sources
The decision of the owner, 2026-11-23 (in conversation). PANPAK-FORMAT.md §3.1.5 (the entry); THREAT-MODEL.md (the menu entry); pull requests #92, #93, #105. The proposal to GIMP is a draft that has not been sent.
