// SPDX-License-Identifier: MIT OR Apache-2.0
//! ABI-resolution-only cache in the trusted store (never trust/signature/sandbox).
use anyhow::{Context, Result};
use panpak_core::{abi, Manifest};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs::{self, File}, io::Read, os::unix::fs::MetadataExt, path::{Path, PathBuf}, process::Command};

const MAX_CACHE: u64 = 4 << 20;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    path: PathBuf,
    real: PathBuf,
    dev: u64,
    inode: u64,
    len: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}

impl Stamp {
    fn read(path: &Path) -> Result<Self> {
        let m = fs::metadata(path)?;
        Ok(Self { path: path.into(), real: fs::canonicalize(path)?, dev: m.dev(), inode: m.ino(),
            len: m.len(), mtime: m.mtime(), mtime_ns: m.mtime_nsec(), ctime: m.ctime(), ctime_ns: m.ctime_nsec() })
    }
}

#[derive(Serialize, Deserialize)]
struct Cached {
    key: String,
    loader: Option<PathBuf>,
    ld_path: Vec<PathBuf>,
    stamps: Vec<Stamp>,
}

fn hash_file(p: &Path, hash: &mut blake3::Hasher) -> Result<()> {
    let mut f = File::open(p)?;
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { break; }
        hash.update(&buf[..n]);
    }
    Ok(())
}

fn cache_key(package: &Path, kernel: &[u8], ldconfig: &[u8]) -> Result<String> {
    let mut h = blake3::Hasher::new();
    // A different runtime (even same ABI) could change the policy.
    hash_file(&std::env::current_exe()?, &mut h)?;
    hash_file(package, &mut h)?;
    h.update(kernel);
    h.update(ldconfig);
    Ok(h.finalize().to_hex().to_string())
}

fn load(path: &Path, key: &str) -> Option<abi::Plan> {
    if fs::metadata(path).ok()?.len() > MAX_CACHE { return None; }
    let cached: Cached = toml::from_str(&fs::read_to_string(path).ok()?).ok()?;
    if cached.key != key || cached.stamps.is_empty() { return None; }
    for stamp in &cached.stamps {
        if Stamp::read(&stamp.path).ok().as_ref() != Some(stamp) { return None; }
    }
    Some(abi::Plan { loader: cached.loader, ld_path: cached.ld_path })
}

pub fn resolve(store: &Path, package: &Path, root: &str, tree: &Path, man: &Manifest) -> Result<abi::Plan> {
    let ldconfig = Command::new("ldconfig").arg("-p").output().ok()
        .filter(|o| o.status.success() && !abi::parse_ldconfig(&String::from_utf8_lossy(&o.stdout)).is_empty());
    // Without a reliable host inventory (musl/minimal), no cache:
    // the fallback scan stays always fresh.
    let kernel = fs::read("/proc/sys/kernel/osrelease")?;
    let key = if std::env::var_os("PANPAK_NO_ABI_CACHE").is_none() {
        ldconfig.as_ref().map(|out| cache_key(package, &kernel, &out.stdout)).transpose()?
    } else { None };
    let cache_dir = store.join("abi-cache");
    let cache_path = key.as_ref().map(|key| cache_dir.join(format!("{key}.toml")));
    if let (Some(key), Some(path)) = (&key, &cache_path) {
        if let Some(plan) = load(path, key) {
            trace("hit");
            return Ok(plan);
        }
    }
    trace("miss");
    let a = man.abi.as_ref();
    let mut needs = a.and_then(|v| v.needs.clone()).unwrap_or_default();
    let prefer = a.and_then(|v| v.prefer_host.clone()).unwrap_or_default();
    let allowed = a.and_then(|v| v.allow_missing.clone()).unwrap_or_default();
    let bundled = abi::bundled_libs(tree);
    let host = ldconfig.as_ref().map(|o| abi::parse_ldconfig(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_else(abi::host_libs);
    let mut seen: HashSet<String> = needs.iter().cloned().collect();
    // The needs grow to the closure of DT_NEEDED of what is decided.
    loop {
        let decisions = abi::decide(&needs, &prefer, &allowed, &host, &bundled)?;
        let mut extra = Vec::new();
        for (_, source) in &decisions {
            let path = match source { abi::Source::Host(p) => p.clone(), abi::Source::Bundled(p) => tree.join(p) };
            for name in panpak_core::elf_needs(&path).with_context(|| format!("DT_NEEDED of {}", path.display()))? {
                if seen.insert(name.clone()) { extra.push(name); }
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
    let started = std::time::Instant::now();
    let resolution = abi::resolve_consistent(tree, &man.app.entry, &needs, &prefer, &allowed, &host, &bundled)?;
    trace(&format!(
        "resolved in {} ms (the rules alone: {rules_ms} ms), {} steps, {} libraries changed",
        started.elapsed().as_millis(),
        resolution.steps,
        resolution.changed.len()
    ));
    let decisions = resolution.decisions;
    if !resolution.changed.is_empty() {
        eprintln!("panpak: libraries taken from their other copy so that every symbol is defined: {}", resolution.changed.join(", "));
    }
    if !resolution.diagnosis.missing.is_empty() {
        let d = &resolution.diagnosis;
        eprintln!(
            "panpak: WARNING: {} symbols that the app or its libraries need are defined by none of the chosen libraries; the app will fail if it uses them. `panpak doctor <package>` lists them",
            d.missing.len()
        );
        for (file, sym) in d.missing.iter().take(5) {
            eprintln!("panpak:   {file}: {sym}");
        }
    }
    let plan = abi::apply(store, root, tree, &decisions, &bundled)?;
    if let (Some(key), Some(path)) = (key, cache_path) {
        // Checking farm target AND link prevents hits with replaced
        // or deleted links. Bundle directories (new libs/plugins) invalidate too.
        let mut paths = HashSet::new();
        paths.insert(tree.to_path_buf());
        for (name, source) in &decisions {
            paths.insert(match source { abi::Source::Host(p) => p.clone(), abi::Source::Bundled(p) => tree.join(p) });
            paths.insert(store.join("farms").join(root).join(name));
        }
        if let Some(loader) = &plan.loader { paths.insert(loader.clone()); }
        for dir in &plan.ld_path { paths.insert(dir.clone()); }
        let stamps: Result<Vec<_>> = paths.iter().map(|p| Stamp::read(p)).collect();
        let record = Cached { key, loader: plan.loader.clone(), ld_path: plan.ld_path.clone(), stamps: stamps? };
        // Optional cache: full/RO disk must not block a valid resolution.
        let _ = (|| -> Result<()> {
            fs::create_dir_all(cache_dir)?;
            let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
            fs::write(&tmp, toml::to_string(&record)?)?;
            fs::rename(tmp, path)?;
            Ok(())
        })();
    }
    Ok(plan)
}

fn trace(state: &str) {
    if std::env::var_os("PANPAK_ABI_TRACE").is_some() { eprintln!("panpak: ABI cache {state}"); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_invalidates_on_package_kernel_inventory_library_and_farm() {
        let dir = std::env::temp_dir().join(format!("panpak-cache-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let pkg = dir.join("app");
        fs::write(&pkg, b"package").unwrap();
        let key = cache_key(&pkg, b"kernel1", b"ldconfig1").unwrap();
        assert_ne!(key, cache_key(&pkg, b"kernel2", b"ldconfig1").unwrap());
        assert_ne!(key, cache_key(&pkg, b"kernel1", b"ldconfig2").unwrap());
        fs::write(&pkg, b"package2").unwrap();
        assert_ne!(key, cache_key(&pkg, b"kernel1", b"ldconfig1").unwrap());
        let lib = dir.join("lib");
        fs::write(&lib, b"library").unwrap();
        let farm = dir.join("link");
        std::os::unix::fs::symlink(&lib, &farm).unwrap();
        let path = dir.join("cache");
        let rec = Cached { key: key.clone(), loader: None, ld_path: vec![dir.clone()],
            stamps: vec![Stamp::read(&lib).unwrap(), Stamp::read(&farm).unwrap()] };
        fs::write(&path, toml::to_string(&rec).unwrap()).unwrap();
        assert!(load(&path, &key).is_some());
        fs::remove_file(&farm).unwrap();
        assert!(load(&path, &key).is_none());
        std::os::unix::fs::symlink(&lib, &farm).unwrap();
        fs::write(&lib, b"changed").unwrap();
        assert!(load(&path, &key).is_none());
        fs::write(&path, b"garbage").unwrap();
        assert!(load(&path, &key).is_none());
        fs::remove_dir_all(dir).unwrap();
    }
}
