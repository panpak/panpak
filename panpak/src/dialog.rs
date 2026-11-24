// SPDX-License-Identifier: MIT OR Apache-2.0
//! A question in a window, with `zenity` or `kdialog` (the one of the desktop first). The menu action "Uninstall" and protected mode ask this way, so that
//! a person answers where they are: in a window, not in a command. This only shows the question; what the answer does is the caller's.
//!
//! When neither tool is here, the question is a notification of the portal with two buttons. The process waits for a button up to `ASK_WAIT`, then withdraws the
//! notification (a button that nobody listens to must not stay on the screen) and says `Unanswered`: the caller keeps the question for later or drops it.

use crate::remove::{clean, find_tool, markup_safe};
use std::process::{Command, Stdio};

/// What came of the question.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    /// Neither tool is here, and no notification could be shown (no session bus or no portal): nothing was asked.
    NoTool,
    /// The question was shown as a notification and no button was pressed in time. The notification is withdrawn.
    Unanswered,
}

/// How long the notification waits for a button.
const ASK_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

/// The wait, which a debug build can shorten for the tests (`PANPAK_TEST_ASK_SECS`).
fn ask_wait() -> std::time::Duration {
    #[cfg(debug_assertions)]
    if let Some(s) = std::env::var("PANPAK_TEST_ASK_SECS").ok().and_then(|v| v.parse::<f64>().ok()) {
        return std::time::Duration::from_secs_f64(s);
    }
    ASK_WAIT
}

/// The arguments of `org.freedesktop.portal.Notification.AddNotification (s id, a{sv} notification)` for a question with two buttons. A click on the body
/// is no answer: there is no default action.
pub fn question_args(id: &str, title: &str, text: &str, ok: &str, cancel: &str) -> Vec<panpak_core::dbus::Value> {
    use panpak_core::dbus::Value;
    let entry = |k: &str, v: Value| Value::Entry(Box::new(Value::str(k)), Box::new(Value::variant(v)));
    let button = |label: &str, action: &str| Value::Array("{sv}".into(), vec![entry("label", Value::Str(clean(label))), entry("action", Value::str(action))]);
    vec![
        Value::str(id),
        Value::Array(
            "{sv}".into(),
            vec![
                entry("title", Value::Str(clean(title))),
                entry("body", Value::Str(lines(text, clean))),
                entry("buttons", Value::Array("a{sv}".into(), vec![button(ok, "yes"), button(cancel, "no")])),
            ],
        ),
    ]
}

/// The question as a notification of the portal. `NoTool` when there is no session bus or the portal does not take it.
fn ask_by_notification(title: &str, text: &str, ok: &str, cancel: &str) -> Answer {
    use panpak_core::dbus::{Connection, Value};
    let Ok(mut conn) = Connection::session(std::time::Duration::from_secs(3)) else { return Answer::NoTool };
    let id = format!("panpak-ask-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    let rule = "type='signal',interface='org.freedesktop.portal.Notification',member='ActionInvoked'";
    let _ = conn.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", vec![Value::str(rule)], vec![]);
    let portal = |conn: &mut Connection, member: &str, args: Vec<Value>| {
        conn.call("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Notification", member, args, vec![])
    };
    if portal(&mut conn, "AddNotification", question_args(&id, title, text, ok, cancel)).is_err() {
        return Answer::NoTool;
    }
    let pressed = conn.wait_signal(ask_wait(), |m| {
        m.interface.as_deref() == Some("org.freedesktop.portal.Notification")
            && m.member.as_deref() == Some("ActionInvoked")
            && matches!(m.body.as_slice(), [Value::Str(i), Value::Str(a), ..] if *i == id && (a == "yes" || a == "no"))
    });
    // Withdrawn in every case: after a button the desktop closes it, after the wait nothing must be left to press.
    let _ = portal(&mut conn, "RemoveNotification", vec![Value::str(&id)]);
    match pressed {
        Ok(Some(m)) if matches!(m.body.get(1), Some(Value::Str(a)) if a == "yes") => Answer::Yes,
        Ok(Some(_)) => Answer::No,
        _ => Answer::Unanswered,
    }
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
    let Some((tool, path)) = order.iter().find_map(|t| find_tool(t).map(|p| (*t, p))) else { return ask_by_notification(title, text, ok, cancel) };
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

#[cfg(test)]
mod tests {
    use super::*;
    use panpak_core::dbus::Value;

    #[test]
    fn the_notification_has_two_buttons_and_no_default_action() {
        let a = question_args("id1", "Title\u{7}", "Line one\nLine two", "Yes", "No");
        assert_eq!(a[0], Value::str("id1"));
        let Value::Array(sig, entries) = &a[1] else { panic!("not a dictionary") };
        assert_eq!(sig, "{sv}");
        let keys: Vec<String> = entries.iter().map(|e| if let Value::Entry(k, _) = e { format!("{:?}", k) } else { String::new() }).collect();
        assert_eq!(keys.len(), 3);
        assert!(!keys.iter().any(|k| k.contains("default-action")));
        let text = format!("{:?}", a[1]);
        assert!(text.contains("Title?") && text.contains("Line one\\nLine two"), "{text}");
        assert!(text.contains("\"yes\"") && text.contains("\"no\""));
    }
}
