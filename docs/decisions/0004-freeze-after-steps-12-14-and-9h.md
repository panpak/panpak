# 0004. The format is frozen after steps 12, 14 and 9h

- Status: accepted
- Date: 2026-11-24
- Pull requests: none (a decision of the owner)

## Context
The format has a generation (`version`) that readers refuse if it is not theirs, and rules for what can change without a new generation: optional keys, new critical sections, new `[host]` keys, new device categories (PANPAK-FORMAT.md §2.1, §3.1.9). Browsers and Electron apps (step 12) and programs of the terminal (step 14) are the two kinds of app that are likely to need new fields, and point 9h makes one app of each kind run the whole cycle.

## Decision
The format is frozen only after steps 12 and 14 are complete and after 9h. Until then a list of what the format does not have yet and may need is kept, each entry with "decide now" (the shape cannot change later without a new generation) or "later, as a critical field or in the area of extensions", and the reason.

## Alternatives that were not chosen
Freezing now: the apps that are most likely to need new permissions have not been tried.

## Consequences
The list of open questions is part of the work until the freeze; entries that are about what a signature covers (for example files that are fetched at install time) are decided first.

## Sources
PANPAK-FORMAT.md §2.1 (versioning), §3.1.9 (critical fields); the decision of the owner of 2026-11-24.
