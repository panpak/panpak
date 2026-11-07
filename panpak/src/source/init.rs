// SPDX-License-Identifier: MIT OR Apache-2.0
//! `panpak init`: look at a project folder and write a first recipe (`panpak.toml`) and the lock (`panpak.lock`). Nothing of the project
//! is run. What cannot be guessed is left empty, with a comment, and `panpak build` refuses a recipe with such a hole.
use super::{lock, recipe};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub system: Option<&'static str>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub icon_svg: Option<String>,
    pub toolkit: Option<&'static str>,
    pub git: Option<String>,
    pub commit: Option<String>,
}

fn walk(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with('.') || n == "node_modules" || n == "target" || n == "build" {
            continue;
        }
        match p.symlink_metadata() {
            Ok(m) if m.is_dir() => walk(&p, depth + 1, out),
            Ok(m) if m.is_file() => out.push(p),
            _ => {}
        }
    }
}

/// The value of `key=` in a desktop-entry style file (first `[Desktop Entry]` group only matters here).
fn ini_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')).map(|v| v.trim().to_string()))
}

fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new("git").args(args).current_dir(dir).stdin(std::process::Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

/// `git@host:o/r.git` and `https://host/o/r` to `https://host/o/r.git`.
pub fn https_url(u: &str) -> Option<String> {
    let u = u.trim();
    let r = if let Some(rest) = u.strip_prefix("git@") {
        let (h, p) = rest.split_once(':')?;
        format!("https://{h}/{p}")
    } else if u.starts_with("https://") {
        u.to_string()
    } else if let Some(rest) = u.strip_prefix("ssh://git@") {
        format!("https://{rest}")
    } else {
        return None;
    };
    Some(if r.ends_with(".git") { r } else { format!("{r}.git") })
}

pub fn detect(dir: &Path) -> Found {
    let mut f = Found::default();
    let has = |n: &str| dir.join(n).is_file();
    let text = |n: &str| std::fs::read_to_string(dir.join(n)).unwrap_or_default();
    if has("meson.build") {
        f.system = Some("meson");
        let t = text("meson.build");
        f.version = t.split("version:").nth(1).and_then(|r| r.split(['\'', '"']).nth(1)).map(str::to_string);
        if t.contains("gtk4") || t.contains("libadwaita") {
            f.toolkit = Some("gtk4");
        }
    } else if has("CMakeLists.txt") {
        f.system = Some("cmake");
        let t = text("CMakeLists.txt");
        f.version = t.split("VERSION").nth(1).and_then(|r| r.split_whitespace().next()).map(|v| v.trim_matches(|c: char| !c.is_ascii_digit() && c != '.').to_string()).filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()));
        if t.contains("Qt6") {
            f.toolkit = Some("qt6");
        }
    } else if has("build.zig") {
        f.system = Some("zig");
        f.version = text("build.zig.zon").lines().find_map(|l| l.trim().strip_prefix(".version").and_then(|r| r.split('"').nth(1)).map(str::to_string));
    }
    let mut files = Vec::new();
    walk(dir, 0, &mut files);
    files.sort();
    let rel = |p: &Path| p.strip_prefix(dir).unwrap_or(p).display().to_string();
    // The application id: the name of a desktop entry, or of a D-Bus service file.
    let desk = files.iter().find(|p| p.extension().is_some_and(|e| e == "desktop" || e == "in") && p.file_name().is_some_and(|n| n.to_string_lossy().contains(".desktop")));
    if let Some(d) = desk {
        let stem = d.file_name().unwrap().to_string_lossy().split(".desktop").next().unwrap_or("").to_string();
        if stem.contains('.') {
            f.id = Some(stem);
        }
        f.name = ini_value(&std::fs::read_to_string(d).unwrap_or_default(), "Name");
    }
    if f.id.is_none() {
        for s in files.iter().filter(|p| p.extension().is_some_and(|e| e == "service") && p.to_string_lossy().contains("dbus")) {
            if let Some(n) = ini_value(&std::fs::read_to_string(s).unwrap_or_default(), "Name") {
                f.id = Some(n);
                break;
            }
        }
    }
    if let Some(id) = &f.id {
        f.icon_svg = files.iter().find(|p| p.extension().is_some_and(|e| e == "svg") && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(id.as_str())) && !p.to_string_lossy().contains("symbolic")).map(|p| rel(p));
    }
    if dir.join(".git").exists() {
        f.git = git_out(dir, &["config", "--get", "remote.origin.url"]).and_then(|u| https_url(&u));
        f.commit = git_out(dir, &["rev-parse", "HEAD"]).filter(|c| c.len() == 40);
    }
    f
}

fn q(v: &Option<String>) -> String {
    format!("\"{}\"", v.as_deref().unwrap_or("").replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn recipe_text(f: &Found) -> String {
    let todo = |v: &Option<String>, what: &str| if v.is_none() { format!("   # TODO: {what}") } else { String::new() };
    let sys = f.system.unwrap_or("custom");
    let mut s = String::new();
    s += "# Recipe written by `panpak init`. Fill in what is marked TODO, then run `panpak build --secret-file <key>`.\n\n[app]\n";
    s += &format!("id = {}{}\n", q(&f.id), todo(&f.id, "the application id, reverse DNS (it is also the D-Bus name of a GApplication)"));
    s += &format!("name = {}{}\n", q(&f.name), todo(&f.name, "the name shown in the menu"));
    s += &format!("version = {}{}\n", q(&f.version), todo(&f.version, "the version"));
    s += "entry = \"bin/CHANGE-ME\"   # TODO: the program, relative to the installed tree (the prefix /usr is the root)\n\n[source]\n";
    match &f.git {
        Some(u) => s += &format!("git = {}\n", q(&Some(u.clone()))),
        None => s += "git = \"\"   # TODO: an https:// URL (or use url and sha256 for a tarball)\n",
    }
    s += &format!("commit = {}{}\n", q(&f.commit), if f.commit.is_none() { "   # TODO: the full commit id: a tag can move, so it is refused" } else { "   # the commit that is checked out now" });
    s += "\n[build]\n";
    s += &format!("system = \"{sys}\"\n");
    s += match f.system {
        Some("meson") => "configure = [\"--prefix=/usr\", \"--libdir=lib\", \"--buildtype=release\"]\n",
        Some("cmake") => "configure = [\"-DCMAKE_BUILD_TYPE=Release\", \"-DCMAKE_INSTALL_PREFIX=/usr\", \"-DCMAKE_INSTALL_LIBDIR=lib\"]\n",
        Some("zig") => "configure = [\"--prefix\", \"/usr\", \"-Doptimize=ReleaseFast\", \"-Dcpu=baseline\"]\n# TODO: zig = { version = \"0.15.2\", sha256 = \"<sha256 of the Zig archive>\" }\nnetwork = \"fetch\"\n",
        _ => "steps = []   # TODO: the steps, each a list of arguments (DESTDIR=/out is set; install under /usr)\n",
    };
    s += "packages = []   # TODO: the -dev packages and tools of the build image that this project needs\n# memory = \"2G\"   # `panpak build` measures the peak and tells you what to write here\n";
    s += "\n[collect]\n";
    s += &format!("toolkit = \"{}\"\n", f.toolkit.unwrap_or("none"));
    s += "\n[desktop]\n";
    match &f.icon_svg {
        Some(p) => s += &format!("icon_svg = \"{p}\"\n"),
        None => s += "# icon_svg = \"share/icons/hicolor/scalable/apps/<id>.svg\"   # or icons_png = [...]\n",
    }
    s += "\n[permissions]\n# Ask for what the app needs and no more: network, notifications, tray, xdg_dirs, dbus_own ...\n\n[license]\nspdx = \"\"   # TODO\nfiles = []   # TODO: files of the source, for example [\"COPYING\"]\nbundled = \"auto\"\n";
    s
}

pub fn cmd_init(dir: &Path) -> Result<()> {
    let p = dir.join("panpak.toml");
    if p.exists() {
        bail!("{} exists: init does not overwrite a recipe", p.display());
    }
    let f = detect(dir);
    std::fs::write(&p, recipe_text(&f)).with_context(|| format!("write {}", p.display()))?;
    let (_, wrote) = lock::resolve(dir, recipe::DEFAULT_IMAGE, false)?;
    eprintln!("panpak init: wrote {}{}", p.display(), if wrote { " and panpak.lock" } else { "" });
    eprintln!("  found: build system {}, id {}, name {}, version {}, icon {}, toolkit {}, source {}", f.system.unwrap_or("-"), f.id.as_deref().unwrap_or("-"), f.name.as_deref().unwrap_or("-"), f.version.as_deref().unwrap_or("-"), f.icon_svg.as_deref().unwrap_or("-"), f.toolkit.unwrap_or("-"), f.git.as_deref().unwrap_or("-"));
    eprintln!("  fill in the lines marked TODO in the recipe; `panpak build` refuses a recipe with an empty id, source or commit");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("data/icons")).unwrap();
        std::fs::write(d.join("meson.build"), "project('demo', 'c',\n  version: '2.1.0',\n)\ndependency('gtk4')\n").unwrap();
        std::fs::write(d.join("data/org.example.Demo.desktop.in"), "[Desktop Entry]\nName=Demo App\nExec=demo\n").unwrap();
        std::fs::write(d.join("data/icons/org.example.Demo.svg"), "<svg/>").unwrap();
        std::fs::write(d.join("data/icons/org.example.Demo-symbolic.svg"), "<svg/>").unwrap();
        d
    }

    #[test]
    fn a_meson_gtk_project_is_recognised() {
        let d = project();
        let f = detect(&d);
        assert_eq!((f.system, f.toolkit), (Some("meson"), Some("gtk4")));
        assert_eq!((f.id.as_deref(), f.name.as_deref(), f.version.as_deref()), (Some("org.example.Demo"), Some("Demo App"), Some("2.1.0")));
        assert_eq!(f.icon_svg.as_deref(), Some("data/icons/org.example.Demo.svg"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn what_is_missing_is_marked_and_build_refuses_it() {
        let d = project();
        cmd_init(&d).unwrap();
        let text = std::fs::read_to_string(d.join("panpak.toml")).unwrap();
        assert!(text.contains("TODO") && text.contains("id = \"org.example.Demo\"") && text.contains("commit = \"\""));
        // The recipe does not load until the holes are filled: no commit, no entry.
        assert!(recipe::Recipe::parse(&text).is_err());
        assert!(d.join("panpak.lock").exists());
        // init does not overwrite.
        assert!(cmd_init(&d).unwrap_err().to_string().contains("does not overwrite"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn remote_urls_become_https() {
        assert_eq!(https_url("git@github.com:o/r.git").unwrap(), "https://github.com/o/r.git");
        assert_eq!(https_url("https://gitlab.gnome.org/GNOME/x").unwrap(), "https://gitlab.gnome.org/GNOME/x.git");
        assert_eq!(https_url("ssh://git@host/o/r").unwrap(), "https://host/o/r.git");
        assert!(https_url("/local/path").is_none());
    }
}
