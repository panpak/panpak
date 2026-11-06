// SPDX-License-Identifier: MIT OR Apache-2.0
//! The identity that xdg-desktop-portal gives a host process, from the name of its systemd user unit.
//!
//! The portal (`shared/xdp-app-info-host.c`, `get_app_from_pid` and
//! `_xdp_app_info_host_parse_app_id_from_unit_name`) takes the user unit of the calling process with
//! `sd_pid_get_user_unit`, requires the prefix `app-`, and reads the app id with two regular expressions:
//!
//! ```text
//! scope or slice: ^app-(?:[[:alnum:]]+\-)?(.+?)(?:\-[[:alnum:]]*)(?:\.scope|\.slice)$
//! service:        ^app-(?:[[:alnum:]]+\-)?(.+?)(?:@[[:alnum:]]*|\-autostart)?\.service$
//! ```
//!
//! The first is tried first. The id it finds is unescaped (`\x2d` is a dash) and `.desktop` is added to find the
//! entry. This module is the same two rules written by hand, so that the stub can tell, without asking the portal,
//! whether the unit it runs in already gives the app the identity `panpak.<id>`. The rule here MUST NOT accept a
//! name the portal reads differently: that would give the app the identity of its launcher.

/// The user unit that `sd_pid_get_user_unit` returns for a cgroup path (the `0::` line of `/proc/<pid>/cgroup`):
/// after `user@<uid>.service`, the first component that is not a slice. A process in a sub-cgroup of a service
/// belongs to that service.
pub fn user_unit_of_cgroup(path: &str) -> Option<&str> {
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    parts.find(|p| p.starts_with("user@") && p.ends_with(".service"))?;
    parts.find(|p| !p.ends_with(".slice"))
}

/// `[[:alnum:]]` of the C locale.
fn alnum(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `\xHH` escapes of systemd, as `cunescape` reads them in the relaxed mode: a bad escape stays as it is. Only
/// `\x` is decoded; any other backslash stays, so the result then differs from every id, which is the safe side.
fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        if b[i] == b'\\' && i + 4 <= b.len() && b[i + 1] == b'x' {
            if let Some(v) = std::str::from_utf8(&b[i + 2..i + 4]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// `(.+?)` followed by the end of the scope pattern: what is before the last `-`, if what follows is letters and digits.
fn scope_id(rest: &str) -> Option<&str> {
    let (id, tail) = rest.rsplit_once('-')?;
    (!id.is_empty() && alnum(tail)).then_some(id)
}

/// `(.+?)` followed by the end of the service pattern: the shortest id such that the rest is `@<alnum>*`,
/// `-autostart` or nothing.
fn service_id(rest: &str) -> Option<&str> {
    let at = rest.rfind('@').filter(|&p| alnum(&rest[p + 1..]));
    let auto = rest.strip_suffix("-autostart").map(str::len);
    let end = [at, auto].into_iter().flatten().min().unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// The id the portal reads in a unit name (still escaped), or `None` if neither expression matches.
fn raw_id(unit: &str) -> Option<&str> {
    let body = unit.strip_prefix("app-")?;
    let (rest, find): (&str, fn(&str) -> Option<&str>) = if let Some(r) = body.strip_suffix(".scope").or_else(|| body.strip_suffix(".slice")) {
        (r, scope_id)
    } else {
        (body.strip_suffix(".service")?, service_id)
    };
    // The launcher word is optional and tried first; the expression falls back to none if the rest does not match.
    if let Some((word, after)) = rest.split_once('-') {
        if !word.is_empty() && alnum(word) {
            if let Some(id) = find(after) {
                return Some(id);
            }
        }
    }
    find(rest)
}

/// The app id (unescaped) the portal reads in `unit`, as bytes.
pub fn app_id_from_unit(unit: &str) -> Option<Vec<u8>> {
    raw_id(unit).map(unescape)
}

/// Does the portal take `unit` for the app `panpak.<app_id>`?
pub fn unit_gives_id(unit: &str, app_id: &str) -> bool {
    app_id_from_unit(unit).is_some_and(|id| id == format!("panpak.{app_id}").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "org.example.app";

    #[test]
    fn the_cgroup_path_gives_the_user_unit() {
        let p = "/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-panpak.org.example.app-4242.scope";
        assert_eq!(user_unit_of_cgroup(p), Some("app-gnome-panpak.org.example.app-4242.scope"));
        // A sub-cgroup belongs to the unit above it.
        assert_eq!(user_unit_of_cgroup(&format!("{p}/inner")), Some("app-gnome-panpak.org.example.app-4242.scope"));
        // A process of the login session is in no user unit of the manager.
        assert_eq!(user_unit_of_cgroup("/user.slice/user-1000.slice/session-3.scope"), None);
        assert_eq!(user_unit_of_cgroup("/user.slice/user-1000.slice/user@1000.service/init.scope"), Some("init.scope"));
        assert_eq!(user_unit_of_cgroup(""), None);
    }

    #[test]
    fn scope_names_of_each_launcher() {
        // GNOME, KDE-style launcher word, and no launcher at all.
        assert!(unit_gives_id("app-gnome-panpak.org.example.app-4242.scope", ID));
        assert!(unit_gives_id("app-KDE-panpak.org.example.app-99.scope", ID));
        assert!(unit_gives_id("app-panpak.org.example.app-1a2b3c4d.scope", ID));
        assert!(unit_gives_id("app-gnome-panpak.org.example.app-.scope", ID), "an empty tail is allowed by the expression");
        assert!(unit_gives_id("app-gnome-panpak.org.example.app-4242.slice", ID));
        // Other apps.
        assert!(!unit_gives_id("app-ghostty-surface-transient-3121607.scope", ID));
        assert!(!unit_gives_id("app-gnome-org.gnome.Ptyxis-1234.scope", ID));
        assert!(!unit_gives_id("app-gnome-panpak.org.example.other-4242.scope", ID));
        assert!(!unit_gives_id("app-gnome-panpak.org.example.application-4242.scope", ID), "a longer id");
        assert!(!unit_gives_id("app-gnome-panpak.org.example.ap-4242.scope", ID), "a prefix");
        // Not a desktop unit at all.
        assert!(!unit_gives_id("panpak.org.example.app.scope", ID));
        assert!(!unit_gives_id("session-3.scope", ID));
        assert!(!unit_gives_id("app-panpak.org.example.app.target", ID));
        assert!(!unit_gives_id("app-", ID));
        assert!(!unit_gives_id("", ID));
    }

    #[test]
    fn service_names_of_each_launcher() {
        // app[-<launcher>]-<id>[@<random>].service, and the old -autostart form.
        assert!(unit_gives_id("app-gnome-panpak.org.example.app@1a2b.service", ID));
        assert!(unit_gives_id("app-panpak.org.example.app@1a2b.service", ID));
        assert!(unit_gives_id("app-panpak.org.example.app.service", ID));
        assert!(unit_gives_id("app-gnome-panpak.org.example.app.service", ID));
        assert!(unit_gives_id("app-gnome-panpak.org.example.app-autostart.service", ID));
        assert!(unit_gives_id("app-panpak.org.example.app@.service", ID), "an empty random part");
        // KDE's own apps and other ids.
        assert!(!unit_gives_id("app-org.kde.konsole@1a2b.service", ID));
        assert!(!unit_gives_id("app-plasma-org.kde.konsole@1a2b.service", ID));
        assert!(!unit_gives_id("app-panpak.org.example.other@1a2b.service", ID));
        // The random part is letters and digits: anything else is part of the id and so another id.
        assert!(!unit_gives_id("app-panpak.org.example.app@1-2.service", ID));
        assert_eq!(app_id_from_unit("app-panpak.org.example.app@a@b.service").unwrap(), b"panpak.org.example.app@a");
        // A scope name never matches the service expression and the reverse.
        assert!(!unit_gives_id("app-panpak.org.example.app@1a2b.scope", ID));
        assert!(!unit_gives_id("app-panpak.org.example.app-1.service", ID), "the service expression has no dash tail");
    }

    #[test]
    fn escapes_and_the_launcher_word() {
        // A dash in the id is written \x2d; the portal reads a plain dash in a scope name the same way.
        assert!(unit_gives_id("app-gnome-panpak.org.my\\x2dapp-1.scope", "org.my-app"));
        assert!(unit_gives_id("app-gnome-panpak.org.my-app-1.scope", "org.my-app"));
        assert!(unit_gives_id("app-panpak.org.my\\x2dapp@7.service", "org.my-app"));
        // Other characters can be escaped too.
        assert!(unit_gives_id("app-gnome-panpak\\x2eorg.example.app-1.scope", ID));
        // The reverse: ours has a dash, the name has none; or another character takes its place.
        assert!(!unit_gives_id("app-gnome-panpak.org.myapp-1.scope", "org.my-app"));
        assert!(!unit_gives_id("app-gnome-panpak.org.my\\x5fapp-1.scope", "org.my-app"));
        // A bad or other escape is not read as one.
        assert!(!unit_gives_id("app-gnome-panpak.org.my\\x2zapp-1.scope", "org.my-app"));
        assert!(!unit_gives_id("app-gnome-panpak.org.example.app\\x-1.scope", ID));
        assert!(!unit_gives_id("app-gnome-panpak.org.example.app\\n-1.scope", ID));
        // The id of an app starts with `panpak.`, which has a dot: it can never be taken for a launcher word.
        assert_eq!(app_id_from_unit("app-gnome-panpak.org.example.app-4242.scope").unwrap(), b"panpak.org.example.app");
        assert_eq!(app_id_from_unit("app-panpak.org.example.app-4242.scope").unwrap(), b"panpak.org.example.app");
        // Non-ASCII and control characters in the launcher word or the tail do not match.
        assert!(!unit_gives_id("app-gn\u{f4}me-panpak.org.example.app-1.scope", ID));
        assert!(!unit_gives_id("app-gnome-panpak.org.example.app-\u{e9}.scope", ID));
        assert!(!unit_gives_id("app-gnome-panpak.org.example.app-1\n.scope", ID));
    }
}
