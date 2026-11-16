// SPDX-License-Identifier: MIT OR Apache-2.0
//! The source of a recipe: a git commit or a tarball with a hash. The only step of a build that needs the network (besides the image).
use super::{image, recipe::Source};
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Fetched {
    pub dir: PathBuf,
    /// `SOURCE_DATE_EPOCH` for the build: the recipe's, or the date of the commit, or the newest file of the tarball.
    pub epoch: u64,
    /// For `[provenance]`.
    pub describe: String,
}

fn key_of(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    image::hex(&h.finalize())[..20].to_string()
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let o = Command::new("git").args(args).current_dir(dir).env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null()).output().context("run git (it must be installed)")?;
    ensure!(o.status.success(), "git {} failed: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim());
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

pub fn fetch(cache: &Path, s: &Source) -> Result<Fetched> {
    match (&s.git, &s.url) {
        (Some(url), None) => {
            let commit = s.commit.as_deref().unwrap_or("");
            let dir = cache.join("src").join(key_of(&["git", url, commit]));
            if !dir.join(".panpak-complete").exists() {
                super::say(&format!("fetching {url} at {}", &commit[..12]));
                let _ = image::chmod_all(&dir);
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir)?;
                git(&dir, &["init", "-q"])?;
                git(&dir, &["remote", "add", "origin", url])?;
                git(&dir, &["fetch", "-q", "--depth", "1", "origin", commit]).context("the server must allow fetching a commit by its id")?;
                git(&dir, &["checkout", "-q", "FETCH_HEAD"])?;
                let head = git(&dir, &["rev-parse", "HEAD"])?;
                ensure!(head == commit, "fetched commit is {head}, the recipe says {commit}: refused");
                std::fs::write(dir.join(".panpak-complete"), commit)?;
            }
            let epoch = match s.epoch {
                Some(e) => e,
                None => git(&dir, &["log", "-1", "--format=%ct"])?.parse().context("commit date")?,
            };
            Ok(Fetched { dir, epoch, describe: format!("git {url} @ {commit}") })
        }
        (None, Some(url)) => {
            let want = s.sha256.as_deref().unwrap_or("");
            let dir = cache.join("src").join(key_of(&["url", url, want]));
            let marker = dir.join(".panpak-complete");
            if !marker.exists() {
                super::say(&format!("fetching {url}"));
                let _ = image::chmod_all(&dir);
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(cache.join("dl"))?;
                let file = cache.join("dl").join(format!("{want}.tar.gz"));
                image::download(url, &[], Some(want), &file, 1 << 30)?;
                std::fs::create_dir_all(&dir)?;
                let newest = unpack_tarball(&file, &dir)?;
                std::fs::write(&marker, format!("{newest}"))?;
            }
            let newest: u64 = std::fs::read_to_string(&marker)?.trim().parse().unwrap_or(0);
            Ok(Fetched { dir, epoch: s.epoch.unwrap_or(newest), describe: format!("url {url} sha256 {want}") })
        }
        _ => bail!("[source] needs exactly one of git or url"),
    }
}

/// Unpack a `.tar.gz` into `dir`, dropping the first path component. Returns the newest modification time of the entries.
fn unpack_tarball(file: &Path, dir: &Path) -> Result<u64> {
    let f = std::fs::File::open(file)?;
    let mut magic = [0u8; 2];
    std::io::Read::read_exact(&mut &f, &mut magic)?;
    ensure!(magic == [0x1f, 0x8b], "{}: only .tar.gz sources are supported for now", file.display());
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(file)?));
    ar.set_preserve_permissions(true);
    ar.set_preserve_ownerships(false);
    let mut newest = 0u64;
    for e in ar.entries()? {
        let mut e = e?;
        let t = e.header().entry_type();
        if !(t.is_file() || t.is_dir() || t.is_symlink()) {
            continue;
        }
        newest = newest.max(e.header().mtime().unwrap_or(0));
        let path = e.path()?.into_owned();
        let rel: PathBuf = path.components().skip(1).collect();
        if rel.as_os_str().is_empty() {
            continue;
        }
        ensure!(!path.is_absolute() && !rel.components().any(|c| matches!(c, std::path::Component::ParentDir)), "an entry of the tarball leaves its folder: {}", path.display());
        let dest = dir.join(&rel);
        // The folders above the entry are made one at a time and none may be a link: a link that an earlier entry made would take this one out of `dir`.
        if let Some(p) = dest.parent() {
            super::collect::ensure_dirs(dir, p).with_context(|| format!("unpack {}", rel.display()))?;
        }
        e.unpack(&dest).with_context(|| format!("unpack {}", rel.display()))?;
    }
    Ok(newest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A new, empty folder for one test (never removed: it is small, and nothing here deletes).
    fn fresh(n: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!("panpak-fetch-{n}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    enum E<'a> {
        Dir(&'a str),
        File(&'a str, &'a [u8]),
        Link(&'a str, &'a str),
    }

    /// A `.tar.gz` made of these entries, as written to a file.
    fn tarball(at: &Path, entries: &[E]) {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(std::fs::File::create(at).unwrap(), flate2::Compression::fast()));
        for e in entries {
            let mut h = tar::Header::new_gnu();
            h.set_mtime(1_000);
            match e {
                E::Dir(p) => {
                    h.set_entry_type(tar::EntryType::Directory);
                    h.set_mode(0o755);
                    h.set_size(0);
                    b.append_data(&mut h, p, &[][..]).unwrap();
                }
                E::File(p, d) => {
                    h.set_entry_type(tar::EntryType::Regular);
                    h.set_mode(0o644);
                    h.set_size(d.len() as u64);
                    b.append_data(&mut h, p, *d).unwrap();
                }
                E::Link(p, t) => {
                    h.set_entry_type(tar::EntryType::Symlink);
                    h.set_mode(0o777);
                    h.set_size(0);
                    b.append_link(&mut h, p, t).unwrap();
                }
            }
        }
        b.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn a_tarball_unpacks_files_folders_and_links_inside_its_folder() {
        let (w, dest) = (fresh("ok"), fresh("ok-dest"));
        let f = w.join("a.tar.gz");
        tarball(&f, &[E::Dir("top/"), E::Dir("top/src/"), E::File("top/src/a.c", b"int a;"), E::Link("top/src/b.c", "a.c"), E::File("top/deep/er/c.txt", b"c")]);
        assert_eq!(unpack_tarball(&f, &dest).unwrap(), 1_000);
        assert_eq!(std::fs::read(dest.join("src/a.c")).unwrap(), b"int a;");
        assert_eq!(std::fs::read_link(dest.join("src/b.c")).unwrap(), Path::new("a.c"));
        assert_eq!(std::fs::read(dest.join("deep/er/c.txt")).unwrap(), b"c");
    }

    #[test]
    fn a_file_under_a_link_that_an_earlier_entry_made_is_not_written_through_it() {
        // P2-02-style escape of the source tarball: `src/link -> <folder outside>`, then `src/link/planted.txt`.
        let (w, dest, victim) = (fresh("l1"), fresh("l1-dest"), fresh("l1-victim"));
        let f = w.join("evil.tar.gz");
        tarball(&f, &[E::Link("top/src/link", victim.to_str().unwrap()), E::File("top/src/link/planted.txt", b"x")]);
        assert!(unpack_tarball(&f, &dest).is_err(), "the unpacking must stop");
        assert!(!victim.join("planted.txt").exists(), "a file was written outside the destination");
    }

    #[test]
    fn a_relative_link_that_climbs_out_is_no_way_out_either() {
        let (w, dest) = (fresh("l2"), fresh("l2-dest"));
        let outside = dest.parent().unwrap().join(format!("{}-outside", dest.file_name().unwrap().to_string_lossy()));
        std::fs::create_dir_all(&outside).unwrap();
        let f = w.join("evil.tar.gz");
        tarball(&f, &[E::Link("top/l", &format!("../{}", outside.file_name().unwrap().to_string_lossy())), E::File("top/l/planted.txt", b"x")]);
        assert!(unpack_tarball(&f, &dest).is_err());
        assert!(!outside.join("planted.txt").exists());
    }

    #[test]
    fn a_file_over_a_link_of_an_earlier_entry_does_not_write_through_it() {
        let (w, dest, victim) = (fresh("l3"), fresh("l3-dest"), fresh("l3-victim"));
        let target = victim.join("target.txt");
        std::fs::write(&target, b"original").unwrap();
        let f = w.join("evil.tar.gz");
        tarball(&f, &[E::Link("top/file", target.to_str().unwrap()), E::File("top/file", b"overwritten")]);
        let _ = unpack_tarball(&f, &dest);
        assert_eq!(std::fs::read(&target).unwrap(), b"original", "a file outside the destination was written");
    }
}
