// SPDX-License-Identifier: MIT OR Apache-2.0
//! Manifest fields that become file names, paths or text shown to the user.
//!
//! A valid signature only proves who wrote the manifest, not that its fields are
//! safe: `verify` calls `validate_manifest` after the signature check, so every
//! command and the stub refuse a bad field even in a signed package.
//! Grammar of `app.id` and the other rules: PANPAK-FORMAT.md §3.1.
use crate::{coded, safe_path, ErrorCode, HomePath, Launch, Manifest, MAX_PATH_LEN};
use anyhow::Result;

/// Longest accepted `app.id`, in bytes.
pub const MAX_APP_ID_LEN: usize = 128;
const MAX_LABEL_LEN: usize = 63;
const MAX_VERSION_LEN: usize = 64;
const MAX_NAME_LEN: usize = 256;
const MAX_URL_LEN: usize = 2048;
const MAX_SONAME_LEN: usize = 255;
const MAX_LAUNCH_ITEMS: usize = 64;
const MAX_LAUNCH_LEN: usize = 4096;
const MAX_ENV_NAME_LEN: usize = 128;
const MAX_HOME_PATHS: usize = 32;

/// Placeholders allowed in `launch.args` and `launch.env` values. The stub replaces
/// each one with the private folder of the app. No other `${...}` is valid.
pub const PLACEHOLDERS: &[&str] = &["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMPDIR"];

/// Variables a manifest MUST NOT set in `launch.env`: they change how code is
/// loaded or what the sandbox protects, or the stub sets them itself.
pub const FORBIDDEN_ENV: &[&str] = &[
    "PATH", "HOME", "XDG_RUNTIME_DIR", "PYTHONPATH", "PYTHONHOME", "PERL5LIB", "NODE_OPTIONS", "ELECTRON_RUN_AS_NODE",
    "GIO_MODULE_DIR", "GTK_MODULES", "GTK_PATH", "QT_PLUGIN_PATH",
    // Set by the stub from the private folders.
    "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_CONFIG_DIRS", "XDG_DATA_DIRS", "TMPDIR",
    "SQLITE_TMPDIR",
];
/// Forbidden variable name prefixes (`LD_*` and `PANPAK_*`).
pub const FORBIDDEN_ENV_PREFIXES: &[&str] = &["LD_", "PANPAK_"];

/// Too broad: refused as they are, together with the home itself, "." and the empty
/// path. A path under them is fine (for example `.config/example-app`).
pub const BROAD_HOME_PATHS: &[&str] = &[".config", ".local", ".local/share", ".cache"];

/// Home paths a manifest MUST NOT ask for. A path is refused when it is one of
/// these, lies under one, or contains one. The list is incomplete by nature: it
/// names the places that run code at login or hold credentials and configuration
/// of other programs.
pub const FORBIDDEN_HOME_PATHS: &[&str] = &[
    // Run code at login, shell or session start.
    ".config/autostart", ".config/systemd", ".config/environment.d", ".local/bin", ".local/share/applications",
    ".xinitrc", ".xsession", ".xsessionrc", ".xprofile", ".Xresources", ".bashrc", ".bash_profile", ".bash_login",
    ".profile", ".zshrc", ".zprofile", ".zshenv", ".pam_environment",
    // File manager and desktop integration.
    ".local/share/nautilus", ".local/share/nemo", ".local/share/kio", ".config/mimeapps.list",
    // Panpak itself.
    ".config/panpak", ".local/share/panpak", ".cache/panpak",
    // Editors, shells, window managers, terminals and git.
    ".config/git", ".gitconfig", ".config/fish", ".config/nvim", ".vimrc", ".vim", ".emacs", ".emacs.d", ".config/i3",
    ".config/sway", ".config/hypr", ".config/openbox", ".config/kitty", ".config/alacritty",
    // Credentials and keys.
    ".ssh", ".gnupg", ".pki", ".password-store", ".local/share/keyrings", ".mozilla", ".config/google-chrome",
    ".config/chromium", ".netrc", ".git-credentials", ".config/gh", ".docker", ".kube", ".aws", ".cargo", ".npmrc",
];

/// Escaped, truncated copy of untrusted text, safe to put in a message.
fn shown(s: &str) -> String {
    let t: String = s.chars().take(48).collect();
    format!("{t:?}")
}

fn bad_field(field: &str, why: &str) -> anyhow::Error {
    coded(ErrorCode::E0007, format!("invalid manifest field {field}: {why}"))
}

/// `app.id`: a D-Bus and Flatpak style name, ASCII only. The XDG portals identify
/// an app by this id.
///
/// ```text
/// app-id  = segment 1*( "." segment )     ; 2 segments or more, 128 bytes at most
/// segment = ( ALPHA / "_" ) *62( ALPHA / DIGIT / "_" / "-" )   ; 1 to 63 bytes
/// ```
/// So an id has no segment starting with a digit or a hyphen (the launcher is named
/// after the id, and a name starting with `-` would be read as an option), no `/`, no empty or leading or
/// trailing dot segment, no `..`, no control character and no NUL.
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
    let segments: Vec<&str> = id.split('.').collect();
    if segments.len() < 2 {
        return Err(bad("needs at least two segments, e.g. org.example.app"));
    }
    for seg in segments {
        if seg.is_empty() {
            return Err(bad("empty segment (leading, trailing or doubled dot)"));
        }
        if seg.len() > MAX_LABEL_LEN {
            return Err(bad(&format!("segment longer than {MAX_LABEL_LEN} bytes")));
        }
        if !seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return Err(bad("segments may contain only A-Z, a-z, 0-9, '_' and '-'"));
        }
        if seg.as_bytes()[0].is_ascii_digit() || seg.as_bytes()[0] == b'-' {
            return Err(bad("a segment must not start with a digit or '-'"));
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

/// Text with no control character, bounded; `${...}` only for `PLACEHOLDERS`.
fn launch_text(field: &str, s: &str) -> Result<()> {
    if s.len() > MAX_LAUNCH_LEN {
        return Err(bad_field(field, &format!("longer than {MAX_LAUNCH_LEN} bytes")));
    }
    if s.chars().any(char::is_control) {
        return Err(bad_field(field, "contains a control character"));
    }
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        let after = &rest[i + 2..];
        let end = after.find('}').ok_or_else(|| bad_field(field, "unterminated '${'"))?;
        if !PLACEHOLDERS.contains(&&after[..end]) {
            return Err(bad_field(field, &format!("unknown placeholder {}", shown(&rest[i..i + 3 + end]))));
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

fn env_name_ok(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_ENV_NAME_LEN
        && (b[0].is_ascii_uppercase() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

fn launch(l: &Launch) -> Result<()> {
    let args = l.args.as_deref().unwrap_or_default();
    if args.len() > MAX_LAUNCH_ITEMS {
        return Err(bad_field("launch.args", &format!("more than {MAX_LAUNCH_ITEMS} items")));
    }
    for a in args {
        launch_text("launch.args", a)?;
    }
    if let Some(env) = &l.env {
        if env.len() > MAX_LAUNCH_ITEMS {
            return Err(bad_field("launch.env", &format!("more than {MAX_LAUNCH_ITEMS} variables")));
        }
        for (k, v) in env {
            if !env_name_ok(k) {
                return Err(bad_field("launch.env", &format!("variable name {} must match [A-Z_][A-Z0-9_]*", shown(k))));
            }
            if FORBIDDEN_ENV.contains(&k.as_str()) || FORBIDDEN_ENV_PREFIXES.iter().any(|p| k.starts_with(p)) {
                return Err(bad_field("launch.env", &format!("variable {k} is not allowed")));
            }
            launch_text("launch.env", v)?;
        }
    }
    Ok(())
}

/// True if `a` is `b`, lies under `b`, or contains `b` (whole path segments).
fn overlaps(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

/// Why a relative path in the home folder is not allowed, or None. The same rules for `home_paths`
/// and for the folders found in `user-dirs.dirs`.
pub fn home_rel_problem(p: &str) -> Option<String> {
    if p.is_empty() || p == "." {
        return Some("the home folder itself is not allowed".to_string());
    }
    if p.len() > MAX_PATH_LEN || !p.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Some("path must be printable ASCII and not too long".to_string());
    }
    if p.starts_with('/') || p.ends_with('/') {
        return Some("path must be relative, with no leading or trailing '/'".to_string());
    }
    if p.split('/').any(|s| s.is_empty() || s == "." || s == "..") {
        return Some("path must not contain empty, '.' or '..' segments".to_string());
    }
    if BROAD_HOME_PATHS.contains(&p) {
        return Some(format!("path {} is too broad", shown(p)));
    }
    FORBIDDEN_HOME_PATHS.iter().find(|f| overlaps(p, f)).map(|f| format!("path {} overlaps the protected path {f}", shown(p)))
}

fn home_paths(list: &[HomePath]) -> Result<()> {
    const F: &str = "permissions.home_paths";
    if list.len() > MAX_HOME_PATHS {
        return Err(bad_field(F, &format!("more than {MAX_HOME_PATHS} paths")));
    }
    for (i, h) in list.iter().enumerate() {
        let p = h.path.as_str();
        if let Some(why) = home_rel_problem(p) {
            return Err(bad_field(F, &why));
        }
        if h.access != "ro" && h.access != "rw" {
            return Err(bad_field(F, "access must be \"ro\" or \"rw\""));
        }
        if list[..i].iter().any(|o| o.path == h.path) {
            return Err(bad_field(F, &format!("path {} listed twice", shown(p))));
        }
    }
    Ok(())
}

fn xdg_dirs(list: &[crate::XdgDir]) -> Result<()> {
    const F: &str = "permissions.xdg_dirs";
    if list.len() > crate::XDG_DIR_NAMES.len() {
        return Err(bad_field(F, "more entries than there are user folders"));
    }
    for (i, d) in list.iter().enumerate() {
        if !crate::XDG_DIR_NAMES.contains(&d.name.as_str()) {
            return Err(bad_field(F, &format!("name {} is not one of {}", shown(&d.name), crate::XDG_DIR_NAMES.join(", "))));
        }
        if d.access != "ro" && d.access != "rw" {
            return Err(bad_field(F, "access must be \"ro\" or \"rw\""));
        }
        if list[..i].iter().any(|o| o.name == d.name) {
            return Err(bad_field(F, &format!("{} listed twice", d.name)));
        }
    }
    Ok(())
}

fn desktop(d: &crate::Desktop) -> Result<()> {
    const F: &str = "desktop.schemes";
    let list = d.schemes.as_deref().unwrap_or_default();
    if list.len() > 8 {
        return Err(bad_field(F, "more than 8 schemes"));
    }
    for (i, sc) in list.iter().enumerate() {
        if let Err(why) = crate::check_scheme_name(sc) {
            return Err(bad_field(F, &format!("{}: {why}", shown(sc))));
        }
        if list[..i].contains(sc) {
            return Err(bad_field(F, &format!("{sc} listed twice")));
        }
    }
    let icons = d.icons.as_deref().unwrap_or_default();
    if icons.len() > 6 {
        return Err(bad_field("desktop.icons", "more than 6 icons"));
    }
    for (i, f) in icons.iter().enumerate() {
        if f.len() > MAX_PATH_LEN || safe_path(f).is_err() || !f.to_ascii_lowercase().ends_with(".png") {
            return Err(bad_field("desktop.icons", &format!("{}: must be a relative path to a .png file, without '.', '..' or empty segments", shown(f))));
        }
        if icons[..i].contains(f) {
            return Err(bad_field("desktop.icons", &format!("{} listed twice", shown(f))));
        }
    }
    if let Some(w) = &d.wm_class {
        if !crate::valid_wm_class(w) {
            return Err(bad_field("desktop.wm_class", "1 to 100 characters of letters, digits, space and . _ + -, no space at either end"));
        }
    }
    Ok(())
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
        if let Some(g) = &a.glibc_min {
            let ok = g.len() <= 16 && g.split('.').count() >= 2 && g.split('.').all(|p| !p.is_empty() && p.len() <= 4 && p.bytes().all(|b| b.is_ascii_digit()));
            if !ok {
                return Err(bad_field("abi.glibc_min", "must be a version like 2.39"));
            }
        }
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
        if let Err(e) = crate::dbus::check_dbus_own(p.dbus_own.as_deref().unwrap_or_default(), &man.app.id, &man.app.name, &man.app.entry) {
            return Err(bad_field("permissions.dbus_own", &format!("{e}")));
        }
        home_paths(p.home_paths.as_deref().unwrap_or_default())?;
        xdg_dirs(p.xdg_dirs.as_deref().unwrap_or_default())?;
    }
    if let Some(d) = &man.desktop {
        desktop(d)?;
    }
    if let Some(l) = &man.launch {
        launch(l)?;
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
    fn app_id_accepts_dbus_style_names() {
        for id in [
            "org.example.app", "a.b", "io.github.AppImageLauncher", "com.x-y.app9", "org.example.my_app", "_a._b", "a.b1",
            "org.example.app-name", "org.example.a-", "org.ex-ample.app", &format!("a.{}", "b".repeat(63)),
        ] {
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
            "org.example.app\u{7f}", "/etc.x", "org.example.1app", "1org.example.app", "org.7.app", "-app.example", "org.-app.example", "org.example.-app", "a.-", "org.example.a+b", "org.example.a:b",
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

    fn with(extra: &str) -> Manifest {
        toml::from_str(&format!(
            "[app]\nid=\"org.example.app\"\nname=\"App\"\nversion=\"1.0\"\nentry=\"bin/app\"\n{extra}[update]\nkey=\"ed25519:{}\"\n",
            "0".repeat(64)
        ))
        .unwrap()
    }

    fn home(path: &str, access: &str) -> Manifest {
        with(&format!("[permissions]\n[[permissions.home_paths]]\npath={path:?}\naccess={access:?}\n"))
    }

    #[test]
    fn home_paths_accept_app_folders() {
        for p in [".config/joplin-desktop", "mu_code", ".local/share/org.example", "Documents/My Notes", ".cache/app", ".config/gh-not"] {
            assert!(validate_manifest(&home(p, "rw")).is_ok(), "{p}");
            assert!(validate_manifest(&home(p, "ro")).is_ok(), "{p}");
        }
    }

    #[test]
    fn home_paths_reject_bad_and_protected_paths() {
        for p in [
            "", ".", "..", "../x", "a/../b", "/etc", "a/", "a//b", "a/./b", ".config", ".local", ".local/share", ".cache",
            ".config/autostart", ".config/autostart/x.desktop", ".local/bin", ".local/bin/tool", ".ssh", ".ssh/id_rsa", ".config/gh",
            ".config/gh/hosts.yml", ".bashrc", ".local/share/panpak", ".local/share/panpak/userdata", ".cache/panpak", ".config/panpak",
            ".config/mimeapps.list", ".local/share/applications", ".local/share/keyrings", ".mozilla/firefox", ".cargo/bin",
            "caf\u{e9}", "a\nb", "a\tb", ".gnupg",
        ] {
            let e = validate_manifest(&home(p, "rw")).expect_err(p);
            assert_eq!(code_of(&e, ErrorCode::E0000), ErrorCode::E0007, "{p:?}");
        }
        for a in ["", "RW", "w", "read"] {
            assert!(validate_manifest(&home("x", a)).is_err(), "{a}");
        }
        let twice = with("[permissions]\n[[permissions.home_paths]]\npath=\"x\"\naccess=\"ro\"\n[[permissions.home_paths]]\npath=\"x\"\naccess=\"rw\"\n");
        assert!(validate_manifest(&twice).is_err());
    }

    #[test]
    fn protected_home_paths_are_all_refused_with_what_is_under_them() {
        for f in FORBIDDEN_HOME_PATHS {
            assert!(validate_manifest(&home(f, "ro")).is_err(), "{f}");
            assert!(validate_manifest(&home(&format!("{f}/sub"), "ro")).is_err(), "{f}/sub");
        }
        for b in BROAD_HOME_PATHS {
            assert!(validate_manifest(&home(b, "ro")).is_err(), "{b}");
        }
    }

    fn launch_manifest(args: &str, env: &str) -> Manifest {
        with(&format!("[launch]\nargs={args}\n[launch.env]\n{env}\n"))
    }

    #[test]
    fn bus_fields_are_validated() {
        let ok = |extra: &str| validate_manifest(&with(extra));
        ok("[permissions]\nnotifications=true\ntray=true\ndbus_own=[\"org.example.app.Second\"]\n").unwrap();
        ok("[desktop]\nwm_class=\"Audacity\"\n").unwrap();
        assert_eq!(code(ok("[permissions]\ndbus_own=[\"org.freedesktop.Notifications\"]\n")), ErrorCode::E0007);
        assert_eq!(code(ok("[permissions]\ndbus_own=[\"org.example.*\"]\n")), ErrorCode::E0007);
        assert_eq!(code(ok("[permissions]\ndbus_own=[\"a.b.c1\",\"a.b.c2\",\"a.b.c3\",\"a.b.c4\",\"a.b.c5\",\"a.b.c6\",\"a.b.c7\",\"a.b.c8\",\"a.b.c9\"]\n")), ErrorCode::E0007);
        ok("[desktop]\nwm_class=\"Audacity 4\"\n").unwrap();
        for w in ["", " a", "a ", "a;b", "a\\nb", "é", "a\\tb"] {
            assert_eq!(code(ok(&format!("[desktop]\nwm_class={w:?}\n"))), ErrorCode::E0007, "{w:?}");
        }
        assert_eq!(code(ok(&format!("[desktop]\nwm_class=\"{}\"\n", "a".repeat(101)))), ErrorCode::E0007);
    }

    #[test]
    fn launch_accepts_placeholders() {
        let m = launch_manifest(
            "[\"--profile\", \"${XDG_CONFIG_HOME}/joplin-desktop\", \"$HOME\"]",
            "IPYTHONDIR=\"${XDG_CONFIG_HOME}/ipython\"\n_X1=\"${TMPDIR}:${XDG_STATE_HOME}\"",
        );
        assert!(validate_manifest(&m).is_ok());
        for n in PLACEHOLDERS {
            assert!(validate_manifest(&launch_manifest(&format!("[\"${{{n}}}\"]"), "")).is_ok(), "{n}");
        }
    }

    #[test]
    fn launch_rejects_unsafe_values() {
        for args in ["[\"${HOME}\"]", "[\"${XDG_RUNTIME_DIR}/x\"]", "[\"${XDG_CONFIG_HOME\"]", "[\"a\\nb\"]", "[\"a\\u0000b\"]", "[\"${}\"]"] {
            let e = validate_manifest(&launch_manifest(args, "")).expect_err(args);
            assert_eq!(code_of(&e, ErrorCode::E0000), ErrorCode::E0007, "{args}");
        }
        let too_many = format!("[{}]", vec!["\"a\""; 65].join(","));
        assert!(validate_manifest(&launch_manifest(&too_many, "")).is_err());
        for env in [
            "LD_PRELOAD=\"x\"", "LD_LIBRARY_PATH=\"x\"", "PATH=\"x\"", "HOME=\"x\"", "XDG_RUNTIME_DIR=\"x\"", "PYTHONPATH=\"x\"",
            "PYTHONHOME=\"x\"", "PERL5LIB=\"x\"", "NODE_OPTIONS=\"x\"", "ELECTRON_RUN_AS_NODE=\"1\"", "GIO_MODULE_DIR=\"x\"",
            "GTK_MODULES=\"x\"", "GTK_PATH=\"x\"", "QT_PLUGIN_PATH=\"x\"", "PANPAK_X=\"x\"", "XDG_CONFIG_HOME=\"x\"", "TMPDIR=\"x\"",
            "lower=\"x\"", "\"1A\"=\"x\"", "\"A-B\"=\"x\"", "\"A B\"=\"x\"", "A=\"${NOPE}\"", "A=\"a\\u0001b\"",
        ] {
            assert!(validate_manifest(&launch_manifest("[]", env)).is_err(), "{env}");
        }
    }

    #[test]
    fn every_forbidden_env_name_is_valid_syntax() {
        // The deny list is checked after the syntax: a name that fails the syntax would hide a typo.
        for n in FORBIDDEN_ENV {
            assert!(env_name_ok(n), "{n}");
        }
    }
}
