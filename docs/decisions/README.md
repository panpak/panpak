# Decisions

**When to write a record:** an ADR only for a decision that is hard to change, or that a newcomer would question. A choice that is easy to undo, or that the code and its comments already explain, does not need one. The first nine records (below) are kept as they are.

Short records of decisions that shape the project: what was decided, why, what was not chosen. One file each, `NNNN-title.md`, made from `0000-template.md`. A decision that is replaced is not edited: a new record says that it supersedes it, and the old one gets the status "superseded by NNNN".

| # | Title | Status | Date |
|---|---|---|---|
| 0001 | The FUSE tree of GVfs instead of a router | accepted | 2026-11-24 |
| 0002 | The `.pyc` files are not made by `pack` | accepted | 2026-11-23 |
| 0003 | `network = true` without ports means the full network | accepted (PR 112) | 2026-11-24 |
| 0004 | The format is frozen after steps 12, 14 and 9h | accepted | 2026-11-24 |
| 0005 | Patches: the hash of each patch is in the signed index | accepted | 2026-11-23 |
| 0006 | Times from content, and the private cache emptied at each change of version | accepted | 2026-11-23 |
| 0007 | No app runs that the user did not open | accepted | 2026-11-23 |
| 0008 | The last version that worked is counted in starts, not in days | accepted | 2026-11-23 |
| 0009 | Questions without a window tool: a notification of the portal | accepted | 2026-11-24 |
