// SPDX-License-Identifier: MIT OR Apache-2.0
//! Protected mode, the client side (PANPAK-FORMAT.md 3.1.10).
//!
//! The logic is here and nowhere else: `status_of` says whether an app is in protected mode, whether an update is ready, which permissions it asks for, and
//! which actions the person has (`panpak protected --json`). `act_update` and `set_internet` do the two actions. What follows only shows the state and
//! carries the answer to those functions, with no rule of its own, so that another front end (the Panpak app, step 10b) can take their place:
//!
//! - the terminal (`ask_in_terminal`): a program that is run from a terminal that is interactive asks its two questions with one key;
//! - the notification and the windows (`cmd_notice`, `cmd_allow_internet --ask`): the notification with one button, the window of `zenity` or
//!   `kdialog` that shows the new permissions or explains the risk (the same mechanism as the menu action "Uninstall");
//! - one line on the standard error, for a start that has nobody to ask.
//!
//! The start itself is the stub's: it takes away the network and the programs the app wrote (`panpak_core::release::protect`) and the addresses it may
//! open, and says nothing. The texts are the messages of `panpak_core::messages`, the same for every front end.

use super::{installed_manifest, store_dir, Applied};
use crate::dialog::{self, Answer};
use anyhow::{bail, Result};
use panpak_core::{
    coded,
    i18n::{fill, tr},
    messages as m,
    release::{self, Allow},
    ErrorCode, Manifest,
};
use std::path::{Path, PathBuf};

const NOTICE_WAIT: std::time::Duration = std::time::Duration::from_secs(25);
const NOTICE_SHOWN_MS: i32 = 20_000;

/// How the app is shown to the person: a program with windows, or one that is run in a terminal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Graphical,
    Terminal,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Graphical => "graphical",
            Kind::Terminal => "terminal",
        }
    }
}

/// How an app is shown (see `release::is_graphical`).
pub fn kind_of(man: &Manifest) -> Kind {
    if release::is_graphical(man) {
        Kind::Graphical
    } else {
        Kind::Terminal
    }
}

/// The update that is ready (staged) and fixes the problem: at or above the floor.
#[derive(Debug, Clone)]
pub struct Update {
    pub version: String,
    /// The permissions it asks for that the installed version does not have, as text in the language of the person.
    pub new_permissions: Vec<String>,
    /// The person already accepted them (a copy of the app was running): the next start applies it.
    pub accepted: bool,
}

/// Everything a front end needs to show protected mode and to act. `None` from `status_of` means the app is not in protected mode.
#[derive(Debug, Clone)]
pub struct Status {
    pub app_id: String,
    pub name: String,
    pub kind: Kind,
    pub floor: u64,
    pub revision: u64,
    pub internet_back_on: bool,
    pub update: Option<Update>,
}

impl Status {
    /// The actions the person has: `update` (only when an update is ready), then `internet_on` or `internet_off`.
    pub fn actions(&self) -> Vec<&'static str> {
        let mut a = Vec::new();
        if self.update.is_some() {
            a.push("update");
        }
        a.push(if self.internet_back_on { "internet_off" } else { "internet_on" });
        a
    }

    fn title(&self) -> String {
        fill(&tr(m::PROTECTED_TITLE), &[("name", self.name.clone())])
    }

    /// The text of the notification: without an update ready it also says that Panpak will fetch the fixed version and that the mode ends by itself.
    fn body(&self) -> String {
        let body = fill(&tr(m::PROTECTED_BODY), &[("name", self.name.clone())]);
        if self.update.is_some() {
            body
        } else {
            format!("{body} {}", tr(m::PROTECTED_WAITS))
        }
    }

    fn line(&self) -> String {
        fill(&tr(if self.internet_back_on { m::PROTECTED_LINE_NET } else { m::PROTECTED_LINE }), &[("name", self.name.clone())])
    }

    /// What the terminal questions were asked about: the floor and the update that is ready. Asked once for each; a new version asks again.
    fn question_token(&self) -> String {
        format!("{}:{}", self.floor, self.update.as_ref().map_or("", |u| u.version.as_str()))
    }

    /// The state as JSON, with the texts in the language of the person (the same messages for every front end).
    pub fn json(&self) -> String {
        let j = crate::remove::json_str;
        let update = match &self.update {
            Some(u) => format!(
                "{{\"version\":{},\"new_permissions\":[{}],\"accepted\":{}}}",
                j(&u.version),
                u.new_permissions.iter().map(|p| j(p)).collect::<Vec<_>>().join(","),
                u.accepted
            ),
            None => "null".to_string(),
        };
        let actions = self.actions().iter().map(|a| j(a)).collect::<Vec<_>>().join(",");
        let texts = format!(
            "{{\"title\":{},\"body\":{},\"line\":{},\"update\":{},\"internet_risk\":{}}}",
            j(&self.title()),
            j(&self.body()),
            j(&self.line()),
            j(&tr(m::UPDATE_NOW)),
            j(&fill(&tr(m::INTERNET_RISK), &[("name", self.name.clone())]))
        );
        format!(
            "{{\"id\":{},\"name\":{},\"kind\":{},\"protected\":true,\"floor\":{},\"revision\":{},\"internet_back_on\":{},\"update\":{update},\"actions\":[{actions}],\"texts\":{texts}}}",
            j(&self.app_id),
            j(&self.name),
            j(self.kind.name()),
            self.floor,
            self.revision,
            self.internet_back_on
        )
    }
}

fn revision_of(man: &Manifest) -> u64 {
    man.release.as_ref().map_or(0, |r| r.revision)
}

fn clean_name(man: &Manifest) -> String {
    man.app.name.chars().filter(|c| !c.is_control()).collect()
}

fn appdir_of(app_id: &str) -> Result<PathBuf> {
    panpak_core::validate_app_id(app_id)?;
    Ok(store_dir()?.join("apps").join(app_id))
}

/// The state of protected mode of an installed app.
pub fn status_of(app_id: &str) -> Result<Option<Status>> {
    let appdir = appdir_of(app_id)?;
    let Some(man) = installed_manifest(&appdir) else { return Ok(None) };
    let revision = revision_of(&man);
    let Some(floor) = release::protected_floor(&appdir, revision) else { return Ok(None) };
    Ok(Some(Status {
        app_id: app_id.to_string(),
        name: clean_name(&man),
        kind: kind_of(&man),
        floor,
        revision,
        internet_back_on: release::internet_is_back_on(&appdir),
        update: ready_update(&appdir, &man, floor),
    }))
}

fn ready_update(appdir: &Path, installed: &Manifest, floor: u64) -> Option<Update> {
    let staged = panpak_core::pending::staged_record(appdir)?;
    let new = panpak_core::record::open_verified(&staged).ok().filter(|n| n.is_record)?;
    if revision_of(&new.manifest) < floor {
        return None;
    }
    let (lang, style) = (panpak_core::i18n::current(), super::perm_style());
    let new_permissions = panpak_core::permissions::added(installed, &new.manifest).iter().map(|a| panpak_core::permissions::render_item(lang, style, a)).collect();
    Some(Update { version: new.manifest.app.version.clone(), new_permissions, accepted: panpak_core::pending::read_state(appdir).is_some_and(|s| s.accepted) })
}

/// What `act_update` came to.
#[derive(Debug)]
pub enum Outcome {
    /// The fixed version is in place (the end of the mode was told by the apply).
    Done,
    /// It is accepted and ready, and it applies at the next start (a copy of the app is running).
    Waits,
    /// It asks for permissions and the person has not accepted them.
    NeedsAccept,
    /// There is no update ready.
    NothingReady,
    Failed(String),
}

/// Action: update to the staged version. `accept` says the person accepted the new permissions it asks for (if any).
pub fn act_update(app_id: &str, accept: bool) -> Outcome {
    let Ok(Some(st)) = status_of(app_id) else { return Outcome::NothingReady };
    let Some(u) = st.update else { return Outcome::NothingReady };
    if !u.new_permissions.is_empty() && !accept {
        return Outcome::NeedsAccept;
    }
    let appdir = match appdir_of(app_id) {
        Ok(d) => d,
        Err(e) => return Outcome::Failed(panpak_core::err_text(&e)),
    };
    // Accepted first, so that a start that finds a copy running now applies it without asking again.
    if accept && !u.new_permissions.is_empty() {
        if let Some(s) = panpak_core::pending::read_state(&appdir) {
            if let Err(e) = panpak_core::pending::write_state(&appdir, panpak_core::pending::State { accepted: true, ..s }) {
                return Outcome::Failed(panpak_core::err_text(&e));
            }
        }
    }
    match super::cmd_apply(app_id, true, accept, false, Allow::default()) {
        Ok(Applied::Done { .. }) => Outcome::Done,
        Ok(Applied::Skipped(_)) => Outcome::Waits,
        Err(e) => Outcome::Failed(panpak_core::err_text(&e)),
    }
}

/// Action: turn the internet back on (`on`) or off again for the app, and make the menu entry say it.
pub fn set_internet(app_id: &str, on: bool) -> Result<()> {
    let appdir = appdir_of(app_id)?;
    release::set_internet_back_on(&appdir, on)?;
    super::refresh_menu_entry(app_id);
    Ok(())
}

/// Tell the person something in a notification of the desktop (best effort).
fn tell(title: &str, body: &str) {
    if !crate::remove::notify(title, body) {
        panpak_core::chooserhost::notify(title, body);
    }
}

/// The version that fixes the problem was put in place: when the one before it was in protected mode, the mark and the choice of the person are cleared and the
/// person is told, in a notification and on the standard error (best effort). `old_revision` is the revision that was installed before.
pub fn mode_ended(appdir: &Path, new: &Manifest, old_revision: u64) {
    let was = release::protected_floor(appdir, old_revision).is_some() || release::is_marked_protected(appdir);
    if !was || release::protected_floor(appdir, revision_of(new)).is_some() {
        return;
    }
    let _ = release::set_internet_back_on(appdir, false);
    let _ = std::fs::remove_file(appdir.join(ASKED));
    let _ = release::sync_protected(appdir, revision_of(new));
    let name = clean_name(new);
    let (title, body) = (fill(&tr(m::ENDED_TITLE), &[("name", name.clone())]), fill(&tr(m::ENDED_BODY), &[("name", name)]));
    eprintln!("panpak: {title}\n{body}");
    tell(&title, &body);
}

fn is_tty(fd: i32) -> bool {
    // SAFETY: isatty only looks at the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

/// A question with one key (no Enter): `default` on Enter. Yes is `y`, `Y`, `s`, `S` (`s` is the Italian yes); anything else that is a letter is no.
/// The terminal is put in raw mode for the one key and restored. `None` when the terminal cannot be read.
fn ask_key(question: &str, default: bool) -> Option<bool> {
    use std::io::{Read, Write};
    eprint!("{question} ");
    let _ = std::io::stderr().flush();
    // SAFETY: tcgetattr/tcsetattr on the standard input, with a termios that tcgetattr filled in.
    let saved = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) != 0 {
            return None;
        }
        let saved = t;
        t.c_lflag &= !(libc::ICANON | libc::ECHO);
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
        libc::tcsetattr(0, libc::TCSANOW, &t);
        saved
    };
    let mut b = [0u8; 1];
    let got = std::io::stdin().read(&mut b);
    // SAFETY: restores what tcgetattr read.
    unsafe { libc::tcsetattr(0, libc::TCSANOW, &saved) };
    let answer = match got {
        Ok(1) => match b[0] {
            b'\n' | b'\r' => default,
            b'y' | b'Y' | b's' | b'S' => true,
            _ => false,
        },
        _ => return None,
    };
    eprintln!("{}", if answer { "y" } else { "n" });
    Some(answer)
}

/// The file of an app (`apps/<id>/protected-asked`) that remembers what the questions of the terminal were asked about.
const ASKED: &str = "protected-asked";

fn already_asked(appdir: &Path, st: &Status) -> bool {
    std::fs::read_to_string(appdir.join(ASKED)).is_ok_and(|t| t.trim() == st.question_token())
}

/// The two questions of a program of the terminal, asked where the person is. `true` when the person answered (so they are not asked again).
fn ask_in_terminal(st: &Status) -> bool {
    let mut answered = false;
    eprintln!("panpak: {}\n{}", st.title(), st.body());
    if let Some(u) = &st.update {
        let with_permissions = !u.new_permissions.is_empty();
        if with_permissions {
            eprintln!("{}", tr("This update asks for more than the version you have:"));
            for p in &u.new_permissions {
                eprintln!("  {p}");
            }
            eprintln!("{}", tr("If you do not accept, you stay on the current version."));
        }
        let question = fill(&tr(if with_permissions { m::ASK_UPDATE_NEW } else { m::ASK_UPDATE }), &[("name", st.name.clone())]);
        let answer = ask_key(&question, !with_permissions);
        answered |= answer.is_some();
        if answer == Some(true) {
            match act_update(&st.app_id, true) {
                Outcome::Done | Outcome::NothingReady | Outcome::NeedsAccept => {}
                Outcome::Waits => eprintln!("{}", fill(&tr(m::UPDATE_WAITS), &[("name", st.name.clone())])),
                Outcome::Failed(why) => eprintln!("{}", fill(&tr(m::UPDATE_FAILED), &[("name", st.name.clone()), ("why", why)])),
            }
            return answered;
        }
    }
    if st.internet_back_on {
        let answer = ask_key(&fill(&tr(m::ASK_INTERNET_OFF), &[("name", st.name.clone())]), false);
        answered |= answer.is_some();
        if answer == Some(true) {
            let _ = set_internet(&st.app_id, false);
            eprintln!("{}", fill(&tr(m::INTERNET_OFF_DONE), &[("name", st.name.clone())]));
        }
    } else {
        eprintln!("{}", fill(&tr(m::INTERNET_RISK), &[("name", st.name.clone())]));
        let answer = ask_key(&fill(&tr(m::ASK_INTERNET), &[("name", st.name.clone())]), false);
        answered |= answer.is_some();
        if answer == Some(true) {
            let _ = set_internet(&st.app_id, true);
            eprintln!("{}", fill(&tr(m::INTERNET_ON_NOW), &[("name", st.name.clone())]));
        }
    }
    answered
}

/// `panpak protected-start <app-id>`: run by the stub at a start, before anything of the app is read, when the app is marked as in protected mode. It
/// shows the state the way that fits how the app was started and, for a program of the terminal with an interactive terminal, asks the questions. It
/// never fails the start: the stub goes on whatever happens here.
pub fn cmd_start(app_id: &str) -> Result<()> {
    let Some(st) = status_of(app_id)? else {
        // The mark was out of date: the state is the one that the floor and the revision say.
        if let Some(man) = installed_manifest(&appdir_of(app_id)?) {
            let _ = release::sync_protected(&appdir_of(app_id)?, revision_of(&man));
        }
        return Ok(());
    };
    let interactive = is_tty(0) && is_tty(2);
    match (st.kind, interactive) {
        (Kind::Graphical, _) => {
            if is_tty(2) {
                eprintln!("panpak: {}", st.line());
            }
            spawn_notice(app_id);
        }
        (Kind::Terminal, true) => {
            // Asked once. After an answer, even a "no", the start has the short line only, until a new version is ready.
            let appdir = appdir_of(app_id)?;
            if already_asked(&appdir, &st) {
                eprintln!("panpak: {}", st.line());
            } else if ask_in_terminal(&st) {
                // Taken again, because the answer may have changed the state (the internet back on, an update done).
                if let Ok(Some(now)) = status_of(app_id) {
                    let _ = std::fs::write(appdir.join(ASKED), now.question_token());
                }
            }
        }
        (Kind::Terminal, false) => eprintln!("panpak: {}", st.line()),
    }
    Ok(())
}

/// Start the notification as a process of its own, detached: it waits for the button, and the start does not.
fn spawn_notice(app_id: &str) {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["protected-notice", app_id]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    // SAFETY: setsid is async-signal-safe; the notice leaves the process group of the app and survives its end.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = cmd.spawn();
}

/// `panpak protected-notice <app-id>` (started by `cmd_start`, never by a person): the notification, with the button "Update now" only when an update is
/// ready, and what the button does.
pub fn cmd_notice(app_id: &str, pressed: bool) -> Result<()> {
    let Some(st) = status_of(app_id)? else { return Ok(()) };
    // `--pressed`: what happens when the button is pressed, without the notification (for a front end that has its own button, and for the tests).
    if pressed {
        update_with_windows(&st);
        return Ok(());
    }
    let buttons = st.update.is_some();
    // No bus, no notification service: nothing to show and nothing to wait for.
    let Ok(mut conn) = panpak_core::dbus::Connection::session(std::time::Duration::from_secs(3)) else { return Ok(()) };
    use panpak_core::dbus::Value;
    if buttons {
        let rule = "type='signal',interface='org.freedesktop.Notifications',member='ActionInvoked'";
        let _ = conn.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", vec![Value::str(rule)], vec![]);
    }
    let actions = if buttons { vec![Value::str("update"), Value::str(&tr(m::UPDATE_NOW))] } else { Vec::new() };
    let args = vec![
        Value::str("Panpak"),
        Value::U32(0),
        Value::str("dialog-warning-symbolic"),
        Value::str(&st.title()),
        Value::str(&st.body()),
        Value::Array("s".into(), actions),
        Value::Array("{sv}".into(), Vec::new()),
        Value::I32(NOTICE_SHOWN_MS),
    ];
    let Ok(reply) = conn.call("org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications", "Notify", args, vec![]) else { return Ok(()) };
    if !buttons {
        return Ok(());
    }
    let Some(Value::U32(id)) = reply.body.first().cloned() else { return Ok(()) };
    let pressed = conn.wait_signal(NOTICE_WAIT, |msg| {
        msg.member.as_deref() == Some("ActionInvoked") && matches!(msg.body.as_slice(), [Value::U32(n), Value::Str(k)] if *n == id && k == "update")
    });
    if matches!(pressed, Ok(Some(_))) {
        update_with_windows(&st);
    }
    Ok(())
}

/// What the button does: when the update asks for permissions the person has not seen, a window shows them first; then the update.
fn update_with_windows(st: &Status) {
    let Some(u) = &st.update else { return };
    let accept = !u.new_permissions.is_empty();
    if accept {
        let mut text = format!("{}\n", tr("This update asks for more than the version you have:"));
        for p in &u.new_permissions {
            text.push_str(&format!("\n  {p}"));
        }
        text.push_str(&format!("\n\n{}", tr("If you do not accept, you stay on the current version.")));
        let title = fill(&tr(m::UPDATE_DIALOG_TITLE), &[("name", st.name.clone())]);
        match dialog::ask(&title, &text, &tr(m::UPDATE_DIALOG_OK), &tr(m::NOT_NOW)) {
            Answer::Yes => {}
            Answer::No => return,
            Answer::NoTool => return tell(&st.title(), &fill(&tr(m::NO_DIALOG_FOR_PERMISSIONS), &[("name", st.name.clone()), ("id", st.app_id.clone())])),
        }
    }
    match act_update(&st.app_id, accept) {
        Outcome::Done | Outcome::NothingReady | Outcome::NeedsAccept => {}
        Outcome::Waits => tell(&st.title(), &fill(&tr(m::UPDATE_WAITS), &[("name", st.name.clone())])),
        Outcome::Failed(why) => tell(&st.title(), &fill(&tr(m::UPDATE_FAILED), &[("name", st.name.clone()), ("why", why)])),
    }
}

/// `panpak protected-action <update|internet-on|internet-off> [--accept-new-permissions] <app-id>`: the actions, for a front end of its own. Prints one line of
/// JSON: `{"result": "done" | "waits" | "needs_accept" | "nothing_ready" | "failed", "why": ...}`; the exit status is 0 unless the action failed.
pub fn cmd_action(what: &str, app_id: &str, accept: bool) -> Result<()> {
    let j = crate::remove::json_str;
    let (result, why) = match what {
        "update" => match act_update(app_id, accept) {
            Outcome::Done => ("done", String::new()),
            Outcome::Waits => ("waits", String::new()),
            Outcome::NeedsAccept => ("needs_accept", String::new()),
            Outcome::NothingReady => ("nothing_ready", String::new()),
            Outcome::Failed(why) => ("failed", why),
        },
        "internet-on" | "internet-off" => match set_internet(app_id, what == "internet-on") {
            Ok(()) => ("done", String::new()),
            Err(e) => ("failed", panpak_core::err_text(&e)),
        },
        other => panpak_core::bail_code!(ErrorCode::E5001, "unknown action {other}: update, internet-on or internet-off"),
    };
    println!("{{\"result\":{},\"why\":{}}}", j(result), if why.is_empty() { "null".to_string() } else { j(&why) });
    if result == "failed" {
        bail!("{why}");
    }
    Ok(())
}

/// `panpak allow-internet [--undo] [--yes] [--ask] <app-id>`. With `--ask` (the menu action) the confirmation is a window; otherwise, for a script, `--yes`
/// answers, and without it an interactive terminal is asked and anything else is refused.
pub fn cmd_allow_internet(app_id: &str, undo: bool, yes: bool, ask: bool) -> Result<()> {
    let appdir = appdir_of(app_id)?;
    let Some(man) = installed_manifest(&appdir) else { panpak_core::bail_code!(ErrorCode::E2004, "{app_id} is not installed") };
    let name = clean_name(&man);
    let vals = || [("name", name.clone())];
    let on_now = release::internet_is_back_on(&appdir);
    if undo {
        set_internet(app_id, false)?;
        println!("{}", fill(&tr(m::INTERNET_OFF_DONE), &vals()));
        return Ok(());
    }
    if release::protected_floor(&appdir, revision_of(&man)).is_none() {
        if ask {
            tell(&fill(&tr(m::PROTECTED_TITLE), &vals()), &fill(&tr(m::INTERNET_NOT_NEEDED), &vals()));
            return Ok(());
        }
        bail!("{}", fill(&tr(m::INTERNET_NOT_NEEDED), &vals()));
    }
    if ask {
        // The menu: a window that explains, and the answer. The same action turns the internet off again when it is on.
        let (title, text, ok) = if on_now {
            (fill(&tr(m::INTERNET_OFF_DIALOG_TITLE), &vals()), String::new(), tr(m::INTERNET_OFF_LABEL).into_owned())
        } else {
            (fill(&tr(m::INTERNET_DIALOG_TITLE), &vals()), fill(&tr(m::INTERNET_RISK), &vals()), tr(m::INTERNET_ON_LABEL).into_owned())
        };
        let text = if text.is_empty() { title.clone() } else { text };
        match dialog::ask(&title, &text, &ok, &tr(m::NOT_NOW)) {
            Answer::Yes => {
                set_internet(app_id, !on_now)?;
                tell(&fill(&tr(m::PROTECTED_TITLE), &vals()), &fill(&tr(if on_now { m::INTERNET_OFF_DONE } else { m::INTERNET_ON_DONE }), &vals()));
            }
            Answer::No => {}
            Answer::NoTool => tell(&fill(&tr(m::PROTECTED_TITLE), &vals()), &fill(&tr(if on_now { m::NO_DIALOG_FOR_INTERNET_OFF } else { m::NO_DIALOG_FOR_INTERNET }), &[("name", name.clone()), ("id", app_id.to_string())])),
        }
        return Ok(());
    }
    println!("{}", fill(&tr(m::INTERNET_RISK), &vals()));
    if !yes {
        if !(is_tty(0) && is_tty(2)) {
            return Err(coded(ErrorCode::E5004, tr(m::NEEDS_CONFIRMATION).into_owned()));
        }
        if ask_key(&fill(&tr(m::ASK_INTERNET), &vals()), false) != Some(true) {
            return Err(coded(ErrorCode::E1003, "turning the internet back on: cancelled".to_string()));
        }
    }
    set_internet(app_id, true)?;
    println!("{}", fill(&tr(m::INTERNET_ON_DONE), &vals()));
    Ok(())
}

/// `panpak protected [--json] [<app-id>]`: the state of protected mode, for people and for a front end. Without an id, every app that is in the mode.
pub fn cmd_protected(json: bool, app_id: Option<&str>) -> Result<()> {
    let ids: Vec<String> = match app_id {
        Some(id) => vec![id.to_string()],
        None => installed_ids(),
    };
    let states: Vec<Status> = ids.iter().filter_map(|id| status_of(id).ok().flatten()).collect();
    if json {
        match app_id {
            Some(_) => println!("{}", states.first().map_or("{\"protected\":false}".to_string(), Status::json)),
            None => println!("[{}]", states.iter().map(Status::json).collect::<Vec<_>>().join(",")),
        }
        return Ok(());
    }
    if states.is_empty() {
        println!("{}", if app_id.is_some() { "not in protected mode" } else { "no app is in protected mode" });
    }
    for st in &states {
        println!("{}", fill(&tr(m::PROTECTED_DOCTOR), &[("name", st.name.clone()), ("id", st.app_id.clone())]));
    }
    Ok(())
}

pub(crate) fn installed_ids() -> Vec<String> {
    let Ok(store) = store_dir() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(store.join("apps")) else { return Vec::new() };
    let mut ids: Vec<String> = rd.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| panpak_core::validate_app_id(n).is_ok()).collect();
    ids.sort();
    ids
}

/// The lines that `doctor` prints for the apps that are in protected mode.
pub fn doctor_lines() -> Vec<String> {
    installed_ids()
        .into_iter()
        .filter_map(|id| status_of(&id).ok().flatten())
        .map(|st| {
            let mut line = fill(&tr(m::PROTECTED_DOCTOR), &[("name", st.name.clone()), ("id", st.app_id.clone())]);
            if st.internet_back_on {
                line.push_str(&format!(" ({})", tr(m::PROTECTED_LIST_NET)));
            }
            line
        })
        .collect()
}

/// The lines that `doctor` prints about the trust of each installed app: where its updates come from (and who said so), and the note of a change of channel that
/// took a release of the same revision with other bytes (an error of the author).
pub fn trust_lines() -> Vec<String> {
    let mut out = Vec::new();
    for id in installed_ids() {
        let Ok(appdir) = appdir_of(&id) else { continue };
        let name = installed_manifest(&appdir).map(|m| clean_name(&m)).unwrap_or_else(|| id.clone());
        let url = std::fs::read_to_string(appdir.join("update.url")).ok().map(|u| u.trim().to_string());
        out.push(format!("{name} ({id}): {}", super::updates_line(url.as_deref())));
        if let Some(note) = panpak_core::trust::read(&appdir).and_then(|s| s.switch_note) {
            out.push(fill(&tr(m::DOCTOR_SWITCH), &[("name", name), ("id", id), ("note", note)]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_actions_follow_the_update_and_the_internet() {
        let mut st = Status { app_id: "org.x".into(), name: "X".into(), kind: Kind::Graphical, floor: 2, revision: 1, internet_back_on: false, update: None };
        assert_eq!(st.actions(), vec!["internet_on"]);
        st.update = Some(Update { version: "2".into(), new_permissions: vec!["a \"b\"".into()], accepted: false });
        st.internet_back_on = true;
        assert_eq!(st.actions(), vec!["update", "internet_off"]);
        let j = st.json();
        assert!(j.contains("\"actions\":[\"update\",\"internet_off\"]") && j.contains("\"new_permissions\":[\"a \\\"b\\\"\"]") && j.contains("\"kind\":\"graphical\""), "{j}");
        assert!(j.contains("\"texts\":{\"title\":") && j.contains("\"protected\":true"), "{j}");
    }
}
