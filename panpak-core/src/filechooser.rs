// SPDX-License-Identifier: MIT OR Apache-2.0
//! The file dialog of the desktop for a sandboxed app (docs/FILE-CHOOSER.md). This module has the parts that need no process and no socket: reading the
//! call of the app, building what is asked of the portal, the answer that goes back to the app, and the words of the frames between the router and the
//! actor. The router is confined and reads untrusted bytes; the actor (the outer stub) parses the call again with `parse_call` and trusts nothing that
//! the router said.

use crate::dbus::{parse_message, Limits, Message, Reader, Value, Writer, CALL};
use anyhow::{bail, ensure, Context, Result};
use std::path::PathBuf;

pub const IFACE: &str = "org.freedesktop.portal.FileChooser";
/// Files in one answer.
pub const MAX_FILES: usize = 64;
/// The largest call of the app that is passed to the actor (a frame carries it).
pub const MAX_CALL: usize = 24 * 1024;
const MAX_TEXT: usize = 512;
const MAX_ARRAY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    OpenFile,
    SaveFile,
    SaveFiles,
}

impl Method {
    pub fn from_name(n: &str) -> Option<Method> {
        match n {
            "OpenFile" => Some(Method::OpenFile),
            "SaveFile" => Some(Method::SaveFile),
            "SaveFiles" => Some(Method::SaveFiles),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Method::OpenFile => "OpenFile",
            Method::SaveFile => "SaveFile",
            Method::SaveFiles => "SaveFiles",
        }
    }
}

/// The call of the app, reduced to what the actor passes on.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub method: Method,
    pub parent: String,
    pub title: String,
    /// The options that were kept, under their keys.
    pub opts: Vec<(String, Value)>,
    /// A folder is wanted (`OpenFile` with `directory`).
    pub directory: bool,
    /// The app did not ask to lower the access: the user's files are shared writable when the user can write them.
    pub writable: bool,
    /// For `SaveFiles`: the names to append to the folder (kept for the check; the portal gets them as given).
    pub names: Vec<Vec<u8>>,
}

/// The key, the signature of its value, and the methods it is kept for.
const OPTIONS: &[(&str, &str, &[Method])] = &[
    ("accept_label", "s", &[Method::OpenFile, Method::SaveFile, Method::SaveFiles]),
    ("modal", "b", &[Method::OpenFile, Method::SaveFile, Method::SaveFiles]),
    ("multiple", "b", &[Method::OpenFile]),
    ("directory", "b", &[Method::OpenFile]),
    ("writable", "b", &[Method::OpenFile]),
    ("filters", "a(sa(us))", &[Method::OpenFile, Method::SaveFile]),
    ("current_filter", "(sa(us))", &[Method::OpenFile, Method::SaveFile]),
    ("choices", "a(ssa(ss)s)", &[Method::OpenFile, Method::SaveFile, Method::SaveFiles]),
    ("current_name", "s", &[Method::SaveFile]),
    ("current_folder", "ay", &[Method::OpenFile, Method::SaveFile, Method::SaveFiles]),
    ("current_file", "ay", &[Method::SaveFile]),
    ("files", "aay", &[Method::SaveFiles]),
];

/// A parent window as the portal documents it: `x11:<hex>` or `wayland:<handle>`, or empty.
pub fn parent_ok(p: &str) -> bool {
    if p.is_empty() {
        return true;
    }
    let rest = p.strip_prefix("x11:").or_else(|| p.strip_prefix("wayland:"));
    rest.is_some_and(|r| !r.is_empty() && r.len() <= 128 && r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'))
}

fn bytes_of(v: &Value) -> Option<Vec<u8>> {
    let Value::Array(_, items) = v else { return None };
    items.iter().map(|b| if let Value::Byte(b) = b { Some(*b) } else { None }).collect()
}

fn path_text(v: &Value) -> Option<String> {
    let mut b = bytes_of(v)?;
    if b.last() == Some(&0) {
        b.pop();
    }
    String::from_utf8(b).ok().filter(|s| !s.contains('\0'))
}

/// Strings and arrays are bounded: the app cannot make the actor build a huge call.
fn bounded(v: &Value, depth: usize) -> bool {
    match v {
        Value::Str(s) => s.len() <= MAX_TEXT,
        Value::Array(_, items) => items.len() <= MAX_ARRAY && depth < 8 && items.iter().all(|i| bounded(i, depth + 1)),
        Value::Struct(items) => depth < 8 && items.iter().all(|i| bounded(i, depth + 1)),
        Value::Entry(k, v) => bounded(k, depth + 1) && bounded(v, depth + 1),
        Value::Variant(v) => depth < 8 && bounded(v, depth + 1),
        _ => true,
    }
}

/// Read the call of the app. `doc_prefix` is `<runtime dir>/doc/`: the only place a folder or file the app names may be (the places the app already sees).
pub fn parse_call(raw: &[u8], doc_prefix: &str) -> Result<Call> {
    ensure!(raw.len() <= MAX_CALL, "the call is too large");
    let m: Message = parse_message(raw, 0, &Limits::default()).context("the call is not a D-Bus message")?;
    ensure!(m.kind == CALL && m.interface.as_deref() == Some(IFACE), "not a FileChooser call");
    let method = m.member.as_deref().and_then(Method::from_name).context("unknown FileChooser method")?;
    ensure!(m.signature == "ssa{sv}", "the arguments of {} are (parent_window, title, options)", method.name());
    let (Some(Value::Str(parent)), Some(Value::Str(title)), Some(Value::Array(_, entries))) = (m.body.first(), m.body.get(1), m.body.get(2)) else {
        bail!("the arguments of {} are (parent_window, title, options)", method.name());
    };
    ensure!(parent_ok(parent), "the parent window is not x11:<id> or wayland:<handle>");
    ensure!(title.len() <= MAX_TEXT, "the title is too long");
    ensure!(entries.len() <= 64, "too many options");
    let mut call = Call { method, parent: parent.clone(), title: title.clone(), opts: Vec::new(), directory: false, writable: true, names: Vec::new() };
    for e in entries {
        let Value::Entry(k, v) = e else { continue };
        let (Value::Str(key), Value::Variant(inner)) = (&**k, &**v) else { continue };
        let Some((_, sig, methods)) = OPTIONS.iter().find(|(name, _, _)| name == key) else { continue };
        if !methods.contains(&method) || inner.signature() != *sig || !bounded(inner, 0) {
            continue;
        }
        match key.as_str() {
            "directory" => call.directory = matches!(**inner, Value::Bool(true)),
            "writable" => call.writable = !matches!(**inner, Value::Bool(false)),
            "current_folder" | "current_file" => {
                // Only a place the app sees already: a document path. A path of the host is dropped (the dialog starts where it likes).
                match path_text(inner) {
                    Some(p) if p.starts_with(doc_prefix) && !p.split('/').any(|s| s == "..") => {}
                    _ => continue,
                }
            }
            "files" => {
                let Value::Array(_, names) = &**inner else { continue };
                let list: Option<Vec<Vec<u8>>> = names
                    .iter()
                    .map(|n| {
                        let mut b = bytes_of(n)?;
                        if b.last() == Some(&0) {
                            b.pop();
                        }
                        (!b.is_empty() && b.len() <= 255 && !b.contains(&b'/') && !b.contains(&0) && b != b"." && b != b"..").then_some(b)
                    })
                    .collect();
                match list {
                    Some(l) => call.names = l,
                    None => bail!("a name in `files` is not a file name"),
                }
            }
            _ => {}
        }
        call.opts.push((key.clone(), (**inner).clone()));
    }
    Ok(call)
}

/// The arguments of the call to the portal: the parent, the title and the options that were kept, with the token of the actor.
pub fn portal_args(call: &Call, token: &str) -> Vec<Value> {
    let mut entries = vec![Value::Entry(Box::new(Value::str("handle_token")), Box::new(Value::variant(Value::str(token))))];
    for (k, v) in &call.opts {
        entries.push(Value::Entry(Box::new(Value::str(k)), Box::new(Value::variant(v.clone()))));
    }
    vec![Value::str(&call.parent), Value::str(&call.title), Value::Array("{sv}".into(), entries)]
}

// ---- URIs ------------------------------------------------------------------------------------------------------------

fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `file:///a/b%20c` as a host path. Only `file:` with an empty host or `localhost`; no NUL; absolute.
pub fn file_uri_to_path(uri: &str) -> Result<PathBuf> {
    let rest = uri.strip_prefix("file://").context("not a file: address")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => bail!("the address has no path"),
    };
    ensure!(host.is_empty() || host == "localhost", "the address names another host");
    let b = path.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let (Some(h), Some(l)) = (b.get(i + 1).copied().and_then(hexval), b.get(i + 2).copied().and_then(hexval)) else { bail!("bad percent escape in the address") };
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    ensure!(!out.contains(&0), "the path has a NUL byte");
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

/// Percent-encode bytes for a `file:` address: letters, digits and `-._~/` stay.
pub fn percent_encode(b: &[u8]) -> String {
    let mut s = String::new();
    for &c in b {
        if c.is_ascii_alphanumeric() || b"-._~/".contains(&c) {
            s.push(c as char);
        } else {
            s.push_str(&format!("%{c:02X}"));
        }
    }
    s
}

/// The address the app gets for a shared document: `file://<runtime dir>/doc/<id>/<name>`.
pub fn document_uri(runtime_dir: &str, id: &str, name: &[u8]) -> String {
    format!("file://{}/doc/{id}/{}", percent_encode(runtime_dir.as_bytes()), percent_encode(name))
}

// ---- The answer ------------------------------------------------------------------------------------------------------

/// The `a{sv}` of the Response signal: the addresses, and the `choices` and `current_filter` of the portal's answer when they have the right type.
pub fn result_dict(uris: &[String], portal: &[(String, Value)]) -> Value {
    let mut entries = vec![Value::Entry(Box::new(Value::str("uris")), Box::new(Value::variant(Value::Array("s".into(), uris.iter().map(|u| Value::str(u)).collect()))))];
    for (k, v) in portal {
        let want = match k.as_str() {
            "choices" => "a(ss)",
            "current_filter" => "(sa(us))",
            _ => continue,
        };
        if v.signature() == want && bounded(v, 0) {
            entries.push(Value::Entry(Box::new(Value::str(k)), Box::new(Value::variant(v.clone()))));
        }
    }
    Value::Array("{sv}".into(), entries)
}

/// A dictionary as the router wants it from the actor: only the keys above, with the right types, and at most `MAX_FILES` addresses.
pub fn check_result_dict(v: &Value) -> Option<Value> {
    let Value::Array(sig, entries) = v else { return None };
    if sig != "{sv}" || entries.len() > 3 {
        return None;
    }
    let mut uris = Vec::new();
    let mut portal = Vec::new();
    for e in entries {
        let Value::Entry(k, val) = e else { return None };
        let (Value::Str(k), Value::Variant(inner)) = (&**k, &**val) else { return None };
        match (k.as_str(), &**inner) {
            ("uris", Value::Array(_, items)) if items.len() <= MAX_FILES => {
                for i in items {
                    let Value::Str(s) = i else { return None };
                    uris.push(s.clone());
                }
            }
            ("choices" | "current_filter", _) => portal.push((k.clone(), (**inner).clone())),
            _ => return None,
        }
    }
    Some(result_dict(&uris, &portal))
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    s.as_bytes().chunks(2).map(|p| Some(hexval(p[0])? * 16 + hexval(p[1])?)).collect()
}

// ---- The frames ------------------------------------------------------------------------------------------------------

/// The payload of a request frame: the token of the router (4 bytes) and the call of the app, as it came.
pub fn request_payload(token: u32, call: &[u8]) -> Vec<u8> {
    let mut v = token.to_be_bytes().to_vec();
    v.extend_from_slice(call);
    v
}

pub fn parse_request_payload(p: &[u8]) -> Option<(u32, &[u8])> {
    let t: [u8; 4] = p.get(..4)?.try_into().ok()?;
    Some((u32::from_be_bytes(t), &p[4..]))
}

/// The text of the response frame: `<token> <code> <hex of the a{sv}>`. Code 0 is done, 1 the user closed the dialog, 2 anything else.
pub fn response_text(token: u32, code: u32, dict: Option<&Value>) -> String {
    let mut s = format!("{token} {code}");
    if let Some(d) = dict {
        let mut w = Writer::new();
        if w.value(d).is_ok() {
            s.push(' ');
            s.push_str(&crate::hex(&w.buf));
        }
    }
    s
}

/// Read the text of a response frame: the token, the code and the dictionary (checked).
pub fn parse_response_text(t: &str) -> Option<(u32, u32, Option<Value>)> {
    let mut it = t.split(' ');
    let token: u32 = it.next()?.parse().ok()?;
    let code: u32 = it.next()?.parse().ok()?;
    let dict = match it.next() {
        None => None,
        Some(h) => {
            let b = unhex(h)?;
            let mut r = Reader::new(&b, 0, false, 0, Limits::default());
            let v = r.value("a{sv}", 0).ok()?;
            Some(check_result_dict(&v)?)
        }
    };
    if it.next().is_some() || (code == 0) != dict.is_some() {
        return None;
    }
    Some((token, code, dict))
}
