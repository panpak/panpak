// SPDX-License-Identifier: MIT OR Apache-2.0
//! `panpak.lock`: the build image of a recipe, fixed. The recipe names an image (`debian-13`); the lock says which one exactly: the digest of the
//! image in the registry and the date of the package snapshot. It is written by `panpak init` or the first `panpak build` and changed only by
//! `panpak build --update-lock`, so that two builds of one recipe use one image.
use super::image;
use anyhow::{bail, ensure, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    pub image: LockImage,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockImage {
    /// The name in the recipe.
    pub name: String,
    /// `sha256:<64 hex>`: the manifest of the image for this architecture in the registry.
    pub manifest_digest: String,
    /// `YYYYMMDDTHHMMSSZ`: the snapshot of the package archive.
    pub snapshot: String,
}

pub fn path_in(dir: &Path) -> PathBuf {
    dir.join("panpak.lock")
}

impl Lock {
    pub fn check(&self) -> Result<()> {
        let d = &self.image.manifest_digest;
        ensure!(d.len() == 71 && d.starts_with("sha256:") && d[7..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "panpak.lock: image.manifest_digest must be sha256:<64 lowercase hex>");
        let s = &self.image.snapshot;
        ensure!(s.len() == 16 && s.as_bytes()[8] == b'T' && s.as_bytes()[15] == b'Z' && s[..8].bytes().all(|b| b.is_ascii_digit()) && s[9..15].bytes().all(|b| b.is_ascii_digit()), "panpak.lock: image.snapshot must look like 20261107T000000Z");
        image::known(&self.image.name)?;
        Ok(())
    }

    /// The lock that the tool would write today for an image name.
    pub fn current_for(name: &str) -> Result<Lock> {
        let k = image::known(name)?;
        Ok(Lock { image: LockImage { name: k.name.to_string(), manifest_digest: k.manifest_digest.to_string(), snapshot: k.snapshot.to_string() } })
    }

    pub fn read(dir: &Path) -> Result<Option<Lock>> {
        let p = path_in(dir);
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("read {}", p.display())),
        };
        let l: Lock = toml::from_str(&text).with_context(|| format!("{} is not a valid lock file", p.display()))?;
        l.check()?;
        Ok(Some(l))
    }

    pub fn write(&self, dir: &Path) -> Result<()> {
        let text = format!("# Written by `panpak init` or `panpak build`; changed only by `panpak build --update-lock`.\n{}", toml::to_string(self)?);
        let p = path_in(dir);
        std::fs::write(&p, text).with_context(|| format!("write {}", p.display()))
    }
}

/// The lock to build with. No file: write the current one (and say so). A file: use it, unless `update`. A file for another image
/// than the recipe's: an error that says what to do.
pub fn resolve(dir: &Path, image_name: &str, update: bool) -> Result<(Lock, bool)> {
    match Lock::read(dir)? {
        None => {
            let l = Lock::current_for(image_name)?;
            l.write(dir)?;
            Ok((l, true))
        }
        Some(_) if update => {
            let l = Lock::current_for(image_name)?;
            l.write(dir)?;
            Ok((l, true))
        }
        Some(l) if l.image.name != image_name => bail!(
            "the recipe builds on {image_name} but panpak.lock is for {}: run `panpak build --update-lock` to lock the image the recipe names",
            l.image.name
        ),
        Some(l) => Ok((l, false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-lock-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn written_once_and_changed_only_on_request() {
        let d = dir("a");
        let (l, wrote) = resolve(&d, "debian-13", false).unwrap();
        assert!(wrote && path_in(&d).exists());
        // An edited lock is kept by a build, and replaced by --update-lock.
        let mut old = l.clone();
        old.image.snapshot = "20250101T000000Z".into();
        old.write(&d).unwrap();
        let (kept, wrote) = resolve(&d, "debian-13", false).unwrap();
        assert!(!wrote && kept.image.snapshot == "20250101T000000Z");
        let (new, wrote) = resolve(&d, "debian-13", true).unwrap();
        assert!(wrote && new == l);
        // The recipe names another image: an error that names the way out.
        let e = resolve(&d, "ubuntu-24", false).unwrap_err().to_string();
        assert!(e.contains("--update-lock"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_bad_lock_is_refused() {
        let d = dir("b");
        for bad in [
            "[image]\nname = \"debian-13\"\nmanifest_digest = \"sha256:xyz\"\nsnapshot = \"20261107T000000Z\"\n",
            "[image]\nname = \"debian-13\"\nmanifest_digest = \"sha256:7378260c0a098d27ac206225db5e7d92b8c7c84ea7afccc86eeb513756bbd392\"\nsnapshot = \"yesterday\"\n",
            "[image]\nname = \"nothing\"\nmanifest_digest = \"sha256:7378260c0a098d27ac206225db5e7d92b8c7c84ea7afccc86eeb513756bbd392\"\nsnapshot = \"20261107T000000Z\"\n",
            "[image]\nname = \"debian-13\"\nmanifest_digest = \"sha256:7378260c0a098d27ac206225db5e7d92b8c7c84ea7afccc86eeb513756bbd392\"\nsnapshot = \"20261107T000000Z\"\nextra = 1\n",
        ] {
            std::fs::write(path_in(&d), bad).unwrap();
            assert!(Lock::read(&d).is_err(), "{bad}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
