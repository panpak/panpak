// SPDX-License-Identifier: MIT OR Apache-2.0
//! The recipe: the `panpak.toml` of a project that is built from source. Defaults: `build.image` is `debian-13`, `desktop.sizes` is
//! `[256, 128, 64, 48, 32]`, `build.jobs` is the number of processors (lowered by the memory the machine has).
use anyhow::{bail, ensure, Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

pub const DEFAULT_IMAGE: &str = "debian-13";
pub const DEFAULT_SIZES: &[u32] = &[256, 128, 64, 48, 32];

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub app: panpak_core::App,
    pub source: Source,
    pub build: Build,
    pub collect: Option<Collect>,
    pub env: Option<BTreeMap<String, String>>,
    pub desktop: Option<Desktop>,
    pub launch: Option<panpak_core::Launch>,
    pub permissions: Option<panpak_core::Permissions>,
    pub license: Option<License>,
    pub update: Option<Update>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub git: Option<String>,
    /// The full commit id (40 hex). A tag is refused: a tag can move.
    pub commit: Option<String>,
    pub url: Option<String>,
    pub sha256: Option<String>,
    /// `SOURCE_DATE_EPOCH` of the build. Default: the date of the commit (git) or of the newest file of the tarball.
    pub epoch: Option<u64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub image: Option<String>,
    /// `meson`, `cmake`, `zig` or `custom`.
    pub system: String,
    #[serde(default)]
    pub configure: Vec<String>,
    /// Only for `custom`: each step is a list of arguments, never a shell string.
    #[serde(default)]
    pub steps: Vec<Vec<String>>,
    /// Packages of the image that the build needs (from the snapshot).
    #[serde(default)]
    pub packages: Vec<String>,
    pub memory: Option<String>,
    pub jobs: Option<u32>,
    pub memory_per_job: Option<String>,
    /// `none` (default) or `fetch`: the first run may use the network to fetch what the build system downloads (Zig packages).
    pub network: Option<String>,
    #[serde(default)]
    pub writable_source: bool,
    /// A pinned Zig, for `system = "zig"`.
    pub zig: Option<Zig>,
    #[serde(default)]
    pub image_deps: Vec<ImageDepDecl>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Zig {
    pub version: String,
    pub sha256: String,
}

/// A dependency taken from the build image instead of built from the pinned source.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageDepDecl {
    pub name: String,
    /// The image package that provides it.
    pub package: String,
    pub why: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collect {
    /// `gtk4`, `qt6` or `none`.
    pub toolkit: Option<String>,
    #[serde(default)]
    pub prefer_host: Vec<String>,
    #[serde(default)]
    pub allow_missing: Vec<String>,
    /// Folders of the image's `/usr/share` to copy into the tree (`share/gtksourceview-5`).
    #[serde(default)]
    pub data: Vec<String>,
    /// Globs of tree paths to leave out.
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Desktop {
    /// An SVG of the installed tree, rendered at `sizes` in the build image.
    pub icon_svg: Option<String>,
    /// PNG files of the installed tree, used as they are.
    #[serde(default)]
    pub icons_png: Vec<String>,
    pub sizes: Option<Vec<u32>>,
    pub wm_class: Option<String>,
    pub schemes: Option<Vec<String>>,
    /// MIME types for the menu entry. When it is not set, the `MimeType` of the `.desktop` file of the installed files
    /// (`share/applications/<app.id>.desktop`) is used, without the URL schemes.
    pub mime_types: Option<Vec<String>>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct License {
    pub spdx: Option<String>,
    /// Files of the source tree, copied into the package as `share/licenses/<id>/`.
    #[serde(default)]
    pub files: Vec<String>,
    /// `auto`: the copyright files of the image packages that the collected libraries come from.
    pub bundled: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub url: Option<String>,
}

impl Recipe {
    pub fn image(&self) -> &str {
        self.build.image.as_deref().unwrap_or(DEFAULT_IMAGE)
    }

    pub fn sizes(&self) -> Vec<u32> {
        self.desktop.as_ref().and_then(|d| d.sizes.clone()).unwrap_or_else(|| DEFAULT_SIZES.to_vec())
    }

    pub fn network_fetch(&self) -> bool {
        self.build.network.as_deref() == Some("fetch")
    }

    pub fn load(dir: &Path) -> Result<Recipe> {
        let p = dir.join("panpak.toml");
        let text = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        Recipe::parse(&text).with_context(|| format!("{}", p.display()))
    }

    pub fn parse(text: &str) -> Result<Recipe> {
        let r: Recipe = toml::from_str(text).map_err(|e| anyhow::anyhow!("not a recipe: {e}"))?;
        r.validate()?;
        Ok(r)
    }

    pub fn validate(&self) -> Result<()> {
        panpak_core::validate_app_id(&self.app.id)?;
        ensure!(!self.app.name.is_empty() && !self.app.version.is_empty() && !self.app.entry.is_empty(), "[app] needs id, name, version and entry");
        panpak_core::safe_path(&self.app.entry).context("app.entry")?;
        let s = &self.source;
        match (&s.git, &s.url) {
            (Some(g), None) => {
                ensure!(g.starts_with("https://"), "source.git must be an https:// URL");
                let c = s.commit.as_deref().unwrap_or("");
                ensure!(c.len() == 40 && c.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "source.commit must be the full commit id (40 lowercase hex): a tag or a branch can move, so it is refused");
                ensure!(s.sha256.is_none(), "source.sha256 goes with source.url, not source.git");
            }
            (None, Some(u)) => {
                ensure!(u.starts_with("https://"), "source.url must be an https:// URL");
                let h = s.sha256.as_deref().unwrap_or("");
                ensure!(h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "source.sha256 must be 64 lowercase hex digits: a download without a hash is refused");
                ensure!(s.commit.is_none(), "source.commit goes with source.git, not source.url");
            }
            _ => bail!("[source] needs exactly one of git (with commit) or url (with sha256)"),
        }
        let b = &self.build;
        ensure!(["meson", "cmake", "zig", "custom"].contains(&b.system.as_str()), "build.system must be meson, cmake, zig or custom (not {:?})", b.system);
        ensure!(b.system != "custom" || !b.steps.is_empty(), "build.system = \"custom\" needs build.steps");
        ensure!(b.system == "custom" || b.steps.is_empty(), "build.steps is only for build.system = \"custom\"");
        ensure!(b.system != "zig" || b.zig.is_some(), "build.system = \"zig\" needs build.zig = {{ version, sha256 }}: the compiler is pinned in the recipe");
        if let Some(z) = &b.zig {
            ensure!(!z.version.is_empty() && z.version.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-' || c == b'+'), "build.zig.version is not a version");
            ensure!(z.sha256.len() == 64 && z.sha256.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "build.zig.sha256 must be 64 lowercase hex digits");
        }
        for step in &b.steps {
            ensure!(!step.is_empty() && step.iter().all(|a| !a.contains('\0')), "a build step is a list of arguments, with no empty step");
        }
        ensure!(b.network.as_deref().is_none_or(|n| n == "none" || n == "fetch"), "build.network must be none or fetch");
        for p in &b.packages {
            ensure!(!p.is_empty() && p.bytes().all(|c| c.is_ascii_alphanumeric() || b"+-.:".contains(&c)), "build.packages: {p:?} is not a package name");
        }
        if let Some(m) = &b.memory {
            super::memory::parse_size(m).context("build.memory")?;
        }
        if let Some(m) = &b.memory_per_job {
            super::memory::parse_size(m).context("build.memory_per_job")?;
        }
        ensure!(b.jobs.is_none_or(|j| (1..=256).contains(&j)), "build.jobs must be between 1 and 256");
        for d in &b.image_deps {
            ensure!(!d.name.is_empty() && !d.package.is_empty() && !d.why.trim().is_empty(), "each [[build.image_deps]] needs name, package and why: a dependency taken from the image is declared with its reason");
            ensure!(b.packages.contains(&d.package), "image dependency {}: its package {} is not in build.packages", d.name, d.package);
        }
        if let Some(c) = &self.collect {
            ensure!(c.toolkit.as_deref().is_none_or(|t| ["gtk4", "qt6", "none"].contains(&t)), "collect.toolkit must be gtk4, qt6 or none");
            for d in &c.data {
                ensure!(d.starts_with("share/") && panpak_core::safe_path(d).is_ok(), "collect.data: {d:?} must be a folder under share/");
            }
        }
        if let Some(d) = &self.desktop {
            ensure!(!(d.icon_svg.is_some() && !d.icons_png.is_empty()), "desktop: icon_svg and icons_png are alternatives");
            for s in d.sizes.iter().flatten() {
                ensure!((16..=1024).contains(s), "desktop.sizes: {s} is not between 16 and 1024");
            }
            ensure!(d.sizes.as_ref().is_none_or(|s| s.len() <= 6), "desktop.sizes: at most 6");
            for p in d.icon_svg.iter().chain(d.icons_png.iter()) {
                panpak_core::safe_path(p).with_context(|| format!("desktop icon {p:?}"))?;
            }
        }
        if let Some(env) = &self.env {
            panpak_core::pkgenv::check(env)?;
        }
        if let Some(l) = &self.license {
            ensure!(l.bundled.as_deref().is_none_or(|b| b == "auto" || b == "none"), "license.bundled must be auto or none");
            for f in &l.files {
                panpak_core::safe_path(f).with_context(|| format!("license file {f:?}"))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
[app]
id = "org.example.App"
name = "App"
version = "1.0"
entry = "bin/app"
[source]
git = "https://example.org/app.git"
commit = "0123456789abcdef0123456789abcdef01234567"
[build]
system = "meson"
packages = ["meson", "libgtk-4-dev"]
[[build.image_deps]]
name = "gtk"
package = "libgtk-4-dev"
why = "the system copy"
"#;

    #[test]
    fn defaults_and_a_good_recipe() {
        let r = Recipe::parse(GOOD).unwrap();
        assert_eq!(r.image(), "debian-13");
        assert_eq!(r.sizes(), [256, 128, 64, 48, 32]);
        assert!(!r.network_fetch());
    }

    fn with(from: &str, to: &str) -> String {
        GOOD.replace(from, to)
    }

    #[test]
    fn a_source_must_be_pinned() {
        for bad in [
            with("commit = \"0123456789abcdef0123456789abcdef01234567\"", "commit = \"v1.0\""),
            with("commit = \"0123456789abcdef0123456789abcdef01234567\"", ""),
            with("git = \"https://example.org/app.git\"", "git = \"http://example.org/app.git\""),
            with("git = \"https://example.org/app.git\"", "url = \"https://example.org/a.tar.gz\""),
        ] {
            assert!(Recipe::parse(&bad).is_err(), "{bad}");
        }
        let url = with("git = \"https://example.org/app.git\"\ncommit = \"0123456789abcdef0123456789abcdef01234567\"", "url = \"https://example.org/a.tar.gz\"");
        assert!(Recipe::parse(&url).unwrap_err().to_string().contains("sha256"), "a download without a hash");
        let ok = with("git = \"https://example.org/app.git\"\ncommit = \"0123456789abcdef0123456789abcdef01234567\"", &format!("url = \"https://example.org/a.tar.gz\"\nsha256 = \"{}\"", "ab".repeat(32)));
        Recipe::parse(&ok).unwrap();
    }

    #[test]
    fn mistakes_are_named() {
        let cases: &[(&str, &str, &str)] = &[
            ("system = \"meson\"", "system = \"make\"", "build.system"),
            ("system = \"meson\"", "system = \"zig\"", "build.zig"),
            ("system = \"meson\"", "system = \"custom\"", "build.steps"),
            ("system = \"meson\"", "system = \"meson\"\nmemory = \"lots\"", "build.memory"),
            ("system = \"meson\"", "system = \"meson\"\nnetwork = \"always\"", "build.network"),
            ("why = \"the system copy\"", "why = \"\"", "why"),
            ("package = \"libgtk-4-dev\"", "package = \"libother-dev\"", "not in build.packages"),
            ("system = \"meson\"", "system = \"meson\"\nunknown_key = 1", "unknown field"),
            ("version = \"1.0\"", "version = \"\"", "[app] needs"),
        ];
        for (from, to, want) in cases {
            let e = Recipe::parse(&with(from, to)).unwrap_err().to_string();
            assert!(e.contains(want), "{to}: {e}");
        }
        // [env] goes through the same check as in a manifest.
        let e = Recipe::parse(&format!("{GOOD}\n[env]\nLD_PRELOAD = \"${{PANPAK_ROOT}}/x\"\n")).unwrap_err().to_string();
        assert!(e.contains("env.LD_PRELOAD"), "{e}");
    }
}
