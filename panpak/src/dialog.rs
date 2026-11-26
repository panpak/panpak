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
//!
//! A click on the body of the notification is no answer. The notification has a `default` action that nothing listens to and the hint `resident`, so a click on the
//! body neither answers nor withdraws it, and nothing is started (measured on GNOME 51: without `default` the click closes it). Only the process that asks
//! withdraws it. The portal is used only when the process is in a scope that names an app (`app-...scope`): the portal takes the app from that scope and
//! refuses the call in silence otherwise (`InvalidApp` in the journal, `ok` to the caller, nothing on the screen), so out of such a scope the portal is not asked.

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

/// How long the notification of the service waits for a button: a person who started "Uninstall" may leave for a moment.
const ASK_WAIT: std::time::Duration = std::time::Duration::from_secs(300);
/// The wait on the portal: its notification can be refused later without a sign, so a question that nobody answers is not held for long.
const ASK_WAIT_PORTAL: std::time::Duration = std::time::Duration::from_secs(60);

/// The wait, which a debug build can shorten for the tests (`PANPAK_TEST_ASK_SECS`).
fn ask_wait(portal: bool) -> std::time::Duration {
    #[cfg(debug_assertions)]
    if let Some(s) = std::env::var("PANPAK_TEST_ASK_SECS").ok().and_then(|v| v.parse::<f64>().ok()) {
        return std::time::Duration::from_secs_f64(s);
    }
    if portal {
        ASK_WAIT_PORTAL
    } else {
        ASK_WAIT
    }
}

/// Whether the portal can tell which app a process is: the last part of its cgroup is a scope or a service of an app (`app-...scope`, `app-...service`). A process that
/// the shell started from the menu may be in the service of the shell instead, and the portal refuses it in silence.
pub fn portal_knows_app(cgroup: &str) -> bool {
    cgroup.lines().any(|l| {
        let last = l.splitn(3, ':').nth(2).unwrap_or("").rsplit('/').next().unwrap_or("");
        last.starts_with("app-") && (last.ends_with(".scope") || last.ends_with(".service"))
    })
}

fn portal_knows_us() -> bool {
    #[cfg(debug_assertions)]
    if let Ok(c) = std::env::var("PANPAK_TEST_CGROUP") {
        return portal_knows_app(&c);
    }
    portal_knows_app(&std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default())
}

/// The arguments of `org.freedesktop.portal.Notification.AddNotification (s id, a{sv} notification)` for a question with two buttons. A click on the body
/// is no answer: its default action is a name that nothing listens to (without one the shell starts the app that the portal names).
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
                entry("default-action", Value::str("ignore")),
                entry("buttons", Value::Array("a{sv}".into(), vec![button(ok, "yes"), button(cancel, "no")])),
            ],
        ),
    ]
}

const FDO: &str = "org.freedesktop.Notifications";

/// The arguments of `org.freedesktop.Notifications.Notify (app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout)` for a question with two buttons.
/// The actions are the two buttons and a `default` action that is no answer (it is what a click on the body sends; our reader ignores it), and the hint `resident` keeps
/// the notification after such a click. There is no `desktop-entry` hint, so that the shell does not start the app of that name. The body is read as markup by some servers: the characters that start it are written as entities. Never expires by itself (0):
/// the process that asks withdraws it.
pub fn notify_args(title: &str, text: &str, ok: &str, cancel: &str) -> Vec<panpak_core::dbus::Value> {
    use panpak_core::dbus::Value;
    vec![
        Value::str("Panpak"),
        Value::U32(0),
        Value::str("dialog-question-symbolic"),
        Value::Str(clean(title)),
        Value::Str(lines(text, markup_safe)),
        Value::Array("s".into(), vec![Value::str("default"), Value::str(""), Value::str("yes"), Value::Str(clean(ok)), Value::str("no"), Value::Str(clean(cancel))]),
        Value::Array("{sv}".into(), vec![Value::Entry(Box::new(Value::str("resident")), Box::new(Value::variant(Value::Bool(true))))]),
        Value::I32(0),
    ]
}

/// The arguments of `Notify` for the notice of an app that did not start: a warning with the reason as the body, an optional button (`details`), and the hint `transient`:
/// it shows as a banner and the desktop does not keep it in the list of notifications. The body is read as markup by some servers, so the characters that start it are written
/// as entities. The server chooses how long the banner stays (-1).
pub(crate) fn notice_args(title: &str, body: &str, button: Option<&str>) -> Vec<panpak_core::dbus::Value> {
    use panpak_core::dbus::Value;
    let actions = match button {
        Some(label) => vec![Value::str("details"), Value::Str(clean(label))],
        None => Vec::new(),
    };
    vec![
        Value::str("Panpak"),
        Value::U32(0),
        Value::str("dialog-warning-symbolic"),
        Value::Str(clean(title)),
        Value::Str(lines(body, markup_safe)),
        Value::Array("s".into(), actions),
        Value::Array("{sv}".into(), vec![Value::Entry(Box::new(Value::str("transient")), Box::new(Value::variant(Value::Bool(true))))]),
        Value::I32(-1),
    ]
}

/// Show that notice through the notification service of the desktop and wait up to `wait` for the button. `true` when the button was pressed (and only by the service
/// itself, for our number). With no service nothing is shown (`false`): the log stays on disk. The button is offered only when the service lists `actions` and the answer can be heard.
pub(crate) fn notice(title: &str, body: &str, button: Option<&str>, wait: std::time::Duration) -> bool {
    use panpak_core::dbus::{Connection, Value};
    let Ok(mut conn) = Connection::session(std::time::Duration::from_secs(3)) else { return false };
    let Some(owner) = notification_owner(&mut conn) else { return false };
    let caps = conn.call(&owner, "/org/freedesktop/Notifications", FDO, "GetCapabilities", vec![], vec![]).ok();
    let button = button.filter(|_| caps.is_some_and(|c| shows_buttons(&c.body)) && subscribe(&mut conn, &["ActionInvoked"]));
    let id = match conn.call(&owner, "/org/freedesktop/Notifications", FDO, "Notify", notice_args(title, body, button), vec![]) {
        Ok(r) => match r.body.as_slice() {
            [Value::U32(i)] => *i,
            _ => return false,
        },
        Err(_) => return false,
    };
    if button.is_none() {
        return false;
    }
    let pressed = matches!(conn.wait_signal(wait, |m| action_of(m, &owner, id).as_deref() == Some("details")), Ok(Some(_)));
    let _ = conn.call(&owner, "/org/freedesktop/Notifications", FDO, "CloseNotification", vec![Value::U32(id)], vec![]);
    pressed
}

/// What a signal of the notification service says about the question `id`, if it comes from `owner` (the unique name that owns the service).
#[derive(Debug, PartialEq, Eq)]
pub enum Signal {
    Yes,
    No,
    /// The notification was closed without a button: it expired (1) or the person dismissed it (2). Nothing was answered.
    Closed,
}

/// The key of an `ActionInvoked` of the notification service for the notification `id`, if `owner` (the unique name that owns the service) sent it. `None` for anything
/// else: another sender, another interface or member, another number, another shape of body. These signals are broadcasts and the number is small, so the sender and the
/// number are what make a signal an answer.
pub(crate) fn action_of(m: &panpak_core::dbus::Message, owner: &str, id: u32) -> Option<String> {
    use panpak_core::dbus::Value;
    if m.sender.as_deref() != Some(owner) || m.interface.as_deref() != Some(FDO) || m.member.as_deref() != Some("ActionInvoked") {
        return None;
    }
    match m.body.as_slice() {
        [Value::U32(i), Value::Str(a), ..] if *i == id => Some(a.clone()),
        _ => None,
    }
}

/// The meaning of one message: `Some` only for an `ActionInvoked` with our two keys or a `NotificationClosed` for `id`, sent by `owner`.
pub fn read_signal(m: &panpak_core::dbus::Message, owner: &str, id: u32) -> Option<Signal> {
    use panpak_core::dbus::Value;
    if let Some(a) = action_of(m, owner, id) {
        return match a.as_str() {
            "yes" => Some(Signal::Yes),
            "no" => Some(Signal::No),
            _ => None,
        };
    }
    if m.sender.as_deref() != Some(owner) || m.interface.as_deref() != Some(FDO) {
        return None;
    }
    match (m.member.as_deref(), m.body.as_slice()) {
        (Some("NotificationClosed"), [Value::U32(i), Value::U32(reason), ..]) if *i == id && (*reason == 1 || *reason == 2) => Some(Signal::Closed),
        _ => None,
    }
}

/// The unique name that owns the notification service, or `None` when nobody does (or the bus does not say).
pub(crate) fn notification_owner(conn: &mut panpak_core::dbus::Connection) -> Option<String> {
    use panpak_core::dbus::Value;
    match conn.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner", vec![Value::str(FDO)], vec![]).ok()?.body.as_slice() {
        [Value::Str(n)] => Some(n.clone()),
        _ => None,
    }
}

/// Ask the bus for the signals `members` of the notification service, and only from it. `false` when one subscription fails: without it the press would never arrive.
pub(crate) fn subscribe(conn: &mut panpak_core::dbus::Connection, members: &[&str]) -> bool {
    use panpak_core::dbus::Value;
    members.iter().all(|member| {
        let rule = format!("type='signal',sender='{FDO}',interface='{FDO}',member='{member}'");
        conn.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", vec![Value::str(&rule)], vec![]).is_ok()
    })
}

/// The question as a notification of the desktop's notification service. `None` when the service is not there or does not take it (the portal is tried then).
/// Whether the reply of `GetCapabilities` lists "actions", the buttons.
fn shows_buttons(body: &[panpak_core::dbus::Value]) -> bool {
    use panpak_core::dbus::Value;
    matches!(body, [Value::Array(_, caps)] if caps.iter().any(|c| matches!(c, Value::Str(s) if s == "actions")))
}

fn ask_by_service(title: &str, text: &str, ok: &str, cancel: &str) -> Option<Answer> {
    use panpak_core::dbus::{Connection, Value};
    // A debug build can skip the service, to try the portal on a desktop that has one (`PANPAK_TEST_NO_SERVICE`).
    #[cfg(debug_assertions)]
    if std::env::var_os("PANPAK_TEST_NO_SERVICE").is_some() {
        return None;
    }
    let mut conn = Connection::session(std::time::Duration::from_secs(3)).ok()?;
    // The owner of the name first: its name is what an answer has to come from.
    let owner = notification_owner(&mut conn)?;
    // A service that does not list "actions" shows no buttons: the portal is the way then.
    let caps = conn.call(&owner, "/org/freedesktop/Notifications", FDO, "GetCapabilities", vec![], vec![]).ok()?;
    if !shows_buttons(&caps.body) {
        return None;
    }
    // Without both subscriptions the press would never arrive: the portal is the way then.
    if !subscribe(&mut conn, &["ActionInvoked", "NotificationClosed"]) {
        return None;
    }
    let id = match conn.call(&owner, "/org/freedesktop/Notifications", FDO, "Notify", notify_args(title, text, ok, cancel), vec![]).ok()?.body.as_slice() {
        [Value::U32(i)] => *i,
        _ => return None,
    };
    let mut got = None;
    let waited = conn.wait_signal(ask_wait(false), |m| {
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
/// neither takes it (the portal is not asked from a process that is in no scope of an app).
fn ask_by_notification(title: &str, text: &str, ok: &str, cancel: &str) -> Answer {
    match ask_by_service(title, text, ok, cancel) {
        Some(a) => a,
        None => ask_by_portal(title, text, ok, cancel),
    }
}

/// The question as a notification of the portal. `NoTool` when there is no session bus or the portal does not take it.
fn ask_by_portal(title: &str, text: &str, ok: &str, cancel: &str) -> Answer {
    use panpak_core::dbus::{Connection, Value};
    if !portal_knows_us() {
        return Answer::NoTool;
    }
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
    let pressed = conn.wait_signal(ask_wait(true), |m| {
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
    fn the_service_must_list_actions() {
        let caps = |l: &[&str]| vec![Value::Array("s".into(), l.iter().map(|c| Value::str(c)).collect())];
        assert!(shows_buttons(&caps(&["body", "actions"])));
        assert!(!shows_buttons(&caps(&["body", "persistence"])));
        assert!(!shows_buttons(&caps(&[])));
        assert!(!shows_buttons(&[]));
        assert!(!shows_buttons(&[Value::str("actions")]));
    }

    #[test]
    fn the_notification_of_the_portal_has_two_buttons_and_a_default_action_that_is_no_answer() {
        let a = question_args("id1", "Title\u{7}", "Line one\nLine two", "Yes", "No");
        assert_eq!(a[0], Value::str("id1"));
        let Value::Array(sig, entries) = &a[1] else { panic!("not a dictionary") };
        assert_eq!(sig, "{sv}");
        let keys: Vec<String> = entries.iter().map(|e| if let Value::Entry(k, _) = e { format!("{:?}", k) } else { String::new() }).collect();
        assert_eq!(keys.len(), 4);
        // a default action that no code listens to: a click on the body is no answer and starts nothing
        let text = format!("{:?}", a[1]);
        assert!(text.contains("default-action") && text.contains("\"ignore\""), "{text}");
        let text = format!("{:?}", a[1]);
        assert!(text.contains("Title?") && text.contains("Line one\\nLine two"), "{text}");
        assert!(text.contains("\"yes\"") && text.contains("\"no\""));
    }

    #[test]
    fn the_portal_is_used_only_in_a_scope_of_an_app() {
        assert!(portal_knows_app("0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-panpak.org.example.keptapp-3563.scope\n"));
        assert!(portal_knows_app("0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-org.gnome.Foo@autostart.service\n"));
        // the case of 2026-11-26: a menu entry in the service of the shell, and a login session
        assert!(!portal_knows_app("0::/user.slice/user-1000.slice/user@1000.service/session.slice/org.gnome.Shell@user.service\n"));
        assert!(!portal_knows_app("0::/user.slice/user-1000.slice/session-9.scope\n"));
        // a scope of ours is no app scope; the name of a folder above is not the last part
        assert!(!portal_knows_app("0::/user.slice/user-1000.slice/user@1000.service/app.slice/panpak-remove-org.example.keptapp-8679.scope\n"));
        assert!(!portal_knows_app("0::/app-x.scope/other.scope\n"));
        assert!(!portal_knows_app(""));
    }

    #[test]
    fn the_wait_is_five_minutes_on_the_service_and_one_on_the_portal() {
        if std::env::var("PANPAK_TEST_ASK_SECS").is_err() {
            assert_eq!(ask_wait(false), std::time::Duration::from_secs(300));
            assert_eq!(ask_wait(true), std::time::Duration::from_secs(60));
        }
    }

    fn signal(sender: &str, member: &str, body: Vec<Value>) -> panpak_core::dbus::Message {
        panpak_core::dbus::Message { kind: panpak_core::dbus::SIGNAL, sender: Some(sender.into()), interface: Some(FDO.into()), member: Some(member.into()), body, ..Default::default() }
    }

    #[test]
    fn the_service_gets_two_buttons_a_default_that_is_no_answer_resident_no_desktop_entry_and_text_that_is_not_markup() {
        let a = notify_args("Title\u{7}", "Use <b>this</b> & that\nLine two", "Yes", "No");
        assert_eq!(a.len(), 8);
        assert_eq!(a[0], Value::str("Panpak"));
        assert_eq!(a[3], Value::str("Title?"));
        assert_eq!(a[4], Value::str("Use &lt;b&gt;this&lt;/b&gt; &amp; that\nLine two"));
        // the actions, in pairs of key and label: `default` (a click on the body: no answer) and the two buttons
        assert_eq!(a[5], Value::Array("s".into(), vec![Value::str("default"), Value::str(""), Value::str("yes"), Value::str("Yes"), Value::str("no"), Value::str("No")]));
        // the one hint is `resident` (the notification stays after a click on the body), no `desktop-entry` (the shell must not start an app of that name), no expiry by itself
        let hints = format!("{:?}", a[6]);
        assert!(hints.contains("resident") && hints.contains("Bool(true)") && !hints.contains("desktop-entry"), "{hints}");
        assert_eq!(a[7], Value::I32(0));
    }

    #[test]
    fn the_notice_of_an_app_that_did_not_start_is_transient_has_the_button_only_when_asked_and_escapes_the_body() {
        let a = notice_args("Ask\u{7} App didn't start", "The app <b>closed</b> & stopped.\nSecond line", Some("Show details"));
        assert_eq!(a.len(), 8);
        assert_eq!((&a[0], &a[1]), (&Value::str("Panpak"), &Value::U32(0)));
        assert_eq!(a[3], Value::str("Ask? App didn't start"));
        assert_eq!(a[4], Value::str("The app &lt;b&gt;closed&lt;/b&gt; &amp; stopped.\nSecond line"));
        // the button `details` only (a click on the body is no answer and no default action is made), and the hint `transient`: it is not kept in the list
        assert_eq!(a[5], Value::Array("s".into(), vec![Value::str("details"), Value::str("Show details")]));
        let hints = format!("{:?}", a[6]);
        assert!(hints.contains("transient") && hints.contains("Bool(true)") && !hints.contains("resident") && !hints.contains("desktop-entry"), "{hints}");
        // the server chooses how long the banner stays
        assert_eq!(a[7], Value::I32(-1));
        // no button: no action at all
        assert_eq!(notice_args("T", "t", None)[5], Value::Array("s".into(), Vec::new()));
    }

    #[test]
    fn the_key_of_an_action_counts_only_from_the_owner_for_our_number() {
        let owner = ":1.28";
        let act = |id: u32, key: &str| vec![Value::U32(id), Value::str(key)];
        // the right sender and number: the key, whatever it is (the callers decide which keys they take)
        assert_eq!(action_of(&signal(owner, "ActionInvoked", act(7, "update")), owner, 7).as_deref(), Some("update"));
        assert_eq!(action_of(&signal(owner, "ActionInvoked", act(7, "default")), owner, 7).as_deref(), Some("default"));
        // another client that sends the same signal, with the right number and key
        assert_eq!(action_of(&signal(":1.99", "ActionInvoked", act(7, "update")), owner, 7), None);
        // another number, another member, another interface, a body that is not (u, s)
        assert_eq!(action_of(&signal(owner, "ActionInvoked", act(8, "update")), owner, 7), None);
        assert_eq!(action_of(&signal(owner, "NotificationClosed", act(7, "update")), owner, 7), None);
        let mut other = signal(owner, "ActionInvoked", act(7, "update"));
        other.interface = Some("org.example.Other".into());
        assert_eq!(action_of(&other, owner, 7), None);
        assert_eq!(action_of(&signal(owner, "ActionInvoked", vec![Value::str("update"), Value::U32(7)]), owner, 7), None);
        assert_eq!(action_of(&signal(owner, "ActionInvoked", vec![]), owner, 7), None);
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
