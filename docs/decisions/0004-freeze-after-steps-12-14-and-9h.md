# 0004. The format is frozen after steps 12, 14 and 9h

- Status: accepted
- Date: 2026-11-24
- Pull requests: none (a decision of the project owner)

## Context
The format has a generation (`version`) that readers refuse if it is not theirs, and rules for what can change without a new generation: optional keys, new critical sections, new `[host]` keys (§3.1.11), new device categories (§3.1.5) and critical sections (§3.1.9) (PANPAK-FORMAT.md §2.1). Browsers and Electron apps (step 12) and programs of the terminal (step 14) are the two kinds of app that are likely to need new fields, and point 9h makes one app of each kind run the whole cycle.

## Decision
The format is frozen only after steps 12 and 14 are complete and after 9h. Until then a list of what the format does not have yet and may need is kept in the plan of the project (it is not a document of this repository yet; it moves here when the freeze is decided), each entry with "decide now" (the shape cannot change later without a new generation) or "later, as a critical field or in the area of extensions", and the reason.

## Alternatives that were not chosen
Freezing now: the apps that are most likely to need new permissions have not been tried.

## Consequences
The list of open questions is part of the work until the freeze; entries that are about what a signature covers (for example files that are fetched at install time) are decided first.

## Sources
PANPAK-FORMAT.md §2.1 (versioning), §3.1.5 (device categories), §3.1.9 (critical fields), §3.1.11 (the `[host]` table); the decision of the project owner of 2026-11-24. The "steps" are the numbers of the plan of the project: step 12 is browsers and Electron apps, step 14 programs of the terminal, 9h one app of each kind through the whole cycle.
