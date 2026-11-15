# A store without extraction: composefs, erofs, fs-verity, FUSE

A study, no code. The question: could the store keep each version as one read-only image (or as a metadata image over a store of files) and
mount it, instead of making a tree of files? What it would need without privileges, what it costs with FUSE, and what it would gain.

Measured on one machine (Ubuntu, kernel 7.0, ext4 on LVM, 16 cores, release binaries of step 8a). What was not run here is said so.

## 1. What a tree costs today

| | Text Editor (133 MB of files, 2,230 files, 938 folders) |
|---|---|
| package (compressed, zstd, FastCDC chunks) | 53 MB |
| tree of files in the store (hard links to `blobs/`) | 133 MB |
| first install, from nothing | 0.44 to 0.48 s |
| second GTK app (Ghostty) after it, 77% of the files already there | 0.24 to 0.31 s (median 0.29 s) |
| a start (`--version` of the app, warm) | 23 ms |

Of the 0.3 to 0.5 s of an install, the decoding and writing of the files are the smaller part: the rest is making 938 folders, the links, and
the `fsync`s that keep a power loss from leaving a link to something that is not on disk (PANPAK-FORMAT.md 6.2). A mounted image has none of that: one file is written
and synced. That is the whole attraction.

## 2. The four ways to mount an image

| Way | Needs | Unprivileged? |
|---|---|---|
| **erofs or squashfs mounted by the kernel** | `mount(2)` with `CAP_SYS_ADMIN` in the initial user namespace | No, as far as I know: neither is mountable from a user namespace (a corrupt image is a kernel attack surface, and unprivileged mounts are kept to a short list of file systems). This kernel does not list `erofs` in `/proc/filesystems`. Not tested here: no root, no module |
| **composefs** (a small erofs image of the metadata, mounted under an overlayfs whose lower layer is a store of the real files, content-addressed, checked with fs-verity) | the same `mount` of the erofs image, plus overlayfs | No for the kernel path. `overlay` exists here and can be mounted in a user namespace with `userxattr` (kernel 5.11 and later) but the erofs lower image cannot. composefs has its own FUSE server (`composefs-fuse`) for exactly this case. Not tested here: the tools are not installed |
| **an image through FUSE** (`squashfuse`, `erofsfuse`, `dwarfs`) | `/dev/fuse` and the setuid `fusermount3` (both present here, as on every desktop distribution) | **Yes.** No privileges beyond what a user already has. Measured below with `squashfuse` |
| **systemd `mountfsd`** (systemd 256 and later: a service that mounts a disk image for an unprivileged user through polkit) | an image with a dm-verity signature that the system trusts (its key ring, not ours), a recent systemd | Yes, but only for images signed by a key that the machine trusts. A key of ours is not one. Not tested here |

**fs-verity** (the per-file Merkle tree that makes the kernel check every page that is read) needs the file system to have it. On this ext4, enabling it on a file
by an unprivileged user fails with `EOPNOTSUPP` (the `verity` feature of ext4 is off in the default installer layout, and turning it on needs `tune2fs` and root). tmpfs does not
support it either. f2fs, btrfs and ext4 with the feature can; a default Ubuntu install is not among them. So the integrity half of composefs is not available here
without root, and we cannot count on it on the machines of users.

## 3. What FUSE costs (measured)

The Text Editor tree as a squashfs image (zstd, 41 MB against 133 MB), mounted with `squashfuse` by the user, a start of the app from it against the same
files in the store, 40 alternated runs each:

| | files in the store | `squashfuse` |
|---|---|---|
| `gnome-text-editor --version` (median / best) | 23.0 / 21.5 ms | 26.0 / 23.9 ms (+3 ms, +13%) |
| the first start from a freshly mounted image | | 108 ms |
| `lstat` of its 2,303 files | 25 ms | 103 ms (4 times) |
| reading all 124 MB | 71 ms | 302 ms (4 times) |
| size on disk | 133 MB | 41 MB |

Also measured: a bubblewrap with `--unshare-user` that binds the FUSE mount in reads it (no `allow_other` was needed on this kernel). Not measured: Landlock rules on a FUSE
path, the memory of the FUSE process (one per mounted app, resident while it runs), what happens to the app when the FUSE process is killed (its files fail with
`ENOTCONN`: the app crashes), and the mount at login or at the first start (the 108 ms above is the first start with a mount that was just made; the mount itself is not in it).

## 4. What would be gained

- **Install**: from 0.3 to 0.5 s down to the time of one write and one `fsync` of an image of 41 MB (about 0.1 s, not measured as an install). The folders, the links and the
  thousands of `fsync`s go away.
- **Disk**: the compressed image is a third of the tree (41 MB against 133 MB for Text Editor). Shared files between apps would be shared only by an object store under it (composefs), not by squashfs;
  the hard links of identical files that the tree already has are worth more than the compression for the GTK apps that share libraries (measured in 8a: 219 MB stored for three apps whose trees sum to 380 MB).
- **Integrity**: with composefs and fs-verity, the kernel checks every file as it is read, and a tree cannot be changed under a running app. Today the tree is checked at install and by
  `panpak doctor`, not at every start. Without fs-verity (the situation here) a mounted image gives no more than a tree.
- **A removal** is the unmount and the deletion of one file.

## 5. What would be lost

- **Start time and file access**: +3 ms on a start and 4 times the cost of reading many files, with `squashfuse`; composefs-fuse and `erofsfuse` are different programs and were
  not measured. A kernel mount (root only) would cost nothing.
- **A process per app** that has to run before the app and while it runs, started and supervised by the stub, with its failures to handle (the mount point busy, a stale mount after a
  crash, `fusermount3` missing in a container or on a hardened system, `/dev/fuse` closed by a policy). Today there is nothing to start.
- **A new part of the attack surface**: a parser of images (squashfs, erofs) in a program that runs as the user. It is outside the sandbox of the app, but it reads a file that
  the signed package gave: the image would be made at install from the verified chunks, not taken from the package, so what it parses is what we wrote.
- **The farm and the links** (`.farm`, `.panpak-lib-<h>`) live next to the tree: they would need a place that is not read-only inside the image.
- **Updates**: a new image per version; the chunks of the old one that did not change are not shared with the new one (unless composefs with a store of files, which brings back the files).
- **Portability**: the same code must run where FUSE is not allowed (some containers, some Linux Security Modules profiles, Flatpak/Snap sandboxes). A tree works everywhere.

## 6. Verdict

Not now. The one way that needs no privileges (FUSE) is slower on every read, adds a process and a failure mode per app, and gives no integrity that a tree does not give;
the way that would give integrity (composefs with fs-verity) needs a file system feature that the default installs do not have and a privileged mount that
a user cannot do. What the install costs today, after step 8a, is 0.3 to 0.5 s and it is spent on work that an image would also need at some point (writing the data).

To reconsider after the beta: **`systemd-mountfsd`** (mounting signed images without privileges). It needs an image signed with a key that the machine trusts and a recent systemd, so
it does not fit today; if a distribution or Panpak itself gets a trusted signing path for images, the whole question changes (an erofs image with dm-verity, mounted for the user, with no FUSE process
and the kernel checking every read). Not tested here.

Two things in this study are worth keeping:

1. If the install time still matters after 8b to 8e, the gain is in the *folders and links*: a tree made of fewer, larger directories does not exist, but the folders can be made
   before the data arrives (the index lists them) and the links of already-stored files can be made by `linkat` in batches (`io_uring` could do that; threads already get most of it).
2. A later, separate option for users with root or `systemd-mountfsd`: mount an erofs image with fs-verity, the same record and the same store, chosen at install by what the
   machine offers. That would be an extra, never the only way, and only after a measurement on a machine that can do it.

## 7. How this was measured

`mksquashfs` of the tree (2.1 s, 41 MB); `squashfuse -f` run by the user (version 0.5.2, from the Ubuntu package, extracted into a work folder); the `--version` run is the
one of the study of defect 60 (a run of the app that loads its libraries and exits), alternated between the two roots; `os.walk` with `lstat` and a read of every file for the
last two rows; the fs-verity test is the ioctl `FS_IOC_ENABLE_VERITY` on a new file, which is what an installer without privileges would call.
