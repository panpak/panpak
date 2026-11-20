// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `[host]` table (PANPAK-FORMAT.md §3.1.11): what a package needs of the system it runs on, written by `pack` and signed. `install`, `update` and `doctor`
//! compare it with the system and refuse a package that cannot run here, saying what it needs, what the system has and what to do; `--force-host` installs it anyway.
//! The start of the app checks again only what costs nothing and can change from one session to the next: the display and the Landlock ABI.

use crate::{coded, ErrorCode};
use crate::messages as M;
use anyhow::Result;

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
    /// What this machine and this session have: everything `[host]` can ask. `install`, `update` and `doctor` use it (they can afford `ldd` and the search for
    /// `xdg-dbus-proxy`).
    pub fn detect() -> Facts {
        Facts::read(true, true, true, true, true)
    }

    /// What the start of an app needs to read for the package `h`: only what the table names (a package that names nothing reads nothing).
    pub fn detect_for_start(h: Option<&Host>) -> Facts {
        let h = h.cloned().unwrap_or_default();
        Facts::read(false, h.landlock.is_some(), h.display.is_some(), h.portals.is_some(), h.bus.as_deref() == Some("private"))
    }

    fn read(machine: bool, landlock: bool, display: bool, portals: bool, bus: bool) -> Facts {
        let mut f = Facts { arch: std::env::consts::ARCH.to_string(), ..Facts::default() };
        if landlock {
            f.landlock = crate::detect_abi().ok().map(landlock_number);
        }
        if display {
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
        }
        if machine {
            f.glibc = glibc_of_system();
            f.kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok().map(|t| t.trim().chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect::<String>()).filter(|k| numbers(k).is_some());
        }
        if portals && crate::permissions::documents_available() {
            f.portals.push("document".into());
        }
        if bus {
            f.private_bus = crate::dbus::session_bus_exists().is_some() && crate::dbus::find_proxy().is_some();
        }
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

/// A message of the catalog and what fills it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Msg(pub &'static str, pub Vec<(&'static str, String)>);

impl Msg {
    fn text(&self) -> String {
        crate::i18n::fill(&crate::i18n::tr(self.0), &self.1)
    }
}

fn msg(id: &'static str, vals: &[(&'static str, &str)]) -> Msg {
    Msg(id, vals.iter().map(|(k, v)| (*k, v.to_string())).collect())
}

/// One requirement that the system does not meet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmet {
    pub key: &'static str,
    pub code: ErrorCode,
    pub need: Msg,
    pub have: Msg,
    pub todo: Msg,
}

/// The keys of `[host]` that are properties of the machine (checked at install and update, and again by `doctor`) and those that are properties of the session
/// that runs the command (checked when the app starts: an install over ssh, from a script or in the build of an image has no display and must work).
pub const MACHINE_KEYS: &[&str] = &["arch", "glibc", "kernel", "landlock"];
pub const SESSION_KEYS: &[&str] = &["display", "portals", "bus"];

impl Unmet {
    /// One line, in the language of the person: what the package needs, what the system has, what to do.
    pub fn line(&self) -> String {
        crate::i18n::fill(&crate::i18n::tr(crate::messages::HOST_LINE), &[("need", self.need.text()), ("have", self.have.text()), ("todo", self.todo.text())])
    }
    pub fn is_session(&self) -> bool {
        SESSION_KEYS.contains(&self.key)
    }
}

/// Which requirements are looked at: the ones of the machine (`install`, `update`), the ones of the session and the Landlock ABI (the start of the app), or all
/// of them (`doctor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Machine,
    Start,
    All,
}

/// The requirements of `h` that `f` does not meet, in the order of `KEYS`.
pub fn unmet(h: &Host, f: &Facts, scope: Scope) -> Vec<Unmet> {
    let mut out = Vec::new();
    let at = |key: &str| match scope {
        Scope::All => true,
        Scope::Machine => MACHINE_KEYS.contains(&key),
        Scope::Start => SESSION_KEYS.contains(&key) || key == "landlock",
    };
    if let (true, Some(a)) = (at("arch"), &h.arch) {
        if *a != f.arch {
            out.push(Unmet { key: "arch", code: ErrorCode::E2009, need: msg(M::HOST_ARCH_NEED, &[("arch", a)]), have: msg(M::HOST_ARCH_HAVE, &[("arch", &f.arch)]), todo: msg(M::HOST_ARCH_TODO, &[]) });
        }
    }
    if let (true, Some(g), Some(have)) = (at("glibc"), &h.glibc, &f.glibc) {
        if numbers(have).zip(numbers(g)).is_some_and(|(h, n)| h < n) {
            out.push(Unmet { key: "glibc", code: ErrorCode::E2005, need: msg(M::HOST_GLIBC_NEED, &[("version", g)]), have: msg(M::HOST_GLIBC_HAVE, &[("version", have)]), todo: msg(M::HOST_GLIBC_TODO, &[]) });
        }
    }
    if let (true, Some(k), Some(have)) = (at("kernel"), &h.kernel, &f.kernel) {
        if numbers(have).zip(numbers(k)).is_some_and(|(h, n)| h < n) {
            out.push(Unmet { key: "kernel", code: ErrorCode::E2009, need: msg(M::HOST_KERNEL_NEED, &[("version", k)]), have: msg(M::HOST_KERNEL_HAVE, &[("version", have)]), todo: msg(M::HOST_KERNEL_TODO, &[]) });
        }
    }
    if let (true, Some(l)) = (at("landlock"), h.landlock) {
        if f.landlock.is_none_or(|have| have < l) {
            let have = f.landlock.map_or_else(|| msg(M::HOST_LANDLOCK_NONE, &[]), |v| msg(M::HOST_LANDLOCK_HAVE, &[("version", &v.to_string())]));
            out.push(Unmet { key: "landlock", code: ErrorCode::E2009, need: msg(M::HOST_LANDLOCK_NEED, &[("version", &l.to_string())]), have, todo: msg(M::HOST_LANDLOCK_TODO, &[]) });
        }
    }
    if let (true, Some(d)) = (at("display"), &h.display) {
        if !d.iter().any(|x| f.displays.contains(x)) {
            let have = if f.displays.is_empty() { msg(M::HOST_DISPLAY_NONE, &[]) } else { msg(M::HOST_DISPLAY_HAVE, &[("names", &f.displays.join(" / "))]) };
            out.push(Unmet { key: "display", code: ErrorCode::E2009, need: msg(M::HOST_DISPLAY_NEED, &[("names", &d.join(" / "))]), have, todo: msg(M::HOST_DISPLAY_TODO, &[]) });
        }
    }
    if let (true, Some(p)) = (at("portals"), &h.portals) {
        let missing: Vec<&str> = p.iter().filter(|x| !f.portals.contains(x)).map(|s| s.as_str()).collect();
        if !missing.is_empty() {
            out.push(Unmet { key: "portals", code: ErrorCode::E2009, need: msg(M::HOST_PORTALS_NEED, &[("names", &missing.join(" / "))]), have: msg(M::HOST_PORTALS_HAVE, &[]), todo: msg(M::HOST_PORTALS_TODO, &[]) });
        }
    }
    if let (true, Some("private")) = (at("bus"), h.bus.as_deref()) {
        if !f.private_bus {
            out.push(Unmet { key: "bus", code: ErrorCode::E2009, need: msg(M::HOST_BUS_NEED, &[]), have: msg(M::HOST_BUS_HAVE, &[]), todo: msg(M::HOST_BUS_TODO, &[]) });
        }
    }
    out
}

/// The error for a package that this system cannot run: every unmet requirement, each with what is needed, what is there and what to do, and, for a requirement of the
/// machine, the way to go on. `session` is for the start of an app (a session requirement is not forced).
pub fn refusal(app: &str, list: &[Unmet], session: bool) -> anyhow::Error {
    use crate::i18n::{fill, tr};
    let Some(first) = list.first() else { return anyhow::anyhow!("no unmet requirement") };
    let head = if session { M::HOST_CANNOT_RUN_SESSION } else { M::HOST_CANNOT_RUN };
    let mut text = fill(&tr(head), &[("app", app.to_string())]);
    for u in list {
        text.push_str(&format!("\n  - {}", u.line()));
    }
    if !session {
        text.push('\n');
        text.push_str(&tr(M::HOST_FORCE_HINT));
    }
    coded(first.code, text)
}

/// Check the `[host]` of a manifest at the start of an app: the requirements of the session, and the Landlock ABI unless the app was installed with `--force-host`.
/// The session bus is not one of the refusals: an app that asks for the filtered bus and finds no session bus starts without it (see `start_notes`).
pub fn check_start(man: &crate::Manifest, f: &Facts, forced: bool) -> Result<()> {
    let Some(h) = &man.host else { return Ok(()) };
    let list: Vec<Unmet> = unmet(h, f, Scope::Start).into_iter().filter(|u| u.key != "bus" && !(forced && u.key == "landlock")).collect();
    if list.is_empty() {
        return Ok(());
    }
    Err(refusal(&format!("{} {}", man.app.name, man.app.version), &list, true))
}

/// What the start says and goes on: an app that asks for the filtered session bus (`bus = "private"`) where there is no session bus or no `xdg-dbus-proxy` starts
/// without any bridge to the session, and the person is told what that costs (the portals and the notifications).
pub fn start_notes(man: &crate::Manifest, f: &Facts) -> Vec<String> {
    use crate::i18n::{fill, tr};
    let Some(h) = &man.host else { return Vec::new() };
    unmet(h, f, Scope::Start)
        .into_iter()
        .filter(|u| u.key == "bus")
        .map(|u| fill(&tr(M::HOST_BUS_START_NOTE), &[("app", man.app.name.clone()), ("line", u.line())]))
        .collect()
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
        assert!(unmet(&h, &facts(), Scope::All).is_empty());
        assert!(unmet(&Host::default(), &Facts::default(), Scope::All).is_empty());
    }

    #[test]
    fn each_requirement_that_is_not_met_says_what_is_needed_what_is_there_and_what_to_do() {
        let h = Host { arch: Some("aarch64".into()), glibc: Some("2.40".into()), kernel: Some("6.9".into()), landlock: Some(6), display: Some(vec!["x11".into()]), portals: Some(vec!["document".into()]), bus: Some("private".into()) };
        let f = Facts { portals: vec![], private_bus: false, ..facts() };
        let u = unmet(&h, &f, Scope::All);
        let keys: Vec<&str> = u.iter().map(|u| u.key).collect();
        assert_eq!(keys, ["arch", "glibc", "kernel", "landlock", "display", "portals", "bus"]);
        // Each one has the three parts, from the catalog (the texts, in both languages, are checked by the tests of the catalog and of the command line).
        for x in &u {
            assert!(!x.need.0.is_empty() && !x.have.0.is_empty() && !x.todo.0.is_empty(), "{x:?}");
            assert!(crate::messages::ALL.contains(&x.need.0) && crate::messages::ALL.contains(&x.have.0) && crate::messages::ALL.contains(&x.todo.0), "{x:?}");
        }
        assert_eq!(u[1].code, ErrorCode::E2005, "glibc keeps its code");
        assert_eq!(u[2].code, ErrorCode::E2009);
        assert_eq!(u[1].need.1, [("version", "2.40".to_string())]);
        assert_eq!(u[1].have.1, [("version", "2.39".to_string())]);
        // The machine's refusal offers `--force-host`; at the start the message is about the session and offers no way to force it.
        let e = refusal("App 1.0", &u, false).to_string();
        assert!(e.contains("App 1.0") && e.contains("--force-host"), "{e}");
        let e = refusal("App 1.0", &u[4..5], true).to_string();
        assert!(e.contains("App 1.0") && !e.contains("--force-host"), "{e}");
    }

    #[test]
    fn install_looks_at_the_machine_and_the_start_at_the_session() {
        let h = Host { arch: Some("aarch64".into()), kernel: Some("9.0".into()), landlock: Some(9), display: Some(vec!["x11".into()]), portals: Some(vec!["document".into()]), bus: Some("private".into()), ..Host::default() };
        let f = Facts { displays: vec![], portals: vec![], private_bus: false, ..facts() };
        let keys = |s| unmet(&h, &f, s).iter().map(|u| u.key).collect::<Vec<_>>();
        assert_eq!(keys(Scope::Machine), ["arch", "kernel", "landlock"], "no display, portal or bus is asked of the session of an install");
        assert_eq!(keys(Scope::Start), ["landlock", "display", "portals", "bus"]);
        // The bus is told and does not stop the start: only the others refuse.
        let man: crate::Manifest = toml::from_str("[app]\nid = \"org.example.a\"\nname = \"A\"\nversion = \"1\"\nentry = \"bin/a\"\n[host]\nbus = \"private\"\n").unwrap();
        assert!(check_start(&man, &f, false).is_ok(), "no session bus: the app starts");
        assert_eq!(start_notes(&man, &f).len(), 1);
        assert!(start_notes(&man, &Facts { private_bus: true, ..f.clone() }).is_empty());
    }

    #[test]
    fn a_session_without_a_display_installs_and_does_not_start() {
        let man: crate::Manifest = toml::from_str("[app]\nid = \"org.example.a\"\nname = \"A\"\nversion = \"1\"\nentry = \"bin/a\"\n[host]\ndisplay = [\"x11\"]\nlandlock = 9\n").unwrap();
        let f = Facts { displays: vec![], ..facts() };
        let e = check_start(&man, &f, false).unwrap_err().to_string();
        assert!(e.contains("x11") && e.contains('9'), "{e}");
        // An app that was installed with --force-host is not refused for the Landlock ABI, but the display is the session's.
        let e = check_start(&man, &f, true).unwrap_err().to_string();
        assert!(e.contains("x11") && !e.contains("Landlock") && !e.contains("9"), "{e}");
        let ok = Facts { displays: vec!["x11".into()], landlock: Some(9), ..facts() };
        assert!(check_start(&man, &ok, false).is_ok());
    }

    #[test]
    fn a_system_whose_glibc_or_kernel_cannot_be_read_is_not_refused_for_it() {
        let h = Host { glibc: Some("99.1".into()), kernel: Some("99.1".into()), ..Host::default() };
        let f = Facts { glibc: None, kernel: None, ..facts() };
        assert!(unmet(&h, &f, Scope::All).is_empty());
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
