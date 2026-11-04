// SPDX-License-Identifier: MIT OR Apache-2.0
//! The calls the outer stub makes to the user's systemd (`StartTransientUnit`) and to the document
//! portal (`AddFull`). Both are on the user's session bus, with the stub outside the sandbox.

use super::conn::{Connection, DbusError};
use super::wire::Value;
use anyhow::{bail, ensure, Context, Result};
use std::os::fd::RawFd;
use std::time::Duration;

/// A systemd unit name for an app: `app-panpak.<id>-<n>.scope`, with `-` of the id written as
/// `\x2d` (the portal reads it back with `cunescape`). `id` must already be a valid app id.
pub fn scope_name(id: &str, n: &str) -> Result<String> {
    ensure!(!n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric()), "the start token must be letters and digits");
    ensure!(!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')), "app id with a character a unit name cannot hold");
    let esc: String = id.chars().map(|c| if c == '-' { "\\x2d".to_string() } else { c.to_string() }).collect();
    let name = format!("app-panpak.{esc}-{n}.scope");
    ensure!(name.len() <= 200, "unit name too long");
    Ok(name)
}

/// The id the portal reads back from a unit name made by `scope_name`: the same parse as
/// `_xdp_app_info_host_parse_app_id_from_unit_name` (xdg-desktop-portal 1.21), written out by hand
/// so that a test can check the round trip without a regex library.
pub fn portal_id_from_unit(unit: &str) -> Option<String> {
    let rest = unit.strip_prefix("app-")?;
    let body = rest.strip_suffix(".scope").or_else(|| rest.strip_suffix(".slice"))?;
    // The launcher group `[[:alnum:]]+-` is taken only when the characters before the first `-`
    // are all letters and digits, and something follows; our ids start with `panpak.`, which has a
    // dot, so the group never matches. The id ends before the last `-alnum*`.
    let launcher = body.find('-').filter(|i| *i > 0 && body[..*i].bytes().all(|b| b.is_ascii_alphanumeric()));
    let after = match launcher {
        Some(i) => &body[i + 1..],
        None => body,
    };
    let cut = after.rfind('-')?;
    if !after[cut + 1..].bytes().all(|b| b.is_ascii_alphanumeric()) || cut == 0 {
        return None;
    }
    let raw = &after[..cut];
    // cunescape: `\xHH`
    let mut out = String::new();
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && b.get(i + 1) == Some(&b'x') {
            let h = raw.get(i + 2..i + 4)?;
            out.push(u8::from_str_radix(h, 16).ok()? as char);
            i += 4;
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    Some(out)
}

/// Start the transient scope `unit` and move `pid` into it. Returns when systemd says the job
/// is done. `slice` is the slice to put it in (`app.slice`), tried first and dropped if the
/// manager refuses it.
pub fn start_scope(conn: &mut Connection, unit: &str, description: &str, pid: u32, slice: Option<&str>) -> Result<()> {
    const SD: &str = "org.freedesktop.systemd1";
    conn.call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "AddMatch",
        vec![Value::str("type='signal',sender='org.freedesktop.systemd1',interface='org.freedesktop.systemd1.Manager',member='JobRemoved',path='/org/freedesktop/systemd1'")],
        vec![],
    )?;
    let prop = |k: &str, v: Value| Value::Struct(vec![Value::str(k), Value::variant(v)]);
    let mut last = None;
    for with_slice in [slice.is_some(), false] {
        let mut props = vec![prop("Description", Value::str(description)), prop("PIDs", Value::Array("u".into(), vec![Value::U32(pid)]))];
        if with_slice {
            props.push(prop("Slice", Value::str(slice.unwrap_or("app.slice"))));
        }
        let args = vec![Value::str(unit), Value::str("fail"), Value::Array("(sv)".into(), props), Value::Array("(sa(sv))".into(), vec![])];
        match conn.call(SD, "/org/freedesktop/systemd1", "org.freedesktop.systemd1.Manager", "StartTransientUnit", args, vec![]) {
            Ok(r) => {
                let Some(Value::Path(job)) = r.body.first() else { bail!("StartTransientUnit: unexpected reply") };
                let job = job.clone();
                let sig = conn.wait_signal(Duration::from_secs(10), |m| {
                    m.member.as_deref() == Some("JobRemoved") && matches!(m.body.get(1), Some(Value::Path(p)) if *p == job)
                })?;
                let Some(sig) = sig else { bail!("systemd did not finish the job for {unit} in 10 s") };
                match sig.body.get(3) {
                    Some(Value::Str(res)) if res == "done" => return Ok(()),
                    Some(Value::Str(res)) => bail!("systemd job for {unit} ended with `{res}`"),
                    _ => bail!("JobRemoved: unexpected body"),
                }
            }
            Err(e) => {
                // A slice that does not exist is the only failure worth a second try.
                let retry = with_slice && e.downcast_ref::<DbusError>().is_some();
                last = Some(e);
                if !retry {
                    break;
                }
            }
        }
    }
    Err(last.context("StartTransientUnit failed")?)
}

/// What the stub asks of the document portal for one file: the `permissions` strings are among
/// `read`, `write`, `grant-permissions`, `delete`.
pub struct DocumentGrant<'a> {
    pub app_id: &'a str,
    pub permissions: &'a [&'a str],
    /// Keep the grant after the app ends (flag 2).
    pub persistent: bool,
}

/// Put the file behind `fd` (an `O_PATH` descriptor) in the document portal for `app_id` and give
/// that app the permissions. Returns the document id.
pub fn grant_document(conn: &mut Connection, fd: RawFd, g: &DocumentGrant<'_>) -> Result<String> {
    for p in g.permissions {
        ensure!(matches!(*p, "read" | "write" | "grant-permissions" | "delete"), "unknown document permission `{p}`");
    }
    ensure!(super::wire::valid_dotted_name(g.app_id, false), "app id for the document portal is not a D-Bus name");
    let flags = 1u32 | if g.persistent { 2 } else { 0 };
    let args = vec![
        Value::Array("h".into(), vec![Value::Fd(0)]),
        Value::U32(flags),
        Value::str(g.app_id),
        Value::Array("s".into(), g.permissions.iter().map(|p| Value::str(p)).collect()),
    ];
    let r = conn.call("org.freedesktop.portal.Documents", "/org/freedesktop/portal/documents", "org.freedesktop.portal.Documents", "AddFull", args, vec![fd])?;
    match r.body.first() {
        Some(Value::Array(_, ids)) if ids.len() == 1 => match &ids[0] {
            Value::Str(id) if !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric()) => Ok(id.clone()),
            _ => bail!("AddFull: unexpected document id"),
        },
        _ => bail!("AddFull: unexpected reply"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names_read_back_as_the_id_the_portal_will_use() {
        for id in ["org.kde.kdenlive", "io.github.my-app_x.Y", "com.example.a-b-c", "org.x.app-2", "org.my_app.Test_1", "net.sf.xyz-1-2", "a.b"] {
            let u = scope_name(id, "0a1b2c3d").unwrap();
            assert!(u.starts_with("app-panpak.") && u.ends_with("-0a1b2c3d.scope"), "{u}");
            assert_eq!(portal_id_from_unit(&u).as_deref(), Some(format!("panpak.{id}").as_str()), "{u}");
        }
    }

    #[test]
    fn bad_unit_inputs_are_refused() {
        assert!(scope_name("a.b", "").is_err());
        assert!(scope_name("a.b", "x-y").is_err());
        assert!(scope_name("a.b", "x_y").is_err());
        assert!(scope_name("a/b", "x").is_err());
        assert!(scope_name("a b", "x").is_err());
        assert!(scope_name(&format!("a.{}", "b".repeat(300)), "x").is_err());
    }

    #[test]
    fn a_unit_without_our_prefix_is_not_ours() {
        // a terminal's scope: the portal would read the terminal's id, never ours
        assert_eq!(portal_id_from_unit("app-gnome-org.gnome.Ptyxis-123.scope").as_deref(), Some("org.gnome.Ptyxis"));
        assert_eq!(portal_id_from_unit("app-ghostty-surface-transient-123.scope").as_deref(), Some("surface-transient"));
        assert_eq!(portal_id_from_unit("session-3.scope"), None);
    }
}
