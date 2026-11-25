// SPDX-License-Identifier: MIT OR Apache-2.0
//! A question in a window, with `zenity` or `kdialog` (the one of the desktop first). The menu action "Uninstall" and protected mode ask this way, so that
//! a person answers where they are: in a window, not in a command. This only shows the question; what the answer does is the caller's.
//!
//! When neither tool is here, the question is a notification with two buttons: of the notification service of the desktop (`org.freedesktop.Notifications`) first,
//! and of the notification portal when the service is not there. The process waits for a button up to `ASK_WAIT`, then withdraws the notification (a button that
//! nobody listens to must not stay on the screen) and says `Unanswered`: the caller keeps the question for later or drops it.
//!
//! Why the service first: a process started from the menu of GNOME runs in the cgroup of the shell, in no `app-...scope`, and the portal cannot tell which app it is
//! (`InvalidApp`), so the question was never shown. The service takes any caller. Its answer signals are broadcasts with a number that anybody can guess, so only the
//! signals sent by the owner of the name of the service count.

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

const FDO: &str = "org.freedesktop.Notifications";

/// The arguments of `org.freedesktop.Notifications.Notify (app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout)` for a question with two buttons.
/// The actions are the two buttons only: a click on the body is no answer (there is no `default` action) and there is no `desktop-entry` hint, so that the shell does not
/// start the app of that name. The body is read as markup by some servers: the characters that start it are written as entities. Never expires by itself (0): the process
/// that asks withdraws it.
pub fn notify_args(title: &str, text: &str, ok: &str, cancel: &str) -> Vec<panpak_core::dbus::Value> {
    use panpak_core::dbus::Value;
    vec![
        Value::str("Panpak"),
        Value::U32(0),
        Value::str("dialog-question-symbolic"),
        Value::Str(clean(title)),
        Value::Str(lines(text, markup_safe)),
        Value::Array("s".into(), vec![Value::str("yes"), Value::Str(clean(ok)), Value::str("no"), Value::Str(clean(cancel))]),
        Value::Array("{sv}".into(), Vec::new()),
        Value::I32(0),
    ]
}

/// What a signal of the notification service says about the question `id`, if it comes from `owner` (the unique name that owns the service).
#[derive(Debug, PartialEq, Eq)]
pub enum Signal {
    Yes,
    No,
    /// The notification was closed without a button: it expired (1) or the person dismissed it (2). Nothing was answered.
    Closed,
}

/// The meaning of one message: `Some` only for an `ActionInvoked` with our two keys or a `NotificationClosed` for `id`, sent by `owner`.
pub fn read_signal(m: &panpak_core::dbus::Message, owner: &str, id: u32) -> Option<Signal> {
    use panpak_core::dbus::Value;
    if m.sender.as_deref() != Some(owner) || m.interface.as_deref() != Some(FDO) {
        return None;
    }
    match (m.member.as_deref(), m.body.as_slice()) {
        (Some("ActionInvoked"), [Value::U32(i), Value::Str(a), ..]) if *i == id => match a.as_str() {
            "yes" => Some(Signal::Yes),
            "no" => Some(Signal::No),
            _ => None,
        },
        (Some("NotificationClosed"), [Value::U32(i), Value::U32(reason), ..]) if *i == id && (*reason == 1 || *reason == 2) => Some(Signal::Closed),
        _ => None,
    }
}

/// The question as a notification of the desktop's notification service. `None` when the service is not there or does not take it (the portal is tried then).
fn ask_by_service(title: &str, text: &str, ok: &str, cancel: &str) -> Option<Answer> {
    use panpak_core::dbus::{Connection, Value};
    let mut conn = Connection::session(std::time::Duration::from_secs(3)).ok()?;
    // The owner of the name first: after `Notify` nothing else is called before the wait, because a call drops the signals that arrive during it.
    let owner = match conn.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner", vec![Value::str(FDO)], vec![]).ok()?.body.as_slice() {
        [Value::Str(n)] => n.clone(),
        _ => return None,
    };
    for member in ["ActionInvoked", "NotificationClosed"] {
        let rule = format!("type='signal',sender='{FDO}',interface='{FDO}',member='{member}'");
        let _ = conn.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", vec![Value::str(&rule)], vec![]);
    }
    let id = match conn.call(&owner, "/org/freedesktop/Notifications", FDO, "Notify", notify_args(title, text, ok, cancel), vec![]).ok()?.body.as_slice() {
        [Value::U32(i)] => *i,
        _ => return None,
    };
    let mut got = None;
    let waited = conn.wait_signal(ask_wait(), |m| {
        got = read_signal(m, &owner, id);
        got.is_some()
    });
    // Withdrawn in every case: after a button the desktop closes it, after the wait or a dismissal there is nothing left to press (a notification that is already closed is ignored).
    let _ = conn.call(&owner, "/org/freedesktop/Notifications", FDO, "CloseNotification", vec![Value::U32(id)], vec![]);
    Some(match (waited, got) {
        (Ok(Some(_)), Some(Signal::Yes)) => Answer::Yes,
        (Ok(Some(_)), Some(Signal::No)) => Answer::No,
        _ => Answer::Unanswered,
    })
}

/// The question as a notification: of the notification service of the desktop, and of the portal when the service is not there. `NoTool` when there is no session bus or
/// neither takes it.
fn ask_by_notification(title: &str, text: &str, ok: &str, cancel: &str) -> Answer {
    ask_by_service(title, text, ok, cancel).unwrap_or_else(|| ask_by_portal(title, text, ok, cancel))
}

/// The question as a notification of the portal. `NoTool` when there is no session bus or the portal does not take it.
fn ask_by_portal(title: &str, text: &str, ok: &str, cancel: &str) -> Answer {
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

    fn signal(sender: &str, member: &str, body: Vec<Value>) -> panpak_core::dbus::Message {
        panpak_core::dbus::Message { kind: panpak_core::dbus::SIGNAL, sender: Some(sender.into()), interface: Some(FDO.into()), member: Some(member.into()), body, ..Default::default() }
    }

    #[test]
    fn the_service_gets_two_buttons_no_default_action_no_desktop_entry_and_text_that_is_not_markup() {
        let a = notify_args("Title\u{7}", "Use <b>this</b> & that\nLine two", "Yes", "No");
        assert_eq!(a.len(), 8);
        assert_eq!(a[0], Value::str("Panpak"));
        assert_eq!(a[3], Value::str("Title?"));
        assert_eq!(a[4], Value::str("Use &lt;b&gt;this&lt;/b&gt; &amp; that\nLine two"));
        // the actions: the two buttons only, in pairs of key and label; a click on the body is no answer
        assert_eq!(a[5], Value::Array("s".into(), vec![Value::str("yes"), Value::str("Yes"), Value::str("no"), Value::str("No")]));
        assert!(!format!("{:?}", a[5]).contains("default"));
        // no hints at all (so no `desktop-entry`: the shell must not start an app of that name on a click), and no expiry by itself
        assert_eq!(a[6], Value::Array("{sv}".into(), Vec::new()));
        assert_eq!(a[7], Value::I32(0));
    }

    #[test]
    fn only_the_buttons_and_the_closing_of_our_notification_from_the_service_are_an_answer() {
        let owner = ":1.28";
        let act = |id: u32, key: &str| vec![Value::U32(id), Value::str(key)];
        let closed = |id: u32, why: u32| vec![Value::U32(id), Value::U32(why)];
        assert_eq!(read_signal(&signal(owner, "ActionInvoked", act(7, "yes")), owner, 7), Some(Signal::Yes));
        assert_eq!(read_signal(&signal(owner, "ActionInvoked", act(7, "no")), owner, 7), Some(Signal::No));
        // another notification, another key
        assert_eq!(read_signal(&signal(owner, "ActionInvoked", act(8, "yes")), owner, 7), None);
        assert_eq!(read_signal(&signal(owner, "ActionInvoked", act(7, "default")), owner, 7), None);
        // a signal that another client sends with the number of ours is not an answer: only the owner of the name counts
        assert_eq!(read_signal(&signal(":1.99", "ActionInvoked", act(7, "yes")), owner, 7), None);
        assert_eq!(read_signal(&signal(":1.99", "NotificationClosed", closed(7, 2)), owner, 7), None);
        // closed: expired (1) or dismissed (2) is "nothing answered, nothing to wait for"; the call to CloseNotification (3) and the others are not
        assert_eq!(read_signal(&signal(owner, "NotificationClosed", closed(7, 2)), owner, 7), Some(Signal::Closed));
        assert_eq!(read_signal(&signal(owner, "NotificationClosed", closed(7, 1)), owner, 7), Some(Signal::Closed));
        assert_eq!(read_signal(&signal(owner, "NotificationClosed", closed(7, 3)), owner, 7), None);
        assert_eq!(read_signal(&signal(owner, "NotificationClosed", closed(8, 2)), owner, 7), None);
        // a message without a sender, or of another interface
        let mut m = signal(owner, "ActionInvoked", act(7, "yes"));
        m.sender = None;
        assert_eq!(read_signal(&m, owner, 7), None);
        let mut m = signal(owner, "ActionInvoked", act(7, "yes"));
        m.interface = Some("org.freedesktop.portal.Notification".into());
        assert_eq!(read_signal(&m, owner, 7), None);
    }
}
