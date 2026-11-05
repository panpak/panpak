// SPDX-License-Identifier: MIT OR Apache-2.0
//! ABI-resolution-only cache in the trusted store (never trust/signature/sandbox).
//!
//! The key of an entry is made of: the package (its verified root and its `[abi]` fields), the
//! stub that runs (path, inode, size, change time), the kernel release and the system library
//! cache `/etc/ld.so.cache` (inode, size, change time). The entry also stores the identity (device,
//! inode, size, modification and change time) of every file the decision depends on: the files read,
//! the system copies of the libraries the package needs, also those not chosen, and the loader.
//! A hit costs the key and one `stat` per file, and no other process: the hit path never runs
//! `ldconfig` and never reads a library.
use anyhow::{Context, Result};
use panpak_core::{abi, Manifest};
use std::{collections::HashSet, fs, os::unix::fs::MetadataExt, path::{Path, PathBuf}};

const MAX_CACHE: u64 = 4 << 20;

/// What identifies a file: where it is and what it was when the decision was made.
#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    path: PathBuf,
    dev: u64,
    inode: u64,
    len: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}

impl Stamp {
    /// Follows symbolic links: a link moved to another file changes the identity.
    fn read(path: &Path) -> Result<Self> {
        let m = fs::metadata(path)?;
        Ok(Self { path: path.into(), dev: m.dev(), inode: m.ino(), len: m.len(),
            mtime: m.mtime(), mtime_ns: m.mtime_nsec(), ctime: m.ctime(), ctime_ns: m.ctime_nsec() })
    }
}

struct Cached {
    key: String,
    loader: Option<PathBuf>,
    ld_path: Vec<PathBuf>,
    stamps: Vec<Stamp>,
}

/// The entry as text, one line per item (a text format that is read in microseconds, where TOML
/// took tens of milliseconds for the thousand files of a large app). None if a path cannot be written
/// on one line.
fn encode(c: &Cached) -> Option<String> {
    let one_line = |p: &Path| p.to_str().filter(|t| !t.contains('\n')).map(str::to_string);
    let mut out = format!("panpak-abi-cache 1\nkey {}\n", c.key);
    match &c.loader {
        Some(l) => out += &format!("loader {}\n", one_line(l)?),
        None => out += "loader -\n",
    }
    for d in &c.ld_path {
        out += &format!("ld {}\n", one_line(d)?);
    }
    for s in &c.stamps {
        out += &format!("s {} {} {} {} {} {} {} {}\n", s.dev, s.inode, s.len, s.mtime, s.mtime_ns, s.ctime, s.ctime_ns, one_line(&s.path)?);
    }
    Some(out)
}

fn decode(text: &str) -> Option<Cached> {
    let mut lines = text.lines();
    if lines.next()? != "panpak-abi-cache 1" {
        return None;
    }
    let key = lines.next()?.strip_prefix("key ")?.to_string();
    let loader = match lines.next()?.strip_prefix("loader ")? {
        "-" => None,
        l => Some(PathBuf::from(l)),
    };
    let (mut ld_path, mut stamps) = (Vec::new(), Vec::new());
    for line in lines {
        if let Some(d) = line.strip_prefix("ld ") {
            ld_path.push(PathBuf::from(d));
            continue;
        }
        let mut f = line.strip_prefix("s ")?.splitn(8, ' ');
        let mut n = || f.next()?.parse::<i64>().ok();
        let (dev, inode, len, mtime, mtime_ns, ctime, ctime_ns) = (n()?, n()?, n()?, n()?, n()?, n()?, n()?);
        stamps.push(Stamp {
            path: PathBuf::from(f.next()?),
            dev: dev as u64,
            inode: inode as u64,
            len: len as u64,
            mtime,
            mtime_ns,
            ctime,
            ctime_ns,
        });
    }
    Some(Cached { key, loader, ld_path, stamps })
}

/// The identity of a file as text, or "none" if it is not there.
fn identity(p: &Path) -> String {
    match fs::metadata(p) {
        Ok(m) => format!("{}:{}:{}:{}.{}", m.dev(), m.ino(), m.len(), m.ctime(), m.ctime_nsec()),
        Err(_) => "none".to_string(),
    }
}

/// Everything the key is made of; the pieces are passed in so that a test can change one.
struct KeyParts<'a> {
    root: &'a str,
    abi_toml: &'a str,
    entry: &'a str,
    stub: &'a str,
    kernel: &'a [u8],
    ld_cache: &'a str,
}

fn cache_key(p: &KeyParts) -> String {
    let mut h = blake3::Hasher::new();
    for part in [p.root.as_bytes(), p.abi_toml.as_bytes(), p.entry.as_bytes(), p.stub.as_bytes(), p.kernel, p.ld_cache.as_bytes()] {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part);
    }
    h.finalize().to_hex().to_string()
}

fn load(path: &Path, key: &str) -> Option<abi::Plan> {
    if fs::metadata(path).ok()?.len() > MAX_CACHE { return None; }
    let cached = decode(&fs::read_to_string(path).ok()?)?;
    if cached.key != key || cached.stamps.is_empty() { return None; }
    for stamp in &cached.stamps {
        if Stamp::read(&stamp.path).ok().as_ref() != Some(stamp) { return None; }
    }
    Some(abi::Plan { loader: cached.loader, ld_path: cached.ld_path })
}

/// The system library inventory: `ldconfig -p`, or None if it is missing or empty (musl, minimal
/// systems): then nothing is cached, and the scan of the library folders stays fresh.
fn host_inventory() -> Option<std::collections::HashMap<String, PathBuf>> {
    let map = abi::parse_ldconfig(&abi::ldconfig_output()?);
    (!map.is_empty()).then_some(map)
}

pub fn resolve(store: &Path, _package: &Path, root: &str, tree: &Path, man: &Manifest) -> Result<abi::Plan> {
    let kernel = fs::read("/proc/sys/kernel/osrelease")?;
    let stub = std::env::current_exe().map(|p| identity(&p)).unwrap_or_default();
    resolve_in(store, root, tree, man, &kernel, &stub, &identity(Path::new("/etc/ld.so.cache")), host_inventory)
}

/// `resolve` with the identity of the stub, the kernel, the library cache and the inventory of the
/// system libraries given by the caller.
#[allow(clippy::too_many_arguments)]
fn resolve_in(
    store: &Path,
    root: &str,
    tree: &Path,
    man: &Manifest,
    kernel: &[u8],
    stub: &str,
    ld_cache: &str,
    inventory: impl Fn() -> Option<std::collections::HashMap<String, PathBuf>>,
) -> Result<abi::Plan> {
    let abi_toml = man.abi.as_ref().map(|a| toml::to_string(a).unwrap_or_default()).unwrap_or_default();
    let key = std::env::var_os("PANPAK_NO_ABI_CACHE").is_none().then(|| {
        cache_key(&KeyParts { root, abi_toml: &abi_toml, entry: &man.app.entry, stub, kernel, ld_cache })
    });
    let cache_dir = store.join("abi-cache");
    let cache_path = key.as_ref().map(|key| cache_dir.join(format!("{key}.toml")));
    let checked = std::time::Instant::now();
    if let (Some(key), Some(path)) = (&key, &cache_path) {
        if let Some(plan) = load(path, key) {
            trace(&format!("hit (key and files checked in {} us)", checked.elapsed().as_micros()));
            return Ok(plan);
        }
    }
    trace("miss");
    let a = man.abi.as_ref();
    let mut needs = a.and_then(|v| v.needs.clone()).unwrap_or_default();
    let prefer = a.and_then(|v| v.prefer_host.clone()).unwrap_or_default();
    let allowed = a.and_then(|v| v.allow_missing.clone()).unwrap_or_default();
    let bundled = abi::bundled_libs(tree);
    let inv = inventory();
    // Without a reliable host inventory nothing is cached.
    let cacheable = inv.is_some();
    let host = inv.unwrap_or_else(abi::host_libs);
    let key = key.filter(|_| cacheable);
    let cache_path = cache_path.filter(|_| cacheable);
    let started = std::time::Instant::now();
    let mut files = abi::InfoCache::default();
    // The needs grow to the closure of DT_NEEDED of what is decided.
    let mut seen: HashSet<String> = needs.iter().cloned().collect();
    loop {
        let decisions = abi::decide(&needs, &prefer, &allowed, &host, &bundled)?;
        let paths: Vec<PathBuf> = decisions.iter().map(|(_, s)| abi::source_path(tree, s)).collect();
        files.prefetch(&paths);
        let mut extra = Vec::new();
        for path in &paths {
            let info = files.get(path).with_context(|| format!("DT_NEEDED of {}", path.display()))?;
            for name in &info.needs {
                if seen.insert(name.clone()) { extra.push(name.clone()); }
            }
        }
        if extra.is_empty() { break; }
        needs.extend(extra);
    }
    // The rules again with the C++ runtime choice, then the symbol check: a library whose other copy
    // defines a symbol that nothing defines changes source, with its family, until a fixed point.
    // The result is cached below, so this runs once per package, kernel and library inventory.
    let rules = std::time::Instant::now();
    let _ = abi::decide(&needs, &prefer, &allowed, &host, &bundled)?;
    let rules_ms = rules.elapsed().as_millis();
    let resolution = abi::resolve_consistent(&mut files, tree, &man.app.entry, &needs, &prefer, &allowed, &host, &bundled)?;
    trace(&format!(
        "resolved in {} ms (the rules alone: {rules_ms} ms), {} steps, {} libraries changed",
        started.elapsed().as_millis(),
        resolution.steps,
        resolution.changed.len()
    ));
    let decisions = &resolution.decisions;
    if panpak_core::debug_enabled() && !resolution.changed.is_empty() {
        eprintln!("panpak: libraries taken from their other copy so that every symbol is defined: {}", resolution.changed.join(", "));
    }
    if panpak_core::debug_enabled() && !resolution.diagnosis.missing.is_empty() {
        let d = &resolution.diagnosis;
        eprintln!(
            "panpak: WARNING: {} symbols that the app or its libraries need are defined by none of the chosen libraries; the app will fail if it uses them. `panpak doctor <package>` lists them",
            d.missing.len()
        );
        for (file, sym) in d.missing.iter().take(5) {
            eprintln!("panpak:   {file}: {sym}");
        }
    }
    let plan = abi::apply(store, root, tree, decisions, &bundled)?;
    if let (Some(key), Some(path)) = (key, cache_path) {
        // Every file the decision depends on, plus the farm links and the folders of the plan:
        // a replaced or deleted link, or a system library updated in place, is a miss.
        // The files of the tree cannot change (it is read-only and named by its root), so only the system
        // files count, with the tree and the farm folder (a replaced or deleted link changes the folder).
        let mut paths: HashSet<PathBuf> = resolution.inputs.iter().filter(|p| !p.starts_with(tree)).cloned().collect();
        paths.insert(tree.to_path_buf());
        paths.insert(store.join("farms").join(root));
        for (_, source) in decisions {
            let p = abi::source_path(tree, source);
            if !p.starts_with(tree) { paths.insert(p); }
        }
        if let Some(loader) = &plan.loader { paths.insert(loader.clone()); }
        let mut list: Vec<&PathBuf> = paths.iter().collect();
        list.sort();
        // A file that cannot be read now (a system library that is listed but gone) is left out:
        // it is not a reason to skip the cache.
        let stamps: Vec<Stamp> = list.into_iter().filter_map(|p| Stamp::read(p).ok()).collect();
        let record = Cached { key, loader: plan.loader.clone(), ld_path: plan.ld_path.clone(), stamps };
        // Optional cache: full/RO disk must not block a valid resolution.
        if let Some(text) = encode(&record) {
            let _ = (|| -> Result<()> {
                fs::create_dir_all(&cache_dir)?;
                let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
                fs::write(&tmp, text)?;
                fs::rename(tmp, path)?;
                Ok(())
            })();
        }
    }
    Ok(plan)
}

fn trace(state: &str) {
    if std::env::var_os("PANPAK_ABI_TRACE").is_some() { eprintln!("panpak: ABI cache {state}"); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, process::Command};

    fn parts<'a>(root: &'a str, kernel: &'a [u8], stub: &'a str, ld_cache: &'a str) -> KeyParts<'a> {
        KeyParts { root, abi_toml: "needs=[\"a\"]", entry: "bin/app", stub, kernel, ld_cache }
    }

    #[test]
    fn the_key_follows_every_part_of_it() {
        let base = cache_key(&parts("r1", b"k1", "s1", "c1"));
        assert_eq!(base, cache_key(&parts("r1", b"k1", "s1", "c1")));
        for other in [parts("r2", b"k1", "s1", "c1"), parts("r1", b"k2", "s1", "c1"), parts("r1", b"k1", "s2", "c1"), parts("r1", b"k1", "s1", "c2")] {
            assert_ne!(base, cache_key(&other));
        }
        let mut p = parts("r1", b"k1", "s1", "c1");
        p.abi_toml = "needs=[\"b\"]";
        assert_ne!(base, cache_key(&p));
        p = parts("r1", b"k1", "s1", "c1");
        p.entry = "bin/other";
        assert_ne!(base, cache_key(&p));
        // The parts are length-prefixed: moving a byte from one to the next changes the key.
        assert_ne!(cache_key(&parts("ab", b"c", "s", "x")), cache_key(&parts("a", b"bc", "s", "x")));
    }

    #[test]
    fn a_stamp_changes_when_the_file_is_replaced_or_touched() {
        let dir = std::env::temp_dir().join(format!("panpak-stamp-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let lib = dir.join("lib");
        fs::write(&lib, b"library").unwrap();
        let first = Stamp::read(&lib).unwrap();
        assert_eq!(first, Stamp::read(&lib).unwrap());
        // In place, same size: the change time moves.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&lib, b"LIBRARY").unwrap();
        assert_ne!(first, Stamp::read(&lib).unwrap());
        // Replaced by rename: another inode.
        let second = Stamp::read(&lib).unwrap();
        let tmp = dir.join("new");
        fs::write(&tmp, b"LIBRARY").unwrap();
        fs::rename(&tmp, &lib).unwrap();
        assert_ne!(second, Stamp::read(&lib).unwrap());
        // A link follows its target: moving the link changes the identity.
        let link = dir.join("link");
        std::os::unix::fs::symlink(&lib, &link).unwrap();
        let before = Stamp::read(&link).unwrap();
        let other = dir.join("other");
        fs::write(&other, b"other").unwrap();
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&other, &link).unwrap();
        assert_ne!(before.inode, Stamp::read(&link).unwrap().inode);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_entry_is_a_miss_when_a_file_changes_and_garbage_is_a_miss() {
        let dir = std::env::temp_dir().join(format!("panpak-cache-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let lib = dir.join("lib with space");
        fs::write(&lib, b"library").unwrap();
        let farm = dir.join("farm");
        fs::create_dir_all(&farm).unwrap();
        let path = dir.join("cache");
        let rec = Cached { key: "k".into(), loader: Some(lib.clone()), ld_path: vec![dir.clone()],
            stamps: vec![Stamp::read(&lib).unwrap(), Stamp::read(&farm).unwrap()] };
        let text = encode(&rec).unwrap();
        // The text format round-trips, including a path with a space.
        let back = decode(&text).unwrap();
        assert_eq!((back.key.as_str(), back.loader.as_deref(), back.ld_path.len(), back.stamps.len()), ("k", Some(lib.as_path()), 1, 2));
        assert_eq!(back.stamps[0], rec.stamps[0]);
        fs::write(&path, &text).unwrap();
        assert!(load(&path, "k").is_some());
        assert!(load(&path, "other").is_none());
        // A change to the farm folder (a link replaced or deleted) is a miss.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(farm.join("libx.so"), b"x").unwrap();
        assert!(load(&path, "k").is_none());
        let rec = Cached { key: "k".into(), loader: None, ld_path: vec![], stamps: vec![Stamp::read(&lib).unwrap()] };
        fs::write(&path, encode(&rec).unwrap()).unwrap();
        assert!(load(&path, "k").is_some());
        fs::write(&lib, b"changed").unwrap();
        assert!(load(&path, "k").is_none());
        // Garbage and a path with a newline are not entries.
        for bad in ["", "garbage", "panpak-abi-cache 2\nkey k\n", "panpak-abi-cache 1\nkey k\nloader -\nx nonsense\n", "panpak-abi-cache 1\nkey k\nloader -\ns 1 2 3\n"] {
            assert!(decode(bad).is_none() || decode(bad).unwrap().stamps.is_empty(), "{bad:?}");
        }
        let nl = Cached { key: "k".into(), loader: Some(PathBuf::from("a\nb")), ld_path: vec![], stamps: vec![] };
        assert!(encode(&nl).is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    fn zig_cc(dir: &Path, args: &[&str]) -> bool {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        Command::new(root.join("scripts/zig-cc"))
            .env("PANPAK_ZIG_TARGET", "x86_64-linux-gnu.2.28")
            .current_dir(dir)
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// A system library that is replaced after the first resolution changes the decision, and the
    /// next resolution sees it instead of answering from the cache. The libraries are small ones made
    /// with Zig; the system inventory is a folder of the test, so nothing real is touched.
    #[test]
    fn replacing_a_system_library_forces_a_new_resolution() {
        let dir = std::env::temp_dir().join(format!("panpak-resolve-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for d in ["host", "tree/lib", "tree/bin", "store"] {
            fs::create_dir_all(dir.join(d)).unwrap();
        }
        let w = |f: &str, t: &str| fs::write(dir.join(f), t).unwrap();
        w("bglib.c", "int g_old(void) { return 1; }\n");
        w("hglib1.c", "int g_old(void) { return 1; }\n");
        w("hglib2.c", "int g_old(void) { return 1; }\nint g_new(void) { return 2; }\n");
        w("gobj.c", "int g_new(void);\nint gobj_fn(void) { return g_new() + 10; }\n");
        w("px.c", "int gobj_fn(void);\nint px_fn(void) { return gobj_fn() + 100; }\n");
        w("main.c", "int px_fn(void);\nint g_old(void);\nint main(void) { return px_fn() + g_old() == 0; }\n");
        let so = ["-shared", "-fPIC", "-Wl,-z,now"];
        let cc = |extra: &[&str]| zig_cc(&dir, &[so.as_slice(), extra].concat());
        let built = cc(&["-Wl,-soname,libfoo.so.1", "-o", "tree/lib/libfoo.so.1", "bglib.c"])
            && cc(&["-Wl,-soname,libfoo.so.1", "-o", "host/libfoo.so.1", "hglib1.c"])
            && cc(&["-Wl,-soname,libbar.so.1", "-o", "host/libbar.so.1", "gobj.c", "host/libfoo.so.1"])
            && cc(&["-Wl,-soname,libpx.so.1", "-o", "host/libpx.so.1", "px.c", "host/libbar.so.1"])
            && zig_cc(&dir, &["-o", "tree/bin/app", "main.c", "host/libpx.so.1", "tree/lib/libfoo.so.1", "-Wl,-z,now"]);
        if !built || !Path::new("/lib64/ld-linux-x86-64.so.2").exists() {
            eprintln!("skipped: Zig cannot be run (set PANPAK_ZIG) or no glibc at the usual place");
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        let man: Manifest = toml::from_str(&format!(
            "[app]\nid=\"org.example.t\"\nname=\"T\"\nversion=\"1\"\nentry=\"bin/app\"\n[abi]\nneeds=[\"libfoo.so.1\",\"libpx.so.1\"]\n[update]\nkey=\"ed25519:{}\"\n",
            "0".repeat(64)
        ))
        .unwrap();
        let inventory = || -> Option<HashMap<String, PathBuf>> {
            // The glibc libraries are the real ones (read only); the others are the test's.
            let mut map: HashMap<String, PathBuf> = abi::host_libs()
                .into_iter()
                .filter(|(n, _)| n.starts_with("lib") && (n.starts_with("libc.so") || n.starts_with("libm.so") || n.starts_with("libpthread") || n.starts_with("libdl.so") || n.starts_with("librt.so") || n.starts_with("libutil.so") || n.starts_with("libresolv.so")) || n.starts_with("ld-linux"))
                .collect();
            map.extend([
                ("libfoo.so.1".to_string(), dir.join("host/libfoo.so.1")),
                ("libbar.so.1".to_string(), dir.join("host/libbar.so.1")),
                ("libpx.so.1".to_string(), dir.join("host/libpx.so.1")),
            ]);
            Some(map)
        };
        let store = dir.join("store");
        let tree = dir.join("tree");
        let go = || resolve_in(&store, "root", &tree, &man, b"6.0", "stub1", "ldcache1", inventory).unwrap();
        let glib_link = store.join("farms/root/libfoo.so.1");
        let entries = || fs::read_dir(store.join("abi-cache")).map(|r| r.count()).unwrap_or(0);
        // First: the system libglib lacks g_new, so no copy fixes it: the included libglib stays.
        go();
        assert_eq!(fs::read_link(&glib_link).unwrap(), tree.join("lib/libfoo.so.1"));
        assert_eq!(entries(), 1);
        // Again: answered from the cache, nothing new is written.
        go();
        assert_eq!(entries(), 1);
        // The system libglib is updated (a new file, as a package manager does): the cache no longer
        // answers, the decision is made again and the system copy is taken.
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(zig_cc(&dir, &[so.as_slice(), &["-Wl,-soname,libfoo.so.1", "-o", "host/libfoo.so.1.new", "hglib2.c"]].concat()));
        fs::rename(dir.join("host/libfoo.so.1.new"), dir.join("host/libfoo.so.1")).unwrap();
        go();
        assert_eq!(fs::read_link(&glib_link).unwrap(), dir.join("host/libfoo.so.1"));
        // A change of the stub or of the library cache is a new key.
        resolve_in(&store, "root", &tree, &man, b"6.0", "stub2", "ldcache1", inventory).unwrap();
        resolve_in(&store, "root", &tree, &man, b"6.0", "stub2", "ldcache2", inventory).unwrap();
        assert_eq!(entries(), 3);
        let _ = fs::remove_dir_all(&dir);
    }
}
