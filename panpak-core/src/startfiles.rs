// SPDX-License-Identifier: MIT OR Apache-2.0
//! Files given to an app at its start (a double click, "Open with", the command line): the outer stub shares them through the document portal before the app
//! runs and puts the document paths in their place (docs/FILE-CHOOSER.md, section 3.9). Nothing is done, and no bus is touched, when no argument is a file.

use crate::dbus::Connection;
use crate::docshare::{self, Docs, Opened, Refused};
use crate::filechooser::{self, MAX_FILES};
use anyhow::Result;
use std::{
    os::fd::OwnedFd,
    path::{Path, PathBuf},
};

/// What an argument is.
#[derive(Debug, PartialEq)]
pub enum Arg {
    /// Not a file: left as it is (options, addresses, words).
    Other,
    /// A file or folder that exists, written as a path (`as_uri` is false) or as a `file:` address.
    Existing { path: PathBuf, as_uri: bool },
    /// A name in a folder that exists, for a file the app will create.
    New { path: PathBuf, as_uri: bool },
}

fn looks_like_scheme(a: &str) -> bool {
    let Some((s, rest)) = a.split_once(':') else { return false };
    !s.is_empty() && s.as_bytes()[0].is_ascii_alphabetic() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b)) && !rest.is_empty() && !a.contains('/') || a.contains("://")
}

/// Decide what one argument is. `cwd` is where the stub was started. A name that does not exist is taken for a new file only when it is written as a path
/// (it has a `/`, as `./new.txt` or `/home/me/new.txt`) or as a `file:` address: a bare word such as `org.example.App` or `1.2.3` is something the app
/// understands, and is not turned into a document.
pub fn classify(arg: &str, cwd: &Path) -> Arg {
    if arg.starts_with('-') || arg.is_empty() || arg.contains('\0') {
        return Arg::Other;
    }
    if arg.starts_with("file://") {
        return match filechooser::file_uri_to_path(arg) {
            Ok(p) if p.exists() || std::fs::symlink_metadata(&p).is_ok() => Arg::Existing { path: p, as_uri: true },
            Ok(p) if p.parent().is_some_and(Path::is_dir) => Arg::New { path: p, as_uri: true },
            _ => Arg::Other,
        };
    }
    let p = Path::new(arg);
    let full = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
    // A link counts as existing: it is refused later, with the message, not taken for a new name.
    if std::fs::symlink_metadata(&full).is_ok() {
        return Arg::Existing { path: full, as_uri: false };
    }
    if looks_like_scheme(arg) || !arg.contains('/') || arg.chars().any(char::is_whitespace) {
        return Arg::Other;
    }
    match full.parent() {
        Some(parent) if parent.is_dir() => Arg::New { path: full, as_uri: false },
        _ => Arg::Other,
    }
}

/// True if any argument is a file for the stub to share.
pub fn any_file(args: &[String], cwd: &Path) -> bool {
    args.iter().any(|a| classify(a, cwd) != Arg::Other)
}

/// What came of the sharing: the new arguments, and a line for each argument that was dropped.
#[derive(Debug)]
pub struct Shared {
    pub args: Vec<String>,
    pub dropped: Vec<String>,
}

enum Item {
    Keep(String),
    Existing { idx: usize, as_uri: bool },
    New { parent: OwnedFd, name: Vec<u8>, as_uri: bool },
}

/// Share the files among `args` with the app `app_id` (`panpak.<id>`). `connect` opens the session bus (called once, only when there is something to share).
pub fn share(
    args: &[String],
    cwd: &Path,
    app_id: &str,
    runtime_dir: &str,
    writable_roots: &[PathBuf],
    connect: impl FnOnce() -> Result<Connection>,
) -> Shared {
    let mut dropped = Vec::new();
    let mut items: Vec<Item> = Vec::new();
    let mut opened: Vec<Opened> = Vec::new();
    let mut files = 0usize;
    for a in args {
        let kind = classify(a, cwd);
        if kind == Arg::Other {
            items.push(Item::Keep(a.clone()));
            continue;
        }
        files += 1;
        if files > MAX_FILES {
            dropped.push(format!("{a}: more than {MAX_FILES} files at once"));
            continue;
        }
        match kind {
            Arg::Existing { path, as_uri } => match docshare::open_existing(&path, writable_roots) {
                Ok(o) => {
                    opened.push(o);
                    items.push(Item::Existing { idx: opened.len() - 1, as_uri });
                }
                Err(e) => dropped.push(format!("{a}: {}", reason(&e))),
            },
            Arg::New { path, as_uri } => match docshare::open_parent(&path, writable_roots) {
                Ok((parent, name)) => items.push(Item::New { parent, name, as_uri }),
                Err(e) => dropped.push(format!("{a}: {}", reason(&e))),
            },
            Arg::Other => unreachable!(),
        }
    }
    if opened.is_empty() && !items.iter().any(|i| matches!(i, Item::New { .. })) {
        return Shared { args: kept(items), dropped };
    }
    let mut conn = match connect() {
        Ok(c) => c,
        Err(e) => {
            // No bus, no portal: the files cannot be given. They are dropped, not passed as host paths the view does not show.
            let why = crate::err_text(&e);
            dropped.push(format!("files were not shared: {why}"));
            return Shared { args: kept(items), dropped };
        }
    };
    let version = match docshare::documents_version(&mut conn) {
        Ok(v) => v,
        Err(e) => {
            dropped.push(format!("files were not shared: the document portal is not available ({})", crate::err_text(&e)));
            return Shared { args: kept(items), dropped };
        }
    };
    let mut docs = Docs { conn: &mut conn, version, app_id };
    let ids = if opened.is_empty() { Ok(Vec::new()) } else { docs.share_existing(&opened, true) };
    let ids = match ids {
        Ok(v) => v,
        Err(e) => {
            dropped.push(format!("files were not shared: {}", crate::err_text(&e)));
            return Shared { args: kept(items), dropped };
        }
    };
    let mut out = Vec::new();
    for i in items {
        match i {
            Item::Keep(a) => out.push(a),
            Item::Existing { idx, as_uri } => out.push(doc_arg(&ids[idx], &opened[idx].name, runtime_dir, as_uri)),
            Item::New { parent, name, as_uri } => match docs.share_new(&parent, &name) {
                Ok(id) => out.push(doc_arg(&id, &name, runtime_dir, as_uri)),
                Err(e) => dropped.push(format!("{}: {}", docshare::shown(&name), reason(&e))),
            },
        }
    }
    Shared { args: out, dropped }
}

/// The path form is the plain path (not percent-encoded); the address form is encoded.
fn doc_arg(id: &str, name: &[u8], runtime_dir: &str, as_uri: bool) -> String {
    use std::os::unix::ffi::OsStringExt;
    if as_uri {
        filechooser::document_uri(runtime_dir, id, name)
    } else {
        let mut p = PathBuf::from(runtime_dir);
        p.push("doc");
        p.push(id);
        p.push(std::ffi::OsString::from_vec(name.to_vec()));
        p.to_string_lossy().into_owned()
    }
}

fn kept(items: Vec<Item>) -> Vec<String> {
    items.into_iter().filter_map(|i| if let Item::Keep(a) = i { Some(a) } else { None }).collect()
}

fn reason(e: &anyhow::Error) -> String {
    if let Some(r) = e.downcast_ref::<Refused>() {
        r.0.clone()
    } else {
        crate::err_text(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-startfiles-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn which_arguments_are_files() {
        let d = scratch("classify");
        std::fs::write(d.join("a.txt"), "x").unwrap();
        std::fs::create_dir(d.join("sub")).unwrap();
        std::os::unix::fs::symlink(d.join("a.txt"), d.join("ln")).unwrap();
        let c = |a: &str| classify(a, &d);
        assert_eq!(c("a.txt"), Arg::Existing { path: d.join("a.txt"), as_uri: false });
        assert_eq!(c("sub"), Arg::Existing { path: d.join("sub"), as_uri: false });
        assert_eq!(c(&format!("{}/a.txt", d.display())), Arg::Existing { path: d.join("a.txt"), as_uri: false });
        assert_eq!(c(&format!("file://{}/a%2Etxt", d.display())), Arg::Existing { path: d.join("a.txt"), as_uri: true });
        assert_eq!(c("ln"), Arg::Existing { path: d.join("ln"), as_uri: false }, "a link is recognised, and refused later");
        // A name written as a path that does not exist yet, in a folder that does.
        assert_eq!(c("./new.txt"), Arg::New { path: d.join("./new.txt"), as_uri: false });
        assert_eq!(c("sub/new.md"), Arg::New { path: d.join("sub/new.md"), as_uri: false });
        assert_eq!(c(&format!("file://{}/new.md", d.display())), Arg::New { path: d.join("new.md"), as_uri: true });
        // Left alone: options, addresses, words, names in a folder that is not there, spaces.
        for a in ["--new-window", "-v", "", "https://example.org/a.txt", "mailto:a@b.org", "word", "+list-fonts", "nodir/x.txt", "two words.txt", "--config=/etc/passwd", "new.txt", "org.example.Nobody", "1.2.3"] {
            assert_eq!(c(a), Arg::Other, "{a:?}");
        }
        assert!(any_file(&["--x".into(), "a.txt".into()], &d));
        assert!(!any_file(&["--x".into(), "https://example.org".into()], &d));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn no_file_no_bus() {
        let d = scratch("nobus");
        let args: Vec<String> = vec!["--new-window".into(), "https://example.org".into()];
        let r = share(&args, &d, "panpak.x.y", "/run/user/1000", &[], || panic!("the bus must not be touched"));
        assert_eq!((r.args, r.dropped), (args, Vec::<String>::new()));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn without_a_bus_the_files_are_dropped_with_a_message_not_passed_as_host_paths() {
        let d = scratch("dropped");
        std::fs::write(d.join("a.txt"), "x").unwrap();
        let args: Vec<String> = vec!["--new-window".into(), "a.txt".into(), "nofile".into()];
        let r = share(&args, &d, "panpak.x.y", "/run/user/1000", &[], || anyhow::bail!("no session bus"));
        assert_eq!(r.args, ["--new-window", "nofile"]);
        assert!(r.dropped.iter().any(|m| m.contains("not shared") && m.contains("no session bus")), "{:?}", r.dropped);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_link_is_dropped_with_the_message_before_the_bus_is_asked() {
        let d = scratch("link");
        std::fs::write(d.join("a.txt"), "x").unwrap();
        std::os::unix::fs::symlink(d.join("a.txt"), d.join("ln")).unwrap();
        let r = share(&["ln".to_string()], &d, "panpak.x.y", "/run/user/1000", &[], || panic!("nothing to share, no bus"));
        assert!(r.args.is_empty());
        assert_eq!(r.dropped, [format!("ln: {}", docshare::LINK_MESSAGE)]);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
