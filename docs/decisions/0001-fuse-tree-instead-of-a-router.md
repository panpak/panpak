# 0001. The FUSE tree of GVfs instead of a router

- Status: accepted
- Date: 2026-11-24
- Pull requests: #109

## Context
With the permission `gvfs`, GIMP's "Open Location" worked for `https://` and failed for `sftp://` and `smb://`: GVfs tells the app a FUSE path (`$XDG_RUNTIME_DIR/gvfs/...`) that did not exist inside the view. A first idea was a router rule on the private bus that empties the field `fuse_mountpoint` of six messages of `org.gtk.vfs.MountTracker`, so that GIO copies the file. A test with a stand-in (GIMP 3.2.8, a daemon without FUSE, one trial each, 2026-11-24) showed that GIMP then opened the remote file but could not export PNG or JPEG to it ("Bad address"); XCF saved.

## Decision
With the permission `gvfs`, `$XDG_RUNTIME_DIR/gvfs` goes into the view as a read and write bind mount and gets a Landlock rule, as Flatpak does with `--filesystem=xdg-run/gvfs`. It is opened only if `/proc/self/mountinfo` says it is the mount point of a FUSE file system of the user and it answers a `stat` made by a child process; it is never made by Panpak; it is never `stat`-ed or resolved in the process that starts the app. The permission already weighs as the whole home folder, so the tree adds no new risk.

## Alternatives that were not chosen
- The router that empties `fuse_mountpoint`: more code (a parser of message bodies in the router) and more attack surface, for a weaker result (export fails).
- Leaving `sftp://` and `smb://` unsupported.

## Consequences
`sftp://` and `smb://` should work as paths of files (expected, not yet tried with the real daemon). A stuck `gvfsd-fuse` can stop an app that reads it, and the `stat` of bubblewrap on the bind can delay a start (the same limit as Flatpak); a mount that does not answer is left out. An app that starts before the mount has no FUSE paths until its next start.

## Sources
PANPAK-FORMAT.md §3.1.5, §9.2; THREAT-MODEL.md §4 (GVfs); docs/COMPATIBILITY.md ("Network locations with the permission gvfs"); pull request #109.
