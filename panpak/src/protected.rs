// SPDX-License-Identifier: MIT OR Apache-2.0
//! Protected mode, the client side (PANPAK-FORMAT.md 3.1.10): what the person can do about an installed version that is below the floor its author
//! set, and what is said when the mode ends. The start itself is the stub's: it takes away the network and the programs the app wrote
//! (`panpak_core::release::protect`) and says so.
//!
//! - `panpak allow-internet [--undo] [--yes] <app-id>`: turn the internet back on for one app, after a confirmation that explains the risk. It is
//!   kept in the state of the client (`apps/<id>/protected-net`), shown by `list` and `doctor`, and ends with the mode.
//! - `panpak protected-notice <app-id>`: started by the stub (detached) when a start has no terminal: shows the notification with the button
//!   "Update now" and does the update if the button is pressed.
//! - `mode_ended`: when the version that fixes the problem is applied, the choice of the person is cleared and the person is told.

use super::{confirm, installed_manifest, store_dir};
use anyhow::{bail, Result};
use panpak_core::{
    i18n::{fill, tr},
    messages as m,
    release::{self, Allow},
    ErrorCode, Manifest,
};
use std::path::Path;

const NOTICE_WAIT: std::time::Duration = std::time::Duration::from_secs(25);
const NOTICE_SHOWN_MS: i32 = 20_000;

fn revision_of(man: &Manifest) -> u64 {
    man.release.as_ref().map_or(0, |r| r.revision)
}

fn name_of(man: &Manifest) -> String {
    man.app.name.chars().filter(|c| !c.is_control()).collect()
}

fn installed(app_id: &str) -> Result<(std::path::PathBuf, Manifest)> {
    panpak_core::validate_app_id(app_id)?;
    let appdir = store_dir()?.join("apps").join(app_id);
    let Some(man) = installed_manifest(&appdir) else {
        panpak_core::bail_code!(ErrorCode::E2004, "{app_id} is not installed");
    };
    Ok((appdir, man))
}

/// The line that names the command to run for the update.
fn update_command(app_id: &str) -> String {
    format!("panpak update {app_id}")
}

/// `panpak allow-internet [--undo] [--yes] <app-id>`.
pub fn cmd_allow_internet(app_id: &str, undo: bool, yes: bool) -> Result<()> {
    let (appdir, man) = installed(app_id)?;
    let name = name_of(&man);
    if undo {
        release::set_internet_back_on(&appdir, false)?;
        println!("{}", fill(&tr(m::INTERNET_OFF_DONE), &[("name", name)]));
        return Ok(());
    }
    if release::protected_floor(&appdir, revision_of(&man)).is_none() {
        bail!("{}", fill(&tr(m::INTERNET_NOT_NEEDED), &[("name", name)]));
    }
    println!("{}", fill(&tr(m::INTERNET_RISK), &[("name", name.clone())]));
    confirm(&fill(&tr(m::INTERNET_ASK), &[("name", name.clone())]), yes, ErrorCode::E1003, "turning the internet back on")?;
    release::set_internet_back_on(&appdir, true)?;
    println!("{}", fill(&tr(m::INTERNET_ON_DONE), &[("name", name), ("command", format!("panpak allow-internet --undo {app_id}"))]));
    Ok(())
}

/// The version that fixes the problem was put in place: when the one before it was in protected mode, the choice of the person is cleared and the
/// person is told, on the terminal and in a notification (best effort). `old_revision` is the revision that was installed before.
pub fn mode_ended(appdir: &Path, new: &Manifest, old_revision: u64) {
    // The floor of the new version was noted before, so the old revision is below it when the mode was on.
    if release::protected_floor(appdir, old_revision).is_none() || release::protected_floor(appdir, revision_of(new)).is_some() {
        return;
    }
    let _ = release::set_internet_back_on(appdir, false);
    let name = name_of(new);
    let (title, body) = (fill(&tr(m::ENDED_TITLE), &[("name", name.clone())]), fill(&tr(m::ENDED_BODY), &[("name", name)]));
    eprintln!("panpak: {title}\n{body}");
    panpak_core::chooserhost::notify(&title, &body);
}

/// `panpak protected-notice <app-id>` (started by the stub, never by a person).
pub fn cmd_notice(app_id: &str) -> Result<()> {
    let (appdir, man) = installed(app_id)?;
    if release::protected_floor(&appdir, revision_of(&man)).is_none() {
        return Ok(());
    }
    let name = name_of(&man);
    let (title, body) = (fill(&tr(m::PROTECTED_TITLE), &[("name", name.clone())]), fill(&tr(m::PROTECTED_BODY), &[("name", name.clone())]));
    // No bus, no notification daemon: nothing to show and nothing to wait for.
    let Ok(mut conn) = panpak_core::dbus::Connection::session(std::time::Duration::from_secs(3)) else { return Ok(()) };
    use panpak_core::dbus::Value;
    let rule = "type='signal',interface='org.freedesktop.Notifications',member='ActionInvoked'";
    let _ = conn.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", vec![Value::str(rule)], vec![]);
    let args = vec![
        Value::str("Panpak"),
        Value::U32(0),
        Value::str("dialog-warning-symbolic"),
        Value::str(&title),
        Value::str(&body),
        Value::Array("s".into(), vec![Value::str("update"), Value::str(&tr(m::UPDATE_NOW))]),
        Value::Array("{sv}".into(), Vec::new()),
        Value::I32(NOTICE_SHOWN_MS),
    ];
    let reply = conn.call("org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications", "Notify", args, vec![]);
    let Ok(reply) = reply else { return Ok(()) };
    let Some(Value::U32(id)) = reply.body.first().cloned() else { return Ok(()) };
    let pressed = conn.wait_signal(NOTICE_WAIT, |msg| {
        msg.member.as_deref() == Some("ActionInvoked") && matches!(msg.body.as_slice(), [Value::U32(n), Value::Str(k)] if *n == id && k == "update")
    });
    if !matches!(pressed, Ok(Some(_))) {
        return Ok(());
    }
    update_now(app_id, &appdir, &man);
    Ok(())
}

/// What the button does: the update, with nothing accepted for the person. New permissions need the terminal, where they are read and accepted.
fn update_now(app_id: &str, appdir: &Path, man: &Manifest) {
    let name = name_of(man);
    let title = fill(&tr(m::PROTECTED_TITLE), &[("name", name.clone())]);
    let say = |body: String| panpak_core::chooserhost::notify(&title, &body);
    match super::cmd_update(app_id, false, false, Allow::default(), false, 30, false) {
        Ok(_) => {
            // The end of the mode is told by the apply. When the copy that is running holds it back, the person is told what to do.
            if release::protected_floor(appdir, revision_of(man)).is_some() && installed_manifest(appdir).is_some_and(|n| release::protected_floor(appdir, revision_of(&n)).is_some()) {
                say(fill(&tr(m::UPDATE_WAITS), &[("name", name)]));
            }
        }
        Err(e) if panpak_core::code_of(&e, ErrorCode::E2000) == ErrorCode::E5004 => say(fill(&tr(m::UPDATE_NEEDS_OK), &[("name", name), ("command", update_command(app_id))])),
        Err(e) => say(fill(&tr(m::UPDATE_FAILED), &[("name", name), ("why", panpak_core::err_text(&e))])),
    }
}

/// The lines that `doctor` prints for the apps that are in protected mode.
pub fn doctor_lines() -> Vec<String> {
    let Ok(store) = store_dir() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(store.join("apps")) else { return Vec::new() };
    let mut ids: Vec<String> = rd.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| panpak_core::validate_app_id(n).is_ok()).collect();
    ids.sort();
    ids.into_iter()
        .filter_map(|id| {
            let appdir = store.join("apps").join(&id);
            let man = installed_manifest(&appdir)?;
            release::protected_floor(&appdir, revision_of(&man))?;
            let mut line = fill(&tr(m::PROTECTED_DOCTOR), &[("name", name_of(&man)), ("id", id.clone()), ("command", update_command(&id))]);
            if release::internet_is_back_on(&appdir) {
                line.push_str(&format!(" ({})", tr(m::PROTECTED_LIST_NET)));
            }
            Some(line)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_to_update_names_the_app() {
        assert_eq!(update_command("org.example.app"), "panpak update org.example.app");
    }
}
