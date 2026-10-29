// SPDX-License-Identifier: MIT OR Apache-2.0
//! The user's folders (`~/.config/user-dirs.dirs`): finding the one an app asks for, the pin made at
//! install, and the `user-dirs.dirs` the app gets in its private config folder.
//! The parser is rigid: any line it does not understand makes the whole file unusable, and the
//! result is a folder below the home folder that passes the same rules as `home_paths`.
//! Rules: PANPAK-FORMAT.md §3.1.4 and §9.1.
use crate::validate::home_rel_problem;
use anyhow::{bail, ensure, Context, Result};
use std::{collections::HashMap, fs, path::Path};

/// The folders an app can ask for with `permissions.xdg_dirs`.
pub const XDG_DIR_NAMES: &[&str] = &["music", "documents", "pictures", "videos", "download"];

/// First line of the `user-dirs.dirs` the stub writes. A file without it is the user's.
pub const USER_DIRS_MARKER: &str = "X-Panpak-Generated";

/// Every key of the file, in its usual order: (name, key).
const KEYS: &[(&str, &str)] = &[
    ("desktop", "DESKTOP"),
    ("download", "DOWNLOAD"),
    ("templates", "TEMPLATES"),
    ("publicshare", "PUBLICSHARE"),
    ("documents", "DOCUMENTS"),
    ("music", "MUSIC"),
    ("pictures", "PICTURES"),
    ("videos", "VIDEOS"),
];

const MAX_LINE: usize = 600;
const MAX_FILE: usize = 16 << 10;
/// The longest folder path: a line `XDG_PUBLICSHARE_DIR="$HOME/<path>"` must fit in `MAX_LINE`.
const MAX_REL: usize = 400;

/// The key of a folder name (`music` -> `MUSIC`).
pub fn user_dirs_key(name: &str) -> Option<&'static str> {
    KEYS.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
}

/// The name xdg-user-dirs uses when there is no file (`Music`, `Documents`, ..., `Downloads`).
pub fn default_user_dir(name: &str) -> Option<&'static str> {
    Some(match name {
        "music" => "Music",
        "documents" => "Documents",
        "pictures" => "Pictures",
        "videos" => "Videos",
        "download" => "Downloads",
        _ => return None,
    })
}

/// A path that can be written between quotes in a file others parse: no quote, backslash or `$`, and
/// short enough for a line of the file to stay under `MAX_LINE`.
fn quote_safe(rel: &str) -> bool {
    rel.len() <= MAX_REL && !rel.bytes().any(|b| matches!(b, b'"' | b'\\' | b'$' | b'`'))
}

/// Parse the file. Result: folder name (`music`) -> path relative to `home`, empty for "no folder"
/// (the file points the key at the home folder itself, which means disabled). Any line that is not
/// blank, a comment or `XDG_<KEY>_DIR="<value>"` with a known key, a value that is `$HOME` or
/// `$HOME/...` or an absolute path in `home`, and no other character that a shell would expand, is an error.
pub fn parse_user_dirs(text: &str, home: &Path) -> Result<HashMap<String, String>> {
    ensure!(text.len() <= MAX_FILE, "user-dirs.dirs is too large");
    let home_text = home.to_str().context("home folder is not UTF-8")?.trim_end_matches('/');
    let mut out = HashMap::new();
    for (n, line) in text.lines().enumerate() {
        let at = || format!("user-dirs.dirs line {}", n + 1);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        ensure!(line.len() <= MAX_LINE && !line.chars().any(char::is_control), "{}: not understood", at());
        let rest = line.strip_prefix("XDG_").with_context(|| format!("{}: not understood", at()))?;
        let (key, value) = rest.split_once("_DIR=\"").with_context(|| format!("{}: not understood", at()))?;
        let value = value.strip_suffix('"').with_context(|| format!("{}: not understood", at()))?;
        let name = KEYS.iter().find(|(_, k)| *k == key).map(|(n, _)| *n).with_context(|| format!("{}: unknown key {key}", at()))?;
        ensure!(!value.contains('"') && !value.contains('\\') && !value.contains('`'), "{}: not understood", at());
        // One slash after the home folder and one at the end are normal; any other empty part is kept so
        // that the rules refuse it.
        let tidy = |after: &str| -> String {
            let rest = after.strip_prefix('/').unwrap_or(after);
            rest.strip_suffix('/').unwrap_or(rest).to_string()
        };
        let rel = if let Some(after) = value.strip_prefix("$HOME") {
            ensure!(after.is_empty() || after.starts_with('/'), "{}: not understood", at());
            tidy(after)
        } else if let Some(after) = value.strip_prefix(home_text) {
            ensure!(after.is_empty() || after.starts_with('/'), "{}: outside the home folder", at());
            tidy(after)
        } else {
            bail!("{}: {name} is outside the home folder", at());
        };
        ensure!(!rel.contains('$'), "{}: not understood", at());
        ensure!(out.insert(name.to_string(), rel).is_none(), "{}: {key} twice", at());
    }
    Ok(out)
}

/// The folder for `name` as a path relative to `home`, from the text of the user's `user-dirs.dirs`
/// (None if there is no such file). Without a file the default name is used (the folder is checked
/// like any other). With a file that is not understood, or that does not name the folder, or that
/// points it at the home folder, the answer is an error: the permission is not granted.
pub fn resolve_xdg_dir(name: &str, home: &Path, text: Option<&str>) -> Result<String> {
    ensure!(XDG_DIR_NAMES.contains(&name), "unknown user folder {name}");
    let rel = match text {
        None => default_user_dir(name).context("unknown user folder")?.to_string(),
        Some(t) => {
            let map = parse_user_dirs(t, home)?;
            let rel = map.get(name).with_context(|| format!("user-dirs.dirs does not name {name}"))?.clone();
            ensure!(!rel.is_empty(), "{name} points to the home folder (disabled)");
            rel
        }
    };
    if let Some(why) = home_rel_problem(&rel) {
        bail!("{name}: {why}");
    }
    ensure!(quote_safe(&rel), "{name}: the path has a character that is not allowed");
    Ok(rel)
}

/// Refuse a link or a file in the way: every part of `rel` below `home` that exists must be a real
/// folder (a missing part ends the check; the last part may be missing).
pub fn no_links(home: &Path, rel: &str) -> Result<()> {
    let mut cur = home.to_path_buf();
    let parts: Vec<&str> = rel.split('/').collect();
    for part in parts.iter() {
        cur.push(part);
        match fs::symlink_metadata(&cur) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("check {}", cur.display())),
            Ok(m) => {
                ensure!(!m.file_type().is_symlink(), "{} is a symbolic link", cur.display());
                ensure!(m.is_dir(), "{} is not a folder", cur.display());
            }
        }
    }
    Ok(())
}

/// The pin kept in the store: one `name=path` line per granted folder.
pub fn format_pin(dirs: &[(String, String)]) -> String {
    let mut out = String::from("# panpak: user folders of this app, fixed when it was installed or updated\n");
    for (n, rel) in dirs {
        out += &format!("{n}={rel}\n");
    }
    out
}

/// Read a pin. Strict: an unknown name or a path that fails the rules is an error.
pub fn parse_pin(text: &str) -> Result<HashMap<String, String>> {
    ensure!(text.len() <= MAX_FILE, "pin too large");
    let mut out = HashMap::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (n, rel) = line.split_once('=').context("pin line without =")?;
        ensure!(XDG_DIR_NAMES.contains(&n), "pin: unknown folder {n}");
        ensure!(home_rel_problem(rel).is_none() && quote_safe(rel), "pin: bad path for {n}");
        ensure!(out.insert(n.to_string(), rel.to_string()).is_none(), "pin: {n} twice");
    }
    Ok(out)
}

/// The `user-dirs.dirs` for the private config folder: the granted folders with their real
/// path, every other key at `$HOME/`, which xdg-user-dirs, GLib and Qt read as "not available".
pub fn generated_user_dirs(granted: &[(String, String)]) -> Result<String> {
    let mut out = format!("# {USER_DIRS_MARKER}: written by panpak-stub at every start. Delete this line to keep your own copy.\n");
    for (name, key) in KEYS {
        match granted.iter().find(|(n, _)| n == name) {
            Some((_, rel)) => {
                ensure!(home_rel_problem(rel).is_none() && quote_safe(rel), "bad path for {name}");
                out += &format!("XDG_{key}_DIR=\"$HOME/{rel}\"\n");
            }
            None => out += &format!("XDG_{key}_DIR=\"$HOME/\"\n"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/u";

    fn sample() -> &'static str {
        "# This file is written by xdg-user-dirs-update\nXDG_DESKTOP_DIR=\"$HOME/Scrivania\"\nXDG_DOWNLOAD_DIR=\"$HOME/Scaricati\"\n\
XDG_TEMPLATES_DIR=\"$HOME/Modelli\"\nXDG_PUBLICSHARE_DIR=\"$HOME/Pubblici\"\nXDG_DOCUMENTS_DIR=\"$HOME/Documenti\"\n\
XDG_MUSIC_DIR=\"$HOME/Musica\"\nXDG_PICTURES_DIR=\"$HOME/Immagini\"\nXDG_VIDEOS_DIR=\"$HOME/Video\"\n"
    }

    #[test]
    fn a_normal_file_gives_the_folders() {
        let m = parse_user_dirs(sample(), Path::new(HOME)).unwrap();
        assert_eq!(m["music"], "Musica");
        assert_eq!(m["download"], "Scaricati");
        assert_eq!(resolve_xdg_dir("music", Path::new(HOME), Some(sample())).unwrap(), "Musica");
        assert_eq!(resolve_xdg_dir("videos", Path::new(HOME), Some(sample())).unwrap(), "Video");
        // An absolute path inside the home folder is accepted; a nested folder too.
        let t = "XDG_MUSIC_DIR=\"/home/u/Media/Music\"\n";
        assert_eq!(resolve_xdg_dir("music", Path::new(HOME), Some(t)).unwrap(), "Media/Music");
        let t = "XDG_MUSIC_DIR=\"$HOME/Media/Music/\"\n";
        assert_eq!(resolve_xdg_dir("music", Path::new(HOME), Some(t)).unwrap(), "Media/Music");
    }

    #[test]
    fn without_a_file_the_default_name_is_used() {
        assert_eq!(resolve_xdg_dir("music", Path::new(HOME), None).unwrap(), "Music");
        assert_eq!(resolve_xdg_dir("download", Path::new(HOME), None).unwrap(), "Downloads");
        assert!(resolve_xdg_dir("desktop", Path::new(HOME), None).is_err());
        assert!(resolve_xdg_dir("nonsense", Path::new(HOME), None).is_err());
    }

    #[test]
    fn a_hostile_or_odd_file_gives_no_folder() {
        let home = Path::new(HOME);
        for bad in [
            "XDG_MUSIC_DIR=\"/etc\"\n",                       // outside the home folder
            "XDG_MUSIC_DIR=\"/home/uu/Music\"\n",             // another home with the same prefix
            "XDG_MUSIC_DIR=\"$HOME/../x\"\n",                 // ..
            "XDG_MUSIC_DIR=\"$HOME/a/../../x\"\n",
            "XDG_MUSIC_DIR=\"$HOME\"\n",                      // the home folder itself: disabled
            "XDG_MUSIC_DIR=\"$HOME/\"\n",
            "XDG_MUSIC_DIR=\"$HOME/.ssh\"\n",                  // protected
            "XDG_MUSIC_DIR=\"$HOME/.gnupg/x\"\n",
            "XDG_MUSIC_DIR=\"$HOME/.config\"\n",               // too broad
            "XDG_MUSIC_DIR=\"$HOME/.local/bin\"\n",
            "XDG_MUSIC_DIR=\"$HOME/Music $(id)\"\n",           // shell expansion
            "XDG_MUSIC_DIR=\"$HOME/$X\"\n",
            "XDG_MUSIC_DIR=\"$HOME/a`b`\"\n",
            "XDG_MUSIC_DIR=\"$HOME/a\\\"b\"\n",
            "XDG_MUSIC_DIR=\"$HOMEx\"\n",
            "XDG_MUSIC_DIR=$HOME/Music\n",                    // no quotes
            "XDG_MUSIC_DIR=\"$HOME/Music\" extra\n",
            "XDG_MUSIC_DIR=\"$HOME/Music\"\nXDG_MUSIC_DIR=\"$HOME/Other\"\n", // twice
            "XDG_FOO_DIR=\"$HOME/x\"\n",                      // unknown key
            "rm -rf /\n",
            "XDG_MUSIC_DIR=\"$HOME//Music\"\n",               // empty part
            "XDG_MUSIC_DIR=\"$HOME/Mu\u{1}sic\"\n",           // control character
        ] {
            assert!(resolve_xdg_dir("music", home, Some(bad)).is_err(), "{bad:?}");
        }
        // A key that is missing is not guessed.
        assert!(resolve_xdg_dir("music", home, Some("XDG_VIDEOS_DIR=\"$HOME/V\"\n")).is_err());
        // Too large or too long.
        assert!(parse_user_dirs(&"#".repeat(MAX_FILE + 1), home).is_err());
        assert!(parse_user_dirs(&format!("XDG_MUSIC_DIR=\"$HOME/{}\"\n", "a".repeat(700)), home).is_err());
    }

    #[test]
    fn links_in_the_way_are_refused() {
        let base = std::env::temp_dir().join(format!("panpak-userdirs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("real/sub")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        fs::write(base.join("file"), b"x").unwrap();
        assert!(no_links(&base, "real/sub").is_ok());
        assert!(no_links(&base, "missing/deeper").is_ok());
        assert!(no_links(&base, "link").is_err());
        assert!(no_links(&base, "link/sub").is_err());
        assert!(no_links(&base, "file").is_err(), "a file is not a folder");
        assert!(no_links(&base, "file/x").is_err());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_pin_round_trips_and_is_strict() {
        let dirs = vec![("music".to_string(), "Musica".to_string()), ("documents".to_string(), "Media/Docs".to_string())];
        let text = format_pin(&dirs);
        let back = parse_pin(&text).unwrap();
        assert_eq!(back["music"], "Musica");
        assert_eq!(back["documents"], "Media/Docs");
        for bad in ["music", "desktop=Desk", "music=../x", "music=/abs", "music=a\"b", "music=a$b", "music=A\nmusic=B", "music=.ssh"] {
            assert!(parse_pin(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_generated_file_has_the_marker_and_only_the_granted_folders() {
        let t = generated_user_dirs(&[("music".to_string(), "Musica".to_string())]).unwrap();
        assert!(t.starts_with("# X-Panpak-Generated"));
        assert!(t.contains("XDG_MUSIC_DIR=\"$HOME/Musica\"\n"));
        for k in ["DESKTOP", "DOWNLOAD", "TEMPLATES", "PUBLICSHARE", "DOCUMENTS", "PICTURES", "VIDEOS"] {
            assert!(t.contains(&format!("XDG_{k}_DIR=\"$HOME/\"\n")), "{k}");
        }
        // What the stub writes, the parser reads back, and the ungranted ones are "disabled".
        let m = parse_user_dirs(&t, Path::new(HOME)).unwrap();
        assert_eq!(m["music"], "Musica");
        assert_eq!(m["documents"], "");
        assert!(generated_user_dirs(&[("music".to_string(), "a\"b".to_string())]).is_err());
        // A long path is refused everywhere, so that the generated file always parses.
        let long = "a".repeat(MAX_REL + 1);
        assert!(generated_user_dirs(&[("music".to_string(), long.clone())]).is_err());
        assert!(parse_pin(&format!("music={long}\n")).is_err());
        assert!(resolve_xdg_dir("music", Path::new(HOME), Some(&format!("XDG_MUSIC_DIR=\"$HOME/{long}\"\n"))).is_err());
    }
}
