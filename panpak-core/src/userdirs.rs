// SPDX-License-Identifier: MIT OR Apache-2.0
//! The user's folders (`~/.config/user-dirs.dirs`): finding the one an app asks for, the pin made at
//! install, and the `user-dirs.dirs` the app gets in its private config folder.
//! The parser reads the lines it understands and skips the others one by one (a key it does not know is never granted); the
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

/// Why a line of the file gave no folder, and the folder it was about when it was one of ours.
#[derive(Debug, PartialEq, Eq)]
pub struct Skipped {
    pub name: Option<&'static str>,
    pub why: String,
}

/// Parse the file, line by line. The result is the folders it names well (folder name, such as `music`, to the path relative to `home`, empty for "no folder": the file
/// points the key at the home folder itself, which means disabled) and the lines that gave nothing, with the reason.
///
/// A line is useful when it is `XDG_<KEY>_DIR="<value>"` with one of the keys of `KEYS` and a value that is `$HOME` or `$HOME/...` or an absolute path in `home`, with no other
/// character that a shell would expand. Everything else is skipped, and only that line: a blank line, a comment, a key that this version does not know (xdg-user-dirs 0.19 writes
/// `XDG_PROJECTS_DIR`, and a later one may write more), a line that is not understood, a value that is outside the home folder. An unknown key is never granted. A key that is
/// written twice is dropped (which of the two is meant is not guessed). A file larger than `MAX_FILE` is not read at all.
fn parse_lenient(text: &str, home: &Path) -> Result<(HashMap<String, String>, Vec<Skipped>)> {
    ensure!(text.len() <= MAX_FILE, "user-dirs.dirs is too large");
    let home_text = home.to_str().context("home folder is not UTF-8")?.trim_end_matches('/');
    let mut out: HashMap<String, String> = HashMap::new();
    let mut skipped: Vec<Skipped> = Vec::new();
    let mut twice: Vec<&'static str> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let at = |why: &str| format!("user-dirs.dirs line {}: {why}", n + 1);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.len() > MAX_LINE || line.chars().any(char::is_control) {
            skipped.push(Skipped { name: None, why: at("not understood") });
            continue;
        }
        let Some((key, value)) = line.strip_prefix("XDG_").and_then(|r| r.split_once("_DIR=\"")).and_then(|(k, v)| Some((k, v.strip_suffix('"')?))) else {
            skipped.push(Skipped { name: None, why: at("not understood") });
            continue;
        };
        let Some(name) = KEYS.iter().find(|(_, k)| *k == key).map(|(n, _)| *n) else { continue };
        let mut refuse = |why: &str| skipped.push(Skipped { name: Some(name), why: at(why) });
        if value.contains('"') || value.contains('\\') || value.contains('`') {
            refuse("not understood");
            continue;
        }
        // One slash after the home folder and one at the end are normal; any other empty part is kept so
        // that the rules refuse it.
        let tidy = |after: &str| -> String {
            let rest = after.strip_prefix('/').unwrap_or(after);
            rest.strip_suffix('/').unwrap_or(rest).to_string()
        };
        let rel = if let Some(after) = value.strip_prefix("$HOME") {
            if !(after.is_empty() || after.starts_with('/')) {
                refuse("not understood");
                continue;
            }
            tidy(after)
        } else if let Some(after) = value.strip_prefix(home_text) {
            if !(after.is_empty() || after.starts_with('/')) {
                refuse("outside the home folder");
                continue;
            }
            tidy(after)
        } else {
            refuse(&format!("{name} is outside the home folder"));
            continue;
        };
        if rel.contains('$') {
            refuse("not understood");
            continue;
        }
        if out.insert(name.to_string(), rel).is_some() {
            twice.push(name);
            skipped.push(Skipped { name: Some(name), why: at(&format!("{key} twice")) });
        }
    }
    for name in twice {
        out.remove(name);
    }
    Ok((out, skipped))
}

/// The folders the file names well (see `parse_lenient`); an error only when the file is too large.
pub fn parse_user_dirs(text: &str, home: &Path) -> Result<HashMap<String, String>> {
    parse_lenient(text, home).map(|(m, _)| m)
}

/// The folder for `name` as a path relative to `home`, from the text of the user's `user-dirs.dirs`
/// (None if there is no such file). Without a file the default name is used (the folder is checked
/// like any other). With a file that does not name the folder well, or that points it at the home folder,
/// the answer is an error: the permission is not granted. The other lines of the file do not matter.
pub fn resolve_xdg_dir(name: &str, home: &Path, text: Option<&str>) -> Result<String> {
    ensure!(XDG_DIR_NAMES.contains(&name), "unknown user folder {name}");
    let rel = match text {
        None => default_user_dir(name).context("unknown user folder")?.to_string(),
        Some(t) => {
            let (map, skipped) = parse_lenient(t, home)?;
            let rel = match map.get(name) {
                Some(r) => r.clone(),
                // The line about this folder was refused: say why; or the file has none.
                None => match skipped.iter().find(|k| k.name == Some(name)) {
                    Some(k) => bail!("{}", k.why),
                    None => bail!("user-dirs.dirs does not name {name}"),
                },
            };
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
        "# This file is written by xdg-user-dirs-update\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n\
XDG_TEMPLATES_DIR=\"$HOME/Templates\"\nXDG_PUBLICSHARE_DIR=\"$HOME/Public\"\nXDG_DOCUMENTS_DIR=\"$HOME/Documents\"\n\
XDG_MUSIC_DIR=\"$HOME/Music\"\nXDG_PICTURES_DIR=\"$HOME/Pictures\"\nXDG_VIDEOS_DIR=\"$HOME/Videos\"\n"
    }

    #[test]
    fn a_normal_file_gives_the_folders() {
        let m = parse_user_dirs(sample(), Path::new(HOME)).unwrap();
        assert_eq!(m["music"], "Music");
        assert_eq!(m["download"], "Downloads");
        assert_eq!(resolve_xdg_dir("music", Path::new(HOME), Some(sample())).unwrap(), "Music");
        assert_eq!(resolve_xdg_dir("videos", Path::new(HOME), Some(sample())).unwrap(), "Videos");
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
        // Too large: not read at all. Too long a line: skipped, and then the folder is not named.
        assert!(parse_user_dirs(&"#".repeat(MAX_FILE + 1), home).is_err());
        assert!(parse_user_dirs(&format!("XDG_MUSIC_DIR=\"$HOME/{}\"\n", "a".repeat(700)), home).unwrap().is_empty());
    }

    /// The file that xdg-user-dirs 0.19 wrote on Fedora 45 (KDE), with the key `PROJECTS` that the first version of the parser refused as a whole (the folders were not granted).
    const FEDORA_45: &str = "# This file is written by xdg-user-dirs-update\n# If you want to change or add directories, just edit the line you're\n# interested in. All local changes will be retained on the next run.\n# Format is XDG_xxx_DIR=\"$HOME/yyy\", where yyy is a shell-escaped\n# homedir-relative path, or XDG_xxx_DIR=\"/yyy\", where /yyy is an\n# absolute path. No other format is supported.\n# \nXDG_DESKTOP_DIR=\"$HOME/Scrivania\"\nXDG_DOWNLOAD_DIR=\"$HOME/Scaricati\"\nXDG_TEMPLATES_DIR=\"$HOME/Modelli\"\nXDG_PUBLICSHARE_DIR=\"$HOME/Pubblici\"\nXDG_DOCUMENTS_DIR=\"$HOME/Documenti\"\nXDG_MUSIC_DIR=\"$HOME/Musica\"\nXDG_PICTURES_DIR=\"$HOME/Immagini\"\nXDG_VIDEOS_DIR=\"$HOME/Video\"\nXDG_PROJECTS_DIR=\"$HOME/Progetti\"\n";

    #[test]
    fn a_real_file_of_fedora_45_with_a_key_this_version_does_not_know_gives_the_folders() {
        let home = Path::new("/home/fabrizio");
        for (name, want) in [("download", "Scaricati"), ("documents", "Documenti"), ("music", "Musica"), ("pictures", "Immagini"), ("videos", "Video")] {
            assert_eq!(resolve_xdg_dir(name, home, Some(FEDORA_45)).unwrap(), want, "{name}");
        }
        let m = parse_user_dirs(FEDORA_45, home).unwrap();
        assert_eq!(m.len(), 8, "the eight keys of the list, and nothing for PROJECTS: {m:?}");
        assert!(!m.contains_key("projects"));
    }

    #[test]
    fn unknown_keys_and_bad_lines_are_skipped_one_by_one_and_never_granted() {
        let home = Path::new(HOME);
        let t = "XDG_MADE_UP_DIR=\"$HOME/Made\"\nXDG_MUSIC_DIR=\"$HOME/Music\"\nrm -rf /\nXDG_FUTURE_DIR=\"/etc\"\nnot even a line\nXDG_DOCUMENTS_DIR=\"$HOME/Docs\"\n\
XDG_PICTURES_DIR=\"/etc/Pictures\"\nXDG_VIDEOS_DIR=\"$HOME/V\" extra\nXDG_DOWNLOAD_DIR=\"$HOME/Down\"\nXDG_DOWNLOAD_DIR=\"$HOME/Other\"\nXDG_LONG_DIR=\"$HOME/Long\u{1}\"\n";
        // The good lines are kept, whatever surrounds them.
        assert_eq!(resolve_xdg_dir("music", home, Some(t)).unwrap(), "Music");
        assert_eq!(resolve_xdg_dir("documents", home, Some(t)).unwrap(), "Docs");
        // An invented key is not a folder an app can have, a bad value for a known key gives that folder an error with the reason, and the others are not touched by it.
        assert!(resolve_xdg_dir("made_up", home, Some(t)).is_err());
        let e = resolve_xdg_dir("pictures", home, Some(t)).unwrap_err().to_string();
        assert!(e.contains("line 7") && e.contains("outside the home folder"), "{e}");
        assert!(resolve_xdg_dir("videos", home, Some(t)).is_err(), "a line with text after the quote is not understood");
        // A key that is written twice is dropped: which one is meant is not guessed.
        let e = resolve_xdg_dir("download", home, Some(t)).unwrap_err().to_string();
        assert!(e.contains("twice"), "{e}");
        let m = parse_user_dirs(t, home).unwrap();
        let mut names: Vec<&str> = m.keys().map(String::as_str).collect();
        names.sort();
        assert_eq!(names, ["documents", "music"]);
        // Nothing an invented key says is ever a folder: not even a plausible one.
        for key in ["PROJECTS", "FOO", "MUSICX", "music", "", "XDG"] {
            let t = format!("XDG_{key}_DIR=\"$HOME/Anything\"\n");
            for name in XDG_DIR_NAMES {
                assert!(resolve_xdg_dir(name, home, Some(&t)).is_err(), "{key} -> {name}");
            }
        }
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
        let dirs = vec![("music".to_string(), "Music".to_string()), ("documents".to_string(), "Media/Docs".to_string())];
        let text = format_pin(&dirs);
        let back = parse_pin(&text).unwrap();
        assert_eq!(back["music"], "Music");
        assert_eq!(back["documents"], "Media/Docs");
        for bad in ["music", "desktop=Desk", "music=../x", "music=/abs", "music=a\"b", "music=a$b", "music=A\nmusic=B", "music=.ssh"] {
            assert!(parse_pin(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_generated_file_has_the_marker_and_only_the_granted_folders() {
        let t = generated_user_dirs(&[("music".to_string(), "Music".to_string())]).unwrap();
        assert!(t.starts_with("# X-Panpak-Generated"));
        assert!(t.contains("XDG_MUSIC_DIR=\"$HOME/Music\"\n"));
        for k in ["DESKTOP", "DOWNLOAD", "TEMPLATES", "PUBLICSHARE", "DOCUMENTS", "PICTURES", "VIDEOS"] {
            assert!(t.contains(&format!("XDG_{k}_DIR=\"$HOME/\"\n")), "{k}");
        }
        // What the stub writes, the parser reads back, and the ungranted ones are "disabled".
        let m = parse_user_dirs(&t, Path::new(HOME)).unwrap();
        assert_eq!(m["music"], "Music");
        assert_eq!(m["documents"], "");
        assert!(generated_user_dirs(&[("music".to_string(), "a\"b".to_string())]).is_err());
        // A long path is refused everywhere, so that the generated file always parses.
        let long = "a".repeat(MAX_REL + 1);
        assert!(generated_user_dirs(&[("music".to_string(), long.clone())]).is_err());
        assert!(parse_pin(&format!("music={long}\n")).is_err());
        assert!(resolve_xdg_dir("music", Path::new(HOME), Some(&format!("XDG_MUSIC_DIR=\"$HOME/{long}\"\n"))).is_err());
    }
}
