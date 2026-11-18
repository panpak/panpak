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
pub const PROTECTED_BODY: &str = "The author of {name} reported a serious problem in this version. Until you update, it runs without internet and does not run programs that it downloads. Your files are safe and stay open.";
pub const PROTECTED_LINE: &str = "{name} is in protected mode: no internet, no downloaded programs. Your files are safe.";
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
pub const INTERNET_RISK: &str = "The author of {name} reported a serious problem in this version. With the internet on, the problem can be used against you, for example to send your files out. This applies only to {name}, it ends when you update, and you can cancel it at any time.";
pub const INTERNET_ON_LABEL: &str = "Turn the internet on";
pub const INTERNET_OFF_DIALOG_TITLE: &str = "Turn the internet off again for {name}?";
pub const INTERNET_OFF_LABEL: &str = "Turn the internet off";
pub const INTERNET_ON_DONE: &str = "The internet is back on for {name} from its next start, until you update.";
pub const INTERNET_ON_NOW: &str = "The internet is back on for {name}, until you update.";
pub const INTERNET_OFF_DONE: &str = "The internet is off again for {name}.";
pub const INTERNET_NOT_NEEDED: &str = "{name} is not in protected mode: there is nothing to turn back on.";
pub const ENDED_TITLE: &str = "{name}: update done";
pub const ENDED_BODY: &str = "{name} now runs a version that fixes the problem. Protected mode is over: the internet and downloaded programs work again.";
pub const UPDATE_WAITS: &str = "The update of {name} is ready and applies the next time it starts. Close it and open it again.";
pub const UPDATE_FAILED: &str = "{name} was not updated: {why}";
pub const NO_DIALOG_FOR_PERMISSIONS: &str = "{name} asks for new permissions, and nothing here can show them to you (a dialog tool such as zenity or kdialog is missing).";
pub const MENU_INTERNET_ON: &str = "Turn the internet back on…";
pub const MENU_INTERNET_OFF: &str = "Turn the internet off again…";
pub const NEEDS_CONFIRMATION: &str = "Nothing was changed: this needs your answer, and there is no terminal and no dialog to ask it.";

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
    MENU_INTERNET_ON,
    MENU_INTERNET_OFF,
    NEEDS_CONFIRMATION,
];
