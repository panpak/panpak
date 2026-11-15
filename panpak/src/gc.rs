// SPDX-License-Identifier: MIT OR Apache-2.0
//! `panpak gc`: what no record reaches is taken out of the store. PANPAK-FORMAT.md section 6.8.
//!
//! Liveness comes from the records, never from counts: the records that `current`, `previous` or `pending` of an app name are alive; their roots keep the
//! trees and the farms (a farm lives while any record with its root lives); a blob that no tree links is dead. The order is the order of the dependencies, so
//! that a stop in the middle leaves things that the next run finishes and never a link that names nothing: records, trees, farms, blobs, then what is only a cache.
//! No clock decides anything about work in progress: every command that makes or changes what an app is made of holds the store lock shared for as long as it
//! works, and a collection takes it exclusive without waiting, so it never sees work that is half done (when it cannot, it is left for the next time).
//! Time is used only for what costs space and nothing else: the ABI cache (30 days) and the folders of abandoned downloads (7 days).
use super::*;
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, SystemTime};

/// A download folder that was not used for this long is abandoned.
const STAGING_DAYS: u64 = 7;
/// An entry of the ABI cache that was not rewritten for this long is rebuilt at the next start (it is only a cache).
const CACHE_DAYS: u64 = 30;

#[derive(Default, Debug)]
pub struct Report {
    /// (what, name) of each thing taken out (or that would be).
    pub items: Vec<(&'static str, String)>,
    pub bytes: u64,
    /// A command was working on the store: nothing was done.
    pub deferred: bool,
}

impl Report {
    fn add(&mut self, what: &'static str, name: impl Into<String>, bytes: u64) {
        self.items.push((what, name.into()));
        self.bytes += bytes;
    }
    pub fn count(&self, what: &str) -> usize {
        self.items.iter().filter(|(w, _)| *w == what).count()
    }
}

pub struct Options {
    pub dry_run: bool,
}

fn age(m: &fs::Metadata) -> Duration {
    m.modified().ok().and_then(|t| SystemTime::now().duration_since(t).ok()).unwrap_or_default()
}

fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The root and the kind of a name in `trees/`: `<root>`, `<root>.readonly`, `.lock`, `.partial`, `.damaged<pid>`. Anything else is not ours.
pub fn tree_entry(name: &str) -> Option<(&str, &str)> {
    if name.len() < 64 || !is_hex(&name[..64], 64) {
        return None;
    }
    let (root, rest) = name.split_at(64);
    let ok = matches!(rest, "" | ".readonly" | ".lock" | ".partial") || rest.strip_prefix(".damaged").is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
    ok.then_some((root, rest))
}

/// `<hash>-<raw size>`: a blob of `blobs/`.
pub fn is_blob_name(name: &str) -> bool {
    name.len() > 65 && is_hex(&name[..64], 64) && name.as_bytes()[64] == b'-' && name[65..].bytes().all(|b| b.is_ascii_digit())
}

fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

fn dir_size(p: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
            if m.is_dir() {
                stack.push(e.path());
            } else if m.nlink() == 1 {
                total += m.len();
            }
        }
    }
    total
}

struct Rec {
    path: PathBuf,
    id: String,
    root: String,
    bytes: u64,
}

/// Run the collection. `held` are the ids whose app lock the caller already holds (an apply, a removal). Locks of the other apps are taken without waiting
/// for long; an app that is busy, or that has a copy running, keeps everything it has.
pub fn run(store: &Path, opts: &Options, held: &[&str]) -> Result<Report> {
    remove::recover();
    let mut rep = Report::default();
    // Exclusive, without waiting: a command at work holds it shared.
    let Some(_store_lock) = panpak_core::applock::store_exclusive_try(store)? else {
        rep.deferred = true;
        return Ok(rep);
    };
    let apps_dir = store.join("apps");
    let ids: Vec<String> = fs::read_dir(&apps_dir)
        .map(|rd| rd.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| panpak_core::validate_app_id(n).is_ok()).collect())
        .unwrap_or_default();
    // The locks: each app, so that its links are read as one. An app that cannot be locked, or has a copy running, is kept whole.
    let mut locks = Vec::new();
    let mut busy: HashSet<String> = HashSet::new();
    for id in &ids {
        // An app whose lock the caller holds (it is applying a version of it, so no copy runs either) needs neither.
        if held.contains(&id.as_str()) {
            continue;
        }
        match panpak_core::applock::lock_for(store, id, Duration::from_millis(500)) {
            Ok(l) => locks.push(l),
            Err(_) => {
                busy.insert(id.clone());
                continue;
            }
        }
        match remove::lock_no_copies(id, "the collection") {
            Ok(l) => drop(l),
            Err(_) => {
                busy.insert(id.clone());
            }
        }
    }
    // The records.
    let packages = store.join("packages");
    let mut recs: Vec<Rec> = Vec::new();
    let mut tmps: Vec<(PathBuf, u64)> = Vec::new();
    if let Ok(rd) = fs::read_dir(&packages) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
            if !m.file_type().is_file() {
                continue;
            }
            if name.contains(".rec.tmp") {
                let pid = name.rsplit(".tmp").next().and_then(|p| p.parse::<i32>().ok());
                if !pid.is_some_and(pid_alive) {
                    tmps.push((e.path(), m.len()));
                }
                continue;
            }
            // A `.panpak` here is a package that a store from before step 8a kept: kept only while a link names it, like a record.
            if !name.ends_with(".rec") && !name.ends_with(".panpak") {
                continue;
            }
            match panpak_core::record::open_verified(&e.path()) {
                Ok(o) => recs.push(Rec { path: e.path(), id: o.manifest.app.id.clone(), root: panpak_core::hex(&o.footer.root), bytes: m.len() }),
                // One that does not verify: nothing can start from it, and no link may name it (checked below by the path).
                _ => recs.push(Rec { path: e.path(), id: String::new(), root: String::new(), bytes: m.len() }),
            }
        }
    }
    let by_path: HashMap<PathBuf, usize> = recs.iter().enumerate().map(|(i, r)| (r.path.clone(), i)).collect();
    let mut keep = vec![false; recs.len()];
    for id in &ids {
        for link in ["current", "previous", panpak_core::pending::LINK] {
            let Ok(t) = panpak_core::durable::resolve_link(&apps_dir.join(id).join(link)) else { continue };
            let Some(name) = t.file_name() else { continue };
            if let Some(&i) = by_path.get(&packages.join(name)) {
                keep[i] = true;
            }
        }
    }
    for (i, r) in recs.iter().enumerate() {
        if busy.contains(&r.id) {
            keep[i] = true;
        }
    }
    let roots: HashSet<String> = recs.iter().zip(&keep).filter(|(_, k)| **k).map(|(r, _)| r.root.clone()).collect();

    // 1. records
    for (r, k) in recs.iter().zip(&keep) {
        if !*k {
            rep.add("record", r.path.file_name().unwrap_or_default().to_string_lossy(), r.bytes);
            if !opts.dry_run {
                let _ = fs::remove_file(&r.path);
            }
        }
    }
    for (p, len) in &tmps {
        rep.add("record", p.file_name().unwrap_or_default().to_string_lossy(), *len);
        if !opts.dry_run {
            let _ = fs::remove_file(p);
        }
    }
    if !opts.dry_run {
        let _ = panpak_core::durable::fsync_dir(&packages);
    }
    panpak_core::failpoint!("gc.records_removed");

    // 2. trees
    let trees = store.join("trees");
    if let Ok(rd) = fs::read_dir(&trees) {
        let mut names: Vec<String> = rd.flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
        names.sort();
        for name in names {
            let Some((root, kind)) = tree_entry(&name) else { continue };
            let path = trees.join(&name);
            let Ok(m) = fs::symlink_metadata(&path) else { continue };
            let alive = roots.contains(root);
            // Leftovers of an interrupted make or repair go even when the tree is alive; the tree itself and its marks only when it is not.
            let leftover = kind == ".partial" || kind.starts_with(".damaged");
            if alive && !leftover {
                continue;
            }
            // The lock that whoever makes the tree holds: busy means somebody is at it.
            let lock_path = trees.join(format!("{root}.lock"));
            let guard = fs::OpenOptions::new().write(true).open(&lock_path).ok();
            if let Some(g) = &guard {
                // SAFETY: flock on a descriptor that `g` owns.
                if unsafe { libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(g), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                    continue;
                }
            }
            let bytes = if m.is_dir() { dir_size(&path) } else { m.len() };
            // The marks of a tree (`.readonly`, `.lock`) go with it and are not counted again.
            if matches!(kind, "" | ".partial") || kind.starts_with(".damaged") {
                rep.add("tree", name.clone(), bytes);
            }
            if !opts.dry_run {
                if m.is_dir() {
                    let _ = remove_store_tree(&path);
                } else {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        if !opts.dry_run {
            let _ = panpak_core::durable::fsync_dir(&trees);
        }
    }
    panpak_core::failpoint!("gc.trees_removed");

    // 3. farms: a farm lives while any record with its root lives.
    let farms = store.join("farms");
    if let Ok(rd) = fs::read_dir(&farms) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
            if !is_hex(&name, 64) || !m.is_dir() || roots.contains(&name) {
                continue;
            }
            rep.add("farm", name, 0);
            if !opts.dry_run {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
    panpak_core::failpoint!("gc.farms_removed");

    // 4. blobs: a blob that no tree links.
    let blobs = store.join("blobs");
    if let Ok(rd) = fs::read_dir(&blobs) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
            if m.file_type().is_file() && is_blob_name(&name) && m.nlink() == 1 {
                rep.add("blob", name, m.len());
                if !opts.dry_run {
                    let _ = fs::remove_file(e.path());
                }
            } else if m.is_dir() {
                // `.work-<pid>-<time>`: the work folder of a start that died.
                let pid = name.strip_prefix(".work-").and_then(|r| r.split('-').next()).and_then(|p| p.parse::<i32>().ok());
                if pid.is_some_and(|p| !pid_alive(p)) {
                    rep.add("work", name, dir_size(&e.path()));
                    if !opts.dry_run {
                        let _ = fs::remove_dir_all(e.path());
                    }
                }
            }
        }
        if !opts.dry_run {
            let _ = panpak_core::durable::fsync_dir(&blobs);
        }
    }
    panpak_core::failpoint!("gc.blobs_removed");

    // 5. what is only a cache or a leftover: the ABI cache, the download folders, the runtimes nobody names.
    cache_step(store, opts, &mut rep);
    staging_step(store, opts, &ids, &busy, &mut rep);
    runtimes_step(store, opts, &ids, &mut rep);
    drop(locks);
    Ok(rep)
}

fn cache_step(store: &Path, opts: &Options, rep: &mut Report) {
    let dir = store.join("abi-cache");
    let Ok(rd) = fs::read_dir(&dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
        if !m.file_type().is_file() || !name.ends_with(".toml") {
            continue;
        }
        // The entry names the farm that it was made for (in its stamps): when that farm is gone the entry is of no use; an old entry goes too.
        let text = fs::read_to_string(e.path()).unwrap_or_default();
        let farm_gone = text.match_indices("/farms/").any(|(i, _)| {
            let root: String = text[i + 7..].chars().take(64).collect();
            is_hex(&root, 64) && !store.join("farms").join(&root).exists()
        });
        if farm_gone || age(&m) > Duration::from_secs(CACHE_DAYS * 86400) {
            rep.add("cache", name, m.len());
            if !opts.dry_run {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

fn staging_step(store: &Path, opts: &Options, ids: &[String], busy: &HashSet<String>, rep: &mut Report) {
    let dir = store.join("staging");
    let Ok(rd) = fs::read_dir(&dir) else { return };
    for e in rd.flatten() {
        let id = e.file_name().to_string_lossy().into_owned();
        let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
        if !m.is_dir() || busy.contains(&id) || panpak_core::validate_app_id(&id).is_err() {
            continue;
        }
        let installed = ids.contains(&id);
        // An app that is not installed has no download in progress (the store lock says so): its folders go at once.
        let limit = if installed { Duration::from_secs(STAGING_DAYS * 86400) } else { Duration::ZERO };
        // One folder per version being downloaded: it is in use while something in it is written (the newest entry decides).
        for c in fs::read_dir(e.path()).into_iter().flatten().flatten() {
            let Ok(cm) = fs::symlink_metadata(c.path()) else { continue };
            if !cm.is_dir() {
                continue;
            }
            let newest = fs::read_dir(c.path()).ok().into_iter().flatten().flatten().filter_map(|x| fs::symlink_metadata(x.path()).ok()).map(|x| age(&x)).min().unwrap_or_else(|| age(&cm));
            if newest >= limit {
                rep.add("staging", format!("{id}/{}", c.file_name().to_string_lossy()), dir_size(&c.path()));
                if !opts.dry_run {
                    let _ = fs::remove_dir_all(c.path());
                }
            }
        }
        if !opts.dry_run {
            // Empty now: the folder of the app goes too (remove_dir only takes an empty one).
            let _ = fs::remove_dir(e.path());
        }
    }
}

fn runtimes_step(store: &Path, opts: &Options, ids: &[String], rep: &mut Report) {
    let dir = store.join("runtimes");
    let Ok(rd) = fs::read_dir(&dir) else { return };
    // What names a runtime: the stable link of the store, and the launchers and entries of installed apps (older ones name `runtimes/<n>/` directly).
    let mut named: HashSet<String> = HashSet::new();
    if let Ok(t) = fs::read_link(dsk::stable_stub(store)) {
        let s = t.to_string_lossy().into_owned();
        if let Some(i) = s.find("runtimes/") {
            named.insert(s[i + 9..].split('/').next().unwrap_or_default().to_string());
        }
    }
    // The best runtime is the one the next install names, whatever links say.
    if let Ok((abi, _)) = best_runtime(store) {
        named.insert(abi.to_string());
    }
    let mut files: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for id in ids {
            files.push(home.join(".local/bin").join(id));
        }
    }
    if let Some(data) = dirs::data_dir() {
        for id in ids {
            files.push(data.join("applications").join(format!("{id}.desktop")));
        }
    }
    for f in files {
        let text = fs::read_to_string(f).unwrap_or_default();
        for (i, _) in text.match_indices("runtimes/") {
            named.insert(text[i + 9..].split('/').next().unwrap_or_default().to_string());
        }
    }
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
        if !m.is_dir() || named.contains(&name) || name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        rep.add("runtime", name, dir_size(&e.path()));
        if !opts.dry_run {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

/// `panpak gc [--dry-run]`.
pub fn cmd_gc(dry_run: bool) -> Result<()> {
    let store = store_dir()?;
    ensure_code!(store.is_dir(), ErrorCode::E2004, "there is no store yet: nothing to collect");
    let rep = run(&store, &Options { dry_run }, &[])?;
    print_report(&rep, dry_run);
    Ok(())
}

fn print_report(rep: &Report, dry_run: bool) {
    let verb = if dry_run { "would remove" } else { "removed" };
    if rep.deferred {
        println!("another panpak command is working on the store (install, update, repair or remove): the collection was left for the next time");
        return;
    }
    if rep.items.is_empty() {
        println!("nothing to collect");
        return;
    }
    for what in ["record", "tree", "farm", "blob", "work", "cache", "staging", "runtime"] {
        let n = rep.count(what);
        if n > 0 {
            println!("{verb} {n} {what}(s)");
        }
    }
    if dry_run {
        println!("(the blobs that only the trees above link are counted by the real run: they are linked until the trees go)");
    }
    println!("{} {}", if dry_run { "would free about" } else { "freed about" }, humansize(rep.bytes));
}

/// After a step that changes what is alive (apply, remove): collect quietly. The caller has let go of its store lock. A failure never fails the step, and a store
/// that is busy says so and waits for the next time.
pub fn after(store: &Path, held: &[&str]) {
    match run(store, &Options { dry_run: false }, held) {
        Ok(rep) if rep.deferred => eprintln!("panpak: the collection of the store was left for the next time: another command is working on it"),
        Ok(_) => {}
        Err(e) => eprintln!("panpak: the collection of the store did not run: {}", panpak_core::err_text(&e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_of_the_store_are_told_apart() {
        let r = "ab".repeat(32);
        assert_eq!(tree_entry(&r), Some((r.as_str(), "")));
        assert_eq!(tree_entry(&format!("{r}.partial")), Some((r.as_str(), ".partial")));
        assert_eq!(tree_entry(&format!("{r}.damaged123")).map(|t| t.1), Some(".damaged123"));
        assert!(tree_entry(&format!("{r}.damaged")).is_none() && tree_entry(&format!("{r}.x")).is_none() && tree_entry("abc").is_none());
        assert!(tree_entry(&format!("{}.readonly", "AB".repeat(32))).is_none(), "upper case is not ours");
        assert!(is_blob_name(&format!("{r}-1234")) && !is_blob_name(&format!("{r}-")) && !is_blob_name(&format!("{r}-12a")) && !is_blob_name(".work-1-2"));
    }

    #[test]
    fn a_report_counts_by_kind() {
        let mut r = Report::default();
        r.add("tree", "a", 5);
        r.add("blob", "b", 7);
        r.add("blob", "c", 1);
        assert_eq!((r.count("blob"), r.count("tree"), r.bytes), (2, 1, 13));
    }
}
