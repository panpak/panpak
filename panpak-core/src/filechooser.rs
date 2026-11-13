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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::encode_message;

    fn entry(k: &str, v: Value) -> Value {
        Value::Entry(Box::new(Value::str(k)), Box::new(Value::variant(v)))
    }

    fn bytes(s: &[u8]) -> Value {
        Value::Array("y".into(), s.iter().map(|b| Value::Byte(*b)).collect())
    }

    fn raw(method: &str, parent: &str, opts: Vec<Value>) -> Vec<u8> {
        let m = Message {
            kind: CALL,
            serial: 5,
            path: Some("/org/freedesktop/portal/desktop".into()),
            interface: Some(IFACE.into()),
            member: Some(method.into()),
            destination: Some("org.freedesktop.portal.Desktop".into()),
            signature: "ssa{sv}".into(),
            body: vec![Value::str(parent), Value::str("Title"), Value::Array("{sv}".into(), opts)],
            ..Default::default()
        };
        encode_message(&m).unwrap()
    }

    const DOC: &str = "/run/user/1000/doc/";

    #[test]
    fn a_call_keeps_the_known_options_of_its_method_and_nothing_else() {
        let opts = vec![
            entry("modal", Value::Bool(true)),
            entry("multiple", Value::Bool(true)),
            entry("directory", Value::Bool(true)),
            entry("current_name", Value::str("x.txt")),
            entry("evil", Value::str("x")),
            entry("accept_label", Value::U32(3)),
        ];
        let c = parse_call(&raw("OpenFile", "x11:1f", opts.clone()), DOC).unwrap();
        let keys: Vec<_> = c.opts.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["modal", "multiple", "directory"], "current_name is for SaveFile, evil is unknown, accept_label has the wrong type");
        assert!(c.directory && c.writable);
        let s = parse_call(&raw("SaveFile", "wayland:abc", opts), DOC).unwrap();
        assert_eq!(s.opts.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["modal", "current_name"]);
        // The app can only lower `writable`.
        let w = parse_call(&raw("OpenFile", "", vec![entry("writable", Value::Bool(false))]), DOC).unwrap();
        assert!(!w.writable);
    }

    #[test]
    fn a_folder_the_app_names_must_be_a_place_it_already_sees() {
        let folder = |p: &str| parse_call(&raw("OpenFile", "", vec![entry("current_folder", bytes(p.as_bytes()))]), DOC).unwrap().opts.len();
        assert_eq!(folder("/run/user/1000/doc/ab12/dir\0"), 1);
        assert_eq!(folder("/home/user/secret\0"), 0);
        assert_eq!(folder("/run/user/1000/doc/../../../etc\0"), 0);
        assert_eq!(folder("/run/user/1000/doc/x\0y\0"), 0);
    }

    #[test]
    fn a_bad_call_is_refused() {
        for (m, parent) in [("OpenFile", "file:///etc"), ("OpenFile", "x11:"), ("OpenFile", "x11:a b"), ("Nope", "")] {
            assert!(parse_call(&raw(m, parent, vec![]), DOC).is_err(), "{m} {parent}");
        }
        assert!(parse_call(b"not a message", DOC).is_err());
        assert!(parse_call(&vec![0u8; MAX_CALL + 1], DOC).is_err());
        // Names for SaveFiles must be names.
        let bad = raw("SaveFiles", "", vec![entry("files", Value::Array("ay".into(), vec![bytes(b"a/b\0")]))]);
        assert!(parse_call(&bad, DOC).is_err());
        let good = raw("SaveFiles", "", vec![entry("files", Value::Array("ay".into(), vec![bytes(b"a.txt\0"), bytes(b"b.txt\0")]))]);
        assert_eq!(parse_call(&good, DOC).unwrap().names, [b"a.txt".to_vec(), b"b.txt".to_vec()]);
    }

    #[test]
    fn addresses_are_read_and_made_without_loss() {
        assert_eq!(file_uri_to_path("file:///a/b%20c/%C3%A9.txt").unwrap(), PathBuf::from("/a/b c/\u{e9}.txt"));
        assert_eq!(file_uri_to_path("file://localhost/x").unwrap(), PathBuf::from("/x"));
        for bad in ["file://other/x", "http://x/y", "file:///a%00b", "file:///a%2", "file:///a%zz", "file://", "/plain"] {
            assert!(file_uri_to_path(bad).is_err(), "{bad}");
        }
        use std::os::unix::ffi::OsStrExt;
        for p in ["/a/b c/\u{e9}.txt", "/x/%/y", "/with#hash?query"] {
            let uri = format!("file://{}", percent_encode(p.as_bytes()));
            assert!(uri.is_ascii());
            assert_eq!(file_uri_to_path(&uri).unwrap().as_os_str().as_bytes(), p.as_bytes());
        }
        assert_eq!(document_uri("/run/user/1000", "ab12", b"a b.txt"), "file:///run/user/1000/doc/ab12/a%20b.txt");
    }

    #[test]
    fn the_answer_is_checked_on_both_sides_of_the_frame() {
        let d = result_dict(&["file:///run/user/1000/doc/a/x".to_string()], &[("junk".into(), Value::str("x")), ("choices".into(), Value::Array("(ss)".into(), vec![]))]);
        let text = response_text(9, 0, Some(&d));
        let (t, code, back) = parse_response_text(&text).unwrap();
        assert_eq!((t, code), (9, 0));
        assert_eq!(back.as_ref().and_then(check_result_dict), back, "what is accepted is accepted again");
        // Closed or ended: a code and no dictionary; a success without one, or a failure with one, is not an answer.
        assert_eq!(parse_response_text(&response_text(3, 1, None)), Some((3, 1, None)));
        assert!(parse_response_text("3 0").is_none());
        assert!(parse_response_text(&format!("{} extra", text)).is_none());
        assert!(parse_response_text("x 0").is_none());
        // A dictionary with a key that is not on the list is refused.
        let evil = Value::Array("{sv}".into(), vec![entry("writable", Value::Bool(true))]);
        assert!(check_result_dict(&evil).is_none());
        // The payload of a request.
        let p = request_payload(77, b"call");
        assert_eq!(parse_request_payload(&p), Some((77, &b"call"[..])));
        assert!(parse_request_payload(b"abc").is_none());
    }

    /// Writes the seeds of the fuzz target when asked: `PANPAK_WRITE_SEEDS=<folder> cargo test -p panpak-core a_call_keeps`.
    #[test]
    fn seeds_for_the_fuzz_target() {
        let Some(dir) = std::env::var_os("PANPAK_WRITE_SEEDS") else { return };
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let opts = vec![entry("modal", Value::Bool(true)), entry("multiple", Value::Bool(true)), entry("accept_label", Value::str("Open")), entry("current_folder", bytes(b"/run/user/1000/doc/ab/x\0"))];
        std::fs::write(dir.join("open"), raw("OpenFile", "x11:1f", opts.clone())).unwrap();
        std::fs::write(dir.join("save"), raw("SaveFile", "wayland:abc", opts)).unwrap();
        let d = result_dict(&["file:///run/user/1000/doc/a/x".to_string()], &[]);
        std::fs::write(dir.join("answer"), response_text(1, 0, Some(&d))).unwrap();
        std::fs::write(dir.join("payload"), request_payload(5, &raw("OpenFile", "", vec![]))).unwrap();
        std::fs::write(dir.join("uri"), "file:///a/b%20c/x.txt").unwrap();
    }
}
