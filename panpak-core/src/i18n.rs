// SPDX-License-Identifier: MIT OR Apache-2.0
//! Translated messages. The message ids are the English sentences; a catalog `po/<language>.po` is compiled into the program by
//! `build.rs`, so a language is added with a file and no code. A sentence without a translation is shown in English.
//!
//! The language is the first usable entry of `LANGUAGE` (a list), `LC_ALL`, `LC_MESSAGES`, `LANG`. `C` and `POSIX` select English.

use std::borrow::Cow;

/// A compiled catalog: `entries` are `(msgid, translation, plural translation)`, sorted by msgid.
pub struct Catalog {
    pub name: &'static str,
    /// The plural rule is `n != 1` (the only one understood); otherwise the plural stays in English.
    pub simple_plural: bool,
    pub entries: &'static [(&'static str, &'static str, &'static str)],
}

include!(concat!(env!("OUT_DIR"), "/catalogs.rs"));

/// The catalogs found in `po/`, for tests and tools.
pub fn catalogs() -> &'static [Catalog] {
    CATALOGS
}

/// Picks a catalog for a list of locale names such as `["it_IT.UTF-8", "en"]`.
pub fn pick<'a>(cands: &[&str], cats: &'a [Catalog]) -> Option<&'a Catalog> {
    for c in cands {
        let c = c.split(['.', '@']).next().unwrap_or("");
        if c.is_empty() {
            continue;
        }
        if c == "C" || c == "POSIX" {
            return None;
        }
        if let Some(cat) = cats.iter().find(|k| k.name == c) {
            return Some(cat);
        }
        let lang = c.split('_').next().unwrap_or("");
        if let Some(cat) = cats.iter().find(|k| k.name == lang) {
            return Some(cat);
        }
        if lang == "en" {
            return None;
        }
    }
    None
}

/// The catalog of the environment, resolved once.
pub fn current() -> Option<&'static Catalog> {
    use std::sync::OnceLock;
    static CUR: OnceLock<Option<&'static Catalog>> = OnceLock::new();
    *CUR.get_or_init(|| {
        let mut cands: Vec<String> = Vec::new();
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let main = get("LC_ALL").or_else(|| get("LC_MESSAGES")).or_else(|| get("LANG"));
        if main.as_deref().is_some_and(|m| m != "C" && m != "POSIX") {
            if let Some(l) = get("LANGUAGE") {
                cands.extend(l.split(':').map(str::to_string));
            }
        }
        cands.extend(main);
        let refs: Vec<&str> = cands.iter().map(String::as_str).collect();
        pick(&refs, CATALOGS)
    })
}

fn find<'a>(cat: &'a Catalog, id: &str) -> Option<&'a (&'static str, &'static str, &'static str)> {
    cat.entries.binary_search_by(|e| e.0.cmp(id)).ok().map(|i| &cat.entries[i])
}

/// The translation of a message, or the message itself.
pub fn tr(id: &'static str) -> Cow<'static, str> {
    tr_in(current(), id)
}

pub fn tr_in(cat: Option<&Catalog>, id: &'static str) -> Cow<'static, str> {
    match cat.and_then(|c| find(c, id)) {
        Some(e) => Cow::Borrowed(e.1),
        None => Cow::Borrowed(id),
    }
}

/// A message with a plural form: `one` for `n == 1`, `many` otherwise. The message id is `one`.
pub fn trn(one: &'static str, many: &'static str, n: u64) -> Cow<'static, str> {
    trn_in(current(), one, many, n)
}

pub fn trn_in(cat: Option<&Catalog>, one: &'static str, many: &'static str, n: u64) -> Cow<'static, str> {
    if let Some(c) = cat.filter(|c| c.simple_plural) {
        if let Some(e) = find(c, one) {
            return Cow::Borrowed(if n == 1 || e.2.is_empty() { e.1 } else { e.2 });
        }
    }
    Cow::Borrowed(if n == 1 { one } else { many })
}

/// The names `{like_this}` of a message, in order.
pub fn placeholders(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(a) = rest.find('{') {
        let Some(b) = rest[a..].find('}') else { break };
        out.push(rest[a + 1..a + b].to_string());
        rest = &rest[a + b + 1..];
    }
    out.sort();
    out
}

/// Replaces `{name}` by the value; names without a value stay as they are.
pub fn fill(s: &str, vals: &[(&str, String)]) -> String {
    let mut out = s.to_string();
    for (k, v) in vals {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST: &[Catalog] = &[Catalog { name: "xx", simple_plural: true, entries: &[("a", "A", ""), ("b {n}", "B1", "B2")] }];

    #[test]
    fn picks_by_list_and_prefix() {
        assert_eq!(pick(&["xx_YY.UTF-8"], TEST).map(|c| c.name), Some("xx"));
        assert_eq!(pick(&["zz", "xx"], TEST).map(|c| c.name), Some("xx"));
        assert!(pick(&["C"], TEST).is_none() && pick(&["POSIX", "xx"], TEST).is_none());
        assert!(pick(&["en_US.UTF-8", "xx"], TEST).is_none());
        assert!(pick(&[""], TEST).is_none());
    }

    #[test]
    fn untranslated_is_english() {
        assert_eq!(tr_in(TEST.first(), "a"), "A");
        assert_eq!(tr_in(TEST.first(), "other"), "other");
        assert_eq!(trn_in(TEST.first(), "b {n}", "bs {n}", 1), "B1");
        assert_eq!(trn_in(TEST.first(), "b {n}", "bs {n}", 3), "B2");
        assert_eq!(trn_in(None, "b {n}", "bs {n}", 3), "bs {n}");
    }

    #[test]
    fn placeholders_and_fill() {
        assert_eq!(placeholders("x {b} y {a}"), ["a", "b"]);
        assert_eq!(fill("x {a} {a}", &[("a", "1".into())]), "x 1 1");
    }
}
