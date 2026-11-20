#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;

/// A reader of one JSON string written independently of the writer: it takes only what RFC 8259 allows (no raw control character, no raw quote or backslash, the
/// escapes of the RFC), and gives back the text, or `None` when the string is not valid JSON.
fn decode(s: &str) -> Option<String> {
    let b = s.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut it = b.chars();
    while let Some(c) = it.next() {
        match c {
            '"' => return None,
            c if (c as u32) < 0x20 => return None,
            '\\' => match it.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    let hex: String = (0..4).map(|_| it.next()).collect::<Option<_>>()?;
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
    Some(out)
}

fuzz_target!(|data: &[u8]| {
    // Any text that could come from a SONAME, a file name or the name of a converted file: the writer's string reads back identical.
    let text = String::from_utf8_lossy(data);
    let written = panpak_core::sbom::json_string(&text);
    assert_eq!(decode(&written).as_deref(), Some(&*text), "{written:?}");
    // A document made of it has no control character outside its line ends, and the header that a reader looks for is where it is said to be.
    let lib = panpak_core::sbom::Lib { path: format!("lib/{text}"), soname: text.to_string(), blake3: "0".repeat(64) };
    let man: panpak_core::Manifest = toml::from_str("[app]\nid=\"org.example.A\"\nname=\"A\"\nversion=\"1\"\nentry=\"a\"\n").unwrap();
    let (doc, _) = panpak_core::sbom::make(&panpak_core::sbom::Input { manifest: &man, libs: vec![lib] });
    assert!(doc.starts_with("{\n  \"bomFormat\": \"CycloneDX\""));
    assert!(doc.chars().all(|c| c == '\n' || (c as u32) >= 0x20), "a raw control character in the document");
});
