// SPDX-License-Identifier: MIT OR Apache-2.0
//! The record of an installed version. PANPAK-FORMAT.md section 6.3.
//!
//! The store keeps one copy of the content of an app: the tree. What stays of the package is its signed tail: the compressed
//! index, the manifest, the signature and a footer, in a file with its own magic (`PANR`). The signed message is
//! `manifest || root`, where `root` is the hash of the raw index, so the signature of the package stays valid for the record
//! and `verify` reads it unchanged. The record has no stub and no chunks.
//!
//! A record is never a package: `read_footer` (what `install`, `update` and every reader of packages use) refuses it with
//! `PANPAK-E0010`, and `panpak-stub` runs a record only from `<store>/packages/`.
use crate::{coded, encode_footer, read_at, ErrorCode, Footer, FOOTER_LEN, MAGIC};
use anyhow::{ensure, Result};
use std::{fs::File, path::Path};

/// Magic of the footer of a record. Differs from `MAGIC` in the third byte, so that no reader of packages takes one for the other.
pub const RECORD_MAGIC: &[u8; 8] = b"PANR\0\x01\0\0";

/// What a reader of packages says when it is given a record.
pub const NOT_A_PACKAGE: &str = "this file is the record of an installed version, kept in the Panpak store: it holds the signed index and manifest, not the program or its files, so it cannot be installed or run from outside the store; install the .panpak package";

/// The record of the package `f` (whose footer is `ft`, already verified): index, manifest, signature, footer.
pub fn make_record(f: &mut File, ft: &Footer) -> Result<Vec<u8>> {
    let index = read_at(f, ft.index_off, ft.index_len)?;
    let size = f.metadata()?.len();
    let man_sig = read_at(f, ft.man_off, ft.man_len + crate::sig_block_len(size, ft))?;
    let (man, sig) = man_sig.split_at(ft.man_len as usize);
    Ok(record_bytes(&index, man, sig, ft.root))
}

/// The record from its parts, as `update` has them after it read the footer, manifest, signature block and index of a remote package.
pub fn record_bytes(index_comp: &[u8], manifest: &[u8], sig: &[u8], root: [u8; 32]) -> Vec<u8> {
    let footer = Footer { version: crate::FORMAT_VERSION, flags: crate::FORMAT_FLAGS, index_off: 0, index_len: index_comp.len() as u64, man_off: index_comp.len() as u64, man_len: manifest.len() as u64, root };
    let mut b = encode_footer(&footer);
    b[0..8].copy_from_slice(RECORD_MAGIC);
    let mut out = Vec::with_capacity(index_comp.len() + manifest.len() + sig.len() + b.len());
    out.extend_from_slice(index_comp);
    out.extend_from_slice(manifest);
    out.extend_from_slice(sig);
    out.extend_from_slice(&b);
    out
}

/// A package or a record, opened and verified (signature, root hash of the index): what every reader of installed apps needs.
pub struct Opened {
    pub file: File,
    pub footer: Footer,
    pub is_record: bool,
    pub manifest: crate::Manifest,
    pub raw_index: Vec<u8>,
}

/// Open `path` as a package or a record and verify it.
pub fn open_verified(path: &Path) -> Result<Opened> {
    let mut file = File::open(path).map_err(|e| anyhow::anyhow!("open {}: {e}", path.display()))?;
    let (footer, is_record) = read_any_footer(&mut file)?;
    let (manifest, raw_index) = crate::verify(&mut file, &footer)?;
    Ok(Opened { file, footer, is_record, manifest, raw_index })
}

/// Does this version carry the mark of a security update for the one that is installed? `[release].security` is the number of the latest revision
/// that fixed a vulnerability, so a version that has a `security` above the installed revision fixes something that the installed one still has.
/// A number and not a yes or no: a user who skipped a version does not lose the mark.
pub fn security_mark(version: &Opened, installed_revision: u64) -> bool {
    version.manifest.release.as_ref().is_some_and(|r| r.security > installed_revision)
}

/// The BLAKE3 hash of the message that the signature of the package (or record) at `path` covers: what a history names a release by.
pub fn signed_hash_of(path: &Path) -> Result<[u8; 32]> {
    let mut f = File::open(path).map_err(|e| anyhow::anyhow!("open {}: {e}", path.display()))?;
    let (ft, _) = read_any_footer(&mut f)?;
    let manifest = read_at(&mut f, ft.man_off, ft.man_len)?;
    Ok(crate::release::signed_hash(ft.version, ft.flags, &ft.root, &manifest))
}

/// Is the footer in `b` (the last 80 bytes of a file) that of a record?
pub fn is_record_footer(b: &[u8]) -> bool {
    b.len() == FOOTER_LEN as usize && &b[0..8] == RECORD_MAGIC
}

/// The footer of a record, from the last 80 bytes of the file. Same fields as the footer of a package.
pub fn parse_record_footer(size: u64, b: &[u8]) -> Result<Footer> {
    ensure!(size > FOOTER_LEN && b.len() == FOOTER_LEN as usize, "truncated record: damaged file, reinstall the app");
    ensure!(is_record_footer(b), "not a record");
    // The package parser reads the rest; give it the package magic so that it does its checks on version, flags and offsets.
    let mut p = b.to_vec();
    p[0..8].copy_from_slice(MAGIC);
    let ft = crate::parse_footer(size, &p)?;
    ensure!(ft.index_off == 0, "damaged record: the index must start the file");
    Ok(ft)
}

/// Open a file that is a package or a record: the footer, and whether it is a record.
pub fn read_any_footer(f: &mut File) -> Result<(Footer, bool)> {
    let size = f.metadata()?.len();
    ensure!(size > FOOTER_LEN, "file too short to be a .panpak or a record: incomplete or corrupt file");
    let b = read_at(f, size - FOOTER_LEN, FOOTER_LEN)?;
    if is_record_footer(&b) {
        Ok((parse_record_footer(size, &b)?, true))
    } else {
        Ok((crate::parse_footer(size, &b)?, false))
    }
}

/// The name of the record of `app_id` at `version` in `<store>/packages/`: `<id>@<version>@<root>.rec`, with the whole root (64 hex characters).
/// `@` is in neither an app id nor an escaped version, so the three parts can be told apart and two (id, version, root) never give one name; the
/// version keeps `A-Z a-z 0-9 . - _` and every other byte becomes `%XX`. A version so odd that the name would not fit in a file name is replaced by
/// the hash of the version. The root is not truncated: with 48 bits of it a package could be made to take another record's name.
pub fn record_name(app_id: &str, version: &str, root_hex: &str) -> String {
    let mut safe = String::new();
    for b in version.bytes() {
        if b.is_ascii_alphanumeric() || b".-_".contains(&b) {
            safe.push(b as char);
        } else {
            safe.push_str(&format!("%{b:02X}"));
        }
    }
    if app_id.len() + safe.len() + root_hex.len() + 6 > 250 {
        safe = format!("h{}", &blake3::hash(version.as_bytes()).to_hex()[..32]);
    }
    format!("{app_id}@{safe}@{root_hex}.rec")
}

/// A record is what its name says: the file name is the one `record_name` gives for the id, the version and the root of the record itself, and, when the
/// record is reached through `<store>/apps/<id>/<link>`, the id of that folder is the id of the manifest. A record that was put under another name, or
/// that another app's link names, is refused (PP-01): the pin and the permissions are those of the app that was started.
pub fn check_identity(given: &Path, man_id: &str, man_version: &str, root_hex: &str) -> Result<()> {
    let real = std::fs::canonicalize(given)?;
    let name = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name != record_name(man_id, man_version, root_hex) {
        return Err(coded(
            ErrorCode::E0010,
            format!("{}: the name of the record does not match what it holds (app {man_id}, version {man_version}): it was not written by `panpak install`; reinstall the app", real.display()),
        ));
    }
    let app_dir = given.parent();
    let in_apps = app_dir.and_then(|d| d.parent()).and_then(|a| a.file_name()).is_some_and(|n| n == "apps");
    if let (true, Some(id)) = (in_apps, app_dir.and_then(|d| d.file_name())) {
        if id != std::ffi::OsStr::new(man_id) {
            return Err(coded(
                ErrorCode::E1004,
                format!("{}: the link of {} names the record of another app ({man_id}); reinstall the app", given.display(), id.to_string_lossy()),
            ));
        }
    }
    Ok(())
}

/// A record runs only from the store: `path` must resolve to a file directly inside `<store>/packages/`.
pub fn ensure_in_store(path: &Path, store: &Path) -> Result<()> {
    let real = std::fs::canonicalize(path)?;
    let packages = std::fs::canonicalize(store.join("packages"))?;
    if real.parent() != Some(packages.as_path()) {
        return Err(coded(ErrorCode::E0010, format!("{}: a record of an installed version runs only from the folder packages of the store ({}); it is not a package", path.display(), store.display())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write, path::PathBuf};

    /// A package-shaped file with a given index and manifest: the layout the readers check (index, manifest, signature, footer),
    /// preceded by a few bytes standing for the stub and the chunks.
    fn package(dir: &Path) -> (PathBuf, Footer) {
        let index = vec![7u8; 100];
        let manifest = b"[app]\nid=\"a.b\"\n".to_vec();
        let mut body = b"STUBANDCHUNKS".to_vec();
        let index_off = body.len() as u64;
        body.extend_from_slice(&index);
        let man_off = body.len() as u64;
        body.extend_from_slice(&manifest);
        body.extend_from_slice(&crate::encode_sig_block(&[9u8; 64]));
        let ft = Footer { version: 1, flags: 0, index_off, index_len: index.len() as u64, man_off, man_len: manifest.len() as u64, root: [5u8; 32] };
        body.extend_from_slice(&encode_footer(&ft));
        let p = dir.join("p.panpak");
        fs::File::create(&p).unwrap().write_all(&body).unwrap();
        (p, ft)
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-record-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_record_is_the_tail_of_the_package_with_its_own_magic() {
        let d = tmp("tail");
        let (p, ft) = package(&d);
        let mut f = File::open(&p).unwrap();
        let rec = make_record(&mut f, &ft).unwrap();
        let rp = d.join("a.b-1.rec");
        fs::write(&rp, &rec).unwrap();
        let mut r = File::open(&rp).unwrap();
        let (rft, is_rec) = read_any_footer(&mut r).unwrap();
        assert!(is_rec);
        assert_eq!((rft.index_off, rft.index_len, rft.man_off, rft.man_len, rft.root), (0, ft.index_len, ft.index_len, ft.man_len, ft.root));
        // What the footer names is what the package held, byte for byte: the same layout check that `verify` runs passes.
        crate::check_layout(rec.len() as u64, &rft).unwrap();
        assert_eq!(read_at(&mut r, rft.index_off, rft.index_len).unwrap(), vec![7u8; 100]);
        assert_eq!(read_at(&mut r, rft.man_off, rft.man_len).unwrap(), b"[app]\nid=\"a.b\"\n");
        // A package is read as a package.
        let mut f = File::open(&p).unwrap();
        assert!(!read_any_footer(&mut f).unwrap().1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn readers_of_packages_refuse_a_record_with_a_clear_code() {
        let d = tmp("refuse");
        let (p, ft) = package(&d);
        let rec = make_record(&mut File::open(&p).unwrap(), &ft).unwrap();
        let rp = d.join("r.rec");
        fs::write(&rp, &rec).unwrap();
        let err = crate::read_footer(&mut File::open(&rp).unwrap()).unwrap_err();
        assert_eq!(crate::code_of(&err, ErrorCode::E0000), ErrorCode::E0010, "{err:#}");
        assert!(format!("{err:#}").contains("install the .panpak package"));
        // Not a record by accident: a package footer with the record magic changed in one byte is an ordinary bad magic.
        let mut bad = fs::read(&p).unwrap();
        let n = bad.len();
        bad[n - 80 + 3] ^= 1;
        fs::write(d.join("bad"), &bad).unwrap();
        let err = crate::read_footer(&mut File::open(d.join("bad")).unwrap()).unwrap_err();
        assert_ne!(crate::code_of(&err, ErrorCode::E0000), ErrorCode::E0010);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_record_runs_only_from_the_packages_folder_of_the_store() {
        let d = tmp("store");
        let store = d.join("store");
        fs::create_dir_all(store.join("packages")).unwrap();
        fs::create_dir_all(d.join("elsewhere")).unwrap();
        fs::write(store.join("packages/a.b-1.rec"), b"x").unwrap();
        fs::write(d.join("elsewhere/a.b-1.rec"), b"x").unwrap();
        ensure_in_store(&store.join("packages/a.b-1.rec"), &store).unwrap();
        // Through the `current` link, as the launcher does.
        fs::create_dir_all(store.join("apps/a.b")).unwrap();
        std::os::unix::fs::symlink(store.join("packages/a.b-1.rec"), store.join("apps/a.b/current")).unwrap();
        ensure_in_store(&store.join("apps/a.b/current"), &store).unwrap();
        // A copy somewhere else, and a link in the store to a file outside it, are refused.
        let e = ensure_in_store(&d.join("elsewhere/a.b-1.rec"), &store).unwrap_err();
        assert_eq!(crate::code_of(&e, ErrorCode::E0000), ErrorCode::E0010);
        std::os::unix::fs::symlink(d.join("elsewhere/a.b-1.rec"), store.join("apps/a.b/out")).unwrap();
        assert!(ensure_in_store(&store.join("apps/a.b/out"), &store).is_err());
        assert_eq!(record_name("a.b", "1.2+x y", "0123456789abcdef"), "a.b@1.2%2Bx%20y@0123456789abcdef.rec");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn no_two_records_get_the_same_name() {
        let root = "ab".repeat(32);
        // The id and the version may both hold `-`: these two used to be the same file name, so one install replaced the record of the other app.
        assert_ne!(record_name("org.a-1", "2", &root), record_name("org.a", "1-2", &root));
        // Versions that differ only in a character that is not safe in a file name.
        assert_ne!(record_name("org.a", "1+x", &root), record_name("org.a", "1~x", &root));
        // Two roots never share a name: the whole root is in it, not its first 12 characters.
        assert_ne!(record_name("org.a", "1", &root), record_name("org.a", "1", &format!("{}{}", &root[..12], "cd".repeat(26))));
        assert!(record_name("org.a", "1", &root).contains(&root));
        // The longest id and a version of unsafe characters still make a name that a file system takes.
        let id = format!("org.{}", "a".repeat(120));
        let long = record_name(&id, &"+".repeat(64), &root);
        assert!(long.len() <= 255, "{}", long.len());
        assert_ne!(long, record_name(&id, &"~".repeat(64), &root));
    }
}
