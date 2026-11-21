# Step 9b: Panpak and Flatpak on Fedora Silverblue

What this measures: what a person with a Fedora Silverblue machine pays to get **daily-use apps** with Panpak, and with Flatpak, in bytes that come, space on the disk, time until the window is usable and bytes of an
update; and what each system lets an app do. Two virtual machines made from one clone of one Silverblue, `silverblue-flatpak` and `silverblue-panpak`, each with its own tool and nothing of the other.
Nothing here changes the code of Panpak.

**The apps of the series** are three that a person adds from Flathub, of three toolkits and three Flatpak runtimes (so that the first app of each pays its runtime): **GIMP 3.2.8** (GTK 3, GNOME 51), **Kdenlive 26.08.3**
(Qt 6, KDE 6.11) and **ONLYOFFICE Desktop Editors 10.0.0** (Qt 5 with CEF, freedesktop 25.08); and, fourth, **qBittorrent 5.3.0 built from source**, which is the example of the build pipeline and not the centre
of the comparison (it shares the KDE 6.11 runtime of Kdenlive, which is what makes the sharing of a Flatpak runtime visible). They are installed in two orders (GIMP, Kdenlive, ONLYOFFICE, qBittorrent and the reverse),
because the first app is where Flatpak pays its runtime and the later ones are where it shares, and showing only the first would be the convenient case for us (principle 6 of `docs/PRINCIPLES.md`).
**GNOME Text Editor is not in the series**: it is the app of the test of permissions and files against the one that Fedora brings (section 6), with no measure of space or time.

**What a Panpak package is here: a conversion, made by us.** The Panpak side of GIMP, Kdenlive and ONLYOFFICE is the **official AppImage of the authors converted with `panpak convert`**, by us. They are
**not packages made or signed by the authors**: every one is signed with one key, the key of the converter, and says so in `[origin]` (the file it was made from, its SHA-256, `signed_by_upstream = false`); each AppImage
is checked against the SHA-256 that its author publishes before it is converted. The pages of results say it in those words. A conversion carries what the AppImage carries (its own libraries, for GIMP its own glibc), and what
an author would have done differently in a native package is not in the numbers. qBittorrent is not a conversion: it is built from the source with `panpak build` (recipe in `recipes/qbittorrent`), signed with the same key.

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
   Font Viewer and Fedora Media Writer). They are part of what a person receives, on both machines (the Panpak machine is a clone and has them too), and they use the runtime of Fedora, not the runtimes of Flathub:
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
7. Write down: `rpm-ostree status`, `ldd --version` (glibc), `flatpak --version`, `gnome-shell --version`, `uname -r`, `lscpu`, `free -m`, the date (`base` records most of it).

**`silverblue-flatpak` only.** The Flathub remote is added by the script at the user level (`flatpak remote-add --user flathub ...`); the apps of the series are installed in the user installation, the Fedora ones are in the system one.

**`silverblue-panpak` only.**

1. Copy `panpak` and `panpak-stub` from the host build (`target/x86_64-unknown-linux-musl/release/`) to `~/.local/bin/` of the machine, both executable. Nothing else is
   installed: Silverblue already has `bwrap` and `xdg-dbus-proxy` (they come with Flatpak).
2. `panpak doctor` shows what the system lacks (user namespaces for `bwrap`, Landlock). Write its output down. A line that says that the isolated view is not possible
   makes the Panpak start measure something that is not the product: stop and look at it first.
3. **The packages are served by the host over HTTPS** (nothing is copied into the machine, so that the bytes that come are counted the same way as for Flatpak, and an update is a real one, by range requests, from the address in
   the manifest). On the host: `python3 serve_https.py <folder with the .panpak files>` (it makes a certificate of a test authority for `192.168.122.1`, the host as the machines of libvirt see it, and serves with `Range`; if the machines are on
   another network, `--address <the address they see>` and the packages must be made with that address, see the next point). It writes `ca.pem` (the authority: copy **only that file** to the machine). The
   machine is given the authority with `--ca ca.pem` (the script uses it for `curl` and sets `network.ca_file` of Panpak, which is how Panpak is told to trust a network with its own authority).
4. **The conversions** are made on the host, once, with the address that the machine sees: `python3 make_conversions.py --appimages <folder with the AppImages> --out <folder to serve> --key <key of the converter> --update-url-base https://192.168.122.1:8443`
   (it refuses an AppImage whose SHA-256 is not the one of its author; it makes for each app the previous version, `<id>.prev.panpak`, and the current one, `<id>.panpak`, which follows it so that the revision goes up). qBittorrent is built
   with `verify_sbom_build.py --secret-file <the same key>`. The files are named `<app id>.panpak` and `<app id>.prev.panpak`.

**The state `clean`.** When everything above is done and none of the apps of the series is installed (the Fedora ones are there: they are the base), run `base`, shut the machine down and take a snapshot named
`clean` (`virsh snapshot-create-as <vm> clean`, or the same in virt-manager). Every series below starts from `clean`.

## 2. The apps, and what each system gives them

| | GIMP | Kdenlive | ONLYOFFICE Desktop Editors | qBittorrent |
|---|---|---|---|---|
| Toolkit | GTK 3 | Qt 6 (KDE Frameworks 6) | Qt 5 and CEF (Chromium 109) | Qt 6 |
| Flathub, version | `org.gimp.GIMP` 3.2.8 | `org.kde.kdenlive` 26.08.3 | `org.onlyoffice.desktopeditors` 10.0.0 (it repackages the official release) | `org.qbittorrent.qBittorrent` 5.3.0 |
| Flatpak runtime | `org.gnome.Platform` 51 | `org.kde.Platform` 6.11 | `org.freedesktop.Platform` 25.08 (and the Electron base app) | `org.kde.Platform` 6.11 (shared with Kdenlive) |
| Flatpak download / installed | 103 MB / 269 MB | 108 MB / 264 MB | 416 MB / 1.1 GB | 15 MB / 26 MB |
| Official AppImage (same version) | `GIMP-3.2.8-x86_64.AppImage`, download.gimp.org, 245.6 MB | `kdenlive-26.08.3-x86_64.AppImage`, download.kde.org, 214.3 MB | `DesktopEditors-x86_64.AppImage` v10.0.0, GitHub, 425.8 MB | none: built from source |
| Panpak package | conversion, 282.1 MB | conversion, 255.1 MB | conversion, 485.7 MB | built from source, tag `release-5.3.0` with libtorrent 2.1.2 |
| Previous version in both formats (for the update) | **none**: the AppImage 3.2.6 exists, the history of Flathub does not keep the commit of 3.2.6 | 26.08.2: the AppImage and the Flathub commit `b1fc34ac…` | 9.4.0: the AppImage (626 MB) and the Flathub commit `5806f472…` | none |
| Update measured | no pair: written as such, not invented | yes, 26.08.2 to 26.08.3 | yes, 9.4.0 to 10.0.0 | no |

The Flathub versions and the commits were read on 2026-11-21 (`flatpak remote-info --log`); read them again the day of the series, and the history of Flathub keeps only a few commits: if the commit of a previous version is gone,
the update of that app has no pair and the script says so (`update` refuses). `flatpak info` must say the version of the previous commit after the downgrade (the script checks it and stops if it does not).

**The permissions of each side (default of each format, nothing taken away to look safer).** Flatpak: what the manifest of Flathub asks. Panpak: what `convert` is told for the conversion (`conversions/<app>.toml`): the functions that the
Flatpak gives, as far as Panpak has an equivalent. The effective ones are recorded on the machines (`permissions`: `flatpak info --show-permissions`, `panpak inspect`).

| Function | Flatpak (finish-args of Flathub) | Panpak conversion |
|---|---|---|
| Network | GIMP, Kdenlive, ONLYOFFICE: `--share=network` | `network = true` in all three |
| Sound | Kdenlive and ONLYOFFICE: `--socket=pulseaudio` | `audio = true` for both |
| Display | GIMP: `--socket=fallback-x11` and Wayland; Kdenlive: the same; ONLYOFFICE: `--socket=x11` and Wayland | GIMP and Kdenlive: Wayland (no `x11`); ONLYOFFICE: `x11 = true` (the app has only the xcb plugin of Qt 5) |
| Files | `--filesystem=host` for all three (every folder, by path) and `/tmp` | the user folders where the app works (`pictures`, `documents`, `download`; Kdenlive also `videos`, `music`), in read and write, and the file dialog of the portal |
| Other | GIMP and Kdenlive: `--device=all` (graphics tablets, cameras, capture); Kdenlive: UDisks2 on the system bus; GIMP: gvfs, the screenshot services and the file manager of the session; ONLYOFFICE: gvfs; GIMP and ONLYOFFICE: `--share=ipc` | not given (below) |

**Where Panpak gives less than the Flatpak, and says so** (each of these is a function that the Flatpak has and the conversion does not, and each goes on the results page as a place where Panpak loses, not in a footnote):
`--filesystem=host` (a file outside the user folders, by path, only through the dialog of the portal or the one-file grant); `--device=all` (a graphics tablet in GIMP, a camera or capture device in Kdenlive: the category
`input` of `permissions.devices` is a name that is reserved and not implemented, so a tablet has no way in); the system bus of UDisks2 (mounting a drive from Kdenlive); gvfs. These are the things the series will show, not
things that were taken away to look more secure: there is no way yet in Panpak to give them.

**The glibc.** The conversions carry what the AppImages carry (GIMP its own glibc, which is why its package says nothing of the glibc of the system); qBittorrent, built in `debian-13`, says `[host].glibc = "2.39"`. The Text Editor of the test
of section 6 is built in `debian-14` (2.43). The machines have 2.44. **The Flatpak does not have the limit of a glibc of the system**: it brings its own runtime; a package that needs a glibc newer than the machine's is refused by Panpak
(`PANPAK-E2005`) and the same app runs under Flatpak. That is a limit of Panpak that the results page says next to the numbers, and it does not bite on these machines.

## 3. What is measured, and how

| Number | How |
|---|---|
| **Bytes that came** | The receive counter of the interface of the default route (`/proc/net/dev`) before and after the whole install, with 5 s of quiet on each side. It counts everything that came on that interface: the TLS of the download and the headers are in. For Panpak the install is the download of the whole package (`curl --cacert` from the host) and `panpak install`: a package is one file and it comes whole, whatever the store already has. For Flatpak it is `flatpak install --user`, which brings the app and, if it is not there, its runtime. |
| **Space added** | The blocks used (`du -sx --block-size=1`) by the folders that each system writes to, before and after: Flatpak `~/.local/share/flatpak` and `~/.var/app`; Panpak `~/.local/share/panpak` and `~/.cache/panpak`. The used space of the file system is also written down as a cross-check. Measured after the install and before the first start, and the file system is left to settle for 5 s. The base (section 1) is not in it: it was there before. |
| **Second, third and fourth app** | The same two numbers for the apps that are installed after the first (`position1`, `position2`, `position3`): they show what a system shares between apps. For Flatpak: the runtime that each of the first three pays and that qBittorrent shares with Kdenlive (GNOME 51, KDE 6.11, freedesktop 25.08, then nothing new for qBittorrent). For Panpak: whatever the store of the first shares with the next, which is what the number says and nothing else is claimed. |
| **Time to the window** | From just before the command is started to the **first frame** of a window of the app, as GNOME Shell reports it (the extension writes the real-time clock when `first-frame` comes). The command is started inside a user scope (`systemd-run --user --scope`) for both systems, so that the cost of the scope is the same, and is stopped through it (and `flatpak kill` for Flatpak). A start that shows no window in 60 s is written down as a failure and is not left out. The first frame is the moment a person sees the window; it is not the moment the app is ready to be used (an image open, a project loaded). |
| **An update** | `update --system S --app A`: for Flatpak the app is installed, taken back to the Flathub commit of the previous version (the version is checked), and `flatpak update` to the latest is measured; for Panpak the conversion of the previous version is installed and `panpak update` (the address in the manifest, by range requests: only the chunks that the installed version does not have) is measured. Bytes that came, space added (an update keeps the old version for the rollback), time; Panpak also says how many bytes it downloaded. `R = 3` times, each from a clean state. |

Three kinds of start:

1. **First start after the install** (`first`). The install is the thing just before it: the page cache holds what the install just wrote. To have a number with some
   spread, the install and the first start are repeated `R` times (`cycle`): remove (with the data), drop the caches, install, start. Each repeat counts its bytes and its space too.
2. **After a reboot** (`after-reboot`). Every start is the first one after a boot, with the caches empty: the series reboots the machine, logs in (autologin), waits a
   fixed time of quiet (45 s after the extension is up), measures one start, and reboots again. One start per reboot, `R` reboots for each app.
3. **Following starts** (`warm`). `N` starts one after the other, with a pause of 3 s, each ended through its scope.

The numbers to use: `R = 5` for the installs and first starts, `R = 5` reboots per app, `N = 15` warm starts. They are the defaults. If the spread (MAD as a percent of the median) is above 10 %,
run it again with more trials and say so; do not drop a trial because it looks wrong.

## 4. The series

Four series. Each one starts from the `clean` snapshot of its machine. The order of the apps is the thing that changes between the first and the later ones.

| Series | Machine | Order |
|---|---|---|
| 1 | `silverblue-flatpak` | GIMP, Kdenlive, ONLYOFFICE, qBittorrent |
| 2 | `silverblue-flatpak` | qBittorrent, ONLYOFFICE, Kdenlive, GIMP |
| 3 | `silverblue-panpak` | GIMP, Kdenlive, ONLYOFFICE, qBittorrent |
| 4 | `silverblue-panpak` | qBittorrent, ONLYOFFICE, Kdenlive, GIMP |

Inside the machine, as the logged-in user of the graphical session (a terminal of the session, not ssh; if ssh is used, `export XDG_RUNTIME_DIR=/run/user/$(id -u)
DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus`):

```
# Series 1 (flatpak). For series 2 swap the order; for 3 and 4 use --system panpak, --pkg-base-url, --keys and --ca (below).
export BENCH_RESULTS=~/panpak-bench-results/gimp-kdenlive-onlyoffice-qb          # the name of the folder is the order; one folder per series
./silverblue_bench.py check --system flatpak
./silverblue_bench.py base --system flatpak                # the space of what Silverblue brings, before anything is installed
./silverblue_bench.py scenario --system flatpak --order gimp,kdenlive,onlyoffice,qb --warm 15
for a in gimp kdenlive onlyoffice qb; do ./silverblue_bench.py cycle --system flatpak --app $a --trials 5; done     # repeated installs and first starts: the spread
./silverblue_bench.py update --system flatpak --app kdenlive --trials 3
./silverblue_bench.py update --system flatpak --app onlyoffice --trials 3
# the apps are installed again by the last cycle; now the series of reboots (5 per app; the machine reboots by itself and logs in):
./silverblue_bench.py reboot-series start --system flatpak --apps gimp,kdenlive,onlyoffice,qb --count 5
sudo systemctl reboot
```

For Panpak, before the series, once: `./silverblue_bench.py prerequisites --system panpak --trials 5` (the launcher and its prerequisites: the time and space of `install-runtime`, in a private home, so that the machine stays as it is); `install` then records on the first app whether it installed the runtime by itself (`runtime_installed`, `runtime_bytes_before`/`after`).

For Panpak the same with `--system panpak --pkg-base-url https://192.168.122.1:8443 --keys '*=<key of the converter>' --ca ~/ca.pem` for `scenario` and `update`, and `--pkg-url https://192.168.122.1:8443/<app id>.panpak` for `cycle`
(the same `--keys` and `--ca`). The script installs with `panpak install --yes --expect-key <key>`: a package of another key is refused and nothing is pinned. It never uses `--trust-first-key`: the comparison shows the
safe way (the key that the person was told), not the one that trusts what comes. The key is the one of the converter, written by whoever made the packages (read it from `panpak inspect <package>` on the host that made them, never on
this machine). The series of reboots ends by itself (the log shows `series done`; `reboot-series status` says where it is). The results are lines of JSON in `$BENCH_RESULTS/<system>.jsonl`.

When a series is done: copy the folder out of the machine (`scp -r`), then revert the machine to `clean`.

## 5. The tables

On the host, with the four folders copied (named `fp-gimp-first`, `fp-qb-first`, `pp-gimp-first`, `pp-qb-first`):

```
python3 silverblue_bench.py summarize --dir fp-gimp-first fp-qb-first pp-gimp-first pp-qb-first
```

prints, for each series, system, app and kind, the number of trials, the median, the minimum and the maximum, the quartiles and the MAD, and, for the installs,
the megabytes that came and the space added (`position0` is the first app of a series, then `position1`, `position2`, `position3`), and for the updates the bytes of each. The report takes these tables as they are and adds what they cannot
say (the versions, the permissions, the state of the machines, the base), and the comparison of the two orders.

## 6. Permissions and files: the Text Editor (no space, no time)

One app, two systems, one list of things a person does with a document. On `silverblue-flatpak` the Text Editor is **the one of Fedora that is already there** (what a real user uses; nothing is installed from Flathub for it); on
`silverblue-panpak` it is ours, built from the source (`recipes/text-editor`, `debian-14`, the portal of the files, signed with the same key). No number of space or time is taken for it: the script refuses
(`install`, `start`, `cycle` say that it is not an app of the series).

1. **The permissions that the system gives it**, as the system says them: `./silverblue_bench.py permissions --system flatpak --app te` (`flatpak info`, `--show-permissions` and `--show-metadata`) and
   `./silverblue_bench.py permissions --system panpak --app te` (`panpak inspect`, text and JSON). The files are in the folder of results (`permissions-<system>-te.txt`). The two lists are not the same words: say in the report what each allows with files, display, network and the bus.
2. **The operations.** In the session, with a folder `~/Documents/ops` that has `a.txt` and `b.txt` (a few lines each). Open a document the way a person does: **from the file manager** (Files: right click, Open With, Text Editor). On the Panpak machine
   the menu also has the Text Editor of Fedora (it is there on both machines): choose the entry that is ours (look at `panpak list`; to be sure, `gio launch ~/.local/share/applications/panpak.org.gnome.TextEditor.desktop ~/Documents/ops/a.txt` does the
   same thing as the file manager for that entry). Write, for each system, what happened at each step:

   | # | Step | What to write down |
   |---|---|---|
   | 1 | Open `a.txt` from the file manager | does it open; does the title show the right name |
   | 2 | Change a line and save (Ctrl+S) | saved in place, or an error, or a "Save as" dialog; read the file from a terminal after |
   | 3 | Save as `a2.txt` in the same folder | is the dialog the portal's or the app's own; does the file exist |
   | 4 | Save as in another folder (`~/Downloads`) | the same |
   | 5 | Close the app and open it again; open `a.txt` from the recent files (of the app, and of the file manager) | does it open; can it be saved |
   | 6 | With `a.txt` open, try to open `b.txt`, in the same folder, **from inside the app** (Open) | what the dialog shows; does it need the person to choose the file |
   | 7 | A terminal check of what the app can see: `flatpak run --command=ls org.gnome.TextEditor ~/Documents/ops` on the Flatpak machine | the files that it lists (none, if the app has no access to the folder) |

   The line that the person cares about is 6: **the second document in the same folder that stays inaccessible** unless the person chooses it. A system that gives the whole folder when one file was meant has
   a permission that is larger than what the person wanted, and the report says it for whichever system does so. Say also, for step 2, what the system grants to the document that was opened by the file manager (read, or read and write): it is the
   thing that decides whether Ctrl+S works.
3. What is written is the table above for each machine and the two files of permissions: no numbers of time or space.

**The files** for these operations are made on the host with `python3 make_ops_files.py ops` (two text files in one folder, a picture, a document, two 6 second test videos made with ffmpeg, and, in `ops/outside`, a picture, a document and a video in a folder that is not a user folder; the same bytes on both machines): copy `ops/Documents`, `ops/Pictures`, `ops/Videos` and `ops/outside` into the home of both machines. The test video of Kdenlive is `Videos/clip.mp4`; `outside/clip-outside.mp4` is the one for a clip by path outside the granted folders.

**The torrent of qBittorrent is a controlled peer on the host, not a public one.** `python3 seed_peer.py` serves one file of 8 MiB, always the same bytes (`panpak-bench-testfile.bin`), from `192.168.122.1:6891`
only (it refuses an address that listens on every interface), and prints the magnet link, which names the peer (`x.pe`), so no tracker and no DHT is needed and nothing is announced. Leave it running during the operations of qBittorrent on both machines
(`python3 seed_peer.py --self-test` checks it: it takes the metadata and every piece with a client of its own). The magnet link is in the checklist.

**The checklist to print**: `python3 make_checklist.py checklist.html`, open it in a browser and print it. It has one row per operation, for each app and for each machine (what to do with the mouse, what must happen, boxes for worked and failed, a line for what was seen); the operations are the ones in that script (GIMP, Kdenlive, ONLYOFFICE, qBittorrent and the Text Editor of this section) and the sheet is the record of the manual part.

**A step that fails with Panpak because the conversion lacks a permission that the Flatpak has** (`filesystem=host`, `device=all`, UDisks2, gvfs) is written in `RESULTS.md` as a hole to close (principle 3), with the exact operation that fails: not only as a loss.

**The prerequisites of Panpak.** The first `panpak install` on a machine may also install the `panpak-stub` that comes with the program (the runtime of the launcher): the script records, for the first install of a series, whether it did (`runtime_installed`), and measures it in every case as **launcher and prerequisites**, next to the runtimes of Flatpak: its space and the time it took. If `install` does not install it by itself, the first step of the procedure is `panpak install-runtime <stub> --allow-unsigned` and the same two numbers are taken for it. The user should never have to run it by hand (step 10a): in the procedure it is a recorded prerequisite, not a part of the test.

**Wayland on GNOME.** The security contexts of Wayland are not there in GNOME (Mutter does not support them yet, `panpak doctor` says so): an app has on the desktop the same powers with Panpak as with Flatpak. It is a limit of GNOME, not of the formats, and the results page says it once, in the part about the sandbox.

## 7. The sandbox of an app with a browser engine (ONLYOFFICE, CEF)

`./silverblue_bench.py sandbox --system flatpak|panpak --app onlyoffice` starts the app, takes the process of the window from the compositor, and reads every process below it from `/proc`: the filter of system
calls (`Seccomp: 2` is a filter), `NoNewPrivs`, the capabilities, which namespaces it shares with the host, and whether its command line has `--no-sandbox`. Chromium (CEF 109) has a sandbox of its own, for every renderer;
**the app turns it off itself** (`--no-sandbox` on every helper, whatever the system does), and neither system adds it. The question for the page is: does the Flatpak isolate the processes of the editor better than Panpak, and where does it not.
A first reading, on 2026-11-21 on the host (Flatpak in a scratch installation, Panpak with the conversion, both under a private display): in both, `DesktopEditors` and its six helpers run in **one**
sandbox, `Seccomp: 2` (a filter of system calls), `NoNewPrivs: 1`, no capability, every helper with `--no-sandbox`, in the user, pid and mount namespaces of the sandbox and sharing the **network and the IPC namespace with the host**
(both ask for them: the network, and for X11 the IPC). No process of the browser engine is isolated from the others, in either. So on this the Flatpak is **not better** than Panpak: it is the same design, and the difference is the
grant of files (`--filesystem=host` against three user folders). The page says so with the table of the two machines. If the machines show something else (a Flatpak that starts the renderers apart), it goes on the page among the places
where Panpak loses.

## 8. The SBOM of a real build (`coverage = "build"`)

On the host (it needs the network, the registry and the Debian snapshot service, and about 3.5 GB of memory for qBittorrent; one build at a time, in a capped scope):

```
python3 verify_sbom_build.py --secret-file <key of the converter> recipes/qbittorrent recipes/text-editor
```

For each recipe it runs `panpak build` twice, then `panpak sbom` on each package, and checks: `coverage` is `build`; the SBOM names the source (the commit of the recipe), the build image and
every dependency taken from the image that the recipe declares; every library that the recipe builds from an archive is a component with the hash and the address of the recipe; the libraries of the tree are listed with their hashes; the two builds give the **same SBOM bytes**; the signed hash of the
manifest is the hash of the file; and `panpak sbom --export` has the name and the version of the recipe. A line that fails stops the check and says which. Results: `RESULTS.md`. (The conversions have `coverage = "best-effort"`: they are made from a binary, and say what the files show.)

## 9. What to hand back

The four result folders (each with its `base-<system>.txt`, the files `permissions-<system>-te.txt` and the lines of `sandbox`); the tables of sections 6 and 7 for the two machines; the text of `panpak doctor` (the Panpak machine); the notes of section 1 point 7 of both machines;
whether any trial failed (`failed` column); any step that had to be done another way, and why. Nothing is changed in the repository by the series.

## 10. What this does not say

Two machines are not two users: a Silverblue that has had other Flathub apps installed shares runtimes with them, and the first-app cost of Flatpak then falls; here it is a clean machine with what Silverblue brings (the Fedora apps, which share nothing with
the runtimes of Flathub). The network is one network at one time: Flathub's CDN and the host that serves the Panpak packages are not the same path, so the **time** of the downloads is written down but not compared; the bytes are.
The time to the first frame is the time with 3D off on both machines (software drawing), which is slower than a machine with a GPU and does not have to be slower in the same way for the two systems. **What Panpak cannot do that Flatpak can, and what Flatpak cannot do that Panpak does**
are written on the results page (`RESULTS.md`) before any number. The Panpak packages are conversions of the AppImages of the authors: what a native package could save (a shared runtime, fewer bundled libraries) is not measured here, and the
update of a conversion depends on how the AppImage was rebuilt (a rebuilt AppImage shares fewer chunks than a Flatpak shares objects), which is a property of the conversion and not of the format.
