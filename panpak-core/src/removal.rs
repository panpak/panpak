// SPDX-License-Identifier: MIT OR Apache-2.0
//! The journal of a removal: what `panpak remove` still has to delete from the shared parts of the
//! store once the folder of the app has been moved aside. The journal sits inside that folder, so a
//! removal that was interrupted is finished by the next panpak command from this file alone.
//! Layout and rules: PANPAK-FORMAT.md §9.6.
use crate::{validate_app_id, ErrorCode};
use anyhow::Result;

/// First line of the journal.
pub const JOURNAL_MAGIC: &str = "panpak-removal 1";
/// Name of the journal inside the moved folder.
pub const JOURNAL_FILE: &str = "journal";
/// A journal is read up to this size and these many lines.
pub const MAX_JOURNAL_BYTES: usize = 1 << 20;
pub const MAX_JOURNAL_LINES: usize = 10_000;

/// What a removal has still to do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Journal {
    pub app_id: String,
    /// File names in `packages/` that belong to the app.
    pub packages: Vec<String>,
    /// Tree roots (64 lower-case hex digits) that the app used: removed from `trees/` and `farms/`
    /// unless a package that stays uses them too.
    pub roots: Vec<String>,
}

/// A package file name as the store writes it: `<id>-<version>.panpak`, plain characters only.
pub fn valid_package_name(n: &str) -> bool {
    n.len() <= 255
        && n.ends_with(".panpak")
        && !n.starts_with('.')
        && n.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
        && !n.contains("..")
}

pub fn valid_root(r: &str) -> bool {
    r.len() == 64 && r.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Journal {
    pub fn render(&self) -> String {
        let mut s = format!("{JOURNAL_MAGIC}\napp {}\n", self.app_id);
        for p in &self.packages {
            s.push_str(&format!("package {p}\n"));
        }
        for r in &self.roots {
            s.push_str(&format!("root {r}\n"));
        }
        s
    }

    /// Read a journal. Anything that is not exactly the format is an error: the names it lists are
    /// deleted from the store, so they are checked here, not trusted.
    pub fn parse(bytes: &[u8]) -> Result<Journal> {
        let bad = |m: &str| crate::coded(ErrorCode::E2003, format!("unreadable removal journal: {m}"));
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(bad("too large"));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| bad("not UTF-8"))?;
        let mut lines = text.lines();
        if lines.next() != Some(JOURNAL_MAGIC) {
            return Err(bad("wrong first line"));
        }
        let mut j = Journal::default();
        let mut have_app = false;
        for (i, line) in lines.enumerate() {
            if i >= MAX_JOURNAL_LINES {
                return Err(bad("too many lines"));
            }
            let (key, val) = line.split_once(' ').ok_or_else(|| bad("a line without a value"))?;
            match key {
                "app" => {
                    if have_app {
                        return Err(bad("two app lines"));
                    }
                    validate_app_id(val).map_err(|_| bad("invalid app id"))?;
                    j.app_id = val.to_string();
                    have_app = true;
                }
                "package" => {
                    if !valid_package_name(val) {
                        return Err(bad("invalid package name"));
                    }
                    if !j.packages.iter().any(|p| p == val) {
                        j.packages.push(val.to_string());
                    }
                }
                "root" => {
                    if !valid_root(val) {
                        return Err(bad("invalid tree root"));
                    }
                    if !j.roots.iter().any(|r| r == val) {
                        j.roots.push(val.to_string());
                    }
                }
                _ => return Err(bad("unknown line")),
            }
        }
        if !have_app {
            return Err(bad("no app line"));
        }
        Ok(j)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Journal {
        Journal { app_id: "org.example.app".into(), packages: vec!["org.example.app-1.0.panpak".into()], roots: vec!["a".repeat(64)] }
    }

    #[test]
    fn a_journal_survives_a_round_trip() {
        let j = sample();
        assert_eq!(Journal::parse(j.render().as_bytes()).unwrap(), j);
    }

    #[test]
    fn names_that_could_leave_the_folder_are_refused() {
        for p in ["../x.panpak", "a/b.panpak", "/etc/passwd.panpak", ".hidden.panpak", "a..b.panpak", "x", "x.panpak\n", "a b.panpak", ""] {
            let t = format!("{JOURNAL_MAGIC}\napp org.example.app\npackage {p}\n");
            assert!(Journal::parse(t.as_bytes()).is_err(), "{p:?}");
        }
        for r in ["", "abc", &"A".repeat(64), &"g".repeat(64), &"a".repeat(65), "../../x"] {
            let t = format!("{JOURNAL_MAGIC}\napp org.example.app\nroot {r}\n");
            assert!(Journal::parse(t.as_bytes()).is_err(), "{r:?}");
        }
    }

    #[test]
    fn the_format_is_strict() {
        let ok = sample().render();
        assert!(Journal::parse(ok.replace("panpak-removal 1", "panpak-removal 2").as_bytes()).is_err());
        assert!(Journal::parse(format!("{ok}extra line\n").as_bytes()).is_err());
        assert!(Journal::parse(format!("{ok}app org.example.other\n").as_bytes()).is_err());
        assert!(Journal::parse(format!("{JOURNAL_MAGIC}\npackage x.panpak\n").as_bytes()).is_err());
        assert!(Journal::parse(b"").is_err());
        assert!(Journal::parse(&[0xff, 0xfe]).is_err());
        assert!(Journal::parse(&vec![b'x'; MAX_JOURNAL_BYTES + 1]).is_err());
    }

    #[test]
    fn duplicates_are_kept_once() {
        let t = format!("{JOURNAL_MAGIC}\napp org.example.app\npackage a.panpak\npackage a.panpak\n");
        assert_eq!(Journal::parse(t.as_bytes()).unwrap().packages.len(), 1);
    }
}
