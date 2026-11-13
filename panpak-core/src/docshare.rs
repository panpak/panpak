// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sharing files of the user with an app through the document portal (docs/FILE-CHOOSER.md, sections 3.1, 3.5 and 4). This runs on the host side, in the
//! outer stub (or in `panpak remove`), never in the confined router: it opens each file itself, does not follow a final link, and gives the app only what
//! the user chose, as `read` and, when the user can write the file, `write`. `grant-permissions` and `delete` are never given.

use crate::dbus::{Connection, Value};
use anyhow::{bail, ensure, Context, Result};
use std::{
    ffi::CString,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::ffi::OsStrExt,
    },
    path::{Component, Path, PathBuf},
};

pub const DOCUMENTS: &str = "org.freedesktop.portal.Documents";
const DOCS_PATH: &str = "/org/freedesktop/portal/documents";
/// The same file gets the same document each time, and the grant outlives the session.
const REUSE: u32 = 1;
const PERSISTENT: u32 = 2;
const EXPORT_DIRECTORY: u32 = 8;

/// What the user sees when the file they chose is a link (it may be one the app made).
pub const LINK_MESSAGE: &str = "the file you chose is a link: choose the original file";

/// A file or folder opened for sharing: an `O_PATH` descriptor, its name, and what the user may do with it.
#[derive(Debug)]
pub struct Opened {
    pub fd: OwnedFd,
    pub name: Vec<u8>,
    pub is_dir: bool,
    /// The user can write it (the portal decides again from the descriptor).
    pub writable: bool,
}

fn cstr(p: &[u8]) -> Result<CString> {
    CString::new(p).context("the path has a NUL byte")
}

fn open_path_at(dir: RawFd, name: &[u8], extra: libc::c_int) -> std::io::Result<OwnedFd> {
    let c = CString::new(name).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: a valid C string; the descriptor is taken over by OwnedFd only when it is not negative.
    let fd = unsafe { libc::openat(dir, c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC | extra) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn fstat(fd: RawFd) -> std::io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: st is valid for writing; an O_PATH descriptor can be stat'ed.
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(st)
}

/// Why a path cannot be shared, in words for the user.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// A link in a folder the app can write is how the app could make the user's choice point at another file. Walk the folder part of the path: a link
/// is accepted only when the folder that holds it is not under a place the app can write (`writable_roots`).
fn check_parent_links(parent: &Path, writable_roots: &[PathBuf]) -> Result<()> {
    let mut cur = PathBuf::new();
    for c in parent.components() {
        let holder = cur.clone();
        cur.push(c);
        if matches!(c, Component::RootDir | Component::Prefix(_)) {
            continue;
        }
        let is_link = std::fs::symlink_metadata(&cur).map(|m| m.file_type().is_symlink()).unwrap_or(false);
        if is_link && writable_roots.iter().any(|r| holder.starts_with(r)) {
            return Err(Refused(LINK_MESSAGE.into()).into());
        }
    }
    Ok(())
}

/// Open the parent folder of `path` (`O_PATH | O_DIRECTORY`) and return it with the final name.
pub fn open_parent(path: &Path, writable_roots: &[PathBuf]) -> Result<(OwnedFd, Vec<u8>)> {
    ensure!(path.is_absolute(), "the path is not absolute");
    let name = path.file_name().context("the path has no file name")?.as_bytes().to_vec();
    let parent = path.parent().context("the path has no folder")?;
    check_parent_links(parent, writable_roots)?;
    let p = cstr(parent.as_os_str().as_bytes())?;
    // SAFETY: a valid C string.
    let fd = unsafe { libc::open(p.as_ptr(), libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("cannot open the folder {}", parent.display()));
    }
    // SAFETY: fd is a fresh descriptor.
    Ok((unsafe { OwnedFd::from_raw_fd(fd) }, name))
}

/// Open an existing file or folder the way section 4 of the design says: the final component is not followed. A regular file or a folder only.
pub fn open_existing(path: &Path, writable_roots: &[PathBuf]) -> Result<Opened> {
    let (dir, name) = open_parent(path, writable_roots)?;
    let fd = match open_path_at(dir.as_raw_fd(), &name, libc::O_NOFOLLOW) {
        Ok(fd) => fd,
        Err(e) => return Err(e).with_context(|| format!("cannot open {}", path.display())),
    };
    let st = fstat(fd.as_raw_fd())?;
    let kind = st.st_mode & libc::S_IFMT;
    if kind == libc::S_IFLNK {
        return Err(Refused(LINK_MESSAGE.into()).into());
    }
    ensure!(kind == libc::S_IFREG || kind == libc::S_IFDIR, "{} is not a file or a folder", path.display());
    let proc_path = cstr(format!("/proc/self/fd/{}", fd.as_raw_fd()).as_bytes())?;
    // SAFETY: a valid C string. The magic link of an O_PATH descriptor leads to the file itself.
    let writable = unsafe { libc::access(proc_path.as_ptr(), libc::W_OK) == 0 };
    // The document portal refuses a descriptor opened with O_NOFOLLOW ("Invalid fd passed", found with the real portal). The file was checked above, so the
    // descriptor given to the portal is a second one to the same file, made from the first (no name is looked up again, so there is no race).
    // SAFETY: a valid C string; the descriptor is taken over below.
    let again = unsafe { libc::open(proc_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if again < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("cannot reopen {}", path.display()));
    }
    // SAFETY: again is a fresh descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(again) };
    Ok(Opened { fd, name, is_dir: kind == libc::S_IFDIR, writable })
}

fn permissions(write: bool) -> Value {
    let mut p = vec![Value::str("read")];
    if write {
        p.push(Value::str("write"));
    }
    Value::Array("s".into(), p)
}

fn doc_id(v: Option<&Value>) -> Result<String> {
    match v {
        Some(Value::Str(id)) if !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric()) => Ok(id.clone()),
        _ => bail!("the document portal gave an id that is not one"),
    }
}

/// The version of the document portal, or an error when it is not there.
pub fn documents_version(conn: &mut Connection) -> Result<u32> {
    let r = conn.call(DOCUMENTS, DOCS_PATH, "org.freedesktop.DBus.Properties", "Get", vec![Value::str(DOCUMENTS), Value::str("version")], vec![])?;
    match r.body.first() {
        Some(Value::Variant(v)) => match **v {
            Value::U32(n) => Ok(n),
            _ => bail!("the version of the document portal is not a number"),
        },
        _ => bail!("the version of the document portal was not given"),
    }
}

/// The calls to the document portal for one app.
pub struct Docs<'a> {
    pub conn: &'a mut Connection,
    pub version: u32,
    /// `panpak.<id>`: the identity the portals know the app by.
    pub app_id: &'a str,
}

impl Docs<'_> {
    /// Share files and folders that exist. `want_write`: also give `write` where the user can write (the app may lower this, never raise it).
    /// Returns the document id of each, in the order given.
    pub fn share_existing(&mut self, items: &[Opened], want_write: bool) -> Result<Vec<String>> {
        ensure!(self.version >= 2, "the document portal is too old (version {}): AddFull needs 2", self.version);
        if items.iter().any(|i| i.is_dir) {
            ensure!(self.version >= 4, "the document portal is too old (version {}) to share a folder: it needs 4", self.version);
        }
        let mut ids = vec![String::new(); items.len()];
        // One call for each kind of grant: the permissions and the flags are the same for all the descriptors of a call.
        for dir in [false, true] {
            for write in [false, true] {
                let group: Vec<usize> = (0..items.len()).filter(|&i| items[i].is_dir == dir && (want_write && items[i].writable) == write).collect();
                if group.is_empty() {
                    continue;
                }
                let flags = REUSE | PERSISTENT | if dir { EXPORT_DIRECTORY } else { 0 };
                let fds: Vec<RawFd> = group.iter().map(|&i| items[i].fd.as_raw_fd()).collect();
                let args = vec![
                    Value::Array("h".into(), (0..fds.len() as u32).map(Value::Fd).collect()),
                    Value::U32(flags),
                    Value::str(self.app_id),
                    permissions(write),
                ];
                let r = self.conn.call(DOCUMENTS, DOCS_PATH, DOCUMENTS, "AddFull", args, fds).context("AddFull")?;
                let Some(Value::Array(_, got)) = r.body.first() else { bail!("AddFull: unexpected reply") };
                ensure!(got.len() == group.len(), "AddFull gave {} ids for {} files", got.len(), group.len());
                for (k, &i) in group.iter().enumerate() {
                    ids[i] = doc_id(got.get(k))?;
                }
            }
        }
        Ok(ids)
    }

    /// Share a name in a folder that may not have the file yet (a file to save): the app creates it with its first write.
    pub fn share_new(&mut self, parent: &OwnedFd, name: &[u8]) -> Result<String> {
        ensure!(self.version >= 3, "the document portal is too old (version {}): AddNamedFull needs 3", self.version);
        ensure!(!name.is_empty() && name.len() <= 255 && !name.contains(&b'/') && !name.contains(&0), "the name is not a file name");
        let mut bytes: Vec<Value> = name.iter().map(|b| Value::Byte(*b)).collect();
        bytes.push(Value::Byte(0));
        let args = vec![Value::Fd(0), Value::Array("y".into(), bytes), Value::U32(REUSE | PERSISTENT), Value::str(self.app_id), permissions(true)];
        let r = self.conn.call(DOCUMENTS, DOCS_PATH, DOCUMENTS, "AddNamedFull", args, vec![parent.as_raw_fd()]).context("AddNamedFull")?;
        doc_id(r.body.first())
    }
}

/// The documents the portal holds for an app: their ids.
pub fn list_documents(conn: &mut Connection, app_id: &str) -> Result<Vec<String>> {
    let r = conn.call(DOCUMENTS, DOCS_PATH, DOCUMENTS, "List", vec![Value::str(app_id)], vec![])?;
    let Some(Value::Array(_, entries)) = r.body.first() else { bail!("List: unexpected reply") };
    let mut ids = Vec::new();
    for e in entries {
        if let Value::Entry(k, _) = e {
            ids.push(doc_id(Some(k))?);
        }
    }
    Ok(ids)
}

/// Take every permission of `app_id` away from a document.
pub fn revoke(conn: &mut Connection, id: &str, app_id: &str) -> Result<()> {
    let all = Value::Array("s".into(), ["read", "write", "grant-permissions", "delete"].iter().map(|p| Value::str(p)).collect());
    conn.call(DOCUMENTS, DOCS_PATH, DOCUMENTS, "RevokePermissions", vec![Value::str(id), Value::str(app_id), all], vec![])?;
    Ok(())
}

/// True if an app other than `app_id` holds a permission on the document (read from the permission store).
pub fn held_by_others(conn: &mut Connection, id: &str, app_id: &str) -> Result<bool> {
    let r = conn.call(
        "org.freedesktop.impl.portal.PermissionStore",
        "/org/freedesktop/impl/portal/PermissionStore",
        "org.freedesktop.impl.portal.PermissionStore",
        "Lookup",
        vec![Value::str("documents"), Value::str(id)],
        vec![],
    )?;
    let Some(Value::Array(_, entries)) = r.body.first() else { bail!("Lookup: unexpected reply") };
    Ok(entries.iter().any(|e| matches!(e, Value::Entry(k, v) if matches!(&**k, Value::Str(a) if a != app_id) && matches!(&**v, Value::Array(_, p) if !p.is_empty()))))
}

/// Delete a document from the store (host callers have the right).
pub fn delete(conn: &mut Connection, id: &str) -> Result<()> {
    conn.call(DOCUMENTS, DOCS_PATH, DOCUMENTS, "Delete", vec![Value::str(id)], vec![])?;
    Ok(())
}

/// For `panpak remove`: revoke every document of the app; with `purge`, delete those that no other app holds. Returns the number revoked.
pub fn revoke_app(conn: &mut Connection, app_id: &str, purge: bool) -> Result<usize> {
    let ids = list_documents(conn, app_id)?;
    for id in &ids {
        revoke(conn, id, app_id).with_context(|| format!("cannot revoke document {id}"))?;
        if purge && !held_by_others(conn, id, app_id).unwrap_or(true) {
            delete(conn, id).with_context(|| format!("cannot delete document {id}"))?;
        }
    }
    Ok(ids.len())
}

/// A name for the log: lossy, no control characters.
pub fn shown(name: &[u8]) -> String {
    String::from_utf8_lossy(name).chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn scratch(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-docshare-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_file_a_folder_and_a_missing_name() {
        let d = scratch("open");
        std::fs::write(d.join("a.txt"), "x").unwrap();
        std::fs::create_dir(d.join("sub")).unwrap();
        let f = open_existing(&d.join("a.txt"), &[]).unwrap();
        assert!(!f.is_dir && f.writable && f.name == b"a.txt");
        let s = open_existing(&d.join("sub"), &[]).unwrap();
        assert!(s.is_dir);
        assert!(open_existing(&d.join("missing"), &[]).is_err());
        let (parent, name) = open_parent(&d.join("new.txt"), &[]).unwrap();
        assert_eq!(name, b"new.txt");
        assert!(fstat(parent.as_raw_fd()).is_ok());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_final_link_is_refused_with_the_message() {
        let d = scratch("link");
        std::fs::write(d.join("real.txt"), "x").unwrap();
        symlink(d.join("real.txt"), d.join("notes.txt")).unwrap();
        symlink(&d, d.join("loop")).unwrap();
        for l in ["notes.txt", "loop"] {
            let e = open_existing(&d.join(l), &[]).unwrap_err();
            assert_eq!(e.to_string(), LINK_MESSAGE, "{l}");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_link_in_a_parent_folder_is_accepted_unless_the_app_can_write_there() {
        let d = scratch("parent");
        std::fs::create_dir(d.join("real")).unwrap();
        std::fs::write(d.join("real/f.txt"), "x").unwrap();
        symlink(d.join("real"), d.join("via")).unwrap();
        // The link is in a folder the app cannot write: fine (a system or user setup, such as ~/Documents -> /data/docs).
        assert!(open_existing(&d.join("via/f.txt"), &[]).is_ok());
        assert!(open_existing(&d.join("via/f.txt"), &[PathBuf::from("/nonexistent")]).is_ok());
        // The app can write in the folder that holds the link: refused.
        let e = open_existing(&d.join("via/f.txt"), std::slice::from_ref(&d)).unwrap_err();
        assert_eq!(e.to_string(), LINK_MESSAGE);
        // A link made in a folder the app cannot write is not its doing.
        assert!(open_existing(&d.join("real/f.txt"), std::slice::from_ref(&d)).is_ok());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn what_is_not_a_file_or_a_folder_is_refused() {
        let e = open_existing(Path::new("/dev/null"), &[]).unwrap_err();
        assert!(e.to_string().contains("not a file or a folder"), "{e}");
        assert!(open_existing(Path::new("relative"), &[]).is_err());
        assert!(open_existing(Path::new("/"), &[]).is_err());
    }
}
