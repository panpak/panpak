// SPDX-License-Identifier: MIT OR Apache-2.0
//! The host side of the file dialog: ask the portal for the dialog as a plain client of the session bus, wait for the user, open what was chosen and share
//! it with the app through the document portal. Run by the actor in the outer stub, one dialog at a time per copy of the app.

use crate::dbus::{Connection, Message, Value};
use crate::docshare::{self, Docs, Refused};
use crate::filechooser::{self, Call, Method, MAX_FILES};
use anyhow::{bail, Context, Result};
use std::{path::PathBuf, time::Duration};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
/// The user takes as long as they take; an abandoned dialog ends with the app or the session.
const DIALOG_WAIT: Duration = Duration::from_secs(8 * 3600);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// What the actor knows about the app.
#[derive(Debug, Clone)]
pub struct HostCtx {
    /// `panpak.<id>`.
    pub app_id: String,
    /// `$XDG_RUNTIME_DIR` of the user: the documents appear under `<it>/doc` for the app.
    pub runtime_dir: String,
    /// Folders the app can write on the host (its own, and the user folders and home paths with `rw`): a link inside one of them is not trusted. Set by
    /// the stub when it has worked them out, before the app starts; until then the list is empty.
    pub writable_roots: std::sync::Arc<std::sync::OnceLock<Vec<PathBuf>>>,
}

impl HostCtx {
    pub fn roots(&self) -> &[PathBuf] {
        self.writable_roots.get().map_or(&[], Vec::as_slice)
    }
}

/// How a dialog ended.
#[derive(Debug)]
pub enum Outcome {
    /// The user chose: the `a{sv}` of the answer, with the addresses of the shared documents.
    Done(Value),
    /// The user closed the dialog (1) or it ended another way (2), with a line for the log and, when the user should read something, a message.
    Ended { code: u32, log: String, user: Option<String> },
}

fn random_token() -> Result<String> {
    use std::io::Read;
    let mut b = [0u8; 6];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(format!("panpak{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>()))
}

fn handle_of(conn: &Connection, token: &str) -> String {
    format!("/org/freedesktop/portal/desktop/request/{}/{token}", conn.unique_name.trim_start_matches(':').replace('.', "_"))
}

fn add_match(conn: &mut Connection, path: &str) -> Result<()> {
    let rule = format!("type='signal',interface='org.freedesktop.portal.Request',member='Response',path='{path}'");
    conn.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", vec![Value::str(&rule)], vec![])?;
    Ok(())
}

fn version_of_chooser(conn: &mut Connection) -> Result<u32> {
    let r = conn.call(PORTAL, PORTAL_PATH, "org.freedesktop.DBus.Properties", "Get", vec![Value::str(filechooser::IFACE), Value::str("version")], vec![])?;
    match r.body.first() {
        Some(Value::Variant(v)) => match **v {
            Value::U32(n) => Ok(n),
            _ => bail!("the version of FileChooser is not a number"),
        },
        _ => bail!("the version of FileChooser was not given"),
    }
}

fn owner_of(conn: &mut Connection, name: &str) -> Result<String> {
    let r = conn.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner", vec![Value::str(name)], vec![])?;
    match r.body.first() {
        Some(Value::Str(o)) if o.starts_with(':') => Ok(o.clone()),
        _ => bail!("GetNameOwner: unexpected reply"),
    }
}

fn entries(v: Option<&Value>) -> Vec<(String, Value)> {
    let Some(Value::Array(_, items)) = v else { return Vec::new() };
    items
        .iter()
        .filter_map(|e| match e {
            Value::Entry(k, v) => match (&**k, &**v) {
                (Value::Str(k), Value::Variant(inner)) => Some((k.clone(), (**inner).clone())),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn ended(code: u32, log: impl Into<String>) -> Outcome {
    Outcome::Ended { code, log: log.into(), user: None }
}

/// Show the dialog and share what the user chose. Never panics on what the portal says.
pub fn run(ctx: &HostCtx, call: &Call) -> Outcome {
    match run_inner(ctx, call) {
        Ok(o) => o,
        Err(e) => {
            let user = e.downcast_ref::<Refused>().map(|r| r.0.clone());
            Outcome::Ended { code: 2, log: format!("{e:#}"), user }
        }
    }
}

fn run_inner(ctx: &HostCtx, call: &Call) -> Result<Outcome> {
    let mut conn = Connection::session(CALL_TIMEOUT).context("cannot reach the session bus")?;
    let owner = owner_of(&mut conn, PORTAL).context("the desktop portal is not running")?;
    let version = version_of_chooser(&mut conn).context("the portal has no file chooser")?;
    if call.method == Method::SaveFiles && version < 4 {
        bail!("the portal's FileChooser is version {version}: SaveFiles needs 4");
    }
    let token = random_token()?;
    let mut handle = handle_of(&conn, &token);
    add_match(&mut conn, &handle)?;
    let r = conn.call(PORTAL, PORTAL_PATH, filechooser::IFACE, call.method.name(), filechooser::portal_args(call, &token), vec![])?;
    if let Some(Value::Path(h)) = r.body.first() {
        if *h != handle {
            handle = h.clone();
            add_match(&mut conn, &handle)?;
        }
    }
    let want = handle.clone();
    let sig: Option<Message> = conn.wait_signal(DIALOG_WAIT, |m| {
        m.interface.as_deref() == Some("org.freedesktop.portal.Request") && m.member.as_deref() == Some("Response") && m.path.as_deref() == Some(want.as_str()) && m.sender.as_deref() == Some(owner.as_str())
    })?;
    let Some(sig) = sig else { return Ok(ended(2, "the dialog was not answered in time")) };
    let (Some(Value::U32(code)), results) = (sig.body.first(), sig.body.get(1)) else { bail!("Response: unexpected body") };
    if *code != 0 {
        return Ok(ended((*code).min(2), format!("the dialog ended with code {code}")));
    }
    let results = entries(results);
    let uris: Vec<String> = match results.iter().find(|(k, _)| k == "uris") {
        Some((_, Value::Array(_, items))) => items.iter().filter_map(|i| if let Value::Str(s) = i { Some(s.clone()) } else { None }).collect(),
        _ => Vec::new(),
    };
    if uris.is_empty() {
        return Ok(ended(1, "the dialog gave no files"));
    }
    if uris.len() > MAX_FILES {
        bail!("the dialog gave {} files: at most {MAX_FILES}", uris.len());
    }
    let paths: Vec<PathBuf> = uris.iter().map(|u| filechooser::file_uri_to_path(u)).collect::<Result<_>>().context("the portal gave an address that is not a file")?;
    // What the app can already reach (under a folder it can write) is given as the real address: no document, nothing opened by the host.
    let mut out_uris: Vec<String> = vec![String::new(); paths.len()];
    let mut to_share: Vec<usize> = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        match docshare::under_roots(p, ctx.roots()) {
            Some(real) => out_uris[i] = docshare::real_uri(&real),
            None => to_share.push(i),
        }
    }
    if !to_share.is_empty() {
        let mut docs_conn = conn;
        let dv = docshare::documents_version(&mut docs_conn).context("the document portal is not running")?;
        let mut docs = Docs { conn: &mut docs_conn, version: dv, app_id: &ctx.app_id };
        match call.method {
            Method::OpenFile => {
                let items: Vec<docshare::Opened> = to_share.iter().map(|&i| docshare::open_existing(&paths[i], ctx.roots())).collect::<Result<_>>()?;
                let ids = docs.share_existing(&items, call.writable)?;
                for ((&i, it), id) in to_share.iter().zip(&items).zip(&ids) {
                    out_uris[i] = filechooser::document_uri(&ctx.runtime_dir, id, &it.name);
                }
            }
            Method::SaveFile | Method::SaveFiles => {
                for &i in &to_share {
                    let p = &paths[i];
                    let (parent, name) = docshare::open_parent(p, ctx.roots())?;
                    // An existing link under the chosen name is how an app would redirect a save: refused, as for an open.
                    if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
                        return Err(Refused(docshare::link_message(p)).into());
                    }
                    let id = docs.share_new(&parent, &name)?;
                    out_uris[i] = filechooser::document_uri(&ctx.runtime_dir, &id, &name);
                }
            }
        }
    }
    Ok(Outcome::Done(filechooser::result_dict(&out_uris, &results)))
}

/// Tell the user something in a notification of the desktop (a refused file, a failed argument). Best effort.
pub fn notify(summary: &str, body: &str) {
    let Ok(mut conn) = Connection::session(CALL_TIMEOUT) else { return };
    let args = vec![
        Value::str("Panpak"),
        Value::U32(0),
        Value::str("dialog-warning-symbolic"),
        Value::str(summary),
        Value::str(body),
        Value::Array("s".into(), Vec::new()),
        Value::Array("{sv}".into(), Vec::new()),
        Value::I32(10_000),
    ];
    let _ = conn.call("org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications", "Notify", args, vec![]);
}
