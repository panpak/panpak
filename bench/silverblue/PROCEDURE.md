# Step 9b: Panpak and Flatpak on Fedora Silverblue

What this measures: what a person with a Fedora Silverblue machine pays to get real apps with Panpak, and with Flatpak, in bytes that come, space on the disk, and time until the window is
usable; and what each system lets an app do with a document. Two virtual machines made from one clone of one Silverblue, `silverblue-flatpak` and `silverblue-panpak`, each with its own tool and
nothing of the other. **Three apps in a series**, at the same upstream version on both sides: GNOME Sudoku and GNOME Mahjongg (two apps on the same GNOME runtime) and qBittorrent (a KDE app),
installed in two orders: Sudoku, Mahjongg, qBittorrent; and qBittorrent, Mahjongg, Sudoku. Showing the first app alone would be the convenient case for us (principle 6 of `docs/PRINCIPLES.md`): the
second app is where Flatpak shares its runtime and Panpak shares what its store shares, and that is in the tables. **GNOME Text Editor is not in the series**: it is the app of the test of permissions and
operations (section 6), with no measure of space or time. Nothing here changes the code of Panpak.

Every number says how many times it was measured, its median and how much it moves (minimum, maximum, quartiles and the median absolute deviation). A number with one measurement is not a result.

## 1. The machines

**The system (written down on 2026-11-20, before the clone).** Fedora Silverblue 45 (`fedora/45/x86_64/silverblue`, version 45.20261119.1, commit `5f50b523…21fd`), glibc 2.44, GNOME Shell 51.1.
**3D acceleration is off on both copies**: the windows are drawn by software on both sides, so that the measures are repeatable and the same for the two systems (it is slower than a machine with a GPU, and the
results say so).

**Both machines (the same, and written down).**

1. One image. Make `silverblue-flatpak` and `silverblue-panpak` as full clones of one Silverblue that is up to date and has been started once. Same number of CPUs and
   same memory in both (record them: `lscpu`, `free -m`), 3D off in both, same disk type. Use a virtual disk that is not shared with the host's disk cache for the measurements of the
   time (a "none" or "directsync" cache mode, or the host's disk cache dropped before a series: say which was used).
2. A user with autologin to **GNOME on Wayland** (the default of Silverblue), automatic screen lock and screen blanking off (`gsettings set org.gnome.desktop.screensaver lock-enabled false`,
   `gsettings set org.gnome.desktop.session idle-delay 0`), and animations as they are. A resolution that is the same in both machines.
3. Nothing that updates by itself, nothing that downloads in the background: `sudo systemctl disable --now rpm-ostreed-automatic.timer`, GNOME Software off
   (`gsettings set org.gnome.software download-updates false`, `gsettings set org.gnome.software allow-updates false`; it is also best to remove it from the autostart). Note that the bytes are counted on the network interface: while a series
   runs, nothing else may use the network of the machine.
   **The Flatpak apps that Silverblue brings stay.** Silverblue is installed with a set of GNOME apps as Flatpaks of Fedora (remote `fedora`, runtime `org.fedoraproject.Platform` 45 and
   `Platform.GL.default`: Text Editor 51.0, Calculator, Calendar, Characters, Connections, Contacts, Decibels, Extensions, Logs, Loupe, Maps, NautilusPreviewer, Papers, Snapshot, Weather, baobab, Clocks,
   Font Viewer and Fedora Media Writer). They are part of what a person receives, on both machines (the Panpak machine is a clone and has them too), and they use the runtime of Fedora, not the GNOME 51 runtime of Flathub:
   nothing is shared between them and the apps of the series. Do not remove them. **Their space is the base**: `./silverblue_bench.py base --system flatpak|panpak` writes it down (the system installation `/var/lib/flatpak`, its apps and
   its runtimes, the list with sizes) before anything is installed, and the series are read from there.
4. **A sudoers rule for two commands, and nothing more** (`sudoers-panpak-bench` in this folder): `sysctl -q vm.drop_caches=3` (the caches are dropped before a measurement of the first
   start) and `systemctl reboot` (the fallback of the reboot series; the user of the session can reboot through polkit). Each is written with all its arguments and no wildcard, so
   `sudo -n` refuses every other command; the script reads the bytes of the interface from `/proc/net/dev` and the space with `du` and `statvfs`, which need no root. It is a change of
   permissions, so it is made by the person who owns the machine: `sudo install -m 0440 -o root -g root sudoers-panpak-bench /etc/sudoers.d/panpak-bench` after replacing `BENCHUSER`
   (`visudo -cf` first). `silverblue_bench.py check` tells if the two commands are allowed for this user (`sudo -n -l <command>`).
5. The window extension, to read the moment of the first frame from the compositor:
   `mkdir -p ~/.local/share/gnome-shell/extensions && cp -r winwatch@panpak.bench ~/.local/share/gnome-shell/extensions/`, then log out and in, then
   `gnome-extensions enable winwatch@panpak.bench`, then log out and in once more. `cat ~/.cache/panpak-bench/windows.jsonl` has a line with `"event": "enabled"`.
   The list of versions of `metadata.json` includes 51 (GNOME Shell 51.1); if GNOME Shell refuses it anyway, say what it printed.
6. The script: `cp silverblue_bench.py ~/` and `chmod +x ~/silverblue_bench.py`. `./silverblue_bench.py check --system flatpak|panpak` must print `ok` on every line.
7. Write down: `rpm-ostree status`, `ldd --version` (glibc, see section 2), `flatpak --version`, `gnome-shell --version`, `uname -r`, `lscpu`, `free -m`, the date (`base` records most of it).

**`silverblue-flatpak` only.** The Flathub remote is added by the script at the user level (`flatpak remote-add --user flathub ...`); the apps of the series are installed in the user installation, the Fedora ones are in the system one.

**`silverblue-panpak` only.**

1. Copy `panpak` and `panpak-stub` from the host build (`target/x86_64-unknown-linux-musl/release/`) to `~/.local/bin/` of the machine, both executable. Nothing else is
   installed: Silverblue already has `bwrap` and `xdg-dbus-proxy` (they come with Flatpak).
2. `panpak doctor` shows what the system lacks (user namespaces for `bwrap`, Landlock). Write its output down. A line that says that the isolated view is not possible
   makes the Panpak start measure something that is not the product: stop and look at it first.
3. The packages are served by the host (nothing is copied into the machine, so that the bytes that come are counted the same way as for Flatpak):
   on the host, `python3 -m http.server 8000 --directory <folder with the .panpak files>` (the script `serve.sh`). The files are named `<app id>.panpak`
   (`org.gnome.Sudoku.panpak`, `org.gnome.Mahjongg.panpak`, `org.qbittorrent.qBittorrent.panpak`, and `org.gnome.TextEditor.panpak` for the test of section 6). The address of the host as seen from the machine is `PKGBASE` below.

**The state `clean`.** When everything above is done and none of the apps of the series is installed (the Fedora ones are there: they are the base), run `base`, shut the machine down and take a snapshot named
`clean` (`virsh snapshot-create-as <vm> clean`, or the same in virt-manager). Every series below starts from `clean`.

## 2. The apps

| | GNOME Sudoku | GNOME Mahjongg | qBittorrent | GNOME Text Editor (section 6 only) |
|---|---|---|---|---|
| Flathub | `org.gnome.Sudoku` 51.0.1, runtime `org.gnome.Platform//51` | `org.gnome.Mahjongg` 51.1, runtime `org.gnome.Platform//51` | `org.qbittorrent.qBittorrent` 5.3.0, runtime `org.kde.Platform//6.11` (libtorrent 2.1.2 and Boost 1.92 built into the app) | not installed from Flathub: the machine has the one of Fedora (51.0) |
| Panpak | 51.0.1 from the tag, `panpak build` | 51.1 from the tag, `panpak build` | 5.3.0 from the tag, `panpak build` | 51.0 from the tag, `panpak build` |
| Recipe | `recipes/sudoku/panpak.toml` | `recipes/mahjongg/panpak.toml` | `recipes/qbittorrent/panpak.toml` | `recipes/text-editor/panpak.toml` |
| Image | `debian-14` | `debian-14` | `debian-13` | `debian-14` |

The two GNOME apps are on the same runtime in Flathub, so the Flatpak of the second pays almost nothing for it: that is its strength and it is in the table; qBittorrent is on the KDE runtime, a second one.
The Fedora apps use the Fedora runtime, which is a third thing that was already on the disk before the series.

**The glibc of the machine, and a limit of ours.** The Panpak packages of the GNOME apps are built in the image `debian-14`, whose glibc is 2.43, and `pack` writes the highest symbol version that their files ask for in `[host].glibc`
(the three packages of 2026-11-21 say `2.43`; qBittorrent, built in `debian-13`, says `2.39`). `panpak install` refuses a package that the system cannot run (`PANPAK-E2005`), so the
system must have at least that glibc; Silverblue 45 has 2.44 and the series can be made as written. **The Flatpak does not have this limit**: it brings its own runtime (the GNOME 51 runtime carries its own glibc), so the same app
runs on a system with an older glibc, where our package is refused. That is a limit of Panpak for an app built on a newer base than the machine, and the results page says it as such, next to the numbers: it is the price of using the glibc of the system, which
is also what keeps the packages small and the graphics drivers working. If a machine of the series had an older glibc, the package would be refused and the way out is another image for that recipe, not `--force-host` (an app run on a glibc that is too old fails
at the start, and that is not a measure).

qBittorrent is built with the same libtorrent as the Flatpak (2.1.2, from the release archive with the hash that Flathub pins, static, C++20: `[[build.deps]]` of the recipe). Boost is the one of the image in the Panpak package and
Boost 1.92 built by Flatpak: both only use its headers, and the results say so. Sudoku uses qqwing 1.3.4 in both (a package of the image for Panpak, built from source by the Flatpak). Flathub builds the release tarball of each tag, Panpak the commit of the tag.
Flathub's versions were read on 2026-11-20 from `https://flathub.org/api/v2/appstream/<id>`. Read them again the day of the series and, if they changed, change the pins in the recipes (the commit of the tag) and say so with the results.
The point is: the same upstream source, a different way to deliver it. What differs on purpose and is part of the answer: Flatpak delivers the app and a **shared runtime** (downloaded once for the apps of that runtime); Panpak delivers one package that carries
its libraries. The permissions are not the same list (Flatpak's qBittorrent has `--filesystem=host`; the Panpak recipe asks for what it needs): write that in the report, not in the numbers.

## 3. What is measured, and how

| Number | How |
|---|---|
| **Bytes that came** | The receive counter of the interface of the default route (`/proc/net/dev`) before and after the whole install, with 5 s of quiet on each side. It counts everything that came on that interface: the TLS of the download and the headers are in. For Panpak the install is the download of the whole package (`curl` from `PKGBASE`) and `panpak install`: a package is one file and it comes whole, whatever the store already has. For Flatpak it is `flatpak install --user`, which brings the app and, if it is not there, its runtime. |
| **Space added** | The blocks used (`du -sx --block-size=1`) by the folders that each system writes to, before and after: Flatpak `~/.local/share/flatpak` and `~/.var/app`; Panpak `~/.local/share/panpak` and `~/.cache/panpak`. The used space of the file system is also written down as a cross-check. Measured after the install and before the first start, and the file system is left to settle for 5 s. The base (section 1) is not in it: it was there before. |
| **Second and third app** | The same two numbers for the apps that are installed after the first (`position1`, `position2`): they show what a system shares between apps. For Flatpak: the GNOME 51 runtime paid by Sudoku and not by Mahjongg, and the KDE runtime paid by qBittorrent. For Panpak: whatever the store of the first shares with the second, which is what the number says and nothing else is claimed. |
| **Time to the window** | From just before the command is started to the **first frame** of a window of the app, as GNOME Shell reports it (the extension writes the real-time clock when `first-frame` comes). The command is started inside a user scope (`systemd-run --user --scope`) for both systems, so that the cost of the scope is the same, and is stopped through it (and `flatpak kill` for Flatpak). A start that shows no window in 60 s is written down as a failure and is not left out. The first frame is the moment a person sees the window; it is not the moment the app is ready to be used (a document open, a list loaded), which for these apps is close to it. |

Three kinds of start:

1. **First start after the install** (`first`). The install is the thing just before it: the page cache holds what the install just wrote. To have a number with some
   spread, the install and the first start are repeated `R` times (`cycle`): remove (with the data), drop the caches, install, start. Each repeat counts its bytes and its space too.
2. **After a reboot** (`after-reboot`). Every start is the first one after a boot, with the caches empty: the series reboots the machine, logs in (autologin), waits a
   fixed time of quiet (45 s after the extension is up), measures one start, and reboots again. One start per reboot, `R` reboots for each app.
3. **Following starts** (`warm`). `N` starts one after the other, with a pause of 3 s, each ended through its scope.

The numbers to use: `R = 5` for the installs and first starts, `R = 5` reboots per app, `N = 15` warm starts. They are the defaults. If the spread (MAD as a percent of the median) is above 10 %,
run it again with more trials and say so; do not drop a trial because it looks wrong.

## 4. The series

Four series. Each one starts from the `clean` snapshot of its machine. The order of the apps is the thing that changes between the first and the second.

| Series | Machine | Order |
|---|---|---|
| 1 | `silverblue-flatpak` | Sudoku, Mahjongg, qBittorrent |
| 2 | `silverblue-flatpak` | qBittorrent, Mahjongg, Sudoku |
| 3 | `silverblue-panpak` | Sudoku, Mahjongg, qBittorrent |
| 4 | `silverblue-panpak` | qBittorrent, Mahjongg, Sudoku |

Inside the machine, as the logged-in user of the graphical session (a terminal of the session, not ssh; if ssh is used, `export XDG_RUNTIME_DIR=/run/user/$(id -u)
DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus`):

```
# Series 1 (flatpak, su then mj then qb). For series 2 swap the order; for 3 and 4 use --system panpak, --pkg-base-url and --keys.
export BENCH_RESULTS=~/panpak-bench-results/su-mj-qb       # the name of the folder is the order; one folder per series
./silverblue_bench.py check --system flatpak
./silverblue_bench.py base --system flatpak                # the space of what Silverblue brings, before anything is installed
./silverblue_bench.py scenario --system flatpak --order su,mj,qb --warm 15
./silverblue_bench.py cycle --system flatpak --app su --trials 5       # repeated installs and first starts: the spread of those two numbers
./silverblue_bench.py cycle --system flatpak --app mj --trials 5
./silverblue_bench.py cycle --system flatpak --app qb --trials 5
# the apps are installed again by the last cycle of each; now the series of reboots (5 per app; the machine reboots by itself and logs in):
./silverblue_bench.py reboot-series start --system flatpak --apps su,mj,qb --count 5
sudo systemctl reboot
```

For Panpak the same with `--system panpak --pkg-base-url http://<host>:8000` for `scenario`, and `--pkg-url http://<host>:8000/<app id>.panpak` for `cycle`, and in both **`--keys su=<key>,mj=<key>,qb=<key>`**:
the key that the author of each app published (the two GNOME apps have one author, so one key, and qBittorrent another: write them from the page of each author, never from the package that is being installed). The script
installs with `panpak install --yes --expect-key <key>`: a package of another key is refused and nothing is pinned. It never uses `--trust-first-key`: the comparison shows the safe way
(the key that the person was told), not the one that trusts what comes. The keys of the packages of the day are in the notes that come with them (`panpak inspect <package>` shows the key of a package: read it on the
machine of the author, not on this one). The series of reboots ends
by itself (the log shows `series done`; `reboot-series status` says where it is). The results are lines of JSON in `$BENCH_RESULTS/<system>.jsonl`.

When a series is done: copy the folder out of the machine (`scp -r`), then revert the machine to `clean`.

## 5. The tables

On the host, with the four folders copied (named `fp-su-mj-qb`, `fp-qb-mj-su`, `pp-su-mj-qb`, `pp-qb-mj-su`):

```
python3 silverblue_bench.py summarize --dir fp-su-mj-qb fp-qb-mj-su pp-su-mj-qb pp-qb-mj-su
```

prints, for each series, system, app and kind, the number of trials, the median, the minimum and the maximum, the quartiles and the MAD, and, for the installs,
the megabytes that came and the space added (`position0` is the first app of a series, `position1` the second, `position2` the third). The report takes these tables as they are and adds what they cannot
say (the versions, the permissions, the state of the machines, the base), and the comparison of the two orders.

## 6. Permissions and operations of the Text Editor (no space, no time)

One app, two systems, one list of things a person does with a document. On `silverblue-flatpak` the Text Editor is **the one of Fedora that is already there** (what a real user uses; nothing is installed from Flathub for it); on
`silverblue-panpak` it is ours (`panpak install --yes --expect-key <key of its author> org.gnome.TextEditor.panpak`; its recipe asks for the portal of the files, as an author who wants Save as to work would). No number of space or time is taken for it: the script refuses
(`start`, `install`, `cycle` say that it is not an app of the series).

1. **The permissions that the system gives it**, as the system says them: `./silverblue_bench.py permissions --system flatpak --app te` (`flatpak info`, `--show-permissions` and `--show-metadata`) and
   `./silverblue_bench.py permissions --system panpak --app te` (`panpak inspect`, text and JSON). The files are in the folder of results (`permissions-<system>-te.txt`). The two lists are not the same words: say in the report what each allows with files, display, network and the bus.
2. **The operations.** In the session, with a folder `~/Documents/ops` that has `a.txt` and `b.txt` (a few lines each). Open a document the way a person does: **from the file manager** (Files: right click, Open With, Text Editor). On the Panpak machine
   the menu also has the Text Editor of Fedora (it is there on both machines): choose the entry that is ours (look at the icon and at `panpak list`; to be sure, `gio launch ~/.local/share/applications/panpak.org.gnome.TextEditor.desktop ~/Documents/ops/a.txt` does the
   same thing as the file manager for that entry). Write, for each system, what happened at each step:

   | # | Step | What to write down |
   |---|---|---|
   | 1 | Open `a.txt` from the file manager | does it open; does the title show the right name |
   | 2 | Change a line and save (Ctrl+S) | saved in place, or an error, or a "Save as" dialog; read the file from a terminal after |
   | 3 | Save as `a2.txt` in the same folder | is the dialog the portal's or the app's own; does the file exist |
   | 4 | Save as in another folder (`~/Downloads`) | the same |
   | 5 | Close the app and open it again; open `a.txt` from the recent files (of the app, and of the file manager) | does it open; can it be saved |
   | 6 | With `a.txt` open, try to open `b.txt`, in the same folder, **from inside the app** (Open) | what the dialog shows; does it need the person to choose the file; what happens if the app asks for it without a choice (it cannot) |
   | 7 | A terminal check of what the app can see: `flatpak run --command=ls org.gnome.TextEditor ~/Documents/ops` on the Flatpak machine | the files that it lists (none, if the app has no access to the folder) |

   The line that the person cares about is 6: **the second document in the same folder that stays inaccessible** unless the person chooses it. A system that gives the whole folder when one file was meant has
   a permission that is larger than what the person wanted, and the report says it for whichever system does so. Say also, for step 2, what the system grants to the document that was opened by the file manager (read, or read and write): it is the
   thing that decides whether Ctrl+S works.
3. What is written is the table above for each machine and the two files of permissions: no numbers of time or space.

## 7. The SBOM of a real build (`coverage = "build"`)

On the host (it needs the network, the registry and the Debian snapshot service, and about 3.5 GB of memory for qBittorrent; one build at a time, in a capped scope):

```
python3 verify_sbom_build.py recipes/sudoku recipes/mahjongg recipes/qbittorrent recipes/text-editor
```

For each recipe it runs `panpak build` twice, then `panpak sbom` on each package, and checks: `coverage` is `build`; the SBOM names the source (the commit of the recipe), the build image and
every dependency taken from the image that the recipe declares; every library that the recipe builds from an archive is a component with the hash and the address of the recipe; the libraries of the tree are listed with their hashes; the two builds give the **same SBOM bytes**; the signed hash of the
manifest is the hash of the file; and `panpak sbom --export` has the name and the version of the recipe. A line that fails stops the check and says which. Result of 2026-11-20 and 2026-11-21: see `RESULTS.md`.

## 8. What to hand back

The four result folders (each with its `base-<system>.txt`, and the files `permissions-<system>-te.txt`); the table of the operations of section 6 for the two machines; the text of `panpak doctor` (the Panpak machine); the notes of section 1 point 7 of both machines;
whether any trial failed (`failed` column); any step that had to be done another way, and why. Nothing is changed in the repository by the series.

## 9. What this does not say

Two machines are not two users: a Silverblue that has had other Flathub apps installed shares runtimes with them, and the first-app cost of Flatpak then falls; here it is a clean machine with what Silverblue brings (the Fedora apps, which share nothing with
the GNOME 51 runtime). The network is one network at one time: Flathub's CDN and the host that serves the Panpak packages are not the same path, so the **time** of the downloads is written down but not compared; the bytes are.
The time to the first frame is the time with 3D off on both machines (software drawing), which is slower than a machine with a GPU and does not have to be slower in the same way for the two systems. **What Panpak cannot do that Flatpak can, and what Flatpak cannot do that Panpak does**
are written on the results page (`RESULTS.md`) before any number: an app built on a base newer than the machine's glibc is refused by Panpak and runs under Flatpak; an app that must keep the same files at the same hash for a signed, offline-checkable
update is something only the Panpak package says. The packages of the series are made by `panpak build` from the upstream source; an AppImage converted by `panpak convert` is a different thing and is not measured here.
