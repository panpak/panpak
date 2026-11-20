# Step 9b: Panpak and Flatpak on Fedora Silverblue

What this measures: what a person with a Fedora Silverblue machine pays to get two real apps with Panpak, and with Flatpak, in bytes that come, space on the disk,
and time until the window is usable. Two virtual machines made from one clone of one Silverblue, `silverblue-flatpak` and `silverblue-panpak`, each with its own tool
and nothing of the other. The same two apps, at the same upstream version, on both sides, installed in the two orders. Nothing here changes the code of Panpak.

Every number says how many times it was measured, its median and how much it moves (minimum, maximum, quartiles and the median absolute deviation). A number with
one measurement is not a result.

## 1. The machines

**Both machines (the same, and written down).**

1. One image. Make `silverblue-flatpak` and `silverblue-panpak` as full clones of one Silverblue that is up to date and has been started once. Same number of CPUs and
   same memory in both (record them: `lscpu`, `free -m`). Same disk type. Use a virtual disk that is not shared with the host's disk cache for the measurements of the
   time (a "none" or "directsync" cache mode, or the host's disk cache dropped before a series: say which was used).
2. A user with autologin to **GNOME on Wayland** (the default of Silverblue), automatic screen lock and screen blanking off (`gsettings set org.gnome.desktop.screensaver lock-enabled false`,
   `gsettings set org.gnome.desktop.session idle-delay 0`), and animations as they are. A resolution that is the same in both machines.
3. Nothing that updates by itself, nothing that downloads in the background: `sudo systemctl disable --now rpm-ostreed-automatic.timer`, GNOME Software off
   (`gsettings set org.gnome.software download-updates false`, `gsettings set org.gnome.software allow-updates false`; it is also best to remove it from the autostart),
   and no Flatpak apps installed (`flatpak list` is empty, `flatpak remotes` has no remote with data). Note that the bytes are counted on the network interface: while a series
   runs, nothing else may use the network of the machine.
4. **A sudoers rule for two commands, and nothing more** (`sudoers-panpak-bench` in this folder): `sysctl -q vm.drop_caches=3` (the caches are dropped before a measurement of the first
   start) and `systemctl reboot` (the fallback of the reboot series; the user of the session can reboot through polkit). Each is written with all its arguments and no wildcard, so
   `sudo -n` refuses every other command; the script reads the bytes of the interface from `/proc/net/dev` and the space with `du` and `statvfs`, which need no root. It is a change of
   permissions, so it is made by the person who owns the machine: `sudo install -m 0440 -o root -g root sudoers-panpak-bench /etc/sudoers.d/panpak-bench` after replacing `BENCHUSER`
   (`visudo -cf` first). `silverblue_bench.py check` tells if the two commands are allowed for this user (`sudo -n -l <command>`).
5. The window extension, to read the moment of the first frame from the compositor:
   `mkdir -p ~/.local/share/gnome-shell/extensions && cp -r winwatch@panpak.bench ~/.local/share/gnome-shell/extensions/`, then log out and in, then
   `gnome-extensions enable winwatch@panpak.bench`, then log out and in once more. `cat ~/.cache/panpak-bench/windows.jsonl` has a line with `"event": "enabled"`.
   If GNOME Shell refuses it because of its version, say which version it is (`gnome-shell --version`): the list in `metadata.json` is widened and nothing else changes.
6. The script: `cp silverblue_bench.py ~/` and `chmod +x ~/silverblue_bench.py`. `./silverblue_bench.py check --system flatpak|panpak` must print `ok` on every line.
7. Write down: `rpm-ostree status`, `ldd --version` (glibc, see section 2), `flatpak --version`, `gnome-shell --version`, `uname -r`, `lscpu`, `free -m`, the date.

**`silverblue-flatpak` only.** The Flathub remote is added by the script at the user level (`flatpak remote-add --user flathub ...`). Install nothing before.

**`silverblue-panpak` only.**

1. Copy `panpak` and `panpak-stub` from the host build (`target/x86_64-unknown-linux-musl/release/`) to `~/.local/bin/` of the machine, both executable. Nothing else is
   installed: Silverblue already has `bwrap` and `xdg-dbus-proxy` (they come with Flatpak).
2. `panpak doctor` shows what the system lacks (user namespaces for `bwrap`, Landlock). Write its output down. A line that says that the isolated view is not possible
   makes the Panpak start measure something that is not the product: stop and look at it first.
3. The packages are served by the host (nothing is copied into the machine, so that the bytes that come are counted the same way as for Flatpak):
   on the host, `python3 -m http.server 8000 --directory <folder with the two .panpak files>` (the script `serve.sh`). The files are named `<app id>.panpak`
   (`org.gnome.TextEditor.panpak`, `org.qbittorrent.qBittorrent.panpak`). The address of the host as seen from the machine is `PKGBASE` below.

**The state `clean`.** When everything above is done and nothing of the two apps is installed, shut the machine down and take a snapshot named `clean`
(`virsh snapshot-create-as <vm> clean`, or the same in virt-manager). Every series below starts from `clean`.

## 2. The apps

| | GNOME Text Editor | qBittorrent |
|---|---|---|
| Flathub | `org.gnome.TextEditor`, 51.0, runtime `org.gnome.Platform//51` | `org.qbittorrent.qBittorrent`, 5.3.0, runtime `org.kde.Platform//6.11` (libtorrent 2.1.2 and Boost 1.92 built into the app) |
| Panpak | `org.gnome.TextEditor`, 51.0, built by `panpak build` from the upstream tag | `org.qbittorrent.qBittorrent`, 5.3.0, built by `panpak build` from the upstream tag |
| Recipes | `recipes/text-editor/panpak.toml` | `recipes/qbittorrent/panpak.toml` |

**The glibc of the machine, and a limit of ours.** The Panpak package of Text Editor 51.0 is built in the image `debian-14`, whose glibc is 2.43, and `pack` writes the highest symbol version that its files ask for in `[host].glibc`:
the package in the check of 2026-11-20 says `2.43` (qBittorrent, built in `debian-13`, says `2.39`). `panpak install` refuses a package that the system cannot run (`PANPAK-E2005`), so the
Silverblue of the machines must have glibc 2.43 or later (Fedora 44 or later; `ldd --version`). Write the version down with the rest. **The Flatpak does not have this limit**: it brings its own runtime (the GNOME 51 runtime
carries its own glibc), so the same app runs on a system with an older glibc, and the Flatpak of Text Editor 51.0 would install on a Fedora 43 where our package is refused. That is a limit of Panpak for an app built on a
newer base than the machine, and the results page says it as such, next to the numbers, not in a footnote: it is the price of using the glibc of the system, which is also what keeps the packages small and the graphics drivers working. With Fedora 43 (glibc 2.42) the Text Editor
package is refused, and the series cannot be made as written: the way out is another image for that recipe, not `--force-host` (an app run on a glibc that is too old fails at the start, and that is not a measure).

qBittorrent is built with the same libtorrent as the Flatpak (2.1.2, from the release archive with the hash that Flathub pins, static, C++20: `[[build.deps]]` of the recipe). Boost is the one of the image in the Panpak package and
Boost 1.92 built by Flatpak: both only use its headers, and the results say so. Flathub's versions were read on 2026-11-20 from `https://flathub.org/api/v2/appstream/<id>`. Read them again the day of the series and, if they changed, change the pins in the
recipes (the commit of the tag) and say so with the results. The point is: the same upstream source, a different way to deliver it. What differs on purpose and is part of the answer:
Flatpak delivers the app and a **shared runtime** (that is downloaded once for the apps of that runtime); Panpak delivers one package that carries its libraries. The permissions
are not the same list (Flatpak's qBittorrent has `--filesystem=host`; the Panpak recipe asks for what it needs): write that in the report, not in the numbers.

## 3. What is measured, and how

| Number | How |
|---|---|
| **Bytes that came** | The receive counter of the interface of the default route (`/proc/net/dev`) before and after the whole install, with 5 s of quiet on each side. It counts everything that came on that interface: the TLS of the download and the headers are in. For Panpak the install is the download of the package (`curl` from `PKGBASE`) and `panpak install`. For Flatpak it is `flatpak install --user`, which brings the app and, the first time, its runtime. |
| **Space added** | The blocks used (`du -sx --block-size=1`) by the folders that each system writes to, before and after: Flatpak `~/.local/share/flatpak` and `~/.var/app`; Panpak `~/.local/share/panpak` and `~/.cache/panpak`. The used space of the file system is also written down as a cross-check. Measured after the install and before the first start, and the file system is left to settle for 5 s. |
| **Second app** | The same two numbers for the app that is installed after the first: it shows what a system shares between two apps (for Flatpak, a runtime; for Panpak, nothing is shared across packages, only what the store keeps). |
| **Time to the window** | From just before the command is started to the **first frame** of a window of the app, as GNOME Shell reports it (the extension writes the real-time clock when `first-frame` comes). The command is started inside a user scope (`systemd-run --user --scope`) for both systems, so that the cost of the scope is the same, and is stopped through it (and `flatpak kill` for Flatpak). A start that shows no window in 60 s is written down as a failure and is not left out. The first frame is the moment a person sees the window; it is not the moment the app is ready to be used (a document open, a list loaded), which for these two apps is close to it. |

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
| 1 | `silverblue-flatpak` | Text Editor, then qBittorrent |
| 2 | `silverblue-flatpak` | qBittorrent, then Text Editor |
| 3 | `silverblue-panpak` | Text Editor, then qBittorrent |
| 4 | `silverblue-panpak` | qBittorrent, then Text Editor |

Inside the machine, as the logged-in user of the graphical session (a terminal of the session, not ssh; if ssh is used, `export XDG_RUNTIME_DIR=/run/user/$(id -u)
DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus`):

```
# Series 1 (flatpak, te then qb). For series 2 swap the order; for 3 and 4 use --system panpak and --pkg-base-url.
export BENCH_RESULTS=~/panpak-bench-results/te-qb          # the name of the folder is the order; one folder per series
./silverblue_bench.py check --system flatpak
./silverblue_bench.py scenario --system flatpak --order te,qb --warm 15
./silverblue_bench.py cycle --system flatpak --app te --trials 5       # repeated installs and first starts: the spread of those two numbers
./silverblue_bench.py cycle --system flatpak --app qb --trials 5
# the apps are installed again by the last cycle of each; now the series of reboots (5 per app; the machine reboots by itself and logs in):
./silverblue_bench.py reboot-series start --system flatpak --apps te,qb --count 5
sudo systemctl reboot
```

For Panpak the same with `--system panpak --pkg-base-url http://<host>:8000` for `scenario`, and `--pkg-url http://<host>:8000/<app id>.panpak` for `cycle`. The series of reboots ends
by itself (the log shows `series done`; `reboot-series status` says where it is). The results are lines of JSON in `$BENCH_RESULTS/<system>.jsonl`.

When a series is done: copy the folder out of the machine (`scp -r`), then revert the machine to `clean`.

## 5. The tables

On the host, with the four folders copied (named `fp-te-qb`, `fp-qb-te`, `pp-te-qb`, `pp-qb-te`):

```
python3 silverblue_bench.py summarize --dir fp-te-qb fp-qb-te pp-te-qb pp-qb-te
```

prints, for each series, system, app and kind, the number of trials, the median, the minimum and the maximum, the quartiles and the MAD, and, for the installs,
the megabytes that came and the space added (`position0` is the first app of a series, `position1` the second). The report takes these tables as they are and adds what they cannot
say (the versions, the permissions, the state of the machines), and the comparison of the two orders.

## 6. The SBOM of a real build (`coverage = "build"`)

On the host (it needs the network, the registry and the Debian snapshot service, and about 3.5 GB of memory for qBittorrent; one build at a time, in a capped scope):

```
python3 verify_sbom_build.py recipes/text-editor recipes/qbittorrent
```

For each recipe it runs `panpak build` twice, then `panpak sbom` on each package, and checks: `coverage` is `build`; the SBOM names the source (the commit of the recipe), the build image and
every dependency taken from the image that the recipe declares; the libraries of the tree are listed with their hashes; the two builds give the **same SBOM bytes**; the signed hash of the
manifest is the hash of the file; and `panpak sbom --export` has the name and the version of the recipe. A line that fails stops the check and says which. Result of 2026-11-20 (both recipes, with the panpak of that day): Text Editor 51.0, `coverage = "build"`, 112 components (110 libraries), two builds with the same SBOM, the same root of the tree; qBittorrent 5.3.0, `coverage = "build"`, 85 components (83 libraries), the same. Neither recipe declares a dependency taken from the image, so that part of the check had nothing to look at in these two.

## 7. What to hand back

The four result folders; the text of `panpak doctor` (the Panpak machine); the notes of section 1 point 7 of both machines; whether any trial failed (`failed` column); any step that
had to be done another way, and why. Nothing is changed in the repository by the series.

## 8. What this does not say

Two machines are not two users: a Silverblue that has already had other Flatpak apps installed shares runtimes with them, and the first-app cost of Flatpak then falls. The network
is one network at one time: Flathub's CDN and the host that serves the Panpak packages are not the same path, so the **time** of the downloads is written down but not compared; the bytes are.
The time to the first frame is the time on a virtual GPU (software rendering or virtio-gpu), which is slower than the metal and slower in a way that does not have to be the same for the two
systems. **What Panpak cannot do that Flatpak can, and what Flatpak cannot do that Panpak does** are written on the results page (`RESULTS.md`) before any number: an app built on a base newer than the machine's glibc is refused by Panpak and
runs under Flatpak; an app that must keep the same files at the same hash for a signed, offline-checkable update is something only the Panpak package says. The packages of the series are made by `panpak build` from the upstream source; an AppImage converted by `panpak convert` is a different thing and is not measured here.
