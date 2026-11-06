// SPDX-License-Identifier: MIT OR Apache-2.0
//! `panpak list` and `panpak remove`. Rules: PANPAK-FORMAT.md §9.6, THREAT-MODEL.md "Removal".
//!
//! Everything below the store, the user data, the cache and the runtime folder is reached through
//! descriptors of folders (`panpak_core::safefs`): no path is built from strings after a folder
//! was opened, and a link is never followed.
use super::{confirm, desktop_is_ours, desktop_path, launcher_is_ours, legacy_desktop_path, refresh_desktop_database, store_dir};
use anyhow::{Context, Result};
use panpak_core::{
    coded, ensure_code,
    removal::{valid_package_name, Journal, JOURNAL_FILE, MAX_JOURNAL_BYTES},
    safefs::{Dir, Kind},
    ErrorCode,
};
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    io::{Read, Write},
    path::PathBuf,
};

fn os(s: &str) -> OsString {
    OsString::from(s)
}

/// The store folder, or `None` while nothing was ever installed.
struct Store {
    root: Dir,
}

impl Store {
    fn open() -> Result<Option<Store>> {
        let path = store_dir()?;
        match Dir::open_path(&path) {
            Ok(root) => Ok(Some(Store { root })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(coded(ErrorCode::E2003, format!("cannot open the store {}: {e} (a link in its place is not followed)", path.display()))),
        }
    }

    /// A folder of the store, `None` when it is not there. A link in its place is an error.
    fn sub(&self, name: &str) -> Result<Option<Dir>> {
        self.root.open_dir_opt(OsStr::new(name)).map_err(|e| coded(ErrorCode::E2003, format!("the store folder {name} cannot be used: {e} (a link in its place is not followed)")))
    }
}

/// The folder `<base>/panpak` of a base such as the cache folder, `None` when not there.
fn under(base: Option<PathBuf>, name: &str) -> Option<Dir> {
    let b = Dir::open_path(&base?).ok()?;
    b.open_dir_opt(OsStr::new(name)).ok().flatten()
}

fn cache_panpak() -> Option<Dir> {
    under(dirs::cache_dir(), "panpak")
}

fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

fn human(b: u64) -> String {
    const U: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if b < 1024 {
        return format!("{b} B");
    }
    let mut v = b as f64 / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", U[i])
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// What the manifest and footer of a package file say.
struct PkgInfo {
    id: String,
    name: String,
    version: String,
    root: String,
}

fn read_pkg(packages: &Dir, name: &OsStr) -> Option<PkgInfo> {
    let mut f = packages.open_file(name).ok()?;
    let ft = panpak_core::read_footer(&mut f).ok()?;
    let (m, _) = panpak_core::verify(&mut f, &ft).ok()?;
    Some(PkgInfo { id: m.app.id, name: m.app.name, version: m.app.version, root: panpak_core::hex(&ft.root) })
}

fn package_names(packages: &Option<Dir>) -> Vec<String> {
    let Some(p) = packages else { return Vec::new() };
    let mut v: Vec<String> = p.list().unwrap_or_default().into_iter().filter_map(|n| n.into_string().ok()).filter(|n| valid_package_name(n)).collect();
    v.sort();
    v
}

/// Everything known about one app id.
struct Facts {
    id: String,
    in_apps: bool,
    problems: Vec<String>,
    name: Option<String>,
    version: Option<String>,
    packages: Vec<String>,
    package_bytes: u64,
    roots: Vec<String>,
    tree_bytes: u64,
    data_bytes: Option<u64>,
    cache_bytes: Option<u64>,
    has_launcher: bool,
    has_desktop: bool,
}

impl Facts {
    fn status(&self) -> &'static str {
        if !self.in_apps && self.packages.is_empty() {
            "data-only"
        } else if self.problems.is_empty() {
            "ok"
        } else {
            "incomplete"
        }
    }
    fn found_anything(&self) -> bool {
        self.in_apps || !self.packages.is_empty() || self.data_bytes.is_some() || self.cache_bytes.is_some() || self.has_launcher || self.has_desktop
    }
}

struct Ctx {
    store: Option<Store>,
    apps: Option<Dir>,
    packages: Option<Dir>,
    trees: Option<Dir>,
    farms: Option<Dir>,
    userdata: Option<Dir>,
    cache: Option<Dir>,
}

impl Ctx {
    fn open() -> Result<Ctx> {
        let store = Store::open()?;
        let (apps, packages, trees, farms, userdata) = match &store {
            Some(s) => (s.sub("apps")?, s.sub("packages")?, s.sub("trees")?, s.sub("farms")?, s.sub("userdata")?),
            None => (None, None, None, None, None),
        };
        Ok(Ctx { store, apps, packages, trees, farms, userdata, cache: cache_panpak() })
    }

    fn app_ids(&self) -> Vec<String> {
        let mut ids: HashSet<String> = HashSet::new();
        for d in [&self.apps, &self.userdata].into_iter().flatten() {
            for n in d.list().unwrap_or_default() {
                if let Ok(s) = n.into_string() {
                    // `<id>.in-use` beside the data folder is the mark "in use", not an app.
                    if panpak_core::validate_app_id(&s).is_ok() && !s.ends_with(".in-use") {
                        ids.insert(s);
                    }
                }
            }
        }
        let mut v: Vec<String> = ids.into_iter().collect();
        v.sort();
        v
    }

    fn facts(&self, id: &str, all_ids: &[String]) -> Facts {
        let mut f = Facts {
            id: id.to_string(),
            in_apps: false,
            problems: Vec::new(),
            name: None,
            version: None,
            packages: Vec::new(),
            package_bytes: 0,
            roots: Vec::new(),
            tree_bytes: 0,
            data_bytes: None,
            cache_bytes: None,
            has_launcher: false,
            has_desktop: false,
        };
        let appdir = self.apps.as_ref().and_then(|a| match a.lstat(OsStr::new(id)) {
            Ok(Some(st)) => Some((a, st)),
            _ => None,
        });
        let mut current_pkg: Option<String> = None;
        if let Some((apps, st)) = appdir {
            f.in_apps = true;
            if st.kind != Kind::Dir {
                f.problems.push("its store folder is a link or a file".into());
            } else if let Ok(d) = apps.open_dir(OsStr::new(id)) {
                match d.lstat(OsStr::new("current")) {
                    Ok(Some(c)) if c.kind == Kind::Link => {
                        current_pkg = d.read_link(OsStr::new("current")).ok().and_then(|t| std::path::Path::new(&t).file_name().map(|n| n.to_string_lossy().into_owned()));
                    }
                    Ok(Some(_)) => f.problems.push("`current` is not a link".into()),
                    _ => f.problems.push("no `current` link".into()),
                }
                if !matches!(d.lstat(OsStr::new("pinned-key")), Ok(Some(_))) {
                    f.problems.push("no key pin".into());
                }
            } else {
                f.problems.push("its store folder cannot be opened".into());
            }
        }
        // Packages of the app: by file name, then by what their manifest says.
        if let Some(p) = &self.packages {
            let prefix = format!("{id}-");
            for n in package_names(&self.packages).into_iter().filter(|n| n.starts_with(&prefix)) {
                let info = read_pkg(p, OsStr::new(&n));
                let ours = match &info {
                    Some(i) => i.id == id,
                    None => !all_ids.iter().any(|o| o.len() > id.len() && n.starts_with(&format!("{o}-"))),
                };
                if !ours {
                    continue;
                }
                if let Ok(Some(st)) = p.lstat(OsStr::new(&n)) {
                    f.package_bytes += st.blocks * 512;
                }
                if let Some(i) = info {
                    if current_pkg.as_deref() == Some(n.as_str()) {
                        f.name = Some(i.name.clone());
                        f.version = Some(i.version.clone());
                    }
                    if !f.roots.contains(&i.root) {
                        f.roots.push(i.root);
                    }
                } else {
                    f.problems.push(format!("package {n} is unreadable"));
                }
                f.packages.push(n);
            }
        }
        if f.in_apps {
            match &current_pkg {
                Some(c) if f.packages.contains(c) => {}
                Some(c) => f.problems.push(format!("the package {c} is missing")),
                None => {}
            }
            for r in &f.roots {
                if let Some(t) = &self.trees {
                    f.tree_bytes += t.disk_usage(OsStr::new(r)).unwrap_or(0);
                }
                if let Some(t) = &self.farms {
                    f.tree_bytes += t.disk_usage(OsStr::new(r)).unwrap_or(0);
                }
            }
        }
        f.data_bytes = self.userdata.as_ref().and_then(|d| d.lstat(OsStr::new(id)).ok().flatten().map(|_| d.disk_usage(OsStr::new(id)).unwrap_or(0)));
        f.cache_bytes = self.cache.as_ref().and_then(|d| d.lstat(OsStr::new(id)).ok().flatten().map(|_| d.disk_usage(OsStr::new(id)).unwrap_or(0)));
        if let Some(bin) = dirs::home_dir().map(|h| h.join(".local/bin")).and_then(|p| Dir::open_path(&p).ok()) {
            f.has_launcher = matches!(bin.lstat(OsStr::new(id)), Ok(Some(_)));
        }
        if let Some(apps_dir) = dirs::data_dir().map(|d| d.join("applications")).and_then(|p| Dir::open_path(&p).ok()) {
            f.has_desktop = matches!(apps_dir.lstat(&os(&format!("panpak.{id}.desktop"))), Ok(Some(_)));
        }
        if f.in_apps || !f.packages.is_empty() {
            if !f.has_launcher {
                f.problems.push("no launcher".into());
            }
            if !f.has_desktop {
                f.problems.push("no desktop entry".into());
            }
        }
        f
    }
}

/// `panpak list [--json]`.
pub fn cmd_list(json: bool) -> Result<()> {
    let ctx = Ctx::open()?;
    recover_removals(&ctx);
    let ids = ctx.app_ids();
    let facts: Vec<Facts> = ids.iter().map(|id| ctx.facts(id, &ids)).collect();
    if json {
        let mut out = String::from("[");
        for (i, f) in facts.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let opt = |v: Option<u64>| v.map_or("null".to_string(), |n| n.to_string());
            let strs = |v: &[String]| format!("[{}]", v.iter().map(|s| json_str(s)).collect::<Vec<_>>().join(","));
            out.push_str(&format!(
                "\n  {{\"id\":{},\"name\":{},\"version\":{},\"status\":{},\"problems\":{},\"package_bytes\":{},\"tree_bytes\":{},\"data_bytes\":{},\"cache_bytes\":{},\"packages\":{}}}",
                json_str(&f.id),
                f.name.as_deref().map_or("null".into(), json_str),
                f.version.as_deref().map_or("null".into(), json_str),
                json_str(f.status()),
                strs(&f.problems),
                f.package_bytes,
                f.tree_bytes,
                opt(f.data_bytes),
                opt(f.cache_bytes),
                strs(&f.packages),
            ));
        }
        out.push_str(if facts.is_empty() { "]\n" } else { "\n]\n" });
        print!("{out}");
        return Ok(());
    }
    if facts.is_empty() {
        println!("no apps installed");
        return Ok(());
    }
    let rows: Vec<[String; 6]> = facts
        .iter()
        .map(|f| {
            let status = if f.problems.is_empty() { f.status().to_string() } else { format!("{}: {}", f.status(), f.problems.join(", ")) };
            let data = f.data_bytes.unwrap_or(0) + f.cache_bytes.unwrap_or(0);
            [
                clean(&f.id),
                clean(f.name.as_deref().unwrap_or("-")),
                clean(f.version.as_deref().unwrap_or("-")),
                if f.in_apps || !f.packages.is_empty() { human(f.package_bytes + f.tree_bytes) } else { "-".into() },
                if f.data_bytes.is_some() || f.cache_bytes.is_some() { human(data) } else { "-".into() },
                clean(&status),
            ]
        })
        .collect();
    let head = ["ID", "NAME", "VERSION", "APP", "DATA", "STATUS"];
    let mut w = head.map(str::len);
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(c.chars().count());
        }
    }
    let line = |r: &[String]| {
        let cells: Vec<String> = r.iter().enumerate().map(|(i, c)| if i == 5 { c.clone() } else { format!("{c:<width$}", width = w[i]) }).collect();
        println!("{}", cells.join("  "));
    };
    line(&head.map(String::from));
    for r in &rows {
        line(r);
    }
    println!("APP is the packages and the unpacked trees on disk (a tree shared by two apps is counted in both); DATA is the settings, data and cache of the app.");
    Ok(())
}

fn read_text(d: &Dir, name: &OsStr) -> Option<String> {
    let mut f = d.open_file(name).ok()?;
    let mut buf = Vec::new();
    (&mut f).take(1 << 20).read_to_end(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

/// Remove the launcher and the desktop entries panpak wrote for the app. Returns what was removed and what stayed.
fn remove_integration(id: &str, removed: &mut Vec<String>, kept: &mut Vec<String>) -> Result<()> {
    let launcher_path = dirs::home_dir().context("no home dir")?.join(".local/bin").join(id);
    if let Ok(bin) = Dir::open_path(launcher_path.parent().context("no parent")?) {
        if let Ok(Some(st)) = bin.lstat(OsStr::new(id)) {
            let ours = st.kind == Kind::File && read_text(&bin, OsStr::new(id)).is_some_and(|t| launcher_is_ours(&t, id));
            if ours {
                bin.unlink(OsStr::new(id)).with_context(|| format!("remove {}", launcher_path.display()))?;
                removed.push(format!("launcher {}", launcher_path.display()));
            } else {
                kept.push(format!("{} (not a launcher written by panpak, or a link: not touched)", launcher_path.display()));
            }
        }
    }
    let mut touched = false;
    for path in [desktop_path(id)?, legacy_desktop_path(id)?] {
        let Some(dir) = path.parent().and_then(|p| Dir::open_path(p).ok()) else { continue };
        let name = path.file_name().context("no file name")?.to_os_string();
        if let Ok(Some(st)) = dir.lstat(&name) {
            let ours = st.kind == Kind::File && read_text(&dir, &name).is_some_and(|t| desktop_is_ours(&t, id, &launcher_path));
            if ours {
                dir.unlink(&name).with_context(|| format!("remove {}", path.display()))?;
                removed.push(format!("desktop entry {}", path.display()));
                touched = true;
            } else {
                kept.push(format!("{} (not written by panpak, or a link: not touched)", path.display()));
            }
        }
    }
    if touched {
        if let Some(d) = desktop_path(id)?.parent() {
            refresh_desktop_database(d);
        }
    }
    Ok(())
}

/// The roots of the trees that the packages left in the store use.
fn roots_in_use(packages: &Option<Dir>) -> HashSet<String> {
    let mut set = HashSet::new();
    if let Some(p) = packages {
        for n in package_names(packages) {
            if let Ok(mut f) = p.open_file(OsStr::new(&n)) {
                if let Ok(ft) = panpak_core::read_footer(&mut f) {
                    set.insert(panpak_core::hex(&ft.root));
                }
            }
        }
    }
    set
}

/// Delete what the journal lists from the shared parts of the store.
fn finish_journal(ctx: &Ctx, j: &Journal, out: &mut Vec<String>) -> Result<()> {
    let mut failures: Vec<String> = Vec::new();
    if let Some(p) = &ctx.packages {
        for n in &j.packages {
            match p.lstat(OsStr::new(n)) {
                Ok(Some(st)) if st.kind != Kind::Dir => match p.unlink(OsStr::new(n)) {
                    Ok(()) => out.push(format!("package {n}")),
                    Err(e) => failures.push(format!("{n}: {e}")),
                },
                _ => {}
            }
        }
    }
    let used = roots_in_use(&ctx.packages);
    for r in &j.roots {
        if used.contains(r) {
            out.push(format!("tree {} stays (another package uses it)", &r[..12]));
            continue;
        }
        let mut gone = false;
        for (dir, name) in [(&ctx.trees, r.clone()), (&ctx.trees, format!("{r}.readonly")), (&ctx.farms, r.clone())] {
            if let Some(d) = dir {
                match d.remove_all(OsStr::new(&name)) {
                    Ok(rm) => {
                        gone |= rm.entries > 0;
                        failures.extend(rm.failures);
                    }
                    Err(e) => failures.push(format!("{name}: {e}")),
                }
            }
        }
        if gone {
            out.push(format!("tree {}", &r[..12]));
        }
    }
    ensure_code!(failures.is_empty(), ErrorCode::E2003, "part of the removal of {} could not be finished: {}", j.app_id, failures.join("; "));
    Ok(())
}

/// Finish the removals that were interrupted: every entry of `removing/` is worked through from its journal.
/// Problems are reported on stderr and never stop the command that called this.
fn recover_removals(ctx: &Ctx) {
    let Some(store) = &ctx.store else { return };
    let Ok(Some(removing)) = store.sub("removing") else { return };
    for entry in removing.list().unwrap_or_default() {
        let dir = match removing.open_dir(&entry) {
            Ok(d) => d,
            Err(_) => {
                // Not a folder: a stray file or a link. Remove it itself.
                let _ = removing.remove_all(&entry);
                continue;
            }
        };
        // Another panpak is working on it.
        if !dir.try_lock().unwrap_or(false) {
            continue;
        }
        let has_app = dir.lstat(OsStr::new("app")).ok().flatten().is_some();
        let journal = read_text_bytes(&dir, JOURNAL_FILE).or_else(|| if has_app { read_text_bytes(&dir, "journal.new") } else { None });
        let mut done = Vec::new();
        let result = match journal.as_deref().map(Journal::parse) {
            Some(Ok(j)) => finish_journal(ctx, &j, &mut done).map(|()| Some(j.app_id)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        };
        match result {
            Ok(id) => {
                drop(dir);
                match removing.remove_all(&entry) {
                    Ok(r) if r.failed == 0 => {
                        if let Some(id) = id {
                            eprintln!("panpak: finished an interrupted removal of {id}");
                        }
                    }
                    Ok(r) => eprintln!("panpak: could not delete {:?} in removing/: {}", entry, r.failures.join("; ")),
                    Err(e) => eprintln!("panpak: could not delete {:?} in removing/: {e}", entry),
                }
            }
            Err(e) => eprintln!("panpak: could not finish an interrupted removal ({}): {e:#}", clean(&entry.to_string_lossy())),
        }
    }
}

fn read_text_bytes(d: &Dir, name: &str) -> Option<Vec<u8>> {
    let mut f = d.open_file(OsStr::new(name)).ok()?;
    let mut buf = Vec::new();
    (&mut f).take(MAX_JOURNAL_BYTES as u64 + 1).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Called before the commands that use the store: finish removals that were interrupted.
pub fn recover() {
    if let Ok(ctx) = Ctx::open() {
        recover_removals(&ctx);
    }
}

fn unique_entry(removing: &Dir, id: &str) -> Result<OsString> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    for n in 0..100u32 {
        let name = os(&format!("{id}.{:x}{:x}{n:x}", std::process::id(), nanos));
        if removing.lstat(&name)?.is_none() {
            return Ok(name);
        }
    }
    Err(coded(ErrorCode::E2003, "cannot find a free name in removing/"))
}

/// Test builds only: kill this process at a named point, to test that an interrupted removal is finished later.
#[cfg(debug_assertions)]
fn test_kill(point: &str) {
    if std::env::var("PANPAK_TEST_KILL_AT").is_ok_and(|v| v == point) {
        // SAFETY: raising SIGKILL on ourselves.
        unsafe { libc::raise(libc::SIGKILL) };
    }
}
#[cfg(not(debug_assertions))]
fn test_kill(_point: &str) {}

/// `panpak remove [--purge-data] [--yes] <app-id>`.
pub fn cmd_remove(id: &str, purge: bool, yes: bool) -> Result<()> {
    panpak_core::validate_app_id(id)?;
    let ctx = Ctx::open()?;
    recover_removals(&ctx);

    // No copy of the app may be alive: take the lock the copies hold. It stays ours until the end.
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let Some(runtime) = runtime else {
        return Err(coded(ErrorCode::E2002, format!("XDG_RUNTIME_DIR is not set: cannot tell whether a copy of {id} is running, so nothing was removed")));
    };
    let rt_panpak = Dir::open_path(&runtime).and_then(|d| d.create_dir(OsStr::new("panpak"))).map_err(|e| coded(ErrorCode::E2002, format!("cannot use {}: {e}", runtime.display())))?;
    let rt_app = rt_panpak.create_dir(OsStr::new(id)).map_err(|e| coded(ErrorCode::E2002, format!("cannot use the runtime folder of {id}: {e}")))?;
    let _lock = match rt_app.lock_file(OsStr::new("lock")) {
        Ok(Some(f)) => f,
        Ok(None) => return Err(coded(ErrorCode::E2002, format!("a copy of {id} is running (or another remove is working on it): close it first. Nothing was removed"))),
        Err(e) => return Err(coded(ErrorCode::E2002, format!("cannot take the lock of {id}: {e}"))),
    };

    let ids = ctx.app_ids();
    let facts = ctx.facts(id, &ids);
    ensure_code!(facts.found_anything(), ErrorCode::E2004, "nothing is installed or left over under the id {id}");

    let dirs = panpak_core::app_dirs(id)?;
    if purge {
        println!("This deletes the settings, data and cache of {id}:");
        for (p, b) in [(&dirs.root, facts.data_bytes), (&dirs.cache, facts.cache_bytes)] {
            if let Some(b) = b {
                println!("  {} ({})", p.display(), human(b));
            }
        }
        if facts.data_bytes.is_none() && facts.cache_bytes.is_none() {
            println!("  (there are none)");
        }
        confirm("Delete them together with the app?", yes, ErrorCode::E1003, "deletion")?;
    }

    let mut removed: Vec<String> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();

    // 1. What the desktop knows of the app.
    remove_integration(id, &mut removed, &mut kept)?;

    // 2. The folder of the app in the store, moved aside by one rename; the journal says what is left to do.
    if let (Some(store), true) = (&ctx.store, facts.in_apps || !facts.packages.is_empty()) {
        let removing = store.root.create_dir(OsStr::new("removing")).map_err(|e| coded(ErrorCode::E2003, format!("cannot make removing/: {e}")))?;
        let name = unique_entry(&removing, id)?;
        let entry = removing.create_dir(&name)?;
        entry.try_lock().ok();
        let journal = Journal { app_id: id.to_string(), packages: facts.packages.clone(), roots: facts.roots.clone() };
        {
            let mut f = entry.create_file(OsStr::new("journal.new"))?;
            f.write_all(journal.render().as_bytes())?;
            f.sync_all()?;
        }
        test_kill("journal");
        if let Some(apps) = &ctx.apps {
            if apps.lstat(OsStr::new(id))?.is_some() {
                apps.rename_to(OsStr::new(id), &entry, OsStr::new("app")).map_err(|e| coded(ErrorCode::E2003, format!("cannot move the store folder of {id} aside: {e}")))?;
                removed.push(format!("store folder apps/{id}"));
            }
        }
        entry.rename_to(OsStr::new("journal.new"), &entry, OsStr::new(JOURNAL_FILE))?;
        // From here an interruption is finished by the next panpak command.
        test_kill("moved");
        let mut done = Vec::new();
        match finish_journal(&ctx, &journal, &mut done) {
            Ok(()) => {
                removed.extend(done);
                match removing.remove_all(&name) {
                    Ok(r) if r.failed == 0 => {}
                    Ok(r) => problems.extend(r.failures),
                    Err(e) => problems.push(e.to_string()),
                }
            }
            Err(e) => problems.push(format!("{e:#}")),
        }
    }

    // 3. Settings, data and cache: kept, or deleted on request.
    let stale_roots = panpak_core::stalelocks::roots_of(&dirs);
    let marker = panpak_core::stalelocks::in_use_marker(&dirs);
    let marker_name = marker.file_name().map(|n| n.to_os_string());
    let mut data_kept: Vec<String> = Vec::new();
    for (label, base, expected) in [("data", &ctx.userdata, facts.data_bytes), ("cache", &ctx.cache, facts.cache_bytes)] {
        let Some(base) = base else { continue };
        let Some(bytes) = expected else { continue };
        let path = if label == "data" { dirs.root.clone() } else { dirs.cache.clone() };
        if purge {
            match base.lstat(OsStr::new(id)) {
                Ok(Some(st)) if st.kind != Kind::Dir => problems.push(format!("{} is a link or a file: not followed and not removed (remove it yourself)", path.display())),
                Ok(Some(_)) => match base.remove_all(OsStr::new(id)) {
                    Ok(r) if r.failed == 0 => removed.push(format!("{label} {} ({})", path.display(), human(bytes))),
                    Ok(r) => problems.push(format!("{}: {}", path.display(), r.failures.join("; "))),
                    Err(e) => problems.push(format!("{}: {e}", path.display())),
                },
                _ => {}
            }
        } else {
            data_kept.push(format!("{label}: {} ({})", path.display(), human(bytes)));
        }
    }
    if let Some(m) = marker_name {
        if let Some(ud) = &ctx.userdata {
            if matches!(ud.lstat(&m), Ok(Some(_))) {
                // The last run did not end normally: its stale lock files are cleaned now, because the mark that
                // told the next start to search for them goes away with the app.
                if !purge {
                    let roots: Vec<&std::path::Path> = stale_roots.iter().map(PathBuf::as_path).collect();
                    for g in panpak_core::stalelocks::remove(&roots) {
                        removed.push(format!("stale lock file {}", g.display()));
                    }
                }
                if let Err(e) = ud.unlink(&m) {
                    problems.push(format!("{}: {e}", marker.display()));
                } else {
                    removed.push("the \"in use\" mark".to_string());
                }
            }
        }
    }

    // 4. The shared /tmp and /dev/shm of the app in the runtime folder (no copy is alive: we hold the lock).
    match rt_panpak.remove_all(OsStr::new(id)) {
        Ok(r) if r.failed == 0 => {}
        Ok(r) => problems.extend(r.failures),
        Err(e) => problems.push(e.to_string()),
    }

    println!("removed {id}:");
    if removed.is_empty() {
        println!("  (nothing was left of the app itself)");
    }
    for r in &removed {
        println!("  {r}");
    }
    for k in &kept {
        println!("kept: {k}");
    }
    if !data_kept.is_empty() {
        println!("kept (use `panpak remove --purge-data {id}` or `panpak data clear {id}` to delete):");
        for d in &data_kept {
            println!("  {d}");
        }
    }
    println!("the runtime and the shared blobs stay.");
    ensure_code!(problems.is_empty(), ErrorCode::E2003, "the removal of {id} is not complete: {}", problems.join("; "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_str("a\"b\\c\n\u{1}"), "\"a\\\"b\\\\c\\n\\u0001\"");
        assert_eq!(json_str("é"), "\"é\"");
    }

    #[test]
    fn sizes_are_readable() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1023), "1023 B");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(human(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn control_characters_do_not_reach_the_terminal() {
        assert_eq!(clean("a\u{1b}[31mb"), "a?[31mb");
    }
}
