// SPDX-License-Identifier: MIT OR Apache-2.0
//! The software bill of materials of a package (PANPAK-FORMAT.md §7.3): a CycloneDX 1.6 JSON document that `pack` makes and puts in the tree as
//! `.panpak/sbom.cdx.json`. The manifest signs its BLAKE3 hash and says how much of the package it covers (`[sbom].coverage`). It is not critical: a reader
//! that does not use it loses nothing that matters for safety.
//!
//! The document is deterministic: no time, no random serial number, keys in a fixed order, components in path order. The same inputs make the same bytes.

use anyhow::{ensure, Context, Result};

/// Where the SBOM is in the tree.
pub const PATH: &str = ".panpak/sbom.cdx.json";
/// The largest SBOM a reader takes.
pub const MAX_SBOM: usize = 8 << 20;
/// The coverage of an SBOM made from the recipe of a build: the sources, the build image and the libraries of the image that went into the package.
pub const COVERAGE_BUILD: &str = "build";
/// The coverage of an SBOM made from what the files of the tree show: the libraries that it carries (with their hashes) and, for a conversion, the file it was
/// made from. Versions are given only where something signed names them.
pub const COVERAGE_BEST_EFFORT: &str = "best-effort";

/// A library of the tree: an ELF file with a SONAME.
pub struct Lib {
    pub path: String,
    pub soname: String,
    /// BLAKE3 of the file, in hex.
    pub blake3: String,
}

/// What `pack` knows when it makes the SBOM.
pub struct Input<'a> {
    pub manifest: &'a crate::Manifest,
    pub libs: Vec<Lib>,
}

/// The coverage that the manifest implies: a package made by `panpak build` has `[provenance]`.
pub fn coverage_of(man: &crate::Manifest) -> &'static str {
    if man.provenance.is_some() {
        COVERAGE_BUILD
    } else {
        COVERAGE_BEST_EFFORT
    }
}

/// A JSON string, escaped (RFC 8259): the quote, the backslash and every control character; any other text is written as it is.
pub fn json_string(s: &str) -> String {
    js(s)
}

/// A JSON string, escaped.
fn js(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A JSON object on one line, with its keys in the order given. The values are JSON already.
fn obj(fields: &[(&str, String)]) -> String {
    format!("{{{}}}", fields.iter().map(|(k, v)| format!("{}: {v}", js(k))).collect::<Vec<_>>().join(", "))
}

fn list(items: Vec<String>) -> String {
    format!("[{}]", items.join(", "))
}

/// One property of the `panpak:` family.
fn property(name: &str, value: &str) -> String {
    obj(&[("name", js(&format!("panpak:{name}"))), ("value", js(value))])
}

fn properties(items: &[(&str, &str)]) -> String {
    list(items.iter().map(|(n, v)| property(n, v)).collect())
}

fn hash_of(alg: &str, content: &str) -> String {
    list(vec![obj(&[("alg", js(alg)), ("content", js(content))])])
}

fn reference(kind: &str, url: &str) -> String {
    list(vec![obj(&[("type", js(kind)), ("url", js(url))])])
}

/// The document, as text, and its coverage. One component per line; the first lines are fixed, so that a reader can find the header without parsing.
pub fn make(input: &Input) -> (String, &'static str) {
    let man = input.manifest;
    let coverage = coverage_of(man);
    let mut components: Vec<String> = Vec::new();

    // What the package was made from.
    if let Some(p) = &man.provenance {
        if let Some(src) = &p.source {
            let mut f = vec![("bom-ref", js("source")), ("description", js(src))];
            let words: Vec<&str> = src.split_whitespace().collect();
            match words.as_slice() {
                ["git", url, "@", commit] => {
                    f.push(("externalReferences", reference("vcs", url)));
                    f.push(("version", js(commit)));
                }
                ["url", url, "sha256", hex] => {
                    f.push(("externalReferences", reference("distribution", url)));
                    f.push(("hashes", hash_of("SHA-256", hex)));
                }
                _ => {}
            }
            f.push(("name", js("source")));
            f.push(("properties", properties(&[("role", "source")])));
            f.push(("type", js("application")));
            components.push(obj(&f));
        }
        if let Some(image) = &p.image {
            components.push(obj(&[
                ("bom-ref", js("build-image")),
                ("description", js(image)),
                ("name", js("build-image")),
                ("properties", properties(&[("role", "build-image")])),
                ("type", js("container")),
            ]));
        }
        // A source archive that was built and linked into the app: its name, the address it came from and its SHA-256 (the recipe pins both).
        for (i, d) in p.deps.iter().flatten().enumerate() {
            let words: Vec<&str> = d.split_whitespace().collect();
            if let [name, "url", url, "sha256", hex] = words.as_slice() {
                components.push(obj(&[
                    ("bom-ref", js(&format!("dep-{i}"))),
                    ("externalReferences", reference("distribution", url)),
                    ("hashes", hash_of("SHA-256", hex)),
                    ("name", js(name)),
                    ("properties", properties(&[("role", "built-dependency")])),
                    ("type", js("library")),
                ]));
            }
        }
        for (i, d) in p.image_deps.iter().flatten().enumerate() {
            components.push(obj(&[
                ("bom-ref", js(&format!("image-dep-{i}"))),
                ("name", js(&d.package)),
                ("properties", properties(&[("recipe-name", &d.name), ("role", "build-image-dependency")])),
                ("type", js("library")),
                ("version", js(&d.version)),
            ]));
        }
    }
    if let Some(o) = &man.origin {
        components.push(obj(&[
            ("bom-ref", js("converted-from")),
            ("hashes", hash_of("SHA-256", &o.sha256)),
            ("name", js(&o.converted_from)),
            ("properties", properties(&[("role", "converted-from"), ("signed-by-upstream", if o.signed_by_upstream { "true" } else { "false" })])),
            ("type", js("file")),
            ("version", js(&o.version)),
        ]));
    }

    // The libraries that the tree carries.
    let mut libs: Vec<&Lib> = input.libs.iter().collect();
    libs.sort_by(|a, b| a.path.cmp(&b.path));
    for l in libs {
        components.push(obj(&[
            ("bom-ref", js(&format!("file:{}", l.path))),
            ("hashes", hash_of("BLAKE3", &l.blake3)),
            ("name", js(&l.soname)),
            ("properties", properties(&[("path", &l.path)])),
            ("type", js("library")),
        ]));
    }

    // No time, no serial number, and no name of the app: the tree is addressed by its content, so two builds of the same files, and the same files under two
    // names, are one tree. What the SBOM is of is the package that holds it; the manifest names the app.
    let mut text = String::new();
    text.push_str(PREFIX);
    text.push_str(&format!("  \"metadata\": {{\n    \"properties\": [\n      {},\n      {}\n    ]\n  }},\n", property("coverage", coverage), property("components", &components.len().to_string())));
    text.push_str("  \"components\": [\n");
    text.push_str(&components.iter().map(|c| format!("    {c}")).collect::<Vec<_>>().join(",\n"));
    text.push_str(if components.is_empty() { "  ]\n}\n" } else { "\n  ]\n}\n" });
    (text, coverage)
}

/// The hash that `[sbom].hash` holds: BLAKE3 of the text, in hex.
pub fn hash(text: &[u8]) -> String {
    crate::hex(blake3::hash(text).as_bytes())
}

/// The first lines of every document that `pack` makes.
const PREFIX: &str = "{\n  \"bomFormat\": \"CycloneDX\",\n  \"specVersion\": \"1.6\",\n  \"version\": 1,\n";

/// The value of a `panpak:` property in the header, read from the line that `make` writes.
fn header_property(text: &str, name: &str) -> Option<String> {
    let key = format!("{{\"name\": \"panpak:{name}\", \"value\": \"");
    let head = &text[..text.find("\"components\": [")?];
    let rest = &head[head.find(&key)? + key.len()..];
    Some(rest[..rest.find('"')?].to_string())
}

/// What a reader shows about an SBOM that it has checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub coverage: String,
    /// How many components the header counts; `None` for a document of another form.
    pub components: Option<usize>,
}

/// The text of an SBOM, checked: it is the one that the manifest signs. What is shown of it comes from the header that `pack` writes; a document that has another form
/// (a later `pack` may write a later CycloneDX) is still the signed one, and is shown without a count.
pub fn check(bytes: &[u8], manifest: &crate::Manifest) -> Result<Summary> {
    let s = manifest.sbom.as_ref().context("the manifest has no [sbom]")?;
    ensure!(hash(bytes) == s.hash, "the SBOM does not match the hash that the manifest signs");
    let text = std::str::from_utf8(bytes).context("the SBOM is not UTF-8")?;
    if text.starts_with(PREFIX) {
        let found = header_property(text, "coverage").unwrap_or_default();
        ensure!(found == s.coverage, "the coverage in the SBOM ({found:?}) is not the one in the manifest ({:?})", s.coverage);
    }
    Ok(Summary { coverage: s.coverage.clone(), components: header_property(text, "components").and_then(|n| n.parse().ok()) })
}

/// `secs` (seconds since 1970) as `YYYY-MM-DDThh:mm:ssZ`.
fn utc(secs: u64) -> String {
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    // Civil date from a day count (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// A serial number that is the same for the same package: from the root of the tree and the hash of the SBOM (a UUID of the form of version 8, which is for
/// values that the maker defines).
fn serial(root: &[u8; 32], sbom_hash: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(root);
    h.update(sbom_hash.as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&h.finalize().as_bytes()[..16]);
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;
    let x = crate::hex(&b);
    format!("urn:uuid:{}-{}-{}-{}-{}", &x[..8], &x[8..12], &x[12..16], &x[16..20], &x[20..])
}

/// The complete CycloneDX document for a scanner: the signed file with what it leaves out so that the tree stays the same for the same files, added now from the signed
/// manifest: the name, id and version of the app, a serial number that is the same for the same package, and the time (`now`, seconds since 1970). The file that is
/// signed is not changed; a document that is not of the form `pack` writes is refused (the signed file is `panpak sbom` without `--export`).
pub fn export(bytes: &[u8], manifest: &crate::Manifest, root: &[u8; 32], now: u64) -> Result<Vec<u8>> {
    check(bytes, manifest)?;
    let text = std::str::from_utf8(bytes).context("the SBOM is not UTF-8")?;
    let head = "  \"metadata\": {\n    \"properties\": [";
    ensure!(text.starts_with(PREFIX) && text[PREFIX.len()..].starts_with(head), "this SBOM has a form that this panpak cannot complete: show the signed file with `panpak sbom` and complete it with the tool that reads it");
    let app = obj(&[
        ("bom-ref", js("app")),
        ("name", js(&manifest.app.name)),
        ("properties", properties(&[("id", &manifest.app.id)])),
        ("type", js("application")),
        ("version", js(&manifest.app.version)),
    ]);
    let s = manifest.sbom.as_ref().context("the manifest has no [sbom]")?;
    let mut out = String::with_capacity(text.len() + 512);
    out.push_str(&text[..PREFIX.len()]);
    out.push_str(&format!("  \"serialNumber\": {},\n", js(&serial(root, &s.hash))));
    out.push_str(&format!("  \"metadata\": {{\n    \"component\": {app},\n    \"timestamp\": {},\n    \"properties\": [", js(&utc(now))));
    out.push_str(&text[PREFIX.len() + head.len()..]);
    Ok(out.into_bytes())
}

/// The SBOM of a tree on disk, as bytes, checked. `None`: the manifest has no `[sbom]` (a package made before it existed).
pub fn load_from_tree(tree: &std::path::Path, manifest: &crate::Manifest) -> Result<Option<Vec<u8>>> {
    if manifest.sbom.is_none() {
        return Ok(None);
    }
    let bytes = std::fs::read(tree.join(PATH)).with_context(|| format!("read {}", tree.join(PATH).display()))?;
    check(&bytes, manifest)?;
    Ok(Some(bytes))
}

/// The SBOM of a package file (its chunks), checked.
pub fn load_from_package(f: &mut std::fs::File, raw_index: &[u8], manifest: &crate::Manifest) -> Result<Option<Vec<u8>>> {
    if manifest.sbom.is_none() {
        return Ok(None);
    }
    let bytes = crate::read_package_file(f, raw_index, PATH, MAX_SBOM)?;
    check(&bytes, manifest)?;
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(extra: &str) -> crate::Manifest {
        toml::from_str(&format!("[app]\nid=\"org.example.A\"\nname=\"A\"\nversion=\"1\"\nentry=\"a\"\n{extra}")).unwrap()
    }

    fn lib(path: &str, soname: &str) -> Lib {
        Lib { path: path.into(), soname: soname.into(), blake3: "ab".repeat(32) }
    }

    #[test]
    fn the_same_files_under_another_name_make_the_same_bytes() {
        let (one, two) = (manifest(""), toml::from_str::<crate::Manifest>("[app]\nid=\"org.example.B\"\nname=\"B\"\nversion=\"9\"\nentry=\"b\"\n").unwrap());
        let libs = || vec![lib("lib/a.so.2", "a.so.2")];
        assert_eq!(make(&Input { manifest: &one, libs: libs() }), make(&Input { manifest: &two, libs: libs() }));
    }

    #[test]
    fn the_same_inputs_make_the_same_bytes_whatever_the_order() {
        let man = manifest("");
        let one = make(&Input { manifest: &man, libs: vec![lib("lib/b.so.1", "b.so.1"), lib("lib/a.so.2", "a.so.2")] });
        let two = make(&Input { manifest: &man, libs: vec![lib("lib/a.so.2", "a.so.2"), lib("lib/b.so.1", "b.so.1")] });
        assert_eq!(one, two);
        assert!(one.0.ends_with("}\n"));
    }

    #[test]
    fn a_document_has_the_cyclonedx_header_and_one_component_per_library() {
        let man = manifest("");
        let (text, coverage) = make(&Input { manifest: &man, libs: vec![lib("lib/a.so.2", "a.so.2")] });
        assert_eq!(coverage, COVERAGE_BEST_EFFORT);
        assert!(text.starts_with(PREFIX));
        assert_eq!(header_property(&text, "components").as_deref(), Some("1"));
        assert_eq!(text.matches("\"type\": \"library\"").count(), 1);
        assert!(text.contains("\"alg\": \"BLAKE3\""));
        // No time and no serial number: they would make two builds of one tree differ.
        assert!(!text.contains("serialNumber") && !text.contains("timestamp"));
    }

    #[test]
    fn an_empty_package_still_has_a_closed_document() {
        let man = manifest("");
        let (text, _) = make(&Input { manifest: &man, libs: Vec::new() });
        assert!(text.ends_with("\"components\": [\n  ]\n}\n"), "{text}");
        assert_eq!(header_property(&text, "components").as_deref(), Some("0"));
    }

    #[test]
    fn text_from_the_manifest_is_escaped() {
        let man = manifest("");
        let (text, _) = make(&Input { manifest: &man, libs: vec![lib("lib/a\"b\\c.so.1", "a\"b\\c.so.1")] });
        assert!(text.contains("a\\\"b\\\\c.so.1"), "{text}");
    }

    /// A reader of one JSON string, written apart from the writer: only what RFC 8259 allows.
    fn decode(s: &str) -> Option<String> {
        let b = s.strip_prefix('"')?.strip_suffix('"')?;
        let (mut out, mut it) = (String::new(), b.chars());
        while let Some(c) = it.next() {
            match c {
                '"' => return None,
                c if (c as u32) < 0x20 => return None,
                '\\' => match it.next()? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => out.push(char::from_u32(u32::from_str_radix(&(0..4).map(|_| it.next()).collect::<Option<String>>()?, 16).ok()?)?),
                    _ => return None,
                },
                c => out.push(c),
            }
        }
        Some(out)
    }

    #[test]
    fn hostile_names_are_written_so_that_a_json_reader_gets_them_back() {
        let names = [
            "plain.so.1", "a\"b", "a\\b", "a\\\"b\\", "tab\there", "line\nbreak", "cr\rhere", "\u{1}\u{1f}\u{7f}", "nul-free \u{8}\u{c}", "ünï\u{e9}", "日本語.so.1",
            "emoji \u{1F600} and \u{10FFFF}", "\u{2028}\u{2029}", "</script>", "\"},{\"injected\":true", "{}[]:,",
        ];
        for n in names {
            assert_eq!(decode(&js(n)).as_deref(), Some(n), "{n:?} written as {}", js(n));
        }
        // A whole document made of them has no raw control character except the ends of its lines.
        let man = manifest("");
        let libs = names.iter().map(|n| lib(&format!("lib/{n}"), n)).collect();
        let (doc, _) = make(&Input { manifest: &man, libs });
        assert!(doc.chars().all(|c| c == '\n' || (c as u32) >= 0x20));
        assert_eq!(doc.matches("\"type\": \"library\"").count(), names.len());
    }

    #[test]
    fn a_build_names_its_source_its_image_and_the_libraries_it_took_from_the_image() {
        let man = manifest(&format!(
            "[provenance]\nsource=\"git https://e.org/a.git @ {}\"\nimage=\"img sha256:00 2026-01-01\"\n[[provenance.image_deps]]\nname=\"zlib\"\npackage=\"zlib1g\"\nversion=\"1.3\"\n",
            "ab".repeat(20)
        ));
        let (text, coverage) = make(&Input { manifest: &man, libs: Vec::new() });
        assert_eq!(coverage, COVERAGE_BUILD);
        assert_eq!(header_property(&text, "components").as_deref(), Some("3"));
        assert!(text.contains("\"type\": \"vcs\""));
        assert!(text.contains("\"name\": \"zlib1g\"") && text.contains("\"version\": \"1.3\""));
    }

    #[test]
    fn a_dependency_built_from_an_archive_is_a_component_with_its_hash() {
        let man = manifest(&format!(
            "[provenance]\nsource=\"git https://e.org/a.git @ {}\"\ndeps=[\"libfoo url https://e.org/libfoo-2.1.2.tar.gz sha256 {}\", \"not a dependency line\"]\n",
            "ab".repeat(20),
            "cd".repeat(32)
        ));
        let (text, coverage) = make(&Input { manifest: &man, libs: Vec::new() });
        assert_eq!(coverage, COVERAGE_BUILD);
        assert_eq!(header_property(&text, "components").as_deref(), Some("2"), "the line that is not a dependency is left out");
        assert!(text.contains("\"name\": \"libfoo\"") && text.contains("libfoo-2.1.2.tar.gz") && text.contains(&"cd".repeat(32)) && text.contains("built-dependency"), "{text}");
    }

    #[test]
    fn a_source_archive_is_named_with_its_hash() {
        let man = manifest(&format!("[provenance]\nsource=\"url https://e.org/a.tar.gz sha256 {}\"\n", "cd".repeat(32)));
        let (text, _) = make(&Input { manifest: &man, libs: Vec::new() });
        assert!(text.contains("\"alg\": \"SHA-256\"") && text.contains("\"type\": \"distribution\""));
    }

    #[test]
    fn a_conversion_names_the_file_it_was_made_from() {
        let man = manifest(&format!("[origin]\nconverted_from=\"A.AppImage\"\nversion=\"1\"\nsha256=\"{}\"\nsigned_by_upstream=false\n", "ef".repeat(32)));
        let (text, coverage) = make(&Input { manifest: &man, libs: Vec::new() });
        assert_eq!(coverage, COVERAGE_BEST_EFFORT);
        assert!(text.contains("\"name\": \"A.AppImage\""));
    }

    #[test]
    fn the_export_adds_the_identity_a_serial_number_and_the_time_and_the_file_stays() {
        let mut man = manifest("");
        let (text, coverage) = make(&Input { manifest: &man, libs: vec![lib("lib/a.so.2", "a.so.2")] });
        man.sbom = Some(crate::Sbom { hash: hash(text.as_bytes()), coverage: coverage.to_string() });
        let root = [7u8; 32];
        let out = String::from_utf8(export(text.as_bytes(), &man, &root, 1_700_000_000).unwrap()).unwrap();
        assert!(out.contains("\"timestamp\": \"2023-11-14T22:13:20Z\""), "{out}");
        assert!(out.contains("\"name\": \"A\"") && out.contains("\"version\": \"1\"") && out.contains("\"value\": \"org.example.A\""));
        assert!(out.contains("\"serialNumber\": \"urn:uuid:") && out.starts_with("{\n  \"bomFormat\""));
        // The same package gives the same serial number; another one gives another.
        assert_eq!(out, String::from_utf8(export(text.as_bytes(), &man, &root, 1_700_000_000).unwrap()).unwrap());
        assert_ne!(serial(&root, "aa"), serial(&[8u8; 32], "aa"));
        // The signed file has none of it.
        assert!(!text.contains("serialNumber") && !text.contains("timestamp") && !text.contains("\"org.example.A\""));
        // Other bytes than the signed ones, or a form that is not ours, are not completed.
        assert!(export(format!("{text} ").as_bytes(), &man, &root, 0).is_err());
        let other = "{\"bomFormat\":\"CycloneDX\"}";
        man.sbom = Some(crate::Sbom { hash: hash(other.as_bytes()), coverage: coverage.to_string() });
        assert!(export(other.as_bytes(), &man, &root, 0).is_err());
    }

    #[test]
    fn dates_are_written_in_utc() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn check_refuses_other_bytes_and_a_wrong_coverage() {
        let mut man = manifest("");
        let (text, coverage) = make(&Input { manifest: &man, libs: vec![lib("lib/a.so.2", "a.so.2")] });
        man.sbom = Some(crate::Sbom { hash: hash(text.as_bytes()), coverage: coverage.to_string() });
        assert_eq!(check(text.as_bytes(), &man).unwrap(), Summary { coverage: coverage.into(), components: Some(1) });
        assert!(check(format!("{text} ").as_bytes(), &man).is_err());
        man.sbom.as_mut().unwrap().coverage = COVERAGE_BUILD.into();
        assert!(check(text.as_bytes(), &man).is_err());
    }

    #[test]
    fn a_signed_document_of_another_form_is_shown_without_a_count() {
        let mut man = manifest("");
        let other = "{\"bomFormat\":\"CycloneDX\",\"specVersion\":\"1.7\"}";
        man.sbom = Some(crate::Sbom { hash: hash(other.as_bytes()), coverage: COVERAGE_BEST_EFFORT.into() });
        assert_eq!(check(other.as_bytes(), &man).unwrap().components, None);
    }
}
