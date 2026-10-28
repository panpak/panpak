// SPDX-License-Identifier: MIT OR Apache-2.0
//! Manifest fields that become file names, paths or text shown to the user.
//!
//! A valid signature only proves who wrote the manifest, not that its fields are
//! safe: `verify` calls `validate_manifest` after the signature check, so every
//! command and the stub refuse a bad field even in a signed package.
//! Grammar of `app.id` and the other rules: PANPAK-FORMAT.md §3.1.
use crate::{coded, safe_path, ErrorCode, Manifest, MAX_PATH_LEN};
use anyhow::Result;

/// Longest accepted `app.id`, in bytes.
pub const MAX_APP_ID_LEN: usize = 128;
const MAX_LABEL_LEN: usize = 63;
const MAX_VERSION_LEN: usize = 64;
const MAX_NAME_LEN: usize = 256;
const MAX_URL_LEN: usize = 2048;
const MAX_SONAME_LEN: usize = 255;

/// Escaped, truncated copy of untrusted text, safe to put in a message.
fn shown(s: &str) -> String {
    let t: String = s.chars().take(48).collect();
    format!("{t:?}")
}

fn bad_field(field: &str, why: &str) -> anyhow::Error {
    coded(ErrorCode::E0007, format!("invalid manifest field {field}: {why}"))
}

/// `app.id`: reverse DNS, ASCII only.
///
/// ```text
/// app-id = label 1*( "." label )          ; 2 labels or more, 128 bytes at most
/// label  = alnum [ *( alnum / "-" ) alnum ]   ; 1 to 63 bytes
/// alnum  = %x30-39 / %x41-5A / %x61-7A
/// ```
/// So an id has no `/`, no empty or leading-dot segment, no `..`, no control
/// character and no NUL.
pub fn validate_app_id(id: &str) -> Result<()> {
    let bad = |why: &str| coded(ErrorCode::E0006, format!("invalid app.id {}: {why}", shown(id)));
    if id.is_empty() {
        return Err(bad("empty"));
    }
    if id.len() > MAX_APP_ID_LEN {
        return Err(bad(&format!("longer than {MAX_APP_ID_LEN} bytes")));
    }
    if !id.is_ascii() {
        return Err(bad("not ASCII"));
    }
    let labels: Vec<&str> = id.split('.').collect();
    if labels.len() < 2 {
        return Err(bad("reverse DNS needs at least two segments, e.g. org.example.app"));
    }
    for l in labels {
        if l.is_empty() {
            return Err(bad("empty segment (leading, trailing or doubled dot)"));
        }
        if l.len() > MAX_LABEL_LEN {
            return Err(bad(&format!("segment longer than {MAX_LABEL_LEN} bytes")));
        }
        if !l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(bad("segments may contain only A-Z, a-z, 0-9 and '-'"));
        }
        if l.starts_with('-') || l.ends_with('-') {
            return Err(bad("a segment must not start or end with '-'"));
        }
    }
    Ok(())
}

/// Characters that must never reach a terminal, a file name or a .desktop file:
/// controls (C0, DEL, C1), line and paragraph separators, bidirectional controls.
fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Free text for display: not empty, bounded, no unsafe character.
fn display_text(field: &str, s: &str, max: usize) -> Result<()> {
    if s.is_empty() {
        return Err(bad_field(field, "empty"));
    }
    if s.len() > max {
        return Err(bad_field(field, &format!("longer than {max} bytes")));
    }
    if s.chars().any(is_unsafe_char) {
        return Err(bad_field(field, "contains a control, line-separator or bidirectional character"));
    }
    Ok(())
}

/// `app.version`: starts with a letter or digit, then `[0-9A-Za-z._+~-]`, 64 bytes at most.
fn version(field: &str, s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= MAX_VERSION_LEN
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._+~-".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(bad_field(field, "must start with a letter or digit and use only A-Z a-z 0-9 . _ + ~ -, 64 bytes at most"))
    }
}

/// Library names in `[abi]` (SONAMEs and globs): they name links in the farm directory.
fn soname(field: &str, s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= MAX_SONAME_LEN
        && s != "."
        && s != ".."
        && s.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b'/');
    if ok {
        Ok(())
    } else {
        Err(bad_field(field, "library names are printable ASCII without '/', ' ', '.' or '..'"))
    }
}

/// Validate every manifest field that is used as a path, a file name, text in a
/// .desktop file or text printed to the user.
pub fn validate_manifest(man: &Manifest) -> Result<()> {
    validate_app_id(&man.app.id)?;
    display_text("app.name", &man.app.name, MAX_NAME_LEN)?;
    version("app.version", &man.app.version)?;
    if man.app.entry.len() > MAX_PATH_LEN || safe_path(&man.app.entry).is_err() {
        return Err(bad_field("app.entry", "must be a relative path without '.', '..' or empty segments"));
    }
    if let Some(a) = &man.abi {
        for (field, list) in [("abi.needs", &a.needs), ("abi.prefer_host", &a.prefer_host), ("abi.allow_missing", &a.allow_missing)] {
            for n in list.iter().flatten() {
                soname(field, n)?;
            }
        }
    }
    if let Some(p) = &man.permissions {
        for s in p.sockets_allow.iter().flatten() {
            if s.is_empty() || s.len() > MAX_PATH_LEN || s.chars().any(char::is_control) {
                return Err(bad_field("permissions.sockets_allow", "empty, too long or with control characters"));
            }
        }
    }
    if let Some(o) = &man.origin {
        display_text("origin.converted_from", &o.converted_from, MAX_NAME_LEN)?;
        if o.converted_from.contains('/') {
            return Err(bad_field("origin.converted_from", "must be a file name, not a path"));
        }
        display_text("origin.version", &o.version, MAX_VERSION_LEN)?;
        if o.sha256.len() != 64 || !o.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(bad_field("origin.sha256", "must be 64 lowercase hex digits"));
        }
    }
    if let Some(u) = man.update.as_ref().and_then(|u| u.url.as_deref()) {
        if u.len() > MAX_URL_LEN || u.bytes().any(|b| b <= b' ' || b == 0x7f) {
            return Err(bad_field("update.url", "too long, or with spaces or control characters"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_of;

    fn code(r: Result<()>) -> ErrorCode {
        code_of(&r.unwrap_err(), ErrorCode::E0000)
    }

    fn man(extra_app: &str) -> Manifest {
        toml::from_str(&format!(
            "[app]\nid=\"org.example.app\"\nname=\"App\"\nversion=\"1.0\"\nentry=\"bin/app\"\n{extra_app}[update]\nkey=\"ed25519:{}\"\n",
            "0".repeat(64)
        ))
        .unwrap()
    }

    #[test]
    fn app_id_accepts_reverse_dns() {
        for id in ["org.example.app", "a.b", "io.github.AppImageLauncher", "com.x-y.app9", "org.example.1app", &format!("a.{}", "b".repeat(63))] {
            assert!(validate_app_id(id).is_ok(), "{id}");
        }
    }

    #[test]
    fn app_id_rejects_everything_else() {
        let long_label = format!("a.{}", "b".repeat(64));
        let too_long = format!("a.{}", "b.".repeat(MAX_APP_ID_LEN));
        for id in [
            "", "app", ".org.example", "org.example.", "org..example", "../x.y", "a/b.c", "org.example/../x", "org.exa mple.app",
            "org.example.app\n", "org.example.app\nExec=/bin/sh", "org.exa\0mple.app", "org.example.\u{1b}app", "org.exämple.app",
            "-org.example.app", "org.-example.app", "org.example-.app", "org.example.a_b", "org.example.app\u{7f}", "/etc.x",
            &long_label, &too_long,
        ] {
            let e = validate_app_id(id).expect_err(id);
            assert_eq!(code_of(&e, ErrorCode::E0000), ErrorCode::E0006, "{id:?}");
        }
    }

    #[test]
    fn app_id_message_does_not_echo_control_characters() {
        let e = validate_app_id("a.b\n\u{1b}[31mred").unwrap_err();
        let m = format!("{e}");
        assert!(!m.chars().any(|c| c.is_control()), "{m:?}");
    }

    #[test]
    fn accepted_id_is_one_path_component() {
        for id in ["org.example.app", "a.b"] {
            validate_app_id(id).unwrap();
            let p = std::path::Path::new("/base").join(id);
            assert_eq!(p.parent(), Some(std::path::Path::new("/base")));
        }
    }

    #[test]
    fn manifest_accepts_the_normal_case() {
        assert!(validate_manifest(&man("")).is_ok());
    }

    #[test]
    fn manifest_rejects_bad_fields_with_e0007() {
        let mut m = man("");
        m.app.name = "Evil\nExec=/bin/sh".into();
        assert_eq!(code(validate_manifest(&m)), ErrorCode::E0007);
        for name in ["", "a\rb", "a\u{0}b", "a\u{9b}b", "a\u{2028}b", "a\u{202e}b"] {
            let mut m = man("");
            m.app.name = name.into();
            assert_eq!(code(validate_manifest(&m)), ErrorCode::E0007, "{name:?}");
        }
        for v in ["", "../1", "1/2", "1 0", "1\n", ".1", "-1", &"1".repeat(65)] {
            let mut m = man("");
            m.app.version = v.into();
            assert_eq!(code(validate_manifest(&m)), ErrorCode::E0007, "{v:?}");
        }
        for e in ["", "/bin/sh", "../x", "a/../b", "a//b", "a/./b", "a\nb"] {
            let mut m = man("");
            m.app.entry = e.into();
            assert_eq!(code(validate_manifest(&m)), ErrorCode::E0007, "{e:?}");
        }
        let mut m = man("");
        m.app.id = "../evil".into();
        assert_eq!(code(validate_manifest(&m)), ErrorCode::E0006);
    }

    #[test]
    fn manifest_rejects_bad_library_names_and_urls() {
        let abi = |n: &str| -> Manifest {
            let mut m = man("");
            m.abi = Some(crate::Abi { glibc_min: None, needs: Some(vec![n.into()]), prefer_host: None, allow_missing: None });
            m
        };
        assert!(validate_manifest(&abi("libc.so.6")).is_ok());
        assert!(validate_manifest(&abi("libQt*.so.*")).is_ok());
        for n in ["", "../libx.so", "/etc/passwd", "a/b", "..", ".", "a b", "a\nb", "lib\u{0}"] {
            assert_eq!(code(validate_manifest(&abi(n))), ErrorCode::E0007, "{n:?}");
        }
        let mut m = man("");
        m.update.as_mut().unwrap().url = Some("https://example.org/a b".into());
        assert_eq!(code(validate_manifest(&m)), ErrorCode::E0007);
        m.update.as_mut().unwrap().url = Some("https://example.org/a.panpak".into());
        assert!(validate_manifest(&m).is_ok());
    }

    #[test]
    fn manifest_checks_origin() {
        let origin = |from: &str, sha: &str| -> Manifest {
            let mut m = man("");
            m.origin = Some(crate::Origin { converted_from: from.into(), version: "1.0".into(), sha256: sha.into(), signed_by_upstream: false });
            m
        };
        let sha = "a".repeat(64);
        assert!(validate_manifest(&origin("Foo-1.0-x86_64.AppImage", &sha)).is_ok());
        for (f, s) in [("a/b", sha.as_str()), ("a\nb", sha.as_str()), ("Foo", "XYZ"), ("Foo", &"A".repeat(64))] {
            assert_eq!(code(validate_manifest(&origin(f, s))), ErrorCode::E0007, "{f:?} {s:?}");
        }
    }
}
