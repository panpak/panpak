// SPDX-License-Identifier: MIT OR Apache-2.0
//! Packages built from source: `panpak init` and `panpak build` (docs/SOURCE-BUILD.md).
//!
//! A recipe (`panpak.toml` of a project) says what to build and how; `panpak.lock` next to it fixes the build image; `panpak build`
//! makes the image (a Debian root file system unpacked from the registry by digest, plus the recipe's packages from a dated
//! snapshot), fetches the source at a pinned commit or hash, builds in bubblewrap with no network, collects what the app needs
//! at run time and packs it. Everything runs without root.
pub mod build;
pub mod collect;
pub mod fetch;
pub mod image;
pub mod init;
pub mod lock;
pub mod memory;
pub mod recipe;
pub mod sandbox;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The cache of images, sources and build outputs: `$XDG_CACHE_HOME/panpak/build` (or `~/.cache/panpak/build`).
pub fn cache_dir() -> Result<PathBuf> {
    let base = dirs::cache_dir().context("no cache folder (set XDG_CACHE_HOME or HOME)")?;
    cache_in(&base)
}

/// `<base>/panpak/build`, made, with links and `..` resolved: the build compares paths of the build image with paths it is given, and a cache folder written
/// as `/a/b/../c` (valid for the system) made it say that a library of the image was not in the image.
fn cache_in(base: &Path) -> Result<PathBuf> {
    let d = base.join("panpak").join("build");
    std::fs::create_dir_all(&d).with_context(|| format!("create {}", d.display()))?;
    d.canonicalize().with_context(|| format!("resolve {}", d.display()))
}

/// Print one line of progress to the standard error.
pub fn say(what: &str) {
    eprintln!("panpak build: {what}");
}

#[cfg(test)]
mod cache_tests {
    #[test]
    fn a_cache_folder_with_dotdot_is_resolved() {
        let t = std::env::temp_dir().join(format!("panpak-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(t.join("a/b")).unwrap();
        let odd = t.join("a/b/../c");
        let d = super::cache_in(&odd).unwrap();
        assert_eq!(d, t.canonicalize().unwrap().join("a/c/panpak/build"));
        assert!(!d.to_string_lossy().contains(".."));
        let _ = std::fs::remove_dir_all(&t);
    }
}
