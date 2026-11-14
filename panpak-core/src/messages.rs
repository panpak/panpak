// SPDX-License-Identifier: MIT OR Apache-2.0
//! The messages of `panpak remove` that a person reads in a window or a notification (the menu action "Uninstall"). The ids are the English sentences; a catalog
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
];
