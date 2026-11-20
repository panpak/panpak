// SPDX-License-Identifier: MIT OR Apache-2.0
//! The messages that a person reads in a window or a notification: `panpak remove` (the menu action "Uninstall") and protected mode. The ids are the English sentences; a catalog
//! `po/<language>.po` translates them (see `i18n`). A message with a `{name}` takes the value with `i18n::fill`.

pub const REMOVE_TITLE: &str = "Remove an app";
pub const REMOVE_ASK: &str = "Remove {name}?";
pub const REMOVE_WHAT: &str = "The app and its launcher are removed.";
pub const REMOVE_KEPT: &str = "Your settings and data are kept ({size}). If you install the app again, they are still there.";
pub const REMOVE_NO_DATA: &str = "It has no settings or data.";
pub const REMOVE_OK_LABEL: &str = "Remove";
pub const REMOVE_CANCEL_LABEL: &str = "Cancel";
pub const REMOVED_TITLE: &str = "Panpak: app removed";
pub const REMOVED: &str = "{name} was removed.";
pub const REMOVED_KEPT: &str = "{name} was removed. Your settings and data are kept.";
pub const NOT_REMOVED_TITLE: &str = "Panpak: the app was not removed";
pub const NOTHING_REMOVED_TITLE: &str = "Panpak: nothing was removed";
pub const NO_DIALOG_TOOL: &str = "There is no dialog tool (zenity or kdialog) to ask you. To remove {name}, run in a terminal: {command}";
pub const IS_OPEN: &str = "{name} is open. Close it and try again.";
pub const CANNOT_TAKE_BACK: &str = "Panpak could not take back the files that {name} was given ({why}). Nothing was removed; try again.";

// Protected mode (an installed version below the floor of its author): see PANPAK-FORMAT.md 3.1.10.
pub const PROTECTED_TITLE: &str = "{name} is running in protected mode";
pub const PROTECTED_BODY: &str = "The author of {name} reported a serious problem in this version. Until you update, it runs without internet. Your files are safe and stay open.";
pub const PROTECTED_LINE: &str = "{name} is in protected mode: no internet. Your files are safe.";
pub const PROTECTED_LINE_NET: &str = "{name} is in protected mode: you turned the internet back on. Your files are safe.";
pub const PROTECTED_WAITS: &str = "Panpak will download the fixed version as soon as it can, and the mode will end by itself.";
pub const LINKS_BLOCKED: &str = "{name} is in protected mode: links do not open until you update.";
pub const UPDATE_NOW: &str = "Update now";
pub const PROTECTED_LIST: &str = "protected mode: no internet and no downloaded programs until updated";
pub const PROTECTED_LIST_NET: &str = "protected mode: you turned the internet back on; no downloaded programs until updated";
pub const PROTECTED_DOCTOR: &str = "{name} ({id}) is in protected mode: the author reported a serious problem in the installed version. It runs without internet and does not run programs that it downloads; your files are safe. The mode ends when the app is updated.";
pub const ASK_UPDATE: &str = "Update {name} now? [Y/n]";
pub const ASK_UPDATE_NEW: &str = "Update {name} now and accept these permissions? [y/N]";
pub const ASK_INTERNET: &str = "Turn the internet back on for {name}? [y/N]";
pub const ASK_INTERNET_OFF: &str = "Turn the internet off again for {name}? [y/N]";
pub const UPDATE_DIALOG_TITLE: &str = "Update {name}";
pub const UPDATE_DIALOG_OK: &str = "Update";
pub const NOT_NOW: &str = "Not now";
pub const INTERNET_DIALOG_TITLE: &str = "Turn the internet back on for {name}?";
pub const INTERNET_RISK: &str = "The author of {name} reported a serious problem in this version. With the internet on, the problem can be used against you, for example to send your files out. This applies only to {name}, it ends when you update, and you can cancel it at any time. Even then, it does not run programs that it downloads.";
pub const INTERNET_ON_LABEL: &str = "Turn the internet on";
pub const INTERNET_OFF_DIALOG_TITLE: &str = "Turn the internet off again for {name}?";
pub const INTERNET_OFF_LABEL: &str = "Turn the internet off";
pub const INTERNET_ON_DONE: &str = "Close and reopen {name} to use the internet.";
pub const INTERNET_ON_NOW: &str = "The internet is back on for {name}, until you update.";
pub const INTERNET_OFF_DONE: &str = "The internet is off again for {name}.";
pub const INTERNET_NOT_NEEDED: &str = "{name} is not in protected mode: there is nothing to turn back on.";
pub const ENDED_TITLE: &str = "{name} is updated";
pub const ENDED_BODY: &str = "The problem is fixed and the internet works again.";
pub const UPDATE_WAITS: &str = "The update of {name} is ready and applies the next time it starts. Close it and open it again.";
pub const UPDATE_FAILED: &str = "{name} was not updated: {why}";
pub const NO_DIALOG_FOR_PERMISSIONS: &str = "{name} asks for new permissions, but there is no window to show them to you. To update, open a terminal and type: panpak update {id}";
pub const NO_DIALOG_FOR_INTERNET: &str = "To turn the internet back on for {name} you must confirm, but there is no window to ask you. Open a terminal and type: panpak allow-internet {id}";
pub const NO_DIALOG_FOR_INTERNET_OFF: &str = "There is no window to ask you. To turn the internet off again for {name}, open a terminal and type: panpak allow-internet --undo {id}";
pub const UPDATES_FROM: &str = "Updates: from {host}, named by the author.";
pub const UPDATES_NONE: &str = "This app does not update itself: for a new version, download the file again from the author.";
pub const DOCTOR_SWITCH: &str = "{name}: after switching to another channel you received a version with the same number as the one you had, but with different content. It is not dangerous, but it is an error of the author: tell them.";
pub const KEY_CHANGED: &str = "{name}: the author changed their signing key in the regular way. You do not need to do anything.";
pub const KEY_RECOVERED: &str = "{name}: the author took control of the app back after losing their key, or after it was stolen. Versions signed with the old key are no longer accepted. You do not need to do anything.";
pub const MENU_INTERNET_ON: &str = "Turn the internet back on…";
pub const MENU_INTERNET_OFF: &str = "Turn the internet off again…";
pub const NEEDS_CONFIRMATION: &str = "Nothing was changed: this needs your answer, and there is no terminal and no dialog to ask it.";

pub const DEVICE_GAMEPAD: &str = "game pads";
pub const DEVICE_CAMERA: &str = "cameras";
pub const HOST_CANNOT_RUN: &str = "{app} cannot run on this system:";
pub const HOST_CANNOT_RUN_SESSION: &str = "{app} cannot run in this session:";
pub const HOST_FORCE_HINT: &str = "To install it anyway, give --force-host (it may not start).";
pub const HOST_LINE: &str = "needs {need}; found {have}. {todo}";
pub const HOST_ARCH_NEED: &str = "a {arch} machine";
pub const HOST_ARCH_HAVE: &str = "a {arch} machine";
pub const HOST_ARCH_TODO: &str = "Get the package for this architecture from the author.";
pub const HOST_GLIBC_NEED: &str = "glibc {version} or newer (it uses symbols of that version)";
pub const HOST_GLIBC_HAVE: &str = "glibc {version}";
pub const HOST_GLIBC_TODO: &str = "It cannot run here: install it on a newer distribution.";
pub const HOST_KERNEL_NEED: &str = "Linux {version} or newer";
pub const HOST_KERNEL_HAVE: &str = "Linux {version}";
pub const HOST_KERNEL_TODO: &str = "Update the kernel of the system, or run the app on a newer one.";
pub const HOST_LANDLOCK_NEED: &str = "Landlock ABI {version} or newer";
pub const HOST_LANDLOCK_HAVE: &str = "Landlock ABI {version}";
pub const HOST_LANDLOCK_NONE: &str = "no Landlock";
pub const HOST_LANDLOCK_TODO: &str = "Use a kernel with that Landlock ABI (Linux 5.13 has ABI 1, 6.7 has 4, 6.12 has 6).";
pub const HOST_DISPLAY_NEED: &str = "the display server {names}";
pub const HOST_DISPLAY_HAVE: &str = "the display server {names}";
pub const HOST_DISPLAY_NONE: &str = "no display server in this session";
pub const HOST_DISPLAY_TODO: &str = "Start it from a graphical session, or give a display to the one that you are in.";
pub const HOST_PORTALS_NEED: &str = "the {names} portal";
pub const HOST_PORTALS_HAVE: &str = "no such portal in this session";
pub const HOST_PORTALS_TODO: &str = "Install xdg-desktop-portal and the portal of your desktop, then start a new session.";
pub const HOST_BUS_NEED: &str = "a session bus and xdg-dbus-proxy (the filtered bus)";
pub const HOST_BUS_HAVE: &str = "no session bus, or no xdg-dbus-proxy";
pub const HOST_BUS_START_NOTE: &str = "{app} starts without a connection to your session bus, so what goes through it will not work (portals, notifications): {line}";
pub const HOST_BUS_TODO: &str = "Install xdg-dbus-proxy and start it from a session that has a user bus.";
pub const HOST_SESSION_NOTE: &str = "the current session does not give what {app} needs: {line}";
pub const HOST_FORCED_NOTE: &str = "(installed with --force-host)";
pub const HOST_FORCED_WARNING: &str = "{app} cannot run on this system (installed anyway because of --force-host):";

// The confirmation of the domain of an author (PANPAK-FORMAT.md 3.2.1).
pub const DOMAIN_PUBLISHER: &str = "Published by:";
pub const DOMAIN_CONFIRMED: &str = "Confirmed: {domain} lists this key for {app} (https://{domain}/.well-known/panpak.toml).";
pub const DOMAIN_CONFIRMED_SUCCESSION: &str = "Confirmed: {domain} lists the new key of {app}.";
pub const DOMAIN_SUCCESSION_NOT_CONFIRMED: &str = "The new key is signed by the one before it, so the update is valid. The author's site does not confirm it, though: if anything looks strange to you, ask the author.";
pub const DOMAIN_INSTALL_ONLY_IF: &str = "Install it only if you trust the place where you got the file.";
pub const DOMAIN_NOT_CONFIRMED: &str = "Not confirmed: {why}. The key of the package is the only proof of who made it.";
pub const DOMAIN_WHY_NO_DOMAIN: &str = "the package names no domain";
pub const DOMAIN_WHY_OFF: &str = "the check is turned off (panpak config set trust.confirm_domain on)";
pub const DOMAIN_WHY_OFFLINE: &str = "this command was run with --offline";
pub const DOMAIN_WHY_NOT_LISTED: &str = "{domain} does not list {app}";
pub const DOMAIN_WHY_UNREACHABLE: &str = "{domain} could not be asked ({detail})";
pub const DOMAIN_WHY_MALFORMED: &str = "the file of {domain} is not valid ({detail})";
pub const DOMAIN_WHY_REDIRECT: &str = "{domain} sent the request to another address";
pub const DOMAIN_WHY_TOO_BIG: &str = "the file of {domain} is too large";
pub const DOMAIN_CONTRADICTED_OTHER: &str = "{domain} says otherwise: it lists other keys for {app}, and this package is signed with none of them. Do not go on unless the author tells you, in another way, that the key changed.";
pub const DOMAIN_CONTRADICTED_REPLACED: &str = "{domain} says otherwise: it lists the key of this package as replaced. Whoever holds that key is no longer the author of {app}.";
pub const DOMAIN_CONTRA_REFUSED_INSTALL: &str = "Nothing was installed. If you know better, run the command again with --despite-domain-mismatch and say whom you trust (--trust-first-key or --expect-key).";
pub const DOMAIN_CONTRA_REFUSED_UPDATE: &str = "The update was not downloaded and the version you have stays.";
pub const DOMAIN_CONTRA_FORCED: &str = "Going on because of --despite-domain-mismatch: the domain says otherwise.";
pub const DOMAIN_PRIVACY: &str = "To confirm who stands behind {domain}, Panpak asks https://{domain}/.well-known/panpak.toml. That server sees your internet address and that someone is installing an app of {domain}; nothing else is sent. To never ask: panpak config set trust.confirm_domain off (or --offline for one command).";
pub const DOMAIN_KEY_HIDDEN: &str = "The key is not shown because {domain} confirms it (--show-key shows it).";

/// Every message above, for the template of the translations and its test.
pub const ALL: &[&str] = &[
    REMOVE_TITLE,
    REMOVE_ASK,
    REMOVE_WHAT,
    REMOVE_KEPT,
    REMOVE_NO_DATA,
    REMOVE_OK_LABEL,
    REMOVE_CANCEL_LABEL,
    REMOVED_TITLE,
    REMOVED,
    REMOVED_KEPT,
    NOT_REMOVED_TITLE,
    NOTHING_REMOVED_TITLE,
    NO_DIALOG_TOOL,
    IS_OPEN,
    CANNOT_TAKE_BACK,
    PROTECTED_TITLE,
    PROTECTED_BODY,
    PROTECTED_LINE,
    PROTECTED_LINE_NET,
    PROTECTED_WAITS,
    LINKS_BLOCKED,
    UPDATE_NOW,
    PROTECTED_LIST,
    PROTECTED_LIST_NET,
    PROTECTED_DOCTOR,
    ASK_UPDATE,
    ASK_UPDATE_NEW,
    ASK_INTERNET,
    ASK_INTERNET_OFF,
    UPDATE_DIALOG_TITLE,
    UPDATE_DIALOG_OK,
    NOT_NOW,
    INTERNET_DIALOG_TITLE,
    INTERNET_RISK,
    INTERNET_ON_LABEL,
    INTERNET_OFF_DIALOG_TITLE,
    INTERNET_OFF_LABEL,
    INTERNET_ON_DONE,
    INTERNET_ON_NOW,
    INTERNET_OFF_DONE,
    INTERNET_NOT_NEEDED,
    ENDED_TITLE,
    ENDED_BODY,
    UPDATE_WAITS,
    UPDATE_FAILED,
    NO_DIALOG_FOR_PERMISSIONS,
    NO_DIALOG_FOR_INTERNET,
    NO_DIALOG_FOR_INTERNET_OFF,
    UPDATES_FROM,
    UPDATES_NONE,
    DOCTOR_SWITCH,
    KEY_CHANGED,
    KEY_RECOVERED,
    MENU_INTERNET_ON,
    MENU_INTERNET_OFF,
    NEEDS_CONFIRMATION,
    DEVICE_GAMEPAD,
    DEVICE_CAMERA,
    HOST_CANNOT_RUN,
    HOST_CANNOT_RUN_SESSION,
    HOST_FORCE_HINT,
    HOST_LINE,
    HOST_ARCH_NEED,
    HOST_ARCH_HAVE,
    HOST_ARCH_TODO,
    HOST_GLIBC_NEED,
    HOST_GLIBC_HAVE,
    HOST_GLIBC_TODO,
    HOST_KERNEL_NEED,
    HOST_KERNEL_HAVE,
    HOST_KERNEL_TODO,
    HOST_LANDLOCK_NEED,
    HOST_LANDLOCK_HAVE,
    HOST_LANDLOCK_NONE,
    HOST_LANDLOCK_TODO,
    HOST_DISPLAY_NEED,
    HOST_DISPLAY_HAVE,
    HOST_DISPLAY_NONE,
    HOST_DISPLAY_TODO,
    HOST_PORTALS_NEED,
    HOST_PORTALS_HAVE,
    HOST_PORTALS_TODO,
    HOST_BUS_NEED,
    HOST_BUS_HAVE,
    HOST_BUS_TODO,
    HOST_BUS_START_NOTE,
    HOST_SESSION_NOTE,
    HOST_FORCED_NOTE,
    HOST_FORCED_WARNING,
    DOMAIN_PUBLISHER,
    DOMAIN_CONFIRMED,
    DOMAIN_CONFIRMED_SUCCESSION,
    DOMAIN_SUCCESSION_NOT_CONFIRMED,
    DOMAIN_INSTALL_ONLY_IF,
    DOMAIN_NOT_CONFIRMED,
    DOMAIN_WHY_NO_DOMAIN,
    DOMAIN_WHY_OFF,
    DOMAIN_WHY_OFFLINE,
    DOMAIN_WHY_NOT_LISTED,
    DOMAIN_WHY_UNREACHABLE,
    DOMAIN_WHY_MALFORMED,
    DOMAIN_WHY_REDIRECT,
    DOMAIN_WHY_TOO_BIG,
    DOMAIN_CONTRADICTED_OTHER,
    DOMAIN_CONTRADICTED_REPLACED,
    DOMAIN_CONTRA_REFUSED_INSTALL,
    DOMAIN_CONTRA_REFUSED_UPDATE,
    DOMAIN_CONTRA_FORCED,
    DOMAIN_PRIVACY,
    DOMAIN_KEY_HIDDEN,
];
