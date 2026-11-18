// SPDX-License-Identifier: MIT OR Apache-2.0
//! A question in a window, with `zenity` or `kdialog` (the one of the desktop first). The menu action "Uninstall" and protected mode ask this way, so that
//! a person answers where they are: in a window, not in a command. This only shows the question; what the answer does is the caller's.

use crate::remove::{clean, find_tool, markup_safe};
use std::process::{Command, Stdio};

/// What came of the question.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    /// Neither tool is here: nothing was asked.
    NoTool,
}

/// The text with every line cleaned (a control character other than the line break becomes `?`), by `f`.
fn lines(text: &str, f: impl Fn(&str) -> String) -> String {
    text.split('\n').map(f).collect::<Vec<_>>().join("\n")
}

/// Ask `text` (plain text; line breaks are kept) with a window titled `title`, with the two buttons `ok` and `cancel`. Nothing the caller passes is read as
/// markup.
pub fn ask(title: &str, text: &str, ok: &str, cancel: &str) -> Answer {
    let kde = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_uppercase().contains("KDE"));
    let order: [&str; 2] = if kde { ["kdialog", "zenity"] } else { ["zenity", "kdialog"] };
    let Some((tool, path)) = order.iter().find_map(|t| find_tool(t).map(|p| (*t, p))) else { return Answer::NoTool };
    let mut cmd = Command::new(&path);
    if tool == "zenity" {
        cmd.args(["--question", "--no-markup", "--title", title, "--text", &lines(text, clean), "--ok-label", ok, "--cancel-label", cancel, "--width", "460"]);
    } else {
        cmd.args(["--title", title, "--warningyesno", &lines(text, markup_safe).replace('\n', "<br>"), "--yes-label", ok, "--no-label", cancel]);
    }
    if cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success()) {
        Answer::Yes
    } else {
        Answer::No
    }
}
