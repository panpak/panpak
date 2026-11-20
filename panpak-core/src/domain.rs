// SPDX-License-Identifier: MIT OR Apache-2.0
//! The confirmation of a domain (PANPAK-FORMAT.md §3.2.1). An author who names a domain in `[trust]` publishes, at `https://<domain>/.well-known/panpak.toml`, the
//! keys that may sign the apps of that domain. A client that asks for the file before it trusts a key for the first time, and at every succession, learns one of three
//! things: the domain **confirms** the key, it **does not confirm** it (nothing was learned: the file is not there, the network is off, the app is not listed), or it
//! **contradicts** it (the file is there, it lists the app, and the key is not one of its keys, or is one that it says was replaced). This module is the rules; the
//! network is in the client.
//!
//! ```toml
//! [[app]]
//! id = "org.example.editor"        # an app id, or "org.example.*" for every app under that prefix
//! root = ["ed25519:<64 hex>"]      # the keys that may sign it now (1 to 8)
//! replaces = ["ed25519:<64 hex>"]  # the keys that no longer may (0 to 16)
//! ```

use anyhow::{bail, ensure, Context, Result};

/// The largest file that a client takes.
pub const MAX_FILE: usize = 64 << 10;
/// How many `[[app]]` entries a file may have.
pub const MAX_ENTRIES: usize = 256;
const MAX_ROOT: usize = 8;
const MAX_REPLACES: usize = 16;

/// The address that a domain publishes at.
pub fn url_of(domain: &str) -> String {
    format!("https://{domain}/.well-known/panpak.toml")
}

/// One `[[app]]` of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// An app id, or a prefix that ends in `.*`.
    pub id: String,
    pub root: Vec<String>,
    pub replaces: Vec<String>,
}

impl Entry {
    /// Whether this entry speaks for `app_id`: the id is the same, or the entry is `prefix.*` and the id is `prefix.` followed by at least one more label.
    pub fn covers(&self, app_id: &str) -> bool {
        match self.id.strip_suffix(".*") {
            Some(prefix) => app_id.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('.') && rest.len() > 1),
            None => self.id == app_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub apps: Vec<Entry>,
}

/// What a file says about a key of an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// An entry that covers the app lists the key as one of its roots.
    Confirmed,
    /// No entry covers the app.
    NotListed,
    /// An entry covers the app, and the key is not a root of any entry that covers it.
    OtherKeys,
    /// An entry that covers the app says that this key was replaced.
    Replaced,
}

/// A key as the file and the manifest write it, in one form (`ed25519:` and 64 lowercase hex characters), or an error.
pub fn canonical_key(k: &str) -> Result<String> {
    let k = k.trim();
    let hex = k.strip_prefix("ed25519:").context("a key starts with ed25519:")?;
    ensure!(hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')), "a key is ed25519: and 64 lowercase hex characters");
    Ok(k.to_string())
}

/// Read a file of a domain. `domain` is the one that it was asked of: an entry whose id is not under that domain makes the whole file invalid (a domain speaks for its
/// own apps only). Strict about what it needs, tolerant of what it does not know (an unknown key or table is ignored).
pub fn parse(text: &str, domain: &str) -> Result<File> {
    ensure!(text.len() <= MAX_FILE, "the file is larger than {MAX_FILE} bytes");
    let v: toml::Table = text.parse().context("the file is not TOML")?;
    let list = match v.get("app") {
        None => Vec::new(),
        Some(toml::Value::Array(a)) => a.clone(),
        Some(_) => bail!("`app` must be a list of [[app]] tables"),
    };
    ensure!(list.len() <= MAX_ENTRIES, "more than {MAX_ENTRIES} entries");
    let mut apps = Vec::with_capacity(list.len());
    for (i, e) in list.iter().enumerate() {
        let t = e.as_table().with_context(|| format!("app {i}: not a table"))?;
        let id = t.get("id").and_then(|v| v.as_str()).with_context(|| format!("app {i}: id is a text"))?.to_string();
        let prefix = id.strip_suffix(".*").unwrap_or(&id);
        // The id is an app id (or a prefix of one) that the domain speaks for.
        ensure!(prefix.len() <= 128 && !prefix.is_empty() && prefix.split('.').all(|l| !l.is_empty()), "app {i}: {id:?} is not an app id");
        ensure!(crate::trust::domain_fits_id(domain, prefix), "app {i}: {id:?} is not under the domain {domain}");
        let keys = |name: &str, max: usize, min: usize| -> Result<Vec<String>> {
            let list: Vec<String> = match t.get(name) {
                None => Vec::new(),
                Some(toml::Value::Array(a)) => a.iter().map(|k| k.as_str().context("a key is a text").and_then(canonical_key)).collect::<Result<_>>().with_context(|| format!("app {i}: {name}"))?,
                Some(_) => bail!("app {i}: {name} is a list of keys"),
            };
            ensure!(list.len() >= min && list.len() <= max, "app {i}: {name} has {} keys, from {min} to {max} are allowed", list.len());
            let mut seen = list.clone();
            seen.sort();
            seen.dedup();
            ensure!(seen.len() == list.len(), "app {i}: {name} repeats a key");
            Ok(list)
        };
        let (root, replaces) = (keys("root", MAX_ROOT, 1)?, keys("replaces", MAX_REPLACES, 0)?);
        ensure!(!root.iter().any(|r| replaces.contains(r)), "app {i}: a key is both a root and replaced");
        apps.push(Entry { id, root, replaces });
    }
    Ok(File { apps })
}

impl File {
    /// What this file says of `key` for `app_id`.
    pub fn judge(&self, app_id: &str, key: &str) -> Verdict {
        let key = key.trim();
        let mut covered = false;
        let (mut listed, mut replaced) = (false, false);
        for e in self.apps.iter().filter(|e| e.covers(app_id)) {
            covered = true;
            listed |= e.root.iter().any(|r| r == key);
            replaced |= e.replaces.iter().any(|r| r == key);
        }
        match (covered, listed, replaced) {
            (false, _, _) => Verdict::NotListed,
            (_, _, true) => Verdict::Replaced,
            (_, true, false) => Verdict::Confirmed,
            (_, false, false) => Verdict::OtherKeys,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(n: u8) -> String {
        format!("ed25519:{}", format!("{n:02x}").repeat(32))
    }

    fn file(body: &str) -> Result<File> {
        parse(body, "example.org")
    }

    #[test]
    fn an_entry_covers_its_id_or_what_is_under_its_prefix() {
        let e = |id: &str| Entry { id: id.into(), root: vec![k(1)], replaces: vec![] };
        assert!(e("org.example.editor").covers("org.example.editor") && !e("org.example.editor").covers("org.example.editor2"));
        assert!(e("org.example.*").covers("org.example.editor") && e("org.example.*").covers("org.example.a.b"));
        assert!(!e("org.example.*").covers("org.example") && !e("org.example.*").covers("org.example.") && !e("org.example.*").covers("org.exampler.x"));
    }

    #[test]
    fn a_file_is_read_and_a_key_is_judged() {
        let f = file(&format!(
            "[[app]]\nid=\"org.example.editor\"\nroot=[\"{}\"]\nreplaces=[\"{}\"]\n[[app]]\nid=\"org.example.*\"\nroot=[\"{}\"]\n",
            k(1), k(2), k(3)
        ))
        .unwrap();
        assert_eq!(f.judge("org.example.editor", &k(1)), Verdict::Confirmed);
        assert_eq!(f.judge("org.example.editor", &k(2)), Verdict::Replaced);
        assert_eq!(f.judge("org.example.editor", &k(3)), Verdict::Confirmed, "the prefix entry covers it too");
        assert_eq!(f.judge("org.example.editor", &k(9)), Verdict::OtherKeys);
        assert_eq!(f.judge("org.example.viewer", &k(3)), Verdict::Confirmed);
        assert_eq!(f.judge("org.example.viewer", &k(1)), Verdict::OtherKeys);
        assert_eq!(f.judge("org.other.x", &k(1)), Verdict::NotListed);
        assert_eq!(file("").unwrap().judge("org.example.editor", &k(1)), Verdict::NotListed);
    }

    #[test]
    fn a_file_that_speaks_for_another_domain_or_is_malformed_is_refused_whole() {
        let one = |id: &str, extra: &str| format!("[[app]]\nid=\"{id}\"\nroot=[\"{}\"]\n{extra}", k(1));
        assert!(file(&one("org.example.editor", "")).is_ok());
        for (bad, why) in [
            (one("org.other.editor", ""), "not under the domain"),
            (one("org.*", ""), "not under the domain"),
            (one("org..x", ""), "not an app id"),
            ("[[app]]\nid=\"org.example.a\"\nroot=[]\n".to_string(), "from 1 to 8"),
            ("[[app]]\nid=\"org.example.a\"\n".to_string(), "root"),
            (format!("[[app]]\nid=\"org.example.a\"\nroot=[\"{0}\",\"{0}\"]\n", k(1)), "repeats"),
            (format!("[[app]]\nid=\"org.example.a\"\nroot=[\"{0}\"]\nreplaces=[\"{0}\"]\n", k(1)), "both a root and replaced"),
            ("[[app]]\nid=\"org.example.a\"\nroot=[\"ed25519:ABC\"]\n".to_string(), "root"),
            ("app = 3\n".to_string(), "list of [[app]]"),
            ("[[app".to_string(), "not TOML"),
        ] {
            let e = format!("{:#}", file(&bad).unwrap_err());
            assert!(e.contains(why), "{bad:?}: {e}");
        }
        assert!(parse(&"#".repeat(MAX_FILE + 1), "example.org").is_err());
    }

    #[test]
    fn what_the_reader_does_not_know_is_ignored() {
        let f = file(&format!("format = 7\n[future]\nx = 1\n[[app]]\nid=\"org.example.a\"\nroot=[\"{}\"]\nnote=\"hello\"\n", k(1))).unwrap();
        assert_eq!(f.apps.len(), 1);
    }

    #[test]
    fn the_address_is_the_well_known_file_of_the_domain() {
        assert_eq!(url_of("example.org"), "https://example.org/.well-known/panpak.toml");
    }
}
