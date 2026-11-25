# 0008. The last version that worked is counted in starts, not in days

- Status: accepted
- Date: 2026-11-23 (amended 2026-11-24 for the counters)
- Pull requests: #97; the pull request of the counters (#111)

## Context
When an update does not start, the person should be able to go back to the version that worked, without a clock that decides what "worked" means: an app that is used once a week would take weeks to be "proven".

## Decision
The runtime counts how each start of an installed app ended: good (it ran 20 seconds or more), failed (it ended within 20 s with a non-zero status or a fatal signal), neither (a quick exit 0, SIGKILL, a stop signal that the stub passed on, `--version`). Three good starts make a version the last good one (`apps/<id>/last_good`). Two failed starts in a row, if an earlier version that worked exists, ask the person in a window ("Go back" or "Try again"). Outcomes never leave the computer. The failed starts in a row are forgotten when the version is changed (an install, an update, a rollback) and when the person chooses "Try again"; the end of a start of a version that is not installed any more is not counted.

## Alternatives that were not chosen
A number of days; an automatic rollback (it never happens without the person).

## Consequences
`gc` keeps the last good version (and `previous` until the installed one has worked, or always with `keep-previous`); under 2 GiB or 5% free the last good goes first; nothing below the floor of the author is kept for a rollback.

## Sources
PANPAK-FORMAT.md §6.10; THREAT-MODEL.md §4; docs/DIALOGS.md; pull requests #97 and #111.
