// SPDX-License-Identifier: MIT OR Apache-2.0
//! Reads the translation catalogs `po/<language>.po` and writes them as Rust tables into `$OUT_DIR/catalogs.rs`.
//! Adding a language is adding a file there: no code changes. The template `po/panpak.pot` is not a catalog.

use std::{env, fmt::Write as _, fs, path::PathBuf};

/// One entry: msgid, singular translation, plural translation (empty when the message has no plural).
type Entry = (String, String, String);

fn unquote(s: &str) -> String {
    let s = s.trim();
    let inner = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s);
    let mut out = String::new();
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn parse(text: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    let (mut id, mut plural, mut strs): (Option<String>, Option<String>, Vec<String>) = (None, None, Vec::new());
    let mut fuzzy = false;
    let mut field = 0u8; // 1 msgid, 2 msgid_plural, 3 msgstr[n]
    let mut flush = |id: &mut Option<String>, plural: &mut Option<String>, strs: &mut Vec<String>, fuzzy: &mut bool| {
        if let Some(i) = id.take() {
            // The header (empty msgid) is kept: it carries Plural-Forms.
            let one = strs.first().cloned().unwrap_or_default();
            let two = strs.get(1).cloned().unwrap_or_default();
            if !*fuzzy && !one.is_empty() {
                entries.push((i, one, if plural.is_some() { two } else { String::new() }));
            }
        }
        *plural = None;
        strs.clear();
        *fuzzy = false;
    };
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            flush(&mut id, &mut plural, &mut strs, &mut fuzzy);
            field = 0;
        } else if l.starts_with("#,") {
            if l.contains("fuzzy") {
                fuzzy = true;
            }
        } else if l.starts_with('#') {
        } else if let Some(r) = l.strip_prefix("msgid_plural") {
            plural = Some(unquote(r));
            field = 2;
        } else if let Some(r) = l.strip_prefix("msgid") {
            flush(&mut id, &mut plural, &mut strs, &mut fuzzy);
            id = Some(unquote(r));
            field = 1;
        } else if l.starts_with("msgstr[") {
            let close = l.find(']').unwrap_or(0);
            strs.push(unquote(&l[close + 1..]));
            field = 3;
        } else if let Some(r) = l.strip_prefix("msgstr") {
            strs.push(unquote(r));
            field = 3;
        } else if l.starts_with('"') {
            let add = unquote(l);
            match field {
                1 => id.as_mut().unwrap().push_str(&add),
                2 => plural.as_mut().unwrap().push_str(&add),
                3 => strs.last_mut().unwrap().push_str(&add),
                _ => {}
            }
        }
    }
    flush(&mut id, &mut plural, &mut strs, &mut fuzzy);
    entries
}

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("..").join("po");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut names: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(rd) = fs::read_dir(&root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "po") {
                let name = p.file_stem().unwrap().to_string_lossy().to_string();
                println!("cargo:rerun-if-changed={}", p.display());
                names.push((name, p));
            }
        }
    }
    names.sort();
    let mut out = String::from("pub static CATALOGS: &[Catalog] = &[\n");
    for (name, path) in &names {
        let mut entries = parse(&fs::read_to_string(path).unwrap());
        // Only a plural rule of two forms where the first is for n == 1 is understood.
        let header = entries.iter().find(|e| e.0.is_empty()).map(|e| e.1.clone()).unwrap_or_default();
        let simple_plural = {
            let h = header.replace(' ', "");
            h.contains("nplurals=2;plural=(n!=1)") || h.contains("nplurals=2;plural=n!=1")
        };
        entries.retain(|e| !e.0.is_empty());
        entries.sort();
        let _ = writeln!(out, "    Catalog {{ name: {name:?}, simple_plural: {simple_plural}, entries: &[");
        for (id, one, two) in &entries {
            let _ = writeln!(out, "        ({id:?}, {one:?}, {two:?}),");
        }
        out.push_str("    ] },\n");
    }
    out.push_str("];\n");
    fs::write(PathBuf::from(env::var("OUT_DIR").unwrap()).join("catalogs.rs"), out).unwrap();
}
