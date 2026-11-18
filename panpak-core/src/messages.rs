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
pub const UPDATE_NOW: &str = "Update now";
pub const PROTECTED_HOW: &str = "To update now, run: {command}";
pub const PROTECTED_LIST: &str = "protected mode: no internet and no downloaded programs until updated";
pub const PROTECTED_LIST_NET: &str = "protected mode: you turned the internet back on; no downloaded programs until updated";
pub const PROTECTED_DOCTOR: &str = "{name} ({id}) is in protected mode: the author reported a serious problem in the installed version. It runs without internet and does not run programs that it downloads; your files are safe. To update: {command}";
pub const INTERNET_ASK: &str = "Turn the internet back on for {name}?";
pub const INTERNET_RISK: &str = "The author of {name} reported a serious problem in this version. With the internet on, the problem can be used against you, for example to send your files out. This applies only to {name}, it ends when you update, and you can cancel it at any time.";
pub const INTERNET_ON_DONE: &str = "The internet is back on for {name} from its next start, until you update. To cancel: {command}";
pub const INTERNET_OFF_DONE: &str = "The internet is off again for {name}.";
pub const INTERNET_NOT_NEEDED: &str = "{name} is not in protected mode: there is nothing to turn back on.";
pub const ENDED_TITLE: &str = "{name}: update done";
pub const ENDED_BODY: &str = "{name} now runs a version that fixes the problem. Protected mode is over: the internet and downloaded programs work again.";
pub const UPDATE_NEEDS_OK: &str = "{name} asks for new permissions. To read and accept them, run: {command}";
pub const UPDATE_WAITS: &str = "The update of {name} is ready and applies the next time it starts. Close it and open it again.";
pub const UPDATE_FAILED: &str = "{name} was not updated: {why}";

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
    UPDATE_NOW,
    PROTECTED_HOW,
    PROTECTED_LIST,
    PROTECTED_LIST_NET,
    PROTECTED_DOCTOR,
    INTERNET_ASK,
    INTERNET_RISK,
    INTERNET_ON_DONE,
    INTERNET_OFF_DONE,
    INTERNET_NOT_NEEDED,
    ENDED_TITLE,
    ENDED_BODY,
    UPDATE_NEEDS_OK,
    UPDATE_WAITS,
    UPDATE_FAILED,
];
