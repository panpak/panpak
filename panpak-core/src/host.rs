// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `[host]` table (PANPAK-FORMAT.md §3.1.11): what a package needs of the system it runs on, written by `pack` and signed. `install`, `update` and `doctor`
//! compare it with the system and refuse a package that cannot run here, saying what it needs, what the system has and what to do; `--force-host` installs it anyway.
//! The start of the app checks again only what costs nothing and can change from one session to the next: the display and the Landlock ABI.

use crate::{coded, ErrorCode};
use anyhow::{bail, Result};

/// The keys of `[host]`, for the schema and for the list of critical paths.
pub const KEYS: &[&str] = &["arch", "glibc", "kernel", "landlock", "display", "portals", "bus"];
/// The architectures this panpak knows.
pub const ARCHS: &[&str] = &["x86_64", "aarch64"];
/// The display servers an app may name. One of them is enough.
pub const DISPLAYS: &[&str] = &["x11", "wayland"];
/// The portals an app may name.
pub const PORTALS: &[&str] = &["document"];
/// What the app needs of the session bus: `none` asks nothing, `private` asks for the filtered bus (a session bus and `xdg-dbus-proxy`).
pub const BUSES: &[&str] = &["none", "private"];

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    /// `x86_64` or `aarch64`: written by `pack` (the architecture of the machine that packs, which is the one of the runtime in the package).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// The oldest glibc of the system that the files of the tree can use, `2.39`: written by `pack` from the symbol versions of the files; an author may raise it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glibc: Option<String>,
    /// The oldest Linux kernel the app needs, `5.15`. Given by the author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    /// The Landlock ABI the app needs of the system (1 to 9). Given by the author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landlock: Option<u32>,
    /// The display servers the app can use: the system must have one of them in the session. Given by the author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<Vec<String>>,
    /// The portals the app needs. Given by the author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portals: Option<Vec<String>>,
    /// What the app needs of the session bus (`none` or `private`). Given by the author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus: Option<String>,
}

/// A version `2.39` or `5.15.2`: two to four numbers of at most four digits.
pub fn numbers(v: &str) -> Option<Vec<u32>> {
    let parts: Vec<&str> = v.split('.').collect();
    if v.len() > 24 || !(2..=4).contains(&parts.len()) || parts.iter().any(|p| p.is_empty() || p.len() > 4 || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    parts.iter().map(|p| p.parse().ok()).collect()
}

/// The forms of a `[host]` table of an input (`pack`) or of a signed manifest: a value that is not of the form is a malformed field. A name that this panpak
/// does not know (a display, a portal, a bus, an architecture) is a requirement that it cannot check: "update panpak".
pub fn validate(h: &Host) -> Result<()> {
    let bad = |field: &str, why: &str| crate::validate::bad_field(&format!("host.{field}"), why);
    for (field, v) in [("glibc", &h.glibc), ("kernel", &h.kernel)] {
        if let Some(v) = v {
            if numbers(v).is_none() {
                return Err(bad(field, "must be a version like 2.39"));
            }
        }
    }
    if let Some(l) = h.landlock {
        if !(1..=9).contains(&l) {
            return Err(bad("landlock", "must be a number from 1 to 9"));
        }
    }
    for (field, list) in [("display", &h.display), ("portals", &h.portals)] {
        if let Some(l) = list {
            if l.is_empty() || l.len() > 8 || l.iter().enumerate().any(|(i, x)| l[..i].contains(x)) {
                return Err(bad(field, "must be a list of one to eight different names"));
            }
        }
    }
    let newer = |what: &str, v: &str| coded(ErrorCode::E0002, format!("[host] {what} {v:?} is not known to this panpak: update panpak to the latest version"));
    if let Some(a) = &h.arch {
        if !ARCHS.contains(&a.as_str()) {
            return Err(newer("arch", a));
        }
    }
    for d in h.display.iter().flatten() {
        if !DISPLAYS.contains(&d.as_str()) {
            return Err(newer("display", d));
        }
    }
    for p in h.portals.iter().flatten() {
        if !PORTALS.contains(&p.as_str()) {
            return Err(newer("portal", p));
        }
    }
    if let Some(b) = &h.bus {
        if !BUSES.contains(&b.as_str()) {
            return Err(newer("bus", b));
        }
    }
    Ok(())
}

/// What this system has, as far as `[host]` asks. The program that asks builds it (it reads the machine); the check below is a function of the two.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub arch: String,
    /// The glibc of the system, `None` when it is not glibc or cannot be read: such a system is not refused here (the loader says what is wrong at the first start).
    pub glibc: Option<String>,
    pub kernel: Option<String>,
    /// The Landlock ABI of the kernel, `None` without Landlock.
    pub landlock: Option<u32>,
    /// The display servers of the session that this process is in.
    pub displays: Vec<String>,
    pub portals: Vec<String>,
    /// A session bus and `xdg-dbus-proxy`.
    pub private_bus: bool,
}

impl Facts {
    /// What this machine and this session have. `start` is the cheap reading that the start of an app does (the display and the Landlock ABI); the whole reading runs
    /// `ldd` and looks for `xdg-dbus-proxy`, which `install`, `update` and `doctor` can afford.
    pub fn detect(start: bool) -> Facts {
        let mut f = Facts { arch: std::env::consts::ARCH.to_string(), ..Facts::default() };
        f.landlock = crate::detect_abi().ok().map(landlock_number);
        let set = |v: &str| std::env::var_os(v).is_some_and(|x| !x.is_empty());
        if set("DISPLAY") {
            f.displays.push("x11".into());
        }
        if let Some(w) = std::env::var_os("WAYLAND_DISPLAY").filter(|w| !w.is_empty()) {
            let path = std::path::Path::new(&w);
            let socket = if path.is_absolute() { path.to_path_buf() } else { std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_default()).join(path) };
            if std::fs::metadata(&socket).is_ok_and(|m| std::os::unix::fs::FileTypeExt::is_socket(&m.file_type())) {
                f.displays.push("wayland".into());
            }
        }
        if start {
            return f;
        }
        f.glibc = glibc_of_system();
        f.kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok().map(|t| t.trim().chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect::<String>()).filter(|k| numbers(k).is_some());
        if crate::permissions::documents_available() {
            f.portals.push("document".into());
        }
        f.private_bus = crate::dbus::session_bus_path().is_some() && crate::dbus::find_proxy().is_some();
        f
    }
}

/// The number of a Landlock ABI (`ABI::V4` is 4).
fn landlock_number(a: landlock::ABI) -> u32 {
    use landlock::ABI;
    [ABI::V1, ABI::V2, ABI::V3, ABI::V4, ABI::V5, ABI::V6, ABI::V7, ABI::V8, ABI::V9].iter().rposition(|x| *x <= a).map_or(1, |i| i as u32 + 1)
}

/// The glibc of the system as `ldd --version` prints it (`2.39`); `None` for musl and for a system with no `ldd`.
pub fn glibc_of_system() -> Option<String> {
    let out = std::process::Command::new("ldd").arg("--version").output().ok()?;
    let first = String::from_utf8_lossy(&out.stdout).lines().next()?.to_string();
    if first.contains("musl") || !first.contains("libc") && !first.contains("GLIBC") {
        return None;
    }
    let ver = first.split_whitespace().last()?.to_string();
    let head: String = ver.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    numbers(head.trim_matches('.')).map(|_| head.trim_matches('.').to_string())
}

/// One requirement that the system does not meet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmet {
    pub key: &'static str,
    pub code: ErrorCode,
    pub need: String,
    pub have: String,
    pub todo: String,
}

impl Unmet {
    /// One line: what the package needs, what the system has, what to do.
    pub fn line(&self) -> String {
        format!("needs {}; this system has {}. {}", self.need, self.have, self.todo)
    }
}

/// The requirements of `h` that `f` does not meet, in the order of `KEYS`. `only_at_start` keeps the two that the start of the app checks again (`display`, `landlock`).
pub fn unmet(h: &Host, f: &Facts, only_at_start: bool) -> Vec<Unmet> {
    let mut out = Vec::new();
    let at = |key: &str| !only_at_start || key == "display" || key == "landlock";
    if let (true, Some(a)) = (at("arch"), &h.arch) {
        if *a != f.arch {
            out.push(Unmet { key: "arch", code: ErrorCode::E2009, need: format!("a {a} machine"), have: format!("a {} machine", f.arch), todo: "Get the package for this architecture from the author.".into() });
        }
    }
    if let (true, Some(g), Some(have)) = (at("glibc"), &h.glibc, &f.glibc) {
        if numbers(have).zip(numbers(g)).is_some_and(|(h, n)| h < n) {
            out.push(Unmet { key: "glibc", code: ErrorCode::E2005, need: format!("glibc {g} or newer (it uses symbols of that version)"), have: format!("glibc {have}"), todo: "It cannot run here: install it on a newer distribution.".into() });
        }
    }
    if let (true, Some(k), Some(have)) = (at("kernel"), &h.kernel, &f.kernel) {
        if numbers(have).zip(numbers(k)).is_some_and(|(h, n)| h < n) {
            out.push(Unmet { key: "kernel", code: ErrorCode::E2009, need: format!("Linux {k} or newer"), have: format!("Linux {have}"), todo: "Update the kernel of the system, or run the app on a newer one.".into() });
        }
    }
    if let (true, Some(l)) = (at("landlock"), h.landlock) {
        if f.landlock.is_none_or(|have| have < l) {
            let have = f.landlock.map_or("no Landlock".to_string(), |v| format!("Landlock ABI {v}"));
            out.push(Unmet { key: "landlock", code: ErrorCode::E2009, need: format!("Landlock ABI {l} or newer"), have, todo: "Use a kernel with that Landlock ABI (Linux 5.13 has ABI 1, 6.7 has 4, 6.12 has 6).".into() });
        }
    }
    if let (true, Some(d)) = (at("display"), &h.display) {
        if !d.iter().any(|x| f.displays.contains(x)) {
            let have = if f.displays.is_empty() { "no display server in this session".to_string() } else { format!("the display server {}", f.displays.join(" and ")) };
            out.push(Unmet { key: "display", code: ErrorCode::E2009, need: format!("the display server {}", d.join(" or ")), have, todo: "Install it from a graphical session, or give a display to the one that you are in.".into() });
        }
    }
    if let (true, Some(p)) = (at("portals"), &h.portals) {
        let missing: Vec<&String> = p.iter().filter(|x| !f.portals.contains(x)).collect();
        if !missing.is_empty() {
            let names: Vec<&str> = missing.iter().map(|s| s.as_str()).collect();
            out.push(Unmet { key: "portals", code: ErrorCode::E2009, need: format!("the {} portal", names.join(" and the ")), have: "no such portal in this session".into(), todo: "Install xdg-desktop-portal and the portal of your desktop, then start a new session.".into() });
        }
    }
    if let (true, Some("private")) = (at("bus"), h.bus.as_deref()) {
        if !f.private_bus {
            out.push(Unmet { key: "bus", code: ErrorCode::E2009, need: "a session bus and xdg-dbus-proxy (the filtered bus)".into(), have: "no session bus, or no xdg-dbus-proxy".into(), todo: "Install xdg-dbus-proxy and run from a session that has a user bus.".into() });
        }
    }
    out
}

/// The error for a package that this system cannot run: every unmet requirement, each with what is needed, what is there and what to do, and the way to go on.
pub fn refusal(what: &str, list: &[Unmet]) -> anyhow::Error {
    let Some(first) = list.first() else { return anyhow::anyhow!("no unmet requirement") };
    let mut text = format!("{what} cannot run on this system:");
    for u in list {
        text.push_str(&format!("\n  - {}", u.line()));
    }
    text.push_str("\nTo install it anyway, give --force-host (it may not start).");
    coded(first.code, text)
}

/// Check the `[host]` of a manifest that has one.
pub fn check_or_refuse(man: &crate::Manifest, f: &Facts, only_at_start: bool) -> Result<()> {
    let Some(h) = &man.host else { return Ok(()) };
    let list = unmet(h, f, only_at_start);
    if list.is_empty() {
        return Ok(());
    }
    if man.app.name.is_empty() {
        bail!("host check without a name");
    }
    Err(refusal(&format!("{} {}", man.app.name, man.app.version), &list))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts { arch: "x86_64".into(), glibc: Some("2.39".into()), kernel: Some("6.8.0".into()), landlock: Some(4), displays: vec!["wayland".into()], portals: vec!["document".into()], private_bus: true }
    }

    #[test]
    fn a_package_that_the_system_can_run_has_nothing_unmet() {
        let h = Host { arch: Some("x86_64".into()), glibc: Some("2.31".into()), kernel: Some("5.15".into()), landlock: Some(3), display: Some(vec!["x11".into(), "wayland".into()]), portals: Some(vec!["document".into()]), bus: Some("private".into()) };
        assert!(unmet(&h, &facts(), false).is_empty());
        assert!(unmet(&Host::default(), &Facts::default(), false).is_empty());
    }

    #[test]
    fn each_requirement_that_is_not_met_says_what_is_needed_what_is_there_and_what_to_do() {
        let h = Host { arch: Some("aarch64".into()), glibc: Some("2.40".into()), kernel: Some("6.9".into()), landlock: Some(6), display: Some(vec!["x11".into()]), portals: Some(vec!["document".into()]), bus: Some("private".into()) };
        let f = Facts { portals: vec![], private_bus: false, ..facts() };
        let u = unmet(&h, &f, false);
        let keys: Vec<&str> = u.iter().map(|u| u.key).collect();
        assert_eq!(keys, ["arch", "glibc", "kernel", "landlock", "display", "portals", "bus"]);
        for x in &u {
            assert!(!x.need.is_empty() && !x.have.is_empty() && x.todo.ends_with('.'), "{x:?}");
        }
        assert_eq!(u[1].code, ErrorCode::E2005, "glibc keeps its code");
        assert_eq!(u[2].code, ErrorCode::E2009);
        let e = refusal("App 1.0", &u).to_string();
        assert!(e.contains("App 1.0 cannot run on this system:") && e.contains("needs glibc 2.40 or newer") && e.contains("this system has glibc 2.39") && e.contains("--force-host"), "{e}");
    }

    #[test]
    fn the_start_checks_only_the_display_and_the_landlock_abi() {
        let h = Host { arch: Some("aarch64".into()), kernel: Some("9.0".into()), landlock: Some(9), display: Some(vec!["x11".into()]), ..Host::default() };
        let keys: Vec<&str> = unmet(&h, &facts(), true).iter().map(|u| u.key).collect();
        assert_eq!(keys, ["landlock", "display"]);
    }

    #[test]
    fn a_system_whose_glibc_or_kernel_cannot_be_read_is_not_refused_for_it() {
        let h = Host { glibc: Some("99.1".into()), kernel: Some("99.1".into()), ..Host::default() };
        let f = Facts { glibc: None, kernel: None, ..facts() };
        assert!(unmet(&h, &f, false).is_empty());
    }

    #[test]
    fn forms_and_unknown_names() {
        let ok = Host { glibc: Some("2.39".into()), kernel: Some("5.15.2".into()), landlock: Some(1), display: Some(vec!["x11".into()]), ..Host::default() };
        assert!(validate(&ok).is_ok());
        for bad in [Host { glibc: Some("two".into()), ..Host::default() }, Host { kernel: Some("5".into()), ..Host::default() }, Host { landlock: Some(0), ..Host::default() }, Host { landlock: Some(10), ..Host::default() }, Host { display: Some(vec![]), ..Host::default() }, Host { portals: Some(vec!["document".into(), "document".into()]), ..Host::default() }] {
            assert!(validate(&bad).is_err(), "{bad:?}");
        }
        for newer in [Host { arch: Some("riscv64".into()), ..Host::default() }, Host { display: Some(vec!["mir".into()]), ..Host::default() }, Host { portals: Some(vec!["camera".into()]), ..Host::default() }, Host { bus: Some("shared".into()), ..Host::default() }] {
            let e = validate(&newer).unwrap_err().to_string();
            assert!(e.contains("update panpak"), "{e}");
        }
    }
}
