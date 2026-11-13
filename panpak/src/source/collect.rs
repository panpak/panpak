// SPDX-License-Identifier: MIT OR Apache-2.0
//! What goes into the package besides the program: the libraries it needs, the plugins its toolkit loads with `dlopen`, the data folders,
//! the icons and the license files.
//!
//! The libraries come from the ELF files themselves (`DT_NEEDED`, read with the reader of `panpak_core`, never with `ldd`, which runs the
//! loader), found in the folders of the build image, and the ones the host must provide (glibc, the graphics stack, `libwayland-*`, the list
//! of PANPAK-FORMAT.md section 7) are left out. The toolkits are tables of data: which plugin folders to take, which data, which variables
//! of the `[env]` table to set, and which plugins a linked module needs (a missing one stops the build: a tray icon or a dark mode that
//! silently does not work is worse than an error).
use super::{recipe::Recipe, sandbox};
use anyhow::{bail, ensure, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// A toolkit, as data.
pub struct Toolkit {
    pub name: &'static str,
    /// `(folder of the image, folder of the tree, file name patterns)`: plugins loaded with `dlopen`.
    pub plugins: &'static [(&'static str, &'static str, &'static [&'static str])],
    /// `(folder of the image's /usr/share, folder of the tree's share)`.
    pub data: &'static [(&'static str, &'static str)],
    /// The `[env]` table this toolkit needs.
    pub env: &'static [(&'static str, &'static str)],
    /// `(library prefix linked, file that must be in the tree, the image package that has it)`: a plugin that a linked module needs.
    pub requires: &'static [(&'static str, &'static str, &'static str)],
}

macro_rules! qt {
    ($cat:literal) => {
        concat!("usr/lib/x86_64-linux-gnu/qt6/plugins/", $cat)
    };
}
const IMG_PIXBUF: &str = "usr/lib/x86_64-linux-gnu/gdk-pixbuf-2.0/2.10.0/loaders";

pub const TOOLKITS: &[Toolkit] = &[
    Toolkit {
        name: "gtk4",
        plugins: &[(IMG_PIXBUF, "lib/gdk-pixbuf/loaders", &["*.so"])],
        data: &[("glib-2.0/schemas", "share/glib-2.0/schemas"), ("icons/Adwaita", "share/icons/Adwaita"), ("icons/hicolor", "share/icons/hicolor"), ("mime", "share/mime")],
        env: &[
            ("XDG_DATA_DIRS", "${PANPAK_ROOT}/share"),
            ("GSETTINGS_SCHEMA_DIR", "${PANPAK_ROOT}/share/glib-2.0/schemas"),
            ("GSETTINGS_BACKEND", "keyfile"),
            ("GDK_PIXBUF_MODULE_FILE", "${PANPAK_ROOT}/lib/gdk-pixbuf/loaders.cache"),
        ],
        requires: &[],
    },
    Toolkit {
        name: "qt6",
        plugins: &[
            (qt!("platforms"), "plugins/platforms", &["libqwayland*.so"]),
            (qt!("wayland-shell-integration"), "plugins/wayland-shell-integration", &["*.so"]),
            (qt!("wayland-decoration-client"), "plugins/wayland-decoration-client", &["*.so"]),
            (qt!("wayland-graphics-integration-client"), "plugins/wayland-graphics-integration-client", &["*.so"]),
            (qt!("platformthemes"), "plugins/platformthemes", &["libqxdgdesktopportal*.so"]),
            (qt!("imageformats"), "plugins/imageformats", &["*.so"]),
            (qt!("iconengines"), "plugins/iconengines", &["*.so"]),
            (qt!("tls"), "plugins/tls", &["*.so"]),
            (qt!("networkinformation"), "plugins/networkinformation", &["*.so"]),
        ],
        data: &[],
        env: &[("QT_PLUGIN_PATH", "${PANPAK_ROOT}/plugins"), ("QT_QPA_PLATFORM", "wayland"), ("XDG_DATA_DIRS", "${PANPAK_ROOT}/share")],
        requires: &[
            ("libQt6Core.so", "plugins/platformthemes/libqxdgdesktopportal.so", "qt6-xdgdesktopportal-platformtheme"),
            ("libQt6Core.so", "plugins/platforms/libqwayland-generic.so", "qt6-wayland"),
            ("libQt6Svg.so", "plugins/imageformats/libqsvg.so", "qt6-svg-plugins"),
            ("libQt6Svg.so", "plugins/iconengines/libqsvgicon.so", "qt6-svg-plugins"),
        ],
    },
    Toolkit { name: "none", plugins: &[], data: &[], env: &[], requires: &[] },
];

pub fn toolkit(name: &str) -> Result<&'static Toolkit> {
    TOOLKITS.iter().find(|t| t.name == name).with_context(|| format!("unknown toolkit {name:?}"))
}

pub struct Collected {
    pub env: BTreeMap<String, String>,
    /// Extra `abi.needs` (plugins that the loader opens by name: the farm of the runtime must link them).
    pub needs: Vec<String>,
    pub icons: Vec<String>,
    /// The MIME types of the menu entry (`desktop.mime_types`).
    pub mime_types: Vec<String>,
    pub libs: usize,
    pub bytes: u64,
    pub notes: Vec<String>,
}

pub struct Inputs<'a> {
    pub recipe: &'a Recipe,
    /// The root file system of the build image.
    pub root: &'a Path,
    /// The files the build installed (`DESTDIR`): everything is under `usr/`.
    pub destdir: &'a Path,
    pub source: &'a Path,
    pub tree: &'a Path,
    pub shim: &'a Path,
    pub log: &'a Path,
}

/// A glob with `*` anywhere (it matches any run of characters, `/` included).
pub fn glob(pat: &str, name: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || name.len() < first.len() + last.len() || !name.ends_with(last) {
        return false;
    }
    let mut pos = first.len();
    let end = name.len() - last.len();
    for mid in &parts[1..parts.len() - 1] {
        match name[pos..end].find(mid) {
            Some(i) => pos += i + mid.len(),
            None => return false,
        }
    }
    true
}

/// Copy a folder: files with their mode, links as links.
pub fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let e = e?;
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let m = from.symlink_metadata()?;
        if m.file_type().is_symlink() {
            let _ = std::fs::remove_file(&to);
            std::os::unix::fs::symlink(std::fs::read_link(&from)?, &to)?;
        } else if m.is_dir() {
            copy_tree(&from, &to)?;
        } else if m.is_file() {
            std::fs::copy(&from, &to).with_context(|| format!("copy {}", from.display()))?;
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(m.permissions().mode() & 0o755 | 0o600))?;
        }
    }
    Ok(())
}

fn is_elf(p: &Path) -> bool {
    use std::io::Read;
    let mut b = [0u8; 4];
    std::fs::File::open(p).and_then(|mut f| f.read_exact(&mut b)).is_ok() && &b == b"\x7fELF"
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match p.symlink_metadata() {
            Ok(m) if m.is_dir() => walk_files(&p, out),
            Ok(m) if m.is_file() => out.push(p),
            _ => {}
        }
    }
}

/// The folders where the image's loader looks for libraries: `ld.so.conf` and what it includes, then the standard ones.
pub fn lib_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    fn read(root: &Path, file: &Path, dirs: &mut Vec<PathBuf>, depth: u8) {
        let Ok(t) = std::fs::read_to_string(file) else { return };
        for l in t.lines().map(|l| l.split('#').next().unwrap_or("").trim()) {
            if let Some(g) = l.strip_prefix("include ") {
                if depth < 3 {
                    let g = g.trim().trim_start_matches('/');
                    let (d, pat) = g.rsplit_once('/').unwrap_or((".", g));
                    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join(d)).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| glob(pat, n))).collect();
                    files.sort();
                    for f in files {
                        read(root, &f, dirs, depth + 1);
                    }
                }
            } else if l.starts_with('/') {
                dirs.push(PathBuf::from(l));
            }
        }
    }
    read(root, &root.join("etc/ld.so.conf"), &mut dirs, 0);
    for d in ["/lib/x86_64-linux-gnu", "/usr/lib/x86_64-linux-gnu", "/lib", "/usr/lib"] {
        dirs.push(PathBuf::from(d));
    }
    let mut seen = BTreeSet::new();
    dirs.into_iter().filter(|d| seen.insert(d.clone())).collect()
}

/// Find a library by SONAME in the folders of the image: the real file (links followed inside the image).
pub fn find_lib(root: &Path, dirs: &[PathBuf], soname: &str) -> Option<PathBuf> {
    for d in dirs {
        let p = root.join(d.strip_prefix("/").unwrap_or(d)).join(soname);
        if let Ok(real) = std::fs::canonicalize(&p) {
            if real.starts_with(root) && real.is_file() {
                return Some(real);
            }
        }
    }
    None
}

fn sandbox_run(i: &Inputs, argv: &[&str], what: &str) -> Result<()> {
    sandbox::Run::new(i.root, i.shim, argv).bind(i.tree, "/tree", true).run_logged(i.log, what)
}

pub fn collect(i: &Inputs) -> Result<Collected> {
    let r = i.recipe;
    let tk = toolkit(r.collect.as_ref().and_then(|c| c.toolkit.as_deref()).unwrap_or("none"))?;
    let mut notes = Vec::new();
    // 1. What the build installed: /usr becomes the root of the tree (the program in bin/, its data in share/).
    let usr = i.destdir.join("usr");
    ensure!(usr.is_dir(), "the build installed nothing under /usr (DESTDIR={}): make the prefix /usr", i.destdir.display());
    for e in std::fs::read_dir(i.destdir)?.flatten() {
        if e.file_name() != "usr" {
            notes.push(format!("ignored {} of the installed files (only /usr goes into the package)", Path::new("/").join(e.file_name()).display()));
        }
    }
    let _ = std::fs::remove_dir_all(i.tree);
    copy_tree(&usr, i.tree)?;
    let entry = i.tree.join(&r.app.entry);
    ensure!(entry.is_file() && is_elf(&entry), "the entry {} is not in the installed files, or is not a program (an ELF file)", r.app.entry);
    // What the recipe leaves out (globs of paths of the tree, `*` crosses folders).
    if let Some(c) = &r.collect {
        if !c.exclude.is_empty() {
            let mut all = Vec::new();
            walk_files(i.tree, &mut all);
            for f in all {
                let rel = f.strip_prefix(i.tree).map(|p| p.display().to_string()).unwrap_or_default();
                if c.exclude.iter().any(|g| glob(g, &rel)) {
                    std::fs::remove_file(&f)?;
                }
            }
        }
    }
    // 2. The toolkit: plugins and data from the image.
    let mut seeds_extra: Vec<PathBuf> = Vec::new();
    for (from, to, pats) in tk.plugins {
        let src = i.root.join(from.trim_start_matches('/'));
        let Ok(rd) = std::fs::read_dir(&src) else { continue };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| pats.iter().any(|g| glob(g, n)))).collect();
        files.sort();
        let dst = i.tree.join(to);
        for f in files {
            std::fs::create_dir_all(&dst)?;
            let d = dst.join(f.file_name().unwrap());
            std::fs::copy(&f, &d)?;
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755))?;
            seeds_extra.push(d);
        }
    }
    let collect_data = r.collect.as_ref().map(|c| c.data.clone()).unwrap_or_default();
    for (from, to) in tk.data.iter().map(|(a, b)| (a.to_string(), b.to_string())).chain(collect_data.iter().map(|d| (d.trim_start_matches("share/").to_string(), d.clone()))) {
        let src = i.root.join("usr/share").join(&from);
        if src.is_dir() {
            copy_tree(&src, &i.tree.join(&to))?;
        } else if collect_data.contains(&to) {
            bail!("collect.data {to}: /usr/share/{from} is not in the build image (add its package to build.packages)");
        }
    }
    // 3. The libraries: the closure of DT_NEEDED over every ELF file of the tree.
    let dirs = lib_dirs(i.root);
    let (prefer, allow) = r.collect.as_ref().map(|c| (c.prefer_host.clone(), c.allow_missing.clone())).unwrap_or_default();
    let host_wins = |n: &str| panpak_core::abi::SYSTEM_WINS.iter().any(|g| panpak_core::abi::glob_match(g, n)) || prefer.iter().any(|g| panpak_core::abi::glob_match(g, n)) || allow.iter().any(|g| panpak_core::abi::glob_match(g, n));
    let libdir = i.tree.join("lib");
    std::fs::create_dir_all(&libdir)?;
    let mut have: BTreeSet<String> = panpak_core::abi::bundled_libs(i.tree).keys().cloned().collect();
    let mut origin: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut files = Vec::new();
    walk_files(i.tree, &mut files);
    let mut todo: Vec<PathBuf> = files.into_iter().filter(|p| is_elf(p)).collect();
    let (mut libs, mut bytes) = (0usize, 0u64);
    while let Some(f) = todo.pop() {
        let info = panpak_core::read_elf_info(&f).with_context(|| format!("read {}", f.display()))?;
        // Where the loader of the image would have looked first: the RUNPATH of the file, `$ORIGIN` being the folder the file came from in the image.
        let from_dir = f.file_name().and_then(|n| origin.get(n.to_string_lossy().as_ref())).and_then(|p| p.parent()).map(Path::to_path_buf).unwrap_or_else(|| f.parent().unwrap_or(i.tree).to_path_buf());
        let mut search: Vec<PathBuf> = info
            .runpath
            .iter()
            .map(|r| {
                let r = r.replace("${ORIGIN}", "$ORIGIN");
                match r.strip_prefix("$ORIGIN") {
                    Some(rest) => from_dir.join(rest.trim_start_matches('/')).strip_prefix(i.root).map(|p| Path::new("/").join(p)).unwrap_or_default(),
                    None => PathBuf::from(r),
                }
            })
            .filter(|p| p.is_absolute())
            .collect();
        search.extend(dirs.iter().cloned());
        for n in info.needs {
            if have.contains(&n) || host_wins(&n) {
                continue;
            }
            let Some(real) = find_lib(i.root, &search, &n) else {
                bail!("{} needs {n}, which is not in the build image: add the package that has it to build.packages, or list it in collect.prefer_host or collect.allow_missing", f.strip_prefix(i.tree).unwrap_or(&f).display());
            };
            let dst = libdir.join(&n);
            std::fs::copy(&real, &dst).with_context(|| format!("copy {}", real.display()))?;
            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755))?;
            bytes += dst.metadata().map(|m| m.len()).unwrap_or(0);
            libs += 1;
            have.insert(n.clone());
            origin.insert(n, real);
            todo.push(dst);
        }
    }
    // 4. A plugin that a linked module needs must be there.
    for (lib, plugin, package) in tk.requires {
        if have.iter().any(|n| n.starts_with(lib)) && !i.tree.join(plugin).exists() {
            bail!("the app links {lib} but {plugin} is not in the package: install {package} in the build image (build.packages) and build again");
        }
    }
    // 5. The loader modules that GDK-Pixbuf opens by name.
    let mut needs = Vec::new();
    if tk.name == "gtk4" {
        let ldir = i.tree.join("lib/gdk-pixbuf/loaders");
        let mut mods: Vec<String> = std::fs::read_dir(&ldir).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".so")).collect();
        mods.sort();
        if !mods.is_empty() {
            let q = "/usr/lib/x86_64-linux-gnu/gdk-pixbuf-2.0/gdk-pixbuf-query-loaders";
            let mut argv = vec![q.to_string()];
            argv.extend(mods.iter().map(|m| format!("/tree/lib/gdk-pixbuf/loaders/{m}")));
            let a: Vec<&str> = argv.iter().map(String::as_str).collect();
            let o = sandbox::Run::new(i.root, i.shim, &a).bind(i.tree, "/tree", false).output()?;
            ensure!(o.status.success(), "gdk-pixbuf-query-loaders failed: {}", String::from_utf8_lossy(&o.stderr));
            // The modules are named by file name only: the loader finds them through the library path of the runtime (they are in `abi.needs`).
            let text = String::from_utf8_lossy(&o.stdout).replace("/tree/lib/gdk-pixbuf/loaders/", "");
            std::fs::write(i.tree.join("lib/gdk-pixbuf/loaders.cache"), text)?;
            needs.extend(mods);
        }
        if i.tree.join("share/glib-2.0/schemas").is_dir() {
            sandbox_run(i, &["glib-compile-schemas", "/tree/share/glib-2.0/schemas"], "glib-compile-schemas")?;
        }
    }
    // 6. Strip the ELF files.
    let mut elfs = Vec::new();
    walk_files(i.tree, &mut elfs);
    let elfs: Vec<String> = elfs.into_iter().filter(|p| is_elf(p)).filter_map(|p| p.strip_prefix(i.tree).ok().map(|r| format!("/tree/{}", r.display()))).collect();
    for chunk in elfs.chunks(200) {
        let mut argv = vec!["strip", "--strip-unneeded", "--"];
        argv.extend(chunk.iter().map(String::as_str));
        if let Err(e) = sandbox_run(i, &argv, "strip") {
            notes.push(format!("strip failed for some files (kept as they are): {}", e.to_string().lines().next().unwrap_or("")));
        }
    }
    // 7. Icons.
    let mut icons = Vec::new();
    if let Some(d) = &r.desktop {
        if let Some(svg) = &d.icon_svg {
            ensure!(i.tree.join(svg).is_file(), "desktop.icon_svg {svg} is not in the installed files");
            for s in r.sizes() {
                let dir = format!("share/icons/hicolor/{s}x{s}/apps");
                std::fs::create_dir_all(i.tree.join(&dir))?;
                let out = format!("/tree/{dir}/{}.png", r.app.id);
                sandbox_run(i, &["rsvg-convert", "-w", &s.to_string(), "-h", &s.to_string(), "-o", &out, &format!("/tree/{svg}")], "rsvg-convert")?;
                icons.push(format!("{dir}/{}.png", r.app.id));
            }
        } else {
            for p in &d.icons_png {
                ensure!(i.tree.join(p).is_file(), "desktop.icons_png {p} is not in the installed files");
                icons.push(p.clone());
            }
        }
    }
    // 7b. The cache of every icon theme of the tree. Qt (and GTK, less so) that finds a theme folder without `icon-theme.cache` looks each icon up file by
    // file: 49,000 failed lookups at the start of qBittorrent. The cache is made after the icons are in place; its bytes do not depend on the
    // modification times of the folders (checked), so the package stays reproducible.
    let icons_root = i.tree.join("share/icons");
    if icons_root.is_dir() {
        let mut themes: Vec<String> = std::fs::read_dir(&icons_root)?.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        themes.sort();
        for t in themes {
            let dir = format!("/tree/share/icons/{t}");
            sandbox_run(i, &["gtk-update-icon-cache", "-f", "-t", "-i", &dir], "gtk-update-icon-cache")?;
            notes.push(format!("made icon-theme.cache for share/icons/{t}"));
        }
    }
    // 8. License files of the source, and the copyright files of the image packages the libraries came from.
    let lic_dir = i.tree.join("share/licenses").join(&r.app.id);
    if let Some(l) = &r.license {
        if let Some(spdx) = &l.spdx {
            std::fs::create_dir_all(&lic_dir)?;
            std::fs::write(lic_dir.join("SPDX"), format!("SPDX-License-Identifier: {spdx}\n"))?;
        }
        for f in &l.files {
            let from = i.source.join(f);
            ensure!(from.is_file(), "license.files: {f} is not in the source");
            std::fs::create_dir_all(&lic_dir)?;
            std::fs::copy(&from, lic_dir.join(Path::new(f).file_name().unwrap()))?;
        }
        if l.bundled.as_deref() == Some("auto") && !origin.is_empty() {
            let paths: Vec<String> = origin.values().filter_map(|p| p.strip_prefix(i.root).ok()).map(|p| format!("/{}", p.display())).collect();
            let mut argv = vec!["dpkg", "-S"];
            argv.extend(paths.iter().map(String::as_str));
            let o = sandbox::Run::new(i.root, i.shim, &argv).output()?;
            let mut pkgs: BTreeSet<String> = BTreeSet::new();
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if let Some((p, _)) = line.split_once(": ") {
                    pkgs.insert(p.split(':').next().unwrap_or(p).to_string());
                }
            }
            for p in pkgs {
                let c = i.root.join("usr/share/doc").join(&p).join("copyright");
                if c.is_file() {
                    let d = lic_dir.join("bundled");
                    std::fs::create_dir_all(&d)?;
                    std::fs::copy(&c, d.join(format!("{p}.copyright")))?;
                }
            }
        }
    }
    // 9. The `[env]` table: the toolkit's, then the recipe's (which wins).
    let mut env: BTreeMap<String, String> = tk.env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    for (k, v) in r.env.iter().flatten() {
        env.insert(k.clone(), v.clone());
    }
    let _ = seeds_extra;
    // 10. The MIME types of the menu entry: the recipe, or the `MimeType` of the app'"'"'s own `.desktop` file.
    let mime_types = match r.desktop.as_ref().and_then(|d| d.mime_types.clone()) {
        Some(list) => list,
        None => {
            let f = i.tree.join("share/applications").join(format!("{}.desktop", r.app.id));
            let text = match std::fs::symlink_metadata(&f) {
                Ok(m) if m.file_type().is_file() && m.len() <= 1 << 20 => std::fs::read_to_string(&f).unwrap_or_default(),
                _ => String::new(),
            };
            let (keep, left) = panpak_core::mime_types_from_desktop(&desktop_mime_value(&text));
            for l in left {
                notes.push(format!("MimeType of {}.desktop, not copied: {l}", r.app.id));
            }
            keep
        }
    };
    Ok(Collected { env, needs, icons, mime_types, libs, bytes, notes })
}

/// The value of `MimeType=` in the group `[Desktop Entry]` of a `.desktop` file; empty when there is none.
pub fn desktop_mime_value(text: &str) -> String {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry {
            if let Some(v) = line.strip_prefix("MimeType=") {
                return v.to_string();
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(n: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("panpak-collect-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn toolkits_are_tables_and_the_env_they_set_is_allowed() {
        for t in TOOLKITS {
            let env: BTreeMap<String, String> = t.env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            panpak_core::pkgenv::check(&env).unwrap_or_else(|e| panic!("{}: {e}", t.name));
            for (from, to, _) in t.plugins {
                assert!(!to.starts_with('/') && !to.contains(".."));
                // The source is the folder of one plugin category (not the root of all of them), and the destination is named like it.
                if t.name == "qt6" {
                    assert_eq!(from.rsplit('/').next(), to.rsplit('/').next(), "{from} -> {to}");
                }
            }
        }
        assert!(toolkit("gtk4").is_ok() && toolkit("qt6").is_ok() && toolkit("none").is_ok() && toolkit("gtk3").is_err());
        // Qt: the portal theme, Wayland and SVG are required when the module that needs them is linked.
        let q = toolkit("qt6").unwrap();
        assert!(q.requires.iter().any(|(_, f, p)| f.contains("xdgdesktopportal") && *p == "qt6-xdgdesktopportal-platformtheme"));
    }

    #[test]
    fn libraries_are_found_by_soname_in_the_image_folders() {
        let root = d("a");
        std::fs::create_dir_all(root.join("etc/ld.so.conf.d")).unwrap();
        std::fs::create_dir_all(root.join("usr/lib/x86_64-linux-gnu")).unwrap();
        std::fs::create_dir_all(root.join("opt/more")).unwrap();
        std::fs::write(root.join("etc/ld.so.conf"), "include /etc/ld.so.conf.d/*.conf\n").unwrap();
        std::fs::write(root.join("etc/ld.so.conf.d/x.conf"), "# a comment\n/opt/more\n").unwrap();
        std::fs::write(root.join("usr/lib/x86_64-linux-gnu/libreal.so.1.2.3"), b"x").unwrap();
        std::os::unix::fs::symlink("libreal.so.1.2.3", root.join("usr/lib/x86_64-linux-gnu/libreal.so.1")).unwrap();
        std::fs::write(root.join("opt/more/libmore.so.2"), b"x").unwrap();
        // A link that leaves the image is not followed out.
        std::os::unix::fs::symlink("/etc/passwd", root.join("usr/lib/x86_64-linux-gnu/libevil.so.9")).unwrap();
        let dirs = lib_dirs(&root);
        assert_eq!(dirs[0], Path::new("/opt/more"));
        assert!(find_lib(&root, &dirs, "libreal.so.1").unwrap().ends_with("libreal.so.1.2.3"));
        assert!(find_lib(&root, &dirs, "libmore.so.2").is_some());
        assert!(find_lib(&root, &dirs, "libevil.so.9").is_none());
        assert!(find_lib(&root, &dirs, "libnothing.so.0").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn globs_with_a_star_anywhere() {
        assert!(glob("libqwayland*.so", "libqwayland-egl.so") && glob("libqwayland*.so", "libqwayland.so"));
        assert!(!glob("libqwayland*.so", "libqxcb.so") && !glob("libqwayland*.so", "libqwayland-egl.so.1"));
        assert!(glob("*.so", "a.so") && glob("*", "x") && glob("a*b*c", "aXbYc") && !glob("a*b*c", "aXbY"));
        assert!(glob("share/doc/*", "share/doc/a/b") && glob("exact", "exact") && !glob("exact", "exact2"));
        assert!(!glob("ab*ba", "aba"), "the two ends must not overlap");
    }

    #[test]
    fn copy_tree_keeps_links_and_modes() {
        let (a, b) = (d("c1"), d("c2"));
        std::fs::create_dir_all(a.join("bin")).unwrap();
        std::fs::write(a.join("bin/app"), b"x").unwrap();
        std::fs::set_permissions(a.join("bin/app"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("app", a.join("bin/alias")).unwrap();
        copy_tree(&a, &b.join("t")).unwrap();
        assert_eq!(std::fs::metadata(b.join("t/bin/app")).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read_link(b.join("t/bin/alias")).unwrap(), Path::new("app"));
        let _ = (std::fs::remove_dir_all(&a), std::fs::remove_dir_all(&b));
    }
}
