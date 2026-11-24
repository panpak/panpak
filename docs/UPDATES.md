# When updates are checked, downloaded and applied

This page is for the person who uses Panpak. The rules for implementers are in PANPAK-FORMAT.md section 6.6 and 6.7.

## Short version

- An app checks for an update **once a day**, in the background, at the lowest CPU priority and with idle disk priority. The check starts 20 s after the app starts, or when the last copy of the app ends. It never delays the start of the app and never changes the version that is running.
- A new version is **downloaded and checked** (signature and pinned key) when the check finds one. Nothing is asked at this point.
- The new version is **applied when the app is closed** and no copy of it is running. The next start is the new version.
- A version that asks for **new permissions** is not applied by itself. It waits until you accept it (`panpak update <id>` or the "Update now" window).
- A start never waits for the network.

## What this means in practice

An app that is started twice in a day can be a new version at the second start without a command from you: the first start did the check, and the end of that start applied the update. `panpak list` shows the version that is installed now.

The same job also reads a part of the installed files each day (64 MB by default, in the order of the signed index) to find damaged files, and repairs them when the app is not running.

## Turning it off

- `panpak config set background.updates false` turns the check off for every app. Updates then come only from `panpak update <id>`.
- `panpak config set background.verify_mb 0` turns the daily reading of files off.
- The environment variable `PANPAK_NO_BACKGROUND=1` keeps the stub from starting the job at all (for measures and tests).

## Where the state is

`~/.local/share/panpak/apps/<id>/update-state` holds the time of the last and of the next check and what the job found. The file is plain text.
