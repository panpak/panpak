// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `[env]` table of a manifest (PANPAK-FORMAT.md §3.1.6): environment variables that point an app at files of its own tree.
//!
//! Without this, a program built from source that needs `GSETTINGS_SCHEMA_DIR` or `QT_PLUGIN_PATH` needs a shell script as its entry that
//! works the path out from its own place, and a script is code that nobody reviews. Here the values are data: a name from a fixed list and
//! a value made of `${PANPAK_ROOT}` (the folder of the package tree) and a relative path, or one of a few words. `pack` and `install`
//! check them; the runtime sets them.
use anyhow::{bail, ensure, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// The placeholder for the folder of the package tree.
pub const ROOT: &str = "${PANPAK_ROOT}";
/// At most this many variables.
pub const MAX_VARS: usize = 16;
/// At most this many paths in a list value.
pub const MAX_LIST: usize = 8;

/// How the value of a variable is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// One `${PANPAK_ROOT}/relative/path`.
    Path,
    /// `${PANPAK_ROOT}/a:${PANPAK_ROOT}/b` (a search list); the runtime puts these before what it would set itself.
    PathList,
    /// One of these words and nothing else.
    Word(&'static [&'static str]),
}

/// A variable that a manifest may set, and why it is on the list.
pub struct Allowed {
    pub name: &'static str,
    pub kind: Kind,
    pub why: &'static str,
}

/// The list. A name that is not here is refused: the ones that change how code is loaded from outside the tree (`LD_*`), what the
/// sandbox protects, or what the runtime sets itself (`PANPAK_*`, `HOME`, `PATH`, `XDG_RUNTIME_DIR`, the private folders) are not, and
/// never will be. Every path here stays inside the tree, so the signed package decides what is loaded and nothing else can.
pub const ALLOWED: &[Allowed] = &[
    Allowed { name: "XDG_DATA_DIRS", kind: Kind::PathList, why: "icon themes, MIME data, schemas and the data folders of the toolkits and of the app; put before the system's" },
    Allowed { name: "GSETTINGS_SCHEMA_DIR", kind: Kind::PathList, why: "the compiled GSettings schemas of the app and of GTK" },
    Allowed { name: "GDK_PIXBUF_MODULE_FILE", kind: Kind::Path, why: "the cache of the image loaders (SVG icons in GTK 4 before 4.20)" },
    Allowed { name: "GDK_PIXBUF_MODULEDIR", kind: Kind::Path, why: "the folder of the image loaders" },
    Allowed { name: "GIO_EXTRA_MODULES", kind: Kind::PathList, why: "GIO modules (TLS, proxies) that the app ships" },
    Allowed { name: "GTK_PATH", kind: Kind::PathList, why: "GTK modules and input methods that the app ships" },
    Allowed { name: "GTK_IM_MODULE_FILE", kind: Kind::Path, why: "the cache of the input method modules" },
    Allowed { name: "QT_PLUGIN_PATH", kind: Kind::PathList, why: "the Qt plugins: platform, Wayland integration, image formats, platform theme" },
    Allowed { name: "QML_IMPORT_PATH", kind: Kind::PathList, why: "QML modules of the app" },
    Allowed { name: "QML2_IMPORT_PATH", kind: Kind::PathList, why: "QML modules of the app (Qt 5 name)" },
    Allowed { name: "GST_PLUGIN_PATH", kind: Kind::PathList, why: "GStreamer plugins that the app ships" },
    Allowed { name: "TERMINFO_DIRS", kind: Kind::PathList, why: "the terminfo entry of a terminal emulator" },
    Allowed { name: "XCURSOR_PATH", kind: Kind::PathList, why: "cursor themes that the app ships" },
    Allowed { name: "GSETTINGS_BACKEND", kind: Kind::Word(&["keyfile", "memory"]), why: "settings in a file of the private config folder instead of dconf, which the sandbox does not reach" },
    Allowed { name: "GDK_BACKEND", kind: Kind::Word(&["wayland", "x11", "wayland,x11", "x11,wayland"]), why: "which display protocol GTK uses" },
    Allowed { name: "QT_QPA_PLATFORM", kind: Kind::Word(&["wayland", "xcb", "wayland;xcb", "xcb;wayland"]), why: "which display protocol Qt uses" },
    Allowed { name: "GTK_A11Y", kind: Kind::Word(&["none"]), why: "no accessibility bus (the filtered bus does not carry it)" },
    Allowed { name: "ELECTRON_OZONE_PLATFORM_HINT", kind: Kind::Word(&["auto", "wayland", "x11"]), why: "which display protocol Electron uses" },
];

fn bad(name: &str, why: &str) -> anyhow::Error {
    crate::coded(crate::ErrorCode::E0007, format!("invalid manifest field env.{name}: {why}"))
}

/// The relative part of one `${PANPAK_ROOT}/rel` entry, checked.
fn rel_of<'a>(name: &str, entry: &'a str) -> Result<&'a str> {
    let Some(rest) = entry.strip_prefix(ROOT) else {
        return Err(bad(name, &format!("a path must start with {ROOT}/ (no absolute path, no other variable): {entry:?}")));
    };
    let Some(rel) = rest.strip_prefix('/') else {
        return Err(bad(name, &format!("a path must be {ROOT}/ and a relative path: {entry:?}")));
    };
    if rel.len() > 512 || rel.contains([':', '$']) || crate::safe_path(rel).is_err() {
        return Err(bad(name, &format!("the relative path must stay inside the tree (no '..', no empty segment, no control character): {entry:?}")));
    }
    Ok(rel)
}

/// The paths of the tree that a variable's value names, relative to the root (empty for a word).
pub fn paths_of(name: &str, value: &str) -> Result<Vec<String>> {
    let Some(a) = ALLOWED.iter().find(|a| a.name == name) else {
        return Err(bad(name, "this variable is not on the list of those a package may set (PANPAK-FORMAT.md §3.1.6)"));
    };
    match a.kind {
        Kind::Word(ok) => {
            if !ok.contains(&value) {
                return Err(bad(name, &format!("must be one of {ok:?}")));
            }
            Ok(Vec::new())
        }
        Kind::Path => Ok(vec![rel_of(name, value)?.to_string()]),
        Kind::PathList => {
            let parts: Vec<&str> = value.split(':').collect();
            if parts.len() > MAX_LIST {
                return Err(bad(name, &format!("at most {MAX_LIST} paths")));
            }
            parts.into_iter().map(|p| Ok(rel_of(name, p)?.to_string())).collect()
        }
    }
}

/// Check a whole table: names, values, and how many.
pub fn check(env: &BTreeMap<String, String>) -> Result<()> {
    if env.len() > MAX_VARS {
        return Err(bad("*", &format!("at most {MAX_VARS} variables")));
    }
    for (k, v) in env {
        paths_of(k, v)?;
    }
    Ok(())
}

/// The variables to set for a package at `root`: `(name, value, before)`; `before` is true for the lists that go in front of a
/// value the runtime sets itself (only `XDG_DATA_DIRS`). The table is checked again here: the runtime does not trust a manifest.
pub fn expand(env: &BTreeMap<String, String>, root: &Path) -> Result<Vec<(String, String, bool)>> {
    check(env)?;
    let Some(root) = root.to_str() else { bail!("the package tree has a path that is not UTF-8") };
    ensure!(!root.contains(':') && !root.contains('\0'), "the package tree has a path with ':' in it, which a search list cannot carry");
    Ok(env.iter().map(|(k, v)| (k.clone(), v.replace(ROOT, root), k == "XDG_DATA_DIRS")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(k: &str, v: &str) -> Result<()> {
        check(&BTreeMap::from([(k.to_string(), v.to_string())]))
    }

    #[test]
    fn paths_stay_inside_the_tree() {
        one("GSETTINGS_SCHEMA_DIR", "${PANPAK_ROOT}/share/glib-2.0/schemas").unwrap();
        one("QT_PLUGIN_PATH", "${PANPAK_ROOT}/plugins:${PANPAK_ROOT}/lib/qt").unwrap();
        for bad in [
            "/usr/share/glib-2.0/schemas", "share/schemas", "${PANPAK_ROOT}", "${PANPAK_ROOT}/", "${PANPAK_ROOT}/../etc", "${PANPAK_ROOT}//a",
            "${PANPAK_ROOT}/a/./b", "${HOME}/x", "$PANPAK_ROOT/x", "${PANPAK_ROOT}x", "${PANPAK_ROOT}/a\nb", "${PANPAK_ROOT}/a:/etc", "${XDG_DATA_HOME}/x",
        ] {
            assert!(one("GSETTINGS_SCHEMA_DIR", bad).is_err(), "{bad}");
        }
        // A single path variable takes one path, not a list.
        assert!(one("GDK_PIXBUF_MODULE_FILE", "${PANPAK_ROOT}/a:${PANPAK_ROOT}/b").is_err());
        assert!(one("QT_PLUGIN_PATH", &vec!["${PANPAK_ROOT}/a"; MAX_LIST + 1].join(":")).is_err());
    }

    #[test]
    fn only_the_listed_names_and_words() {
        for name in ["LD_LIBRARY_PATH", "LD_PRELOAD", "PATH", "HOME", "PANPAK_ROOT", "XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "PYTHONPATH", "NODE_OPTIONS", "ELECTRON_RUN_AS_NODE", "GTK_MODULES", "gsettings_backend", ""] {
            assert!(one(name, "${PANPAK_ROOT}/x").is_err(), "{name}");
        }
        one("GSETTINGS_BACKEND", "keyfile").unwrap();
        one("QT_QPA_PLATFORM", "wayland").unwrap();
        assert!(one("GSETTINGS_BACKEND", "dconf").is_err());
        assert!(one("QT_QPA_PLATFORM", "${PANPAK_ROOT}/x").is_err());
        assert!(one("GTK_A11Y", "atspi").is_err());
        let many: BTreeMap<String, String> = (0..MAX_VARS + 1).map(|i| (format!("V{i}"), "x".into())).collect();
        assert!(check(&many).is_err());
    }

    #[test]
    fn every_path_variable_has_a_reason_and_a_word_is_never_a_path() {
        for a in ALLOWED {
            assert!(!a.why.is_empty() && !a.name.starts_with("LD_") && !a.name.starts_with("PANPAK_"), "{}", a.name);
        }
        assert_eq!(paths_of("GSETTINGS_BACKEND", "keyfile").unwrap(), Vec::<String>::new());
        assert_eq!(paths_of("TERMINFO_DIRS", "${PANPAK_ROOT}/share/terminfo").unwrap(), ["share/terminfo"]);
    }

    #[test]
    fn expansion_puts_the_root_in_and_marks_the_data_dirs() {
        let env = BTreeMap::from([
            ("XDG_DATA_DIRS".to_string(), "${PANPAK_ROOT}/share".to_string()),
            ("GSETTINGS_BACKEND".to_string(), "keyfile".to_string()),
        ]);
        let v = expand(&env, Path::new("/home/u/.local/share/panpak/trees/abc")).unwrap();
        assert!(v.contains(&("XDG_DATA_DIRS".into(), "/home/u/.local/share/panpak/trees/abc/share".into(), true)));
        assert!(v.contains(&("GSETTINGS_BACKEND".into(), "keyfile".into(), false)));
        assert!(expand(&env, Path::new("/a:b")).is_err());
        assert!(expand(&BTreeMap::from([("LD_PRELOAD".to_string(), "x".to_string())]), Path::new("/t")).is_err());
    }
}
