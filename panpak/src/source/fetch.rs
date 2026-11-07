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
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p)?;
        }
        e.unpack(&dest).with_context(|| format!("unpack {}", rel.display()))?;
    }
    Ok(newest)
}
