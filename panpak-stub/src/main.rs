// SPDX-License-Identifier: MIT OR Apache-2.0
// Panpak stub: verify (spec §5) -> materialize -> Landlock sandbox -> exec.
// Build: cargo build --release --target x86_64-unknown-linux-musl
use anyhow::{bail, ensure, Context, Result};
use panpak_core::{bail_code, coded, ensure_code, ErrorCode};
use panpak_core::{hex, ChunkDecoder, parse_index, read_at, read_footer, verify, Chunk, Entry, Kind};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{fs::{OpenOptionsExt, PermissionsExt}, io::AsRawFd, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(target_arch = "x86_64")]
mod dbus_layer;
mod opener;
mod probe;
mod sharedtmp;
#[cfg(test)]
mod testutil;
mod view;

/// Files past this threshold are assembled streaming, not in RAM.
const MAX_ASSEMBLE: u64 = 8 << 20;

/// Create `base/rel` one level at a time, refusing symlinks or non-dirs
/// in prefixes: `create_dir_all` would follow them (tmp escape).
fn mkdir_secure(base: &Path, rel: &Path) -> Result<()> {
    let mut cur = base.to_path_buf();
    for c in rel.components() {
        cur.push(c);
        match fs::symlink_metadata(&cur) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&cur)?;
            }
            Err(e) => return Err(e.into()),
            Ok(m) => {
                ensure!(!m.file_type().is_symlink(), "unexpected symlink in path");
                ensure!(m.is_dir(), "non-directory prefix");
            }
        }
    }
    Ok(())
}

fn chunk_bytes<'a>(f: &mut File, c: &Chunk, dec: &mut ChunkDecoder, scratch: &'a mut Vec<u8>) -> Result<&'a [u8]> {
    // Cap = declared raw (validated by the parser: <= 1 MiB).
    dec.decode(&read_at(f, c.off, c.comp as u64)?[..], c.raw as u64, scratch)?;
    panpak_core::verify_chunk(c, scratch)?;
    Ok(scratch)
}

/// Write the shared blob (atomic) and link it with a hardlink;
/// fallback to copy if the fs does not support hardlinks.
fn link_or_copy(blob: &Path, dst: &Path, buf: Option<&[u8]>, mode: u32) -> Result<()> {
    if fs::hard_link(blob, dst).is_ok() {
        return Ok(());
    }
    let data = match buf {
        Some(b) => b.to_vec(),
        None => fs::read(blob)?,
    };
    fs::write(dst, &data)?;
    fs::set_permissions(dst, fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// Strip write bits from files and dirs (symlinks excluded: they have no
/// permissions of their own and set_permissions would follow them). Defense in depth beyond Landlock.
fn strip_write_bits(root: &Path) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let m = fs::symlink_metadata(&p)?;
        if m.file_type().is_symlink() {
            continue;
        }
        if m.is_dir() {
            for e in fs::read_dir(&p)? {
                stack.push(e?.path());
            }
        }
        let mode = m.permissions().mode();
        if mode & 0o222 != 0 {
            fs::set_permissions(&p, fs::Permissions::from_mode(mode & !0o222))?;
        }
    }
    Ok(())
}

/// Make an already materialized tree non-writable (idempotent).
/// The marker is a *sibling* file of the tree: nothing can be created inside,
/// the tree is already a-w.
fn ensure_tree_readonly(tree: &Path) -> Result<()> {
    let marker = tree.with_extension("readonly");
    if marker.exists() {
        return Ok(());
    }
    strip_write_bits(tree)?;
    File::create(&marker)?;
    Ok(())
}

/// Materialize the tree into the store; chunks are verified before use.
/// Identical files (by content+mode) share the same inode from store/blobs.
fn materialize(
    f: &mut File,
    store: &Path,
    root: &[u8; 32],
    chunks: &[Chunk],
    entries: &[Entry],
) -> Result<PathBuf> {
    use std::collections::HashMap;
    // One zstd context and one buffer for every chunk of the package.
    let mut dec = ChunkDecoder::new()?;
    let mut scratch = Vec::new();
    let tree = store.join("trees").join(hex(root));
    if tree.join(".complete").exists() {
        ensure_tree_readonly(&tree)?;
        return Ok(tree);
    } // Fast startup.
    // One builder per tree: starts that overlap on a store without the tree wait here, then find it done.
    // The lock goes with the file descriptor, so a builder that dies never leaves the others stuck.
    fs::create_dir_all(store.join("trees"))?;
    let lock = OpenOptions::new().create(true).truncate(false).write(true).open(tree.with_extension("lock")).context("open the tree lock")?;
    // SAFETY: flock(2) on a descriptor this function owns.
    ensure!(unsafe { libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) } == 0, "cannot lock the tree");
    if tree.join(".complete").exists() {
        ensure_tree_readonly(&tree)?;
        return Ok(tree);
    }
    // Under the lock nobody else writes here: a folder from a start that was killed is removed, and the
    // new one is built apart and published with one rename.
    let tmp = tree.with_extension("partial");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;
    let blobs = store.join("blobs");
    fs::create_dir_all(&blobs)?;

    // Pass 1: dirs and files. No symlink exists yet in tmp.
    for e in entries {
        let dst = tmp.join(&e.path);
        match &e.kind {
            Kind::Dir => {
                mkdir_secure(&tmp, &e.path)?;
                fs::set_permissions(&dst, fs::Permissions::from_mode(e.mode))?;
            }
            Kind::Link(_) => {}
            Kind::File { chunks: ids } => {
                if let Some(parent) = e.path.parent() {
                    if !parent.as_os_str().is_empty() {
                        mkdir_secure(&tmp, parent)?;
                    }
                }
                ensure!(fs::symlink_metadata(&dst).is_err(), "path already exists");
                let total: u64 = ids.iter().map(|&i| chunks[i as usize].raw as u64).sum();
                if total <= MAX_ASSEMBLE {
                    let mut buf = Vec::with_capacity(total as usize);
                    for &id in ids {
                        buf.extend_from_slice(chunk_bytes(f, &chunks[id as usize], &mut dec, &mut scratch)?);
                    }
                    let fid = hex(blake3::hash(&buf).as_bytes());
                    let blob = blobs.join(format!("{fid}-{:o}", e.mode));
                    if !blob.exists() {
                        // Another tree (another lock) may be writing the same blob: a name of its own.
                        let btmp = blob.with_extension(format!("part{}", std::process::id()));
                        fs::write(&btmp, &buf)?;
                        // Shared blob: never writable (defense in depth).
                        fs::set_permissions(&btmp, fs::Permissions::from_mode(e.mode & !0o222))?;
                        let _ = fs::rename(&btmp, &blob); // Identical content wins.
                    } else {
                        ensure_blob_readonly(&blob)?;
                    }
                    link_or_copy(&blob, &dst, Some(&buf), e.mode)?;
                } else {
                    let mut out = File::create(&dst)?;
                    for &id in ids {
                        out.write_all(chunk_bytes(f, &chunks[id as usize], &mut dec, &mut scratch)?)?;
                    }
                    fs::set_permissions(&dst, fs::Permissions::from_mode(e.mode))?;
                }
            }
        }
    }

    // Pass 2: symlinks, checking the target RESOLVED against the
    // link's directory (spec §5.5): a relative target with leading ".."
    // is legal if it stays inside the root (e.g. usr/translations/x ->
    // ../share/... == usr/share/...). A purely lexical check on the
    // lone target would give false positives; chains stay verified
    // later (pass 3).
    fn escaped(link: &Path, target: &str) -> bool {
        use std::path::Component::*;
        if target.is_empty() {
            return true;
        }
        let t = PathBuf::from(target);
        if t.is_absolute() {
            return true;
        }
        let base = link.parent().unwrap_or(Path::new(""));
        let mut norm = base.to_path_buf();
        for c in t.components() {
            match c {
                Normal(x) => norm.push(x),
                ParentDir => {
                    if !norm.pop() {
                        return true;
                    }
                }
                CurDir => {}
                _ => return true,
            }
        }
        false
    }
    let mut links: HashMap<PathBuf, String> = HashMap::new();
    for e in entries {
        if let Kind::Link(t) = &e.kind {
            ensure!(!escaped(&e.path, t), "symlink escapes root: {}", e.path.display());
            let dst = tmp.join(&e.path);
            if let Some(parent) = e.path.parent() {
                if !parent.as_os_str().is_empty() {
                    mkdir_secure(&tmp, parent)?;
                }
            }
            ensure!(fs::symlink_metadata(&dst).is_err(), "path already exists");
            std::os::unix::fs::symlink(t, &dst)?;
            links.insert(e.path.clone(), t.clone());
        }
    }

    // Pass 3: resolve symlink chains per prefix (max 40 iterations)
    // and refuse composed escapes (e.g. a -> b/passwd with b -> ..), which the
    // single-target check cannot see. Pure lexical normalization.
    fn resolve(links: &HashMap<PathBuf, String>, start: &Path) -> Result<()> {
        use std::path::Component::*;
        let mut cur = start.to_path_buf();
        for _ in 0..40 {
            // Longest prefix of cur that is a symlink (cur itself included).
            let mut node: Option<PathBuf> = None;
            let mut anc = cur.as_path();
            loop {
                if links.contains_key(anc) {
                    node = Some(anc.to_path_buf());
                    break;
                }
                match anc.parent() {
                    Some(p) if !p.as_os_str().is_empty() => anc = p,
                    _ => break,
                }
            }
            let node = match node {
                Some(n) => n,
                None => return Ok(()), // Only real files/dirs: inside tmp.
            };
            let t = links[&node].clone();
            ensure!(!PathBuf::from(&t).is_absolute(), "absolute symlink in chain");
            let rest = cur.strip_prefix(&node).unwrap().to_path_buf();
            let base = node.parent().unwrap_or(Path::new("")).to_path_buf();
            let mut norm = base;
            for c in PathBuf::from(&t).components().chain(rest.components()) {
                match c {
                    Normal(x) => norm.push(x),
                    ParentDir => ensure!(norm.pop(), "symlink chain escapes root"),
                    CurDir => {}
                    _ => bail!("invalid component in chain"),
                }
            }
            cur = norm;
        }
        bail!("symlink chain too long or cyclic");
    }
    for path in links.keys() {
        resolve(&links, path)?;
    }

    File::create(tmp.join(".complete"))?;
    // Non-writable tree before publishing it (defense in depth).
    strip_write_bits(&tmp)?;
    fs::rename(&tmp, &tree)?;
    File::create(tree.with_extension("readonly"))?;
    drop(lock);
    Ok(tree)
}

/// Blob from a previous version (still writable): strip the write bits.
fn ensure_blob_readonly(blob: &Path) -> Result<()> {
    let mode = fs::symlink_metadata(blob)?.permissions().mode();
    if mode & 0o222 != 0 {
        fs::set_permissions(blob, fs::Permissions::from_mode(mode & !0o222))?;
    }
    Ok(())
}

/// ABI resolution (spec §7, SONAME-granular v1).
/// Symbol versions are not compared: at equal name the host is used,
/// and if incompatible the loader fails explicitly
/// (fail-closed), never a silently wrong lib.
#[cfg(test)]
use panpak_core::abi;
mod resolution;

/// Landlock core shared with tests: read-only+execute tree,
/// app data and /tmp read+write, rest of fs read-only.
/// No silent fallback (the caller checks NotEnforced).
///
/// Detect the highest supported ABI (best-effort V9..V1) and try
/// per-single-node /dev rules (null, zero, full, urandom, random, tty).
/// If the kernel refuses per-file rules, fall back to whole /dev in RW
/// (privileged nodes stay DAC-protected): never a silent denial,
/// always and only between these two explicit choices.
/// Pure network decision (testable): what to do when the kernel offers/denies V4.
#[derive(Debug, PartialEq, Eq)]
enum NetDecision {
    /// Filter (handle net + allowlist/deny).
    Enforce,
    /// Old kernel + explicit opt-out: run unfiltered, with warning.
    UnfilteredWarn,
    /// Old kernel without opt-out, or unhonorable network=true: refuse.
    Refuse(&'static str),
}

fn decide_net(has_v4: bool, wants_net: bool, allow_unfiltered: bool) -> NetDecision {
    match (has_v4, wants_net, allow_unfiltered) {
        (true, _, _) => NetDecision::Enforce,
        (false, true, _) => NetDecision::Refuse("network=true requires Landlock ABI >= V4"),
        (false, false, true) => NetDecision::UnfilteredWarn,
        (false, false, false) => NetDecision::Refuse(
            "network cannot be filtered on this kernel (Landlock < V4): rerun with --network-unfiltered or upgrade the kernel",
        ),
    }
}

/// Grant for `panpak open`: exact file (always RW if write/rename)
/// plus, only with rename, create/rename/remove rights in the
/// parent folder WITHOUT reading/listing neighbors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct OpenGrant {
    write: bool,
    rename: bool,
}

/// Paths the sandbox opens besides the system ones, the tree and /tmp.
#[derive(Debug, Default)]
struct Access {
    /// Read and write: the two app roots and the `rw` home paths.
    rw: Vec<PathBuf>,
    /// Read only: the user's theme, font and icon files and the `ro` home paths.
    ro: Vec<PathBuf>,
    /// Path sockets every app may reach without listing them (Wayland, journal, resolver, audio with
    /// `audio = true`), only the ones that exist. Landlock V9 gets a `ResolveUnix` rule for each.
    sockets: Vec<PathBuf>,
    /// The sandbox runs inside the isolated view, which hides the sockets the manifest does not list.
    in_view: bool,
    /// The home folder, listable (`ReadDir`). Only set when the isolated view is active: there the
    /// home folder is an empty tmpfs that holds the granted folders only. Never set without the view.
    home_list: Option<PathBuf>,
}

/// Refuse a link in `rel` below `home`: every part that exists must be a real
/// directory (the last one may be a file). Missing parts end the check.
fn reject_links(home: &Path, rel: &str) -> Result<()> {
    let mut cur = home.to_path_buf();
    for part in rel.split('/') {
        cur.push(part);
        match fs::symlink_metadata(&cur) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("check {}", cur.display())),
            Ok(m) => ensure_code!(
                !m.file_type().is_symlink(),
                ErrorCode::E3000,
                "home path {rel}: {} is a symbolic link, refused (a link could point outside the granted folder)",
                cur.display()
            ),
        }
    }
    Ok(())
}

/// Resolve what the sandbox opens: the app roots, the readable user settings and the
/// `home_paths` of the manifest. An `rw` home path is created if missing and must be a
/// folder. Runs before Landlock is applied.
fn access_for(man: &panpak_core::Manifest, dirs: &panpak_core::AppDirs) -> Result<Access> {
    let mut access = Access::default();
    access.rw.extend(dirs.roots().iter().map(|p| p.to_path_buf()));
    access.ro.extend(panpak_core::readonly_user_paths());
    let paths = man.permissions.as_ref().and_then(|p| p.home_paths.as_deref()).unwrap_or_default();
    if paths.is_empty() {
        return Ok(access);
    }
    let home = dirs::home_dir().context("no home dir")?;
    for h in paths {
        reject_links(&home, &h.path)?;
        let full = home.join(&h.path);
        if h.access == "rw" {
            match fs::symlink_metadata(&full) {
                Ok(m) => ensure_code!(m.is_dir(), ErrorCode::E3000, "home path {}: rw needs a folder, found a file", h.path),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir_all(&full).with_context(|| format!("create {}", full.display()))?;
                }
                Err(e) => return Err(e).with_context(|| format!("check {}", full.display())),
            }
            access.rw.push(full);
        } else if full.exists() {
            access.ro.push(full);
        }
    }
    Ok(access)
}

/// The user folders the app may use: the ones pinned at install that `user-dirs.dirs` still names the
/// same way. A folder that changed, or that fails the rules now, is not granted (with a warning).
/// Granted folders go into `access`: `rw` ones are writable, `ro` ones readable. Returns (name, path
/// relative to home) of each.
fn xdg_grants(man: &panpak_core::Manifest, store: &Path, installed_mode: bool, access: &mut Access) -> Result<Vec<(String, String)>> {
    let wanted = man.permissions.as_ref().and_then(|p| p.xdg_dirs.as_deref()).unwrap_or_default();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    if !installed_mode {
        eprintln!("panpak: user folders are only granted to an installed app; none given");
        return Ok(Vec::new());
    }
    let pin = match fs::read_to_string(store.join("apps").join(&man.app.id).join("xdg-dirs")) {
        Ok(t) => match panpak_core::parse_pin(&t) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("panpak: WARNING: user folder pin unusable ({}); none given, reinstall to grant them", panpak_core::err_text(&e));
                return Ok(Vec::new());
            }
        },
        Err(_) => {
            eprintln!("panpak: WARNING: no user folders were fixed at install; none given, reinstall to grant them");
            return Ok(Vec::new());
        }
    };
    let home = dirs::home_dir().context("no home dir")?;
    let text = match dirs::config_dir().map(|c| c.join("user-dirs.dirs")).map(fs::read_to_string) {
        Some(Ok(t)) => Some(t),
        // No file, or unreadable: the default folder names apply; a folder that then differs from the one
        // pinned at install is refused with a message below.
        _ => None,
    };
    let mut out = Vec::new();
    for d in wanted {
        let Some(pinned) = pin.get(&d.name) else { continue };
        let now = panpak_core::resolve_xdg_dir(&d.name, &home, text.as_deref());
        if now.as_deref().ok() != Some(pinned.as_str()) {
            eprintln!("panpak: WARNING: the {} folder changed since install (was ~/{pinned}); not granted, reinstall to accept the new one", d.name);
            continue;
        }
        if let Err(e) = panpak_core::no_links(&home, pinned) {
            eprintln!("panpak: WARNING: the {} folder is not granted: {}", d.name, panpak_core::err_text(&e));
            continue;
        }
        let full = home.join(pinned);
        match fs::symlink_metadata(&full) {
            Ok(m) if m.is_dir() => {}
            _ => {
                eprintln!("panpak: WARNING: ~/{pinned} ({}) is not a folder; not granted", d.name);
                continue;
            }
        }
        if d.access == "rw" {
            access.rw.push(full);
        } else {
            access.ro.push(full);
        }
        out.push((d.name.clone(), pinned.clone()));
    }
    Ok(out)
}

/// Write `$XDG_CONFIG_HOME/user-dirs.dirs` in the private folder: granted folders with their path,
/// every other one at `$HOME/`. A file without the marker is the user's and stays.
fn write_user_dirs(dirs: &panpak_core::AppDirs, granted: &[(String, String)]) -> Result<()> {
    let want = panpak_core::generated_user_dirs(granted)?;
    let path = dirs.config.join("user-dirs.dirs");
    match fs::symlink_metadata(&path) {
        Ok(m) if m.is_file() => {
            let cur = fs::read_to_string(&path).unwrap_or_default();
            if !cur.contains(panpak_core::USER_DIRS_MARKER) || cur == want {
                return Ok(());
            }
        }
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("check {}", path.display())),
    }
    let tmp = dirs.config.join(format!(".user-dirs.dirs.{}", std::process::id()));
    fs::write(&tmp, want)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Write `$XDG_CONFIG_HOME/fontconfig/fonts.conf` in the private folder, so fontconfig finds the
/// user's own configuration and font folders. A file without the marker is the user's and stays.
fn write_fonts_conf(dirs: &panpak_core::AppDirs) -> Result<()> {
    let (Some(config), Some(data), Some(home)) = (dirs::config_dir(), dirs::data_dir(), dirs::home_dir()) else {
        return Ok(());
    };
    let want = panpak_core::fonts_conf(&config, &data, &home);
    let folder = dirs.config.join("fontconfig");
    let path = folder.join("fonts.conf");
    match fs::symlink_metadata(&path) {
        Ok(m) if m.is_file() => {
            let cur = fs::read_to_string(&path).unwrap_or_default();
            if !cur.contains(panpak_core::FONTS_CONF_MARKER) || cur == want {
                return Ok(());
            }
        }
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("check {}", path.display())),
    }
    fs::create_dir_all(&folder)?;
    let tmp = folder.join(format!(".fonts.conf.{}", std::process::id()));
    fs::write(&tmp, want)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

fn apply_landlock(
    tree: &Path,
    access: &Access,
    man: &panpak_core::Manifest,
    allow_unfiltered: bool,
    open: Option<(&Path, OpenGrant)>,
) -> Result<landlock::RestrictionStatus> {
    let abi = panpak_core::detect_abi().context("Landlock unsupported by kernel")?;
    // /dev single nodes first; if the kernel refuses them (build or
    // restrict), explicit fallback to whole /dev. Never a silent denial.
    let rs = match build_ruleset(tree, access, man, abi, true, allow_unfiltered, open) {
        Ok(rs) => rs,
        Err(e) => {
            eprintln!("panpak: cannot build per-node /dev rules on this kernel ({}), using whole /dev", panpak_core::err_text(&e));
            build_ruleset(tree, access, man, abi, false, allow_unfiltered, open)?
        }
    };
    let res = rs.restrict_self();
    let st = match res {
        Ok(st) => st,
        Err(e) => {
            eprintln!("panpak: kernel rejected per-node /dev rules ({e}), using whole /dev (DAC still applies)");
            build_ruleset(tree, access, man, abi, false, allow_unfiltered, open)?
                .restrict_self()
                .map_err(|_| anyhow::anyhow!("cannot enforce sandbox on this kernel: cannot run safely, upgrade the kernel (>= 5.13)"))?
        }
    };
    ensure_code!(
        st.ruleset == landlock::RulesetStatus::FullyEnforced,
        ErrorCode::E3000,
        "partial Landlock ({:?}): required rights exceed ABI, refused",
        st.ruleset
    );
    Ok(st)
}

/// Build the ruleset. With `fine_dev` try per-single-node rules
/// on /dev (null, zero, full, urandom, random, tty) + dri/shm dirs;
/// otherwise whole /dev in RW (privileged nodes DAC-protected).
/// The descriptor for a Landlock rule on `path` (`O_PATH`, see `panpak_core::open_o_path`). A path that is
/// not there gives `None` without a message: there is nothing to allow and access stays denied. Any other
/// failure (permission, wrong type) is reported, because the rule is left out.
fn rule_fd(path: &Path) -> Option<std::os::fd::OwnedFd> {
    match panpak_core::open_o_path(path) {
        Ok(fd) => Some(fd),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!("panpak: WARNING: Landlock rule for {} left out ({e}); access stays denied", path.display());
            None
        }
    }
}

/// Network (V4+), scope (V6+), Unix sockets (V9): only if the ABI offers them,
/// otherwise error only if the manifest really requires them.
fn build_ruleset(
    tree: &Path,
    access: &Access,
    man: &panpak_core::Manifest,
    abi: landlock::ABI,
    fine_dev: bool,
    allow_unfiltered: bool,
    open: Option<(&Path, OpenGrant)>,
) -> Result<landlock::RulesetCreated> {
    use landlock::{
        Access, AccessFs, AccessNet, NetPort, PathBeneath, Ruleset, RulesetAttr,
        RulesetCreatedAttr, Scope,
    };
    let new_abi = abi >= landlock::ABI::V4;
    let new_scope = abi >= landlock::ABI::V6;
    let new_unix = abi >= landlock::ABI::V9;
    let x11 = man.permissions.as_ref().and_then(|p| p.x11).unwrap_or(false);
    // TCP network: Enforce / UnfilteredWarn / Refuse (never silent fallback).
    // unfilterable network=true is always an error, even with the flag.
    let net_wanted = man.permissions.as_ref().and_then(|p| p.network).unwrap_or(false);
    let allow_tcp = man.permissions.as_ref().and_then(|p| p.network_allow_tcp.clone()).unwrap_or_default();
    let bind_tcp = man.permissions.as_ref().and_then(|p| p.network_bind_tcp.clone()).unwrap_or_default();
    let handle_net = match decide_net(new_abi, net_wanted || !allow_tcp.is_empty() || !bind_tcp.is_empty(), allow_unfiltered) {
        NetDecision::Enforce => true,
        NetDecision::UnfilteredWarn => {
            eprintln!("WARNING panpak: network CANNOT be filtered on this kernel ({abi:?} < V4); explicit execution with --network-unfiltered");
            false
        }
        NetDecision::Refuse(msg) => bail_code!(ErrorCode::E3000, "{msg} (detected ABI: {abi:?})"),
    };
    let socks = man.permissions.as_ref().and_then(|p| p.sockets_allow.clone()).unwrap_or_default();
    if !socks.is_empty() {
        // Without V9 the isolated view is what lets the listed sockets (and only them) through.
        ensure_code!(
            new_unix || access.in_view,
            ErrorCode::E3000,
            "sockets_allow requires Landlock ABI >= V9 or the isolated view (bubblewrap), this kernel has {abi:?}"
        );
    }
    let mut b = Ruleset::default().handle_access(AccessFs::from_all(abi))?;
    if handle_net {
        b = b.handle_access(AccessNet::from_all(abi))?;
    }
    if new_scope {
        let scopes = if x11 {
            Scope::from_all(abi) & !Scope::AbstractUnixSocket
        } else {
            Scope::from_all(abi)
        };
        b = b.scope(scopes)?;
    }
    let mut rs = b.create()?;
    if x11 {
        eprintln!("panpak: WARNING X11: the app can observe other apps' input and screen; abstract sockets are not isolated");
        if let Some(fd) = rule_fd(Path::new("/tmp/.X11-unix")) {
            let mut access = AccessFs::from_read(abi);
            if new_unix {
                access |= AccessFs::ResolveUnix;
            }
            rs = rs.add_rule(PathBeneath::new(fd, access))?;
        }
    }
    for p in ["/usr", "/lib", "/lib64", "/etc", "/proc", "/sys", "/run"] {
        if let Some(fd) = rule_fd(Path::new(p)) {
            rs = rs.add_rule(PathBeneath::new(fd, AccessFs::from_read(abi)))?;
        }
    }
    if fine_dev {
        // Minimum legitimate rights for files (no Make*/Remove* from directories).
        // Missing or unopenable nodes (e.g. /dev/tty with no controlling
        // terminal, /dev/dri with no GPU) are skipped: the self-test on
        // /dev/null in sandbox() acts as the safety net.
        for (node, write) in [
            ("/dev/null", true),
            ("/dev/zero", true),
            ("/dev/full", true),
            ("/dev/urandom", false),
            ("/dev/random", false),
            ("/dev/tty", true),
        ] {
            if let Some(fd) = rule_fd(Path::new(node)) {
                let access = if write {
                    AccessFs::ReadFile | AccessFs::WriteFile
                } else {
                    AccessFs::ReadFile.into()
                };
                rs = rs.add_rule(PathBeneath::new(fd, access))?;
            }
        }
        for d in ["/dev/dri", "/dev/shm"] {
            if let Some(fd) = rule_fd(Path::new(d)) {
                let rw = AccessFs::from_all(abi) & !AccessFs::Refer;
                rs = rs.add_rule(PathBeneath::new(fd, rw))?;
            }
        }
    } else if let Some(fd) = rule_fd(Path::new("/dev")) {
        let rw = AccessFs::from_all(abi) & !AccessFs::Refer;
        rs = rs.add_rule(PathBeneath::new(fd, rw))?;
    }
    // Tree: read + execute, NEVER write (blobs are shared
    // across trees and apps: a write would alter them everywhere).
    let rx = AccessFs::from_read(abi) | AccessFs::Execute;
    rs = rs.add_rule(PathBeneath::new(panpak_core::open_o_path(tree).with_context(|| format!("open {}", tree.display()))?, rx))?;
    // Writable dirs (app roots, rw home paths, tmp, dev): every right except Refer (link and rename between
    // directories, ABI V2). Refer on a writable dir would let the app hard-link a tree file into it (the tree has no
    // Refer, but Landlock only asks for the destination to hold the rights of the source), and a write through that
    // link would change the shared blob. Without Refer, a rename inside one directory still works (atomic save).
    // In the isolated view the tree and the system are read-only mounts of their own, and the kernel refuses a link
    // or a move across mounts (EXDEV, tests/app_dirs.py), so there Refer is granted: a program that creates a file in
    // a private folder and renames it into place, as Qt does for its local sockets, then works.
    let rw = if access.in_view { AccessFs::from_all(abi) } else { AccessFs::from_all(abi) & !AccessFs::Refer };
    for dir in &access.rw {
        rs = rs.add_rule(PathBeneath::new(panpak_core::open_o_path(dir).with_context(|| format!("open {}", dir.display()))?, rw))?;
    }
    // The home folder can be listed inside the view, so that the granted user folders show up in it.
    if let (true, Some(home)) = (access.in_view, &access.home_list) {
        rs = rs.add_rule(PathBeneath::new(panpak_core::open_o_path(home).with_context(|| format!("open {}", home.display()))?, AccessFs::ReadDir))?;
    }
    // Read only: theme, fonts and icons of the user, ro home paths. A missing or
    // unreadable entry is skipped. Files get file rights only (the kernel refuses
    // directory rights on a file); nothing here can be executed.
    for path in &access.ro {
        if let Some(fd) = rule_fd(Path::new(path)) {
            let rights = if path.is_dir() { AccessFs::ReadFile | AccessFs::ReadDir } else { AccessFs::ReadFile.into() };
            rs = rs.add_rule(PathBeneath::new(fd, rights))?;
        }
    }
    // Shared /tmp (RW): nearly all apps require it; denying it by
    // default would break even trivial scripts. Future: [permissions]tmp.
    if let Some(fd) = rule_fd(Path::new("/tmp")) {
        rs = rs.add_rule(PathBeneath::new(fd, rw))?;
    }
    // Explicitly opened file (`panpak open` / double click): ONLY that path,
    // read by default, write only with explicit declaration.
    // The caller passes the already canonicalized path.
    if let Some((path, grant)) = open {
        let fd = panpak_core::open_o_path(path)
            .with_context(|| format!("open: cannot open file: {}", path.display()))?;
        let access = if grant.write || grant.rename {
            if grant.rename {
                eprintln!("panpak: WARNING: WRITE access to {} + create/rename/remove in directory {} (neighbors cannot be read or listed, but can be overwritten/deleted)", path.display(), path.parent().map(|p| p.display().to_string()).unwrap_or_default());
            } else {
                eprintln!("panpak: WRITE access to {}", path.display());
            }
            // Truncate is part of writing (open O_TRUNC / fs::write).
            AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::Truncate
        } else {
            eprintln!("panpak: read access to {}", path.display());
            AccessFs::ReadFile.into()
        };
        rs = rs.add_rule(PathBeneath::new(fd, access))?;
        // Atomic save (temp+rename, e.g. gedit): the parent folder
        // grants ONLY execute (traversal), creation, removal:
        // NO read/list. Without it, temps would not be writable and
        // rename would fail (fail-closed); with it, neighbors stay unreadable
        // but overwritable/deletable (documented exposure).
        if grant.rename {
            if let Some(parent) = path.parent() {
                let fd = panpak_core::open_o_path(parent).with_context(|| {
                    format!("open: cannot open directory: {}", parent.display())
                })?;
                rs = rs.add_rule(PathBeneath::new(
                    fd,
                    AccessFs::Execute | AccessFs::MakeReg | AccessFs::RemoveFile | AccessFs::WriteFile | AccessFs::Truncate,
                ))?;
            }
        }
    }
    // Network: port allowlist (deny-all if network=true without ports, or if
    // network=false and the ABI allows it).
    if handle_net {
        for port in &allow_tcp {
            rs = rs.add_rule(NetPort::new(*port, AccessNet::ConnectTcp))?;
        }
        for port in &bind_tcp {
            rs = rs.add_rule(NetPort::new(*port, AccessNet::BindTcp))?;
        }
    }
    // Path Unix sockets (V9+): the sockets every app may reach, then the allowlist from the
    // manifest; under appdata/tmp they already pass via ResolveUnix.
    if new_unix {
        for path in &access.sockets {
            if let Some(fd) = rule_fd(path) {
                let rights = AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::ResolveUnix;
                rs = rs.add_rule(PathBeneath::new(fd, rights))?;
            }
        }
    }
    for s in socks.iter().filter(|_| new_unix) {
        let path = panpak_core::expand_socket_path(s)?;
        match panpak_core::open_o_path(&path) {
            Ok(fd) => {
                let access = AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::ResolveUnix;
                rs = rs.add_rule(PathBeneath::new(fd, access))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("panpak: socket {s} absent, rule omitted (access remains denied)")
            }
            Err(e) => eprintln!("panpak: WARNING: socket {s}: rule omitted ({e}); access remains denied"),
        }
    }
    Ok(rs)
}

/// Landlock from the manifest permissions (spec §3.1, policy in PANPAK-FORMAT.md §9):
/// - app tree: read + execute, never write;
/// - the private app folders (`panpak_core::AppDirs`), the `rw` home paths and /tmp: read+write;
/// - theme, font and icon files of the user and the `ro` home paths: read only;
/// - /usr /lib /etc /proc /sys /run: read-only (host runtime);
/// - /dev: only null/zero/full/urandom/random/tty nodes + dri/shm dirs
///   (explicit fallback to whole /dev if the kernel refuses per-file rules);
/// - network: denied by default (V4+), allowlist from the manifest;
///   on kernels without V4, explicit --network-unfiltered is required (warning at every start).
fn sandbox(tree: &Path, man: &panpak_core::Manifest, access: &Access, allow_unfiltered: bool, open: Option<(&Path, OpenGrant)>) -> Result<()> {
    let st = apply_landlock(tree, access, man, allow_unfiltered, open)?;
    // No silent fallback: if not applied, explicit consent is required.
    ensure_code!(
        st.ruleset != landlock::RulesetStatus::NotEnforced,
        ErrorCode::E3001,
        "Landlock unavailable: use `panpak doctor`"
    );
    // Fail-closed self-test: /dev/null openable, tree writes denied.
    // If the rules had no real effect, fail hard here.
    ensure!(
        fs::OpenOptions::new().read(true).write(true).open("/dev/null").is_ok(),
        "sandbox self-test: cannot open /dev/null"
    );
    // An unnamed temporary file in the tree: if the rules let it be made, nothing stays on disk.
    ensure!(
        fs::OpenOptions::new().write(true).custom_flags(libc::O_TMPFILE).open(tree).is_err(),
        "sandbox self-test: tree writes allowed"
    );
    Ok(())
}

/// How long the start waits for `gsettings` in total.
const GSETTINGS_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

/// Where `gsettings` is looked for: system paths only, never `PATH`. The stub runs it outside
/// the sandbox, so a program placed in a folder the user's `PATH` lists must not be picked.
const GSETTINGS_PATHS: &[&str] = &["/usr/bin/gsettings", "/bin/gsettings"];

fn find_gsettings() -> Option<PathBuf> {
    GSETTINGS_PATHS.iter().map(PathBuf::from).find(|p| p.is_file())
}

/// Variables that point an app at the user's agents; removed from every app's environment.
const AGENT_VARS: &[&str] = &["SSH_AUTH_SOCK", "GPG_AGENT_INFO", "SSH_AGENT_PID"];

/// Value that D-Bus libraries (libdbus, GDBus, sd-bus) read as "no bus": an unknown transport.
const NO_BUS_ADDRESS: &str = "disabled:";

/// Hide the local sockets the environment points to. `allows_session` and `allows_system` are true when
/// the manifest lists that bus; its address variable is then left alone. This is hygiene, not a barrier: below
/// Landlock V9 the app can still open `$XDG_RUNTIME_DIR/bus` or an agent socket by path.
fn hide_local_sockets(cmd: &mut Command, allows_session: bool, allows_system: bool) {
    for v in AGENT_VARS {
        cmd.env_remove(v);
    }
    if !allows_session {
        cmd.env("DBUS_SESSION_BUS_ADDRESS", NO_BUS_ADDRESS);
    }
    if !allows_system {
        cmd.env("DBUS_SYSTEM_BUS_ADDRESS", NO_BUS_ADDRESS);
    }
}

/// Ask `gsettings` for a key of `org.gnome.desktop.interface`; both queries run at once and share
/// `timeout`. Returns `(color-scheme, gtk-theme)` without the quotes, or None on any failure.
fn read_interface_settings(gsettings: &Path, timeout: std::time::Duration) -> Option<(String, String)> {
    use std::io::Read as _;
    use std::process::Stdio;
    let spawn = |key: &str| {
        Command::new(gsettings)
            .args(["get", "org.gnome.desktop.interface", key])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            // Only the dark-theme hint depends on it: a program that cannot start means "no preference".
            .ok()
    };
    let mut kids = [spawn("color-scheme"), spawn("gtk-theme")];
    let deadline = std::time::Instant::now() + timeout;
    let mut out: [Option<String>; 2] = [None, None];
    while out.iter().any(Option::is_none) {
        for (i, kid) in kids.iter_mut().enumerate() {
            let Some(c) = kid else {
                // Could not start: nothing to wait for.
                out[i] = Some(String::new());
                continue;
            };
            if out[i].is_some() {
                continue;
            }
            if let Ok(Some(status)) = c.try_wait() {
                let mut text = String::new();
                if status.success() {
                    if let Some(mut so) = c.stdout.take() {
                        let _ = so.read_to_string(&mut text);
                    }
                }
                out[i] = Some(text);
            }
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    // Whatever is still running after the deadline is stopped and reaped.
    for kid in kids.iter_mut().flatten() {
        if kid.try_wait().ok().flatten().is_none() {
            let _ = kid.kill();
            let _ = kid.wait();
        }
    }
    let unquote = |s: &str| s.trim().trim_matches('\'').to_string();
    let [Some(scheme), Some(theme)] = out else { return None };
    Some((unquote(&scheme), unquote(&theme)))
}

/// `GTK_THEME` for a dark desktop. GTK3 reads the dark preference from dconf, which the app
/// cannot reach with its private config folder. If the user prefers dark, the stub hands the
/// theme over as `<gtk-theme>:dark` (`Adwaita:dark` when no theme is set). The value is read once
/// at start: the app does not follow later changes. None when GTK_THEME is already set,
/// `gsettings` is missing, slow or fails, or the scheme is not `prefer-dark`.
fn gtk_dark_theme(gsettings: Option<&Path>, timeout: std::time::Duration) -> Option<String> {
    if std::env::var_os("GTK_THEME").is_some() {
        return None;
    }
    let (scheme, theme) = read_interface_settings(gsettings?, timeout)?;
    if scheme != "prefer-dark" {
        return None;
    }
    // A theme name is a folder name: refuse anything odd rather than pass it on.
    let ok = theme.bytes().all(|b| b.is_ascii_alphanumeric() || b" ._+-".contains(&b));
    let theme = if theme.is_empty() { "Adwaita" } else { theme.as_str() };
    ok.then(|| format!("{theme}:dark"))
}

/// True if the file starts with the ELF magic. Decides whether to use the included loader.
fn is_elf(path: &Path) -> Result<bool> {
    use std::io::Read;
    let mut magic = [0u8; 4];
    let mut f = File::open(path)?;
    let n = f.read(&mut magic)?;
    Ok(n == 4 && magic == *b"\x7fELF")
}

/// Split the stub-consumed flags from the app arguments.
/// `--network-unfiltered`, `--panpak-open <path>`, `--panpak-open-write` and
/// `--panpak-open-rename` never reach the application. The path is
/// canonicalized here, so the Landlock rule always covers the real target.
type StubFlags = (bool, Option<(PathBuf, OpenGrant)>, Vec<String>);
fn split_stub_flags(args: Vec<String>) -> Result<StubFlags> {
    let mut allow_unfiltered = false;
    let mut open_path: Option<String> = None;
    let mut open_write = false;
    let mut open_rename = false;
    let mut app_args = Vec::with_capacity(args.len());
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--network-unfiltered" {
            allow_unfiltered = true;
        } else if a == "--panpak-open" {
            open_path = Some(it.next().ok_or_else(|| coded(ErrorCode::E5002, "--panpak-open requires a path"))?);
        } else if a == "--panpak-open-write" {
            open_write = true;
        } else if a == "--panpak-open-rename" {
            open_rename = true;
        } else {
            app_args.push(a);
        }
    }
    let open_file = match open_path {
        Some(p) => {
            let abs = std::fs::canonicalize(&p).context("check the path").with_context(|| format!("open: cannot open file: {p}"))?;
            Some((abs, OpenGrant { write: open_write || open_rename, rename: open_rename }))
        }
        None => None,
    };
    if (open_write || open_rename) && open_file.is_none() {
        bail_code!(ErrorCode::E5001, "--panpak-open-write/--panpak-open-rename without --panpak-open");
    }
    Ok((allow_unfiltered, open_file, app_args))
}

fn main() {
    if let Err(e) = real_main() {
        if panpak_core::is_storage_full(&e) {
            eprintln!("out of space: free disk space and retry");
        }
        eprintln!("{}", panpak_core::render(&e, ErrorCode::E2000));
        std::process::exit(1);
    }
}

/// The id of an installed app from the path of its package (`<store>/apps/<id>/current`), when the path has that shape.
fn installed_app_id(pkg: &Path) -> Option<(String, PathBuf)> {
    let dir = pkg.parent()?;
    let id = dir.file_name()?.to_str()?;
    let apps = dir.parent()?;
    if apps.file_name()? != "apps" || panpak_core::validate_app_id(id).is_err() {
        return None;
    }
    Some((id.to_string(), apps.to_path_buf()))
}

/// Take the shared lock of the copies of an installed app, then check that the app is still installed.
/// `None` when the path is not that of an installed app or there is no runtime folder.
fn start_lock(pkg: &Path) -> Result<Option<File>> {
    let Some((id, apps)) = installed_app_id(pkg) else { return Ok(None) };
    let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute()) else { return Ok(None) };
    let lock = sharedtmp::early_lock(&rt, &id)?;
    // `panpak remove` may have taken the app away while this start waited for the lock.
    match fs::symlink_metadata(apps.join(&id)) {
        Ok(m) if m.is_dir() => {}
        _ => bail_code!(ErrorCode::E2004, "{id} was removed (or is being removed): nothing to start"),
    }
    Ok(Some(lock))
}

fn real_main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    // In the isolated view this stub is also `xdg-open` (see opener.rs): hand the address to the outer stub.
    if args.first().is_some_and(|a| opener::is_opener_name(a)) {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute());
        std::process::exit(opener::client(&args[1..], runtime.as_deref()));
    }
    // --abi-version: for `panpak install-runtime` (works on a bare stub too).
    if args.get(1).map(|s| s.as_str()) == Some("--abi-version") {
        println!("{}", panpak_core::RUNTIME_ABI);
        return Ok(());
    }
    // Inside the isolated view: the outer stub already verified the package (see view.rs).
    if args.get(1).map(|s| s.as_str()) == Some("--panpak-inner") {
        let fd: i32 = args
            .get(2)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| coded(ErrorCode::E5002, "usage: panpak-stub --panpak-inner <fd>"))?;
        return run_inner(fd);
    }
    // Two modes: self-contained package (current_exe) or installed runtime
    // (`stub run <package> [args...]`, used by `panpak install` launchers).
    let installed_mode = args.get(1).map(|s| s.as_str()) == Some("run");
    let (pkg, app_args): (PathBuf, Vec<String>) = if installed_mode {
        let p = args.get(2).ok_or_else(|| coded(ErrorCode::E5002, "usage: panpak-stub run <package> [args]"))?.into();
        (p, args.into_iter().skip(3).collect())
    } else {
        (std::env::current_exe()?, args.into_iter().skip(1).collect())
    };
    // Before anything of the app is read from the store: the shared lock of the copies. `panpak remove` holds the
    // exclusive one while it takes the app away, so a start in that moment waits and then finds the app gone.
    let early = if installed_mode { start_lock(&pkg)? } else { None };
    // `--panpak-licenses` (a stub flag, like --panpak-open): print the
    // embedded license texts of the verified package and exit.
    if app_args.iter().any(|a| a == "--panpak-licenses") {
        let mut f = File::open(&pkg)?;
        let ft = read_footer(&mut f)?;
        let (_man, raw_index) = verify(&mut f, &ft)?;
        let files = panpak_core::read_package_licenses(&mut f, &raw_index)?;
        ensure!(!files.is_empty(), "package has no embedded licenses");
        for (name, data) in files {
            println!("===== {name} =====");
            println!("{}", String::from_utf8_lossy(&data));
        }
        return Ok(());
    }
    // Stub flags (never the app's), removed before exec:
    // --network-unfiltered, --panpak-open <path> [--panpak-open-write|--panpak-open-rename].
    let (allow_unfiltered, open_file, app_args) = split_stub_flags(app_args)?;
    let mut f = File::open(&pkg)?;
    let ft = read_footer(&mut f)?;
    let (man, raw_index) = verify(&mut f, &ft)?;
    panpak_core::check_runtime(&man, panpak_core::RUNTIME_ABI)?;

    let store = dirs::data_dir().context("no data dir")?.join("panpak");
    // TOFU (spec §3.2): if the app is installed (pinned key), the signature
    // must match the pin; on direct run show the fingerprint.
    let key = &man.update.as_ref().context("manifest without [update].key")?.key;
    let fp = panpak_core::fingerprint(&panpak_core::parse_pubkey(key)?);
    let pin_path = store.join("apps").join(&man.app.id).join("pinned-key");
    if installed_mode {
        let pinned = fs::read_to_string(&pin_path).context(
            "app not installed (no TOFU pin): use `panpak install`",
        )?;
        ensure_code!(
            pinned.trim() == key.trim(),
            ErrorCode::E1004,
            "key differs from TOFU pin: unauthorized update, reinstall"
        );
    } else {
        eprintln!("panpak: {} {} — key fingerprint: {fp}", man.app.id, man.app.version);
    }
    let (chunks, entries) = parse_index(&raw_index)?;
    // The stub being run (ourselves in direct-mode, never the one in the file in
    // run-mode) must match the signed one: no other stub runs.
    let stub_len = chunks.iter().map(|c| c.off).min().unwrap_or(ft.index_off);
    panpak_core::verify_stub_range(&mut f, stub_len, &man)?;
    let tree = materialize(&mut f, &store, &ft.root, &chunks, &entries)?;

    // On cache hit, no bundle inventory or transitive parsing when warm.
    // Signature/pin/stub/index verified above; Landlock applied below.
    let plan = resolution::resolve(&store, &pkg, &hex(&ft.root), &tree, &man)?;
    let ld_path =
        std::env::join_paths(plan.ld_path.iter()).context("non-UTF-8 LD_LIBRARY_PATH")?;

    // Private folders (0700), created before the sandbox closes the rest of the disk.
    let app_dirs = panpak_core::app_dirs(&man.app.id)?;
    app_dirs.create()?;
    write_fonts_conf(&app_dirs)?;
    let mut access = access_for(&man, &app_dirs)?;
    let granted = xdg_grants(&man, &store, installed_mode, &mut access)?;
    if !granted.is_empty() || man.permissions.as_ref().and_then(|p| p.xdg_dirs.as_ref()).is_some_and(|d| !d.is_empty()) {
        write_user_dirs(&app_dirs, &granted)?;
    }
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute());
    access.sockets = panpak_core::implied_sockets(
        man.permissions.as_ref(),
        runtime_dir.as_deref(),
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
    )
    .into_iter()
    .filter(|p| p.exists())
    .collect();
    let (config_dirs, data_dirs) =
        panpak_core::xdg_search_dirs(std::env::var("XDG_CONFIG_DIRS").ok(), std::env::var("XDG_DATA_DIRS").ok())?;
    // Socket probe (measurement only, off unless asked): the log is opened before Landlock closes the disk.
    let probe_log = match std::env::var_os("PANPAK_SOCKET_PROBE").filter(|v| !v.is_empty()) {
        #[cfg(target_arch = "x86_64")]
        Some(path) => Some(probe::open_log(Path::new(&path))?),
        #[cfg(not(target_arch = "x86_64"))]
        Some(_) => bail!("PANPAK_SOCKET_PROBE is only available on x86-64"),
        None => None,
    };
    // Before Landlock: `gsettings` and dconf are outside the sandbox.
    let dark_theme = gtk_dark_theme(find_gsettings().as_deref(), GSETTINGS_TIMEOUT);
    let launch = man.launch.as_ref();
    let launch_args = launch
        .and_then(|l| l.args.as_deref())
        .unwrap_or_default()
        .iter()
        .map(|a| app_dirs.expand(a))
        .collect::<Result<Vec<_>>>()?;
    let launch_env = launch
        .and_then(|l| l.env.as_ref())
        .into_iter()
        .flatten()
        .map(|(k, v)| Ok((k.clone(), app_dirs.expand(v)?)))
        .collect::<Result<Vec<_>>>()?;

    let mut cmd = match &plan.loader {
        // Included glibc: all of user-space uses it, loader included.
        // BUT only if the entry is an ELF: a script passed to ld.so would die
        // with "invalid ELF header" (found with the GTK test: entry was a wrapper).
        Some(loader) if is_elf(&tree.join(&man.app.entry))? => {
            let mut c = Command::new(loader);
            c.arg("--library-path").arg(&ld_path);
            c.arg(tree.join(&man.app.entry));
            c
        }
        _ => Command::new(tree.join(&man.app.entry)),
    };
    // HOME and XDG_RUNTIME_DIR stay as they are. The app writes only to its private folders.
    cmd.env("XDG_CONFIG_HOME", &app_dirs.config)
        .env("XDG_DATA_HOME", &app_dirs.data)
        .env("XDG_STATE_HOME", &app_dirs.state)
        .env("XDG_CACHE_HOME", &app_dirs.cache)
        .env("TMPDIR", &app_dirs.tmp)
        .env("SQLITE_TMPDIR", &app_dirs.tmp)
        .env("XDG_CONFIG_DIRS", config_dirs)
        .env("XDG_DATA_DIRS", data_dirs)
        .envs(launch_env);
    hide_local_sockets(
        &mut cmd,
        panpak_core::allows_session_bus(man.permissions.as_ref()),
        panpak_core::allows_system_bus(man.permissions.as_ref()),
    );
    if let Some(theme) = dark_theme {
        cmd.env("GTK_THEME", theme);
    }
    cmd.args(&launch_args)
        .args(&app_args)
        .env("PANPAK_APP_ID", &man.app.id)
        .env("LD_LIBRARY_PATH", &ld_path)
        .env_remove("PANPAK_SOCKET_PROBE");
    let open_arg = open_file.as_ref().map(|(p, w)| (p.as_path(), *w));
    #[cfg(target_arch = "x86_64")]
    if let Some(log) = probe_log {
        // The probe measures the sandbox as it is without the view. The app runs as a child and
        // this process stays as the supervisor.
        sandbox(&tree, &man, &access, allow_unfiltered, open_arg)?;
        std::process::exit(probe::run(cmd, log)?);
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = probe_log;
    // Outer stub -> bubblewrap -> inner stub (Landlock) -> app. Without a usable bubblewrap the
    // stub applies Landlock itself and starts the app, as it always did.
    if panpak_core::view_status().usable() {
        // Only with the view: its home folder is an empty tmpfs, so listing it shows nothing else.
        // Only a convenience (the home folder shows the granted folders when listed): without it the listing is empty.
        access.home_list = dirs::home_dir().and_then(|h| fs::canonicalize(h).ok()).filter(|h| h.is_dir() && h != Path::new("/"));
        let mut store_paths = plan.ld_path.clone();
        store_paths.extend(plan.loader.clone());
        store_paths.push(PathBuf::from(cmd.get_program()));
        let ctx = ViewStart { tree: &tree, man: &man, access: &access, allow_unfiltered, open: open_file.as_ref(), runtime_dir: runtime_dir.as_deref(), early_lock: early.as_ref(), store_paths };
        enter_view(&ctx, &mut cmd)?;
    }
    sandbox(&tree, &man, &access, allow_unfiltered, open_arg)?;
    // Without the view this process becomes the app: the lock must go on to hold while the app runs.
    if let Some(f) = &early {
        // SAFETY: fcntl on a descriptor owned by `f`; the descriptor stays open across the exec.
        unsafe {
            let fl = libc::fcntl(f.as_raw_fd(), libc::F_GETFD);
            if fl >= 0 {
                libc::fcntl(f.as_raw_fd(), libc::F_SETFD, fl & !libc::FD_CLOEXEC);
            }
        }
    }
    let err = cmd.exec(); // Replaces the process.
    bail!("exec failed: {err}")
}

/// What `enter_view` needs from `real_main`.
struct ViewStart<'a> {
    tree: &'a Path,
    man: &'a panpak_core::Manifest,
    access: &'a Access,
    allow_unfiltered: bool,
    open: Option<&'a (PathBuf, OpenGrant)>,
    runtime_dir: Option<&'a Path>,
    /// The shared lock taken at the start of an installed app (see `start_lock`).
    early_lock: Option<&'a File>,
    /// What the inner stub and the app read from the store: library folders and the loader.
    store_paths: Vec<PathBuf>,
}

/// Start the app in the isolated view: build the job for the inner stub and replace this
/// process with `bwrap`. Returns only on error.
fn enter_view(ctx: &ViewStart, cmd: &mut Command) -> Result<()> {
    let perms = ctx.man.permissions.as_ref();
    let x11 = perms.is_some_and(|p| p.x11 == Some(true));
    // The private /tmp is in the view: the app's TMPDIR is /tmp there (short, and not shared).
    cmd.env("TMPDIR", "/tmp").env("SQLITE_TMPDIR", "/tmp");
    // Sockets: the ones the manifest lists and the ones every app gets. Only existing ones.
    let mut sockets = Vec::new();
    for s in perms.and_then(|p| p.sockets_allow.as_ref()).into_iter().flatten() {
        let path = panpak_core::expand_socket_path(s)?;
        if path.exists() {
            sockets.push(path);
        }
    }
    sockets.extend(ctx.access.sockets.iter().cloned());
    let mut ro_run_dirs = Vec::new();
    for d in [Some(PathBuf::from("/run/systemd/resolve")), panpak_core::resolv_conf_run_dir()].into_iter().flatten() {
        if d.is_dir() {
            ro_run_dirs.push(d);
        }
    }
    let runtime_text = ctx.runtime_dir.and_then(Path::to_str).filter(|t| t.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))).map(str::to_string);
    let display = std::env::var("DISPLAY").ok();
    let x11_sockets: Vec<PathBuf> = if x11 { view::x11_socket(display.as_deref()).into_iter().collect() } else { Vec::new() };
    // The home folder is an empty tmpfs in the view. Put back only what the app and the inner stub
    // need: the tree, the library folders and the loader of the store, the stub, the private
    // folders and `rw` home paths, the settings Landlock allows to read, the files of `panpak open`
    // and, with X11, the Xauthority file. Paths outside the home folder are mounted too, unless they are in the system folders.
    let home = dirs::home_dir().and_then(|h| fs::canonicalize(h).ok()).filter(|h| h.is_dir() && h != Path::new("/"));
    let in_home = |p: &Path| home.as_ref().is_some_and(|h| p.starts_with(h));
    let real = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let stub = std::env::current_exe().context("path of the stub")?;
    let mut ro_wanted: Vec<PathBuf> = vec![ctx.tree.to_path_buf(), stub.clone()];
    ro_wanted.extend(ctx.store_paths.iter().cloned());
    ro_wanted.extend(ctx.access.ro.iter().cloned());
    let mut xauthority = None;
    if x11 {
        xauthority = std::env::var_os("XAUTHORITY")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".Xauthority")))
            .filter(|p| p.is_file());
        ro_wanted.extend(xauthority.clone());
    }
    let mut binds: Vec<view::Bind> = ctx.access.rw.iter().map(|p| view::Bind { path: real(p), rw: true }).collect();
    if let Some((path, grant)) = ctx.open {
        // The file is writable in the view only if the grant lets the app write it.
        if grant.rename {
            binds.extend(view::existing_parent(path).map(|p| view::Bind { path: real(&p), rw: true }));
        } else if grant.write {
            binds.push(view::Bind { path: real(path), rw: true });
        } else {
            ro_wanted.push(path.clone());
        }
    }
    for p in ro_wanted {
        let p = real(&p);
        // Read-only paths in the system folders of the view are visible as they are; any other is mounted.
        if p.exists() && (in_home(&p) || ctx.runtime_dir.is_some_and(|rt| p.starts_with(rt)) || !view::in_system_view(&p)) {
            binds.push(view::Bind { path: p, rw: false });
        }
    }
    let binds = view::merge_binds(binds);
    let (mut mounts, opener_server) = open_channel(ctx, cmd, &stub, runtime_text.as_deref());
    // The filtered session bus, when this system can give it. Before the sockets and the job are
    // fixed: it adds mounts, variables and one path Landlock must allow.
    let dbus = ctx.runtime_dir.and_then(|rt| match dbus_layer::start(&dbus_layer::Request { man: ctx.man, runtime_dir: rt }) {
        Ok(l) => Some(l),
        Err(e) => {
            if e.downcast_ref::<dbus_layer::Quiet>().is_none() {
                eprintln!("panpak: WARNING: no D-Bus for the app (portals, notifications): {}", panpak_core::err_text(&e));
            }
            None
        }
    });
    if let Some(l) = &dbus {
        mounts.extend(l.mounts.iter().cloned());
        for (k, v) in &l.env {
            cmd.env(k, v);
        }
    }
    // /tmp and /dev/shm are shared by the copies of this app that run together, and by no other app.
    let app_dirs = match panpak_core::app_dirs(&ctx.man.app.id) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("panpak: WARNING: app folders unavailable ({}); no shared /tmp, no stale lock search", panpak_core::err_text(&e));
            None
        }
    };
    let shared = ctx.runtime_dir.zip(app_dirs.as_ref()).and_then(|(rt, dirs)| match sharedtmp::prepare(&rt.join("panpak").join(&ctx.man.app.id), &panpak_core::stalelocks::in_use_marker(dirs), ctx.early_lock.as_ref().and_then(|f| f.try_clone().ok())) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("panpak: shared /tmp off, using a private one: {}", panpak_core::err_text(&e));
            None
        }
    });
    // No other copy is alive and the last run did not end normally (it was killed, or the machine went
    // down): lock files it left hold process ids of a view that no longer exists, and the new view starts
    // counting again. After a normal end there is nothing to look for.
    if let (Some(_), Some(dirs)) = (shared.as_ref().filter(|s| s.stale), app_dirs.as_ref()) {
        let roots = panpak_core::stalelocks::roots_of(dirs);
        for gone in panpak_core::stalelocks::remove(&roots.iter().map(PathBuf::as_path).collect::<Vec<_>>()) {
            eprintln!("panpak: removed a stale lock file: {}", gone.display());
        }
    }
    let plan = view::ViewPlan {
        root: view::root_layout(Path::new("/")),
        shared_tmp: shared.as_ref().map(|s| s.tmp.clone()),
        shared_shm: shared.as_ref().map(|s| s.shm.clone()),
        mounts,
        runtime_dir: ctx.runtime_dir.map(Path::to_path_buf),
        sockets: view::unique(sockets),
        ro_run_dirs: view::unique(ro_run_dirs),
        x11: x11_sockets,
        home,
        binds,
        x11_access: x11,
        no_network: !panpak_core::network_requested(ctx.man.permissions.as_ref()),
        stub,
        fd: -1,
    };
    // Landlock inside the view also lets the app write in its runtime folder (a tmpfs of its own).
    let mut job_rw = ctx.access.rw.clone();
    job_rw.extend(ctx.runtime_dir.map(Path::to_path_buf));
    // With X11 access the app reads the Xauthority file, which is put back into the view above.
    let mut job_ro = ctx.access.ro.clone();
    job_ro.extend(xauthority);
    let parts = view::Job::command_of(cmd)?;
    let job = view::Job {
        version: view::JOB_VERSION,
        tree: ctx.tree.to_path_buf(),
        manifest: ctx.man.clone(),
        rw: job_rw,
        ro: job_ro,
        sockets: ctx.access.sockets.iter().cloned().chain(dbus.iter().flat_map(|l| l.landlock_sockets.iter().cloned())).collect(),
        allow_unfiltered: ctx.allow_unfiltered,
        home_list: ctx.access.home_list.clone(),
        open: ctx.open.map(|(p, g)| view::JobOpen { path: p.clone(), write: g.write, rename: g.rename }),
        program: parts.program,
        args: parts.args,
        env_set: parts.env_set,
        env_remove: parts.env_remove,
    };
    let fd = view::job_memfd(&job)?;
    let plan = view::ViewPlan { fd, ..plan };
    // The lock of the shared folders must outlive bwrap, so the outer stub stays whenever there is one.
    match (opener_server, shared, dbus) {
        (None, None, None) => match view::exec_bwrap(&plan)? {},
        (server, shared, dbus) => match view::run_bwrap(&plan, (server, shared, dbus), |(_, shared, _), clean| {
            if let (true, Some(s)) = (clean, shared) {
                s.ended_normally();
            }
        })? {},
    }
}

/// Set up the way to open addresses from the view: the socket and the server (outer side), the stub
/// as `xdg-open` and its relatives (links first in `PATH`, and over the system's own `xdg-open`, so
/// that a full path is covered too), `BROWSER`, and a private `mimeapps.list` for GIO. Needs the
/// real `xdg-open` in the `PATH` of this process and a plain runtime folder; otherwise nothing is set
/// up and the view has no `xdg-open` that works. Failures only turn the channel off.
fn open_channel(ctx: &ViewStart, cmd: &mut Command, stub: &Path, runtime: Option<&str>) -> (Vec<view::Mount>, Option<opener::Server>) {
    let none = (Vec::new(), None);
    let (Some(rt), Some(path)) = (runtime, std::env::var_os("PATH")) else { return none };
    let Some(real) = opener::find_in_path("xdg-open", &path) else {
        eprintln!("panpak: no xdg-open found in PATH; the app cannot open addresses");
        return none;
    };
    let dir = Path::new(rt).join("panpak").join(&ctx.man.app.id);
    let server = match opener::Server::start(&dir, opener::SystemXdgOpen::new(real)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("panpak: opener channel off: {}", panpak_core::err_text(&e));
            return none;
        }
    };
    let rtp = Path::new(rt);
    let mut mounts = vec![view::Mount::ro(server.socket().to_path_buf(), rtp.join(opener::SOCKET))];
    for n in opener::NAMES {
        mounts.push(view::Mount::ro(stub.to_path_buf(), rtp.join(opener::BIN_DIR).join(n)));
    }
    // Over every `xdg-open` the system has in its PATH, outside the home folder.
    let home = dirs::home_dir();
    for d in std::env::split_paths(&path).filter(|d| d.is_absolute() && home.as_ref().is_none_or(|h| !d.starts_with(h))) {
        let p = d.join("xdg-open");
        if fs::symlink_metadata(&p).is_ok() {
            mounts.push(view::Mount::ro(stub.to_path_buf(), p));
        }
    }
    let bin = format!("{rt}/{}", opener::BIN_DIR);
    let old = cmd.get_envs().find(|(k, _)| *k == "PATH").and_then(|(_, v)| v.map(|v| v.to_os_string())).unwrap_or(path);
    let mut new = std::ffi::OsString::from(format!("{bin}:"));
    new.push(old);
    cmd.env("PATH", new).env("BROWSER", format!("{bin}/xdg-open"));
    if let Err(e) = write_opener_config(&ctx.man.app.id, &format!("{bin}/xdg-open")) {
        eprintln!("panpak: GIO opener setup failed: {}", panpak_core::err_text(&e));
    }
    (mounts, Some(server))
}

const OPENER_MARKER: &str = "X-Panpak-Generated";

/// The private `mimeapps.list` and `.desktop` file that make GIO (`g_app_info_launch_default_for_uri`,
/// GTK, `gio open`) run the opener of the view for web and mail addresses instead of a browser of the
/// system. A file without the marker is left alone.
fn write_opener_config(app_id: &str, exec: &str) -> Result<()> {
    let dirs = panpak_core::app_dirs(app_id)?;
    let ids = ["x-scheme-handler/http", "x-scheme-handler/https", "x-scheme-handler/mailto", "text/html"];
    let mime: String = ids.iter().map(|m| format!("{m};")).collect();
    let list: String = ids.iter().map(|m| format!("{m}=panpak-opener.desktop\n")).collect();
    let files = [
        (dirs.config.join("mimeapps.list"), format!("# {OPENER_MARKER}: written at every start.\n[Default Applications]\n{list}")),
        (
            dirs.data.join("applications").join("panpak-opener.desktop"),
            format!("# {OPENER_MARKER}\n[Desktop Entry]\nType=Application\nName=Link opener\nExec={exec} %u\nMimeType={mime}\nNoDisplay=true\nTerminal=false\n"),
        ),
    ];
    for (path, want) in files {
        match fs::read_to_string(&path) {
            // Already as wanted, or the user's own file: leave it.
            Ok(cur) if cur == want || !cur.contains(OPENER_MARKER) => continue,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        }
        fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        fs::write(&tmp, want)?;
        fs::rename(&tmp, &path)?;
    }
    Ok(())
}

/// The inner stub: Landlock, then the app. The job comes from the outer stub, which verified the package.
fn run_inner(fd: i32) -> Result<()> {
    let job = view::read_job(fd)?;
    let access = Access { rw: job.rw.clone(), ro: job.ro.clone(), sockets: job.sockets.clone(), in_view: true, home_list: job.home_list.clone() };
    let open = job.open.as_ref().map(|o| (o.path.as_path(), OpenGrant { write: o.write, rename: o.rename }));
    sandbox(&job.tree, &job.manifest, &access, job.allow_unfiltered, open)?;
    let err = job.to_command().exec();
    bail!("exec failed: {err}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    use panpak_core::{encode_footer, encode_index, Footer, FOOTER_LEN, MAGIC};

    fn test_keypair() -> (ed25519_dalek::SigningKey, String) {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let vk = sk.verifying_key();
        (sk, format!("ed25519:{}", hex(vk.as_bytes())))
    }

    /// TOML of test panpaks, with [runtime].stub_hash matching the fake stub.
    fn fake_toml(key: &str, manifest_extra: &str) -> Vec<u8> {
        let stub_hash = hex(blake3::hash(b"FAKE-STUB").as_bytes());
        format!(
            "[app]\nid = \"org.example.hello\"\nname = \"Hello\"\nversion = \"0.1.0\"\nentry = \"bin/hello\"\n{manifest_extra}[runtime]\nstub_hash = \"{stub_hash}\"\n[update]\nkey = \"{key}\"\n"
        )
        .into_bytes()
    }

    /// Input for `sign_and_assemble` (struct instead of 8 positional params).
    struct AssembleIn<'a> {
        dir: &'a Path,
        name: &'a str,
        sk: &'a ed25519_dalek::SigningKey,
        key: &'a str,
        chunks: &'a [Chunk],
        chunk_blobs: &'a [Vec<u8>],
        entries: &'a [Entry],
        manifest_extra: &'a str,
    }

    fn sign_and_assemble(p: AssembleIn) -> PathBuf {
        let raw_index = encode_index(p.chunks, p.entries);
        let root = *blake3::hash(&raw_index).as_bytes();
        let toml = fake_toml(p.key, p.manifest_extra);
        let mut msg = Vec::with_capacity(toml.len() + 32);
        msg.extend_from_slice(&toml);
        msg.extend_from_slice(&root);
        let sig = p.sk.sign(&msg).to_bytes();

        let comp_index = zstd::encode_all(&raw_index[..], 0).unwrap();
        let stub = b"FAKE-STUB";
        let index_off = stub.len() as u64 + p.chunk_blobs.iter().map(|b| b.len()).sum::<usize>() as u64;
        let man_off = index_off + comp_index.len() as u64;
        let mut out = Vec::new();
        out.extend_from_slice(stub);
        for b in p.chunk_blobs {
            out.extend_from_slice(b);
        }
        out.extend_from_slice(&comp_index);
        out.extend_from_slice(&toml);
        out.extend_from_slice(&sig);
        let ft = Footer {
            index_off,
            index_len: comp_index.len() as u64,
            man_off,
            man_len: toml.len() as u64,
            root,
        };
        out.extend_from_slice(&encode_footer(&ft));
        assert_eq!(&out[out.len() - 80..out.len() - 72], MAGIC);
        assert_eq!(out.len() as u64 - 80, man_off + toml.len() as u64 + 64);

        let path = p.dir.join(p.name);
        fs::write(&path, &out).unwrap();
        path
    }

    /// Minimal valid .panpak file: empty index, no chunks.
    fn fake_panpak(dir: &Path, sk: &ed25519_dalek::SigningKey, key: &str) -> (PathBuf, Vec<u8>, [u8; 32]) {
        let raw_index = encode_index(&[], &[]);
        let root = *blake3::hash(&raw_index).as_bytes();
        let path = sign_and_assemble(AssembleIn { dir, name: "fake.panpak", sk, key, chunks: &[], chunk_blobs: &[], entries: &[], manifest_extra: "" });
        let toml = fake_toml(key, "");
        (path, toml, root)
    }

    /// .panpak file with one real single-chunk file: for materialize tests.
    fn fake_panpak_with_file(dir: &Path, data: &[u8]) -> (PathBuf, [u8; 32], Vec<Chunk>, Vec<Entry>) {
        let (sk, key) = test_keypair();
        let comp = zstd::encode_all(data, 3).unwrap();
        let chunks = vec![Chunk {
            hash: *blake3::hash(data).as_bytes(),
            off: b"FAKE-STUB".len() as u64,
            comp: comp.len() as u32,
            raw: data.len() as u32,
        }];
        let entries = vec![Entry {
            path: PathBuf::from("bin/hello"),
            mode: 0o755,
            kind: Kind::File { chunks: vec![0] },
        }];
        let raw_index = encode_index(&chunks, &entries);
        let root = *blake3::hash(&raw_index).as_bytes();
        let path = sign_and_assemble(AssembleIn { dir, name: "one.panpak", sk: &sk, key: &key, chunks: &chunks, chunk_blobs: &[comp], entries: &entries, manifest_extra: "" });
        (path, root, chunks, entries)
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("panpak-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Fixture directory outside the paths covered by broad rules
    /// (`/tmp`, appdata, tree): needed to prove the `open` rule covers
    /// only the exact file. `/var/tmp` stays default-deny.
    fn outside_tmpdir(tag: &str) -> PathBuf {
        let dir = PathBuf::from("/var/tmp").join(format!("panpak-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Fixture: signed .panpak (manifest_extra for [permissions]...), tree
    /// materialized, appdata. Returns (dir, panpak, tree, appdata).
    fn fixture_tree(tag: &str, manifest_extra: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let dir = tmpdir(tag);
        let (panpak, tree, appdata) = fixture_tree_in(&dir, manifest_extra);
        (dir, panpak, tree, appdata)
    }

    /// Variant on a pre-existing dir (does not delete it: needed to bind
    /// sockets inside store/ before signing the manifest that lists them.
    fn fixture_tree_in(dir: &Path, manifest_extra: &str) -> (PathBuf, PathBuf, PathBuf) {
        let (sk, key) = test_keypair();
        let data = b"do-not-modify";
        let comp = zstd::encode_all(&data[..], 3).unwrap();
        let chunks = vec![Chunk {
            hash: *blake3::hash(&data[..]).as_bytes(),
            off: b"FAKE-STUB".len() as u64,
            comp: comp.len() as u32,
            raw: data.len() as u32,
        }];
        let entries = vec![Entry {
            path: PathBuf::from("bin/hello"),
            mode: 0o755,
            kind: Kind::File { chunks: vec![0] },
        }];
        let panpak = sign_and_assemble(AssembleIn { dir, name: "t.panpak", sk: &sk, key: &key, chunks: &chunks, chunk_blobs: &[comp], entries: &entries, manifest_extra });
        let store = dir.join("store");
        let mut f = File::open(&panpak).unwrap();
        let ft = read_footer(&mut f).unwrap();
        let tree = materialize(&mut f, &store, &ft.root, &chunks, &entries).unwrap();
        let appdata = dir.join("appdata");
        fs::create_dir_all(&appdata).unwrap();
        (panpak, tree, appdata)
    }

    /// Run `body` in a forked child (the parent stays free);
    /// return the lines written to `results` (inside a writable dir).
    fn fork_lines<F>(results: &Path, body: F) -> String
    where
        F: FnOnce(&mut Vec<String>) -> Result<(), &'static str>,
    {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let code: Result<(), &'static str> = (|| {
                let mut res = Vec::new();
                body(&mut res)?;
                fs::write(results, res.concat()).map_err(|_| "results")?;
                Ok(())
            })();
            unsafe { libc::_exit(i32::from(code.is_err()) * 42) };
        }
        let mut status = 0;
        loop {
            let w = unsafe { libc::waitpid(pid, &mut status, 0) };
            if w == pid {
                break;
            }
            assert!(w >= 0, "waitpid failed");
        }
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "child failed (status={status})"
        );
        fs::read_to_string(results).unwrap()
    }

    /// Panpak manifest + sandbox applied in the child. Returns (man).
    fn child_sandboxed(
        res: &mut Vec<String>,
        panpak: &Path,
        tree: &Path,
        appdata: &Path,
        open: Option<(&Path, OpenGrant)>,
    ) -> Result<panpak_core::Manifest, &'static str> {
        let mut f = File::open(panpak).map_err(|_| "open panpak")?;
        let ft = read_footer(&mut f).map_err(|_| "footer")?;
        let (man, _) = verify(&mut f, &ft).map_err(|_| "verify")?;
        let access = Access { rw: vec![appdata.to_path_buf()], ..Access::default() };
        let st = apply_landlock(tree, &access, &man, false, open).map_err(|_| "apply")?;
        if st.ruleset == landlock::RulesetStatus::NotEnforced {
            return Err("notenforced");
        }
        let _ = res;
        Ok(man)
    }

    fn denied(res: &mut Vec<String>, name: &str, failed: bool) {
        res.push(format!("{name}={}\n", if failed { "deny" } else { "ALLOW" }));
    }

    /// Skip explicitly (with reason) if the kernel ABI is too old.
    fn require_abi(min: landlock::ABI, what: &str) -> bool {
        match panpak_core::detect_abi() {
            Ok(a) if a >= min => true,
            other => {
                eprintln!("SKIP {what}: detected ABI {other:?}, requires {min:?}");
                false
            }
        }
    }

    #[test]
    fn sandbox_network_denied_by_default_and_allowlist() {
        if !crate::testutil::alone() {
            return;
        }
        use std::net::TcpListener;
        if !require_abi(landlock::ABI::V4, "TCP network") {
            return;
        }
        // Parent listener on loopback: the child tries to connect.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (dir, panpak, tree, appdata) = fixture_tree("net-deny", "");
        unsafe { std::env::set_var("PANPAK_TPORT", port.to_string()) };
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, None)?;
            let p: u16 = std::env::var("PANPAK_TPORT").unwrap().parse().unwrap();
            denied(res, "tcp_default", std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], p)),
                std::time::Duration::from_secs(2),
            ).is_err());
            Ok(())
        });
        assert!(res.contains("tcp_default=deny"), "TCP not filtered:\n{res}");
        // listener stays alive for the allow part (same port in allowlist).

        // With an allowlist on the port: the connect passes, the others do not.
        let extra = format!("[permissions]\nnetwork = true\nnetwork_allow_tcp = [{port}]\n");
        let (dir2, panpak2, tree2, appdata2) = fixture_tree("net-allow", &extra);
        unsafe { std::env::set_var("PANPAK_TPORT2", port.to_string()) };
        let listener2 = TcpListener::bind("127.0.0.1:0").unwrap();
        let other = listener2.local_addr().unwrap().port();
        unsafe { std::env::set_var("PANPAK_TPORT_OTHER", other.to_string()) };
        let res2 = fork_lines(&appdata2.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak2, &tree2, &appdata2, None)?;
            let p: u16 = std::env::var("PANPAK_TPORT2").unwrap().parse().unwrap();
            let o: u16 = std::env::var("PANPAK_TPORT_OTHER").unwrap().parse().unwrap();
            let conn_listed = std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], p)),
                std::time::Duration::from_secs(2),
            );
            let ok_listed = conn_listed.is_ok();
            res.push(format!("tcp_allow={}\n", if ok_listed { "ok" } else { "FAIL" }));
            if !ok_listed {
                res.push(format!("tcp_allow_err={conn_listed:?}\n"));
            }
            denied(res, "tcp_other", std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], o)),
                std::time::Duration::from_secs(2),
            ).is_err());
            Ok(())
        });
        assert!(res2.contains("tcp_allow=ok"), "allowlist does not grant access:\n{res2}");
        assert!(res2.contains("tcp_other=deny"), "unlisted port open:\n{res2}");
        drop(listener);
        drop(listener2);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&dir2);
    }

    #[test]
    fn sandbox_scope_signals_and_abstract_sockets() {
        if !crate::testutil::alone() {
            return;
        }
        if !require_abi(landlock::ABI::V6, "scope") {
            return;
        }
        let parent_pid = std::process::id();
        unsafe { std::env::set_var("PANPAK_PARENT_PID", parent_pid.to_string()) };
        // Parent abstract socket: the (scoped) child must not connect.
        let aname = format!("\0panpak-scope-{}", parent_pid);
        let afd = unsafe {
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
            assert!(fd >= 0, "socket failed");
            let mut addr: libc::sockaddr_un = std::mem::zeroed();
            addr.sun_family = libc::AF_UNIX as u16;
            let bytes = aname.as_bytes();
            for (i, b) in bytes.iter().enumerate() {
                addr.sun_path[i] = *b as i8;
            }
            let len = (std::mem::size_of::<libc::sa_family_t>() + bytes.len()) as u32;
            assert_eq!(libc::bind(fd, &addr as *const _ as *const libc::sockaddr, len), 0, "abstract bind failed");
            assert_eq!(libc::listen(fd, 5), 0);
            fd
        };
        let (dir, panpak, tree, appdata) = fixture_tree("scope", "");
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, None)?;
            // Signals: to self ok, to the parent (out of scope) denied.
            let self_ok = unsafe { libc::kill(std::process::id() as i32, 0) == 0 };
            res.push(format!("sig_self={}\n", if self_ok { "ok" } else { "FAIL" }));
            let ppid: i32 = std::env::var("PANPAK_PARENT_PID").unwrap().parse().unwrap();
            denied(res, "sig_parent", unsafe { libc::kill(ppid, 0) } != 0);
            // Abstract: connection denied by the scope.
            let cfd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
            let aname = format!("\0panpak-scope-{ppid}");
            let conn_ok = unsafe {
                let mut addr: libc::sockaddr_un = std::mem::zeroed();
                addr.sun_family = libc::AF_UNIX as u16;
                for (i, b) in aname.as_bytes().iter().enumerate() {
                    addr.sun_path[i] = *b as i8;
                }
                let len = (std::mem::size_of::<libc::sa_family_t>() + aname.len()) as u32;
                libc::connect(cfd, &addr as *const _ as *const libc::sockaddr, len) == 0
            };
            unsafe { libc::close(cfd) };
            denied(res, "abstract_conn", !conn_ok);
            Ok(())
        });
        assert!(res.contains("sig_self=ok"), "self-signal failed:\n{res}");
        assert!(res.contains("sig_parent=deny"), "out-of-scope signal allowed:\n{res}");
        assert!(res.contains("abstract_conn=deny"), "abstract socket reachable:\n{res}");
        unsafe { libc::close(afd) };
        let _ = dir;
    }

    #[test]
    fn x11_keeps_signal_scope() {
        if !crate::testutil::alone() {
            return;
        }
        if !require_abi(landlock::ABI::V6, "X11 signal scope") { return; }
        let (dir, pkg, tree, appdata) = fixture_tree("x11-signal", "[permissions]\nx11 = true\n");
        let parent = std::process::id() as i32;
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            child_sandboxed(res, &pkg, &tree, &appdata, None)?;
            denied(res, "signal_parent", unsafe { libc::kill(parent, 0) } != 0);
            Ok(())
        });
        assert!(res.contains("signal_parent=deny"), "{res}");
        let _ = dir;
    }

    #[test]
    fn sandbox_unix_allow_path() {
        if !crate::testutil::alone() {
            return;
        }
        if !require_abi(landlock::ABI::V9, "unix path") {
            return;
        }
        use std::os::unix::net::UnixListener;
        // Socket under store/ (outside every RW rule): denied without allow,
        // allowed with allow.
        for (tag, listed) in [("deny", false), ("allow", true)] {
            let dir = tmpdir(&format!("ux-{tag}"));
            // Outside /tmp, which the sandbox may write: a socket there passes without any socket rule.
            let outside = PathBuf::from(format!("/var/tmp/panpak-ux-{}-{tag}", std::process::id()));
            fs::create_dir_all(&outside).unwrap();
            let sock = outside.join("sock");
            let dsock = outside.join("dsock");
            let _listener = UnixListener::bind(&sock).unwrap();
            let _dgram = std::os::unix::net::UnixDatagram::bind(&dsock).unwrap();
            let extra = if listed {
                format!("[permissions]\nsockets_allow = [\"{}\", \"{}\"]\n", sock.display(), dsock.display())
            } else {
                String::new()
            };
            let (panpak, tree, appdata) = fixture_tree_in(&dir, &extra);
            let var = format!("PANPAK_USOCK_{tag}");
            unsafe { std::env::set_var(&var, sock.to_str().unwrap()) };
            let dvar = format!("PANPAK_UDSOCK_{tag}");
            unsafe { std::env::set_var(&dvar, dsock.to_str().unwrap()) };
            let expect = if listed { "ok" } else { "deny" };
            let res = fork_lines(&appdata.join("r.txt"), |res| {
                let _man = child_sandboxed(res, &panpak, &tree, &appdata, None)?;
                let p = std::env::var(&var).unwrap();
                let ok = std::os::unix::net::UnixStream::connect(p).is_ok();
                res.push(format!("ux_{tag}={}\n", if ok { "ok" } else { "deny" }));
                let d = std::env::var(&dvar).unwrap();
                let ok = std::os::unix::net::UnixDatagram::unbound().and_then(|u| u.connect(d)).is_ok();
                res.push(format!("uxd_{tag}={}\n", if ok { "ok" } else { "deny" }));
                Ok(())
            });
            assert!(res.contains(&format!("uxd_{tag}={expect}")), "unix datagram socket {tag}: {res}");
            let _ = fs::remove_dir_all(&outside);
            assert!(res.contains(&format!("ux_{tag}={expect}")), "unix socket {tag}: {res}");
            let _ = dir;
        }
    }

    /// .panpak file with arbitrary symlinks (+ one real file for valid targets).
    fn fake_panpak_with_links(
        dir: &Path,
        name: &str,
        links: &[(&str, &str)],
    ) -> (PathBuf, [u8; 32], Vec<Chunk>, Vec<Entry>) {
        let (sk, key) = test_keypair();
        let data = b"content";
        let comp = zstd::encode_all(&data[..], 3).unwrap();
        let chunks = vec![Chunk {
            hash: *blake3::hash(&data[..]).as_bytes(),
            off: b"FAKE-STUB".len() as u64,
            comp: comp.len() as u32,
            raw: data.len() as u32,
        }];
        let mut entries = vec![Entry {
            path: PathBuf::from("bin/hello"),
            mode: 0o755,
            kind: Kind::File { chunks: vec![0] },
        }];
        for (p, t) in links {
            entries.push(Entry {
                path: PathBuf::from(p),
                mode: 0o777,
                kind: Kind::Link(t.to_string()),
            });
        }
        let raw_index = encode_index(&chunks, &entries);
        let root = *blake3::hash(&raw_index).as_bytes();
        let path = sign_and_assemble(AssembleIn { dir, name, sk: &sk, key: &key, chunks: &chunks, chunk_blobs: &[comp], entries: &entries, manifest_extra: "" });
        (path, root, chunks, entries)
    }

    #[test]
    fn verify_accepts_valid_file() {
        let dir = tmpdir("ok");
        let (sk, key) = test_keypair();
        let (path, _toml, root) = fake_panpak(&dir, &sk, &key);
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        assert_eq!(ft.root, root);
        let (man, raw) = verify(&mut f, &ft).unwrap();
        assert_eq!(man.app.id, "org.example.hello");
        assert_eq!(blake3::hash(&raw).as_bytes(), &root);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_rejects_tampered_manifest() {
        let dir = tmpdir("tamper");
        let (sk, key) = test_keypair();
        let (path, toml, _root) = fake_panpak(&dir, &sk, &key);
        let mut bytes = fs::read(&path).unwrap();
        let pos = bytes.windows(toml.len()).position(|w| w == toml).unwrap();
        bytes[pos + 10] ^= 0x01;
        fs::write(&path, &bytes).unwrap();
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        assert!(verify(&mut f, &ft).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_rejects_non_adjacent_layout() {
        let dir = tmpdir("layout");
        let (sk, key) = test_keypair();
        let (path, _toml, _root) = fake_panpak(&dir, &sk, &key);
        let mut bytes = fs::read(&path).unwrap();
        let mut f = File::open(&path).unwrap();
        let ft0 = read_footer(&mut f).unwrap();
        // Ghost byte between index and manifest, with man_off bumped by +1:
        // index_off+index_len != man_off stays violated.
        bytes.insert((ft0.index_off + ft0.index_len) as usize, 0xFF);
        let fpos = bytes.len() - FOOTER_LEN as usize + 32;
        bytes[fpos..fpos + 8].copy_from_slice(&(ft0.man_off + 1).to_le_bytes());
        fs::write(&path, &bytes).unwrap();
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        assert!(verify(&mut f, &ft).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_rejects_replaced_stub() {
        // Altered stub (first bytes): the signature stays valid but the file
        // must NOT be runnable/installable.
        let dir = tmpdir("stub-tamper");
        let data = b"hello-panpak-contents";
        let (path, _root, chunks, _entries) = fake_panpak_with_file(&dir, data);
        let mut bytes = fs::read(&path).unwrap();
        bytes[3] ^= 0xFF; // inside b"FAKE-STUB"
        fs::write(&path, &bytes).unwrap();
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        // The signature covers manifest+root: still valid (no stub in it).
        let (man, _raw) = verify(&mut f, &ft).unwrap();
        // But the stub identity check fails: no execution.
        let stub_len = chunks.iter().map(|c| c.off).min().unwrap_or(ft.index_off);
        assert!(panpak_core::verify_stub_range(&mut f, stub_len, &man).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Give back the write bit on folders so that a materialized tree can be removed.
    fn make_writable(p: &Path) {
        if let Ok(m) = fs::symlink_metadata(p) {
            if m.is_dir() {
                let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
                for e in fs::read_dir(p).into_iter().flatten().flatten() {
                    make_writable(&e.path());
                }
            }
        }
    }

    /// A package of `n` small files, each with a chunk of its own.
    fn many_files_panpak(dir: &Path, n: usize) -> (PathBuf, [u8; 32], Vec<Chunk>, Vec<Entry>) {
        let (sk, key) = test_keypair();
        let (mut chunks, mut blobs, mut entries) = (Vec::new(), Vec::new(), Vec::new());
        let mut off = b"FAKE-STUB".len() as u64;
        for i in 0..n {
            let data = format!("file number {i} of the regression package").into_bytes();
            let comp = zstd::encode_all(&data[..], 3).unwrap();
            chunks.push(Chunk { hash: *blake3::hash(&data).as_bytes(), off, comp: comp.len() as u32, raw: data.len() as u32 });
            off += comp.len() as u64;
            blobs.push(comp);
            entries.push(Entry { path: PathBuf::from(format!("d{}/f{i}", i % 7)), mode: 0o644, kind: Kind::File { chunks: vec![i as u32] } });
        }
        let raw_index = encode_index(&chunks, &entries);
        let root = *blake3::hash(&raw_index).as_bytes();
        let path = sign_and_assemble(AssembleIn { dir, name: "many.panpak", sk: &sk, key: &key, chunks: &chunks, chunk_blobs: &blobs, entries: &entries, manifest_extra: "" });
        (path, root, chunks, entries)
    }

    /// Defect 19: several starts at once on a store that is not materialized yet. They shared one
    /// `.partial` folder, and one removed it while another was writing (PANPAK-E2000, ENOENT).
    #[test]
    fn parallel_first_starts_all_succeed_and_build_the_tree_once() {
        let dir = tmpdir("mat-par");
        let (path, root, chunks, entries) = many_files_panpak(&dir, 400);
        let store = dir.join("store");
        for round in 0..3 {
            let store = store.join(format!("r{round}"));
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let results: Vec<Result<PathBuf>> = std::thread::scope(|sc| {
                let hs: Vec<_> = (0..8)
                    .map(|_| {
                        let (b, path, chunks, entries, store) = (barrier.clone(), &path, &chunks, &entries, &store);
                        sc.spawn(move || {
                            let mut f = File::open(path).unwrap();
                            b.wait();
                            materialize(&mut f, store, &root, chunks, entries)
                        })
                    })
                    .collect();
                hs.into_iter().map(|h| h.join().unwrap()).collect()
            });
            for r in &results {
                assert!(r.is_ok(), "round {round}: {:?}", r.as_ref().err().map(|e| format!("{e:#}")));
            }
            let tree = results[0].as_ref().unwrap().clone();
            assert!(results.iter().all(|r| r.as_ref().unwrap() == &tree));
            assert!(tree.join(".complete").exists());
            assert_eq!(fs::read(tree.join("d3/f10")).unwrap(), b"file number 10 of the regression package");
            let left: Vec<_> = fs::read_dir(store.join("trees")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.contains("partial")).collect();
            assert!(left.is_empty(), "temporary folders left behind: {left:?}");
        }
        make_writable(&dir);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_rebuilds_file_and_verifies_chunks() {
        let dir = tmpdir("mat-ok");
        let data = b"hello-panpak-contents";
        let (path, root, chunks, entries) = fake_panpak_with_file(&dir, data);
        let mut f = File::open(&path).unwrap();
        let tree = materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).unwrap();
        assert_eq!(fs::read(tree.join("bin/hello")).unwrap(), data);
        assert!(tree.join(".complete").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verification_errors_have_stable_codes() {
        let code = |e: anyhow::Error| panpak_core::code_of(&e, ErrorCode::E2000);
        let dir = tmpdir("codes");
        let (sk, key) = test_keypair();
        let (path, _toml, _root) = fake_panpak(&dir, &sk, &key);
        let good = fs::read(&path).unwrap();
        // Valid package verifies.
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        assert!(verify(&mut f, &ft).is_ok());
        // Flip a byte of the signature (the 64 bytes before the footer).
        let mut bad = good.clone();
        let n = bad.len();
        bad[n - 80 - 1] ^= 0x01;
        let p2 = dir.join("badsig.panpak");
        fs::write(&p2, &bad).unwrap();
        let mut f = File::open(&p2).unwrap();
        let ft = read_footer(&mut f).unwrap();
        assert_eq!(code(verify(&mut f, &ft).unwrap_err()), ErrorCode::E1001);
        // Wrong root in the footer (signature is over manifest || root).
        let mut bad = good.clone();
        bad[n - 80 + 48] ^= 0x01;
        let p3 = dir.join("badroot.panpak");
        fs::write(&p3, &bad).unwrap();
        let mut f = File::open(&p3).unwrap();
        let ft = read_footer(&mut f).unwrap();
        assert_eq!(code(verify(&mut f, &ft).unwrap_err()), ErrorCode::E1001);
        // Wrong magic.
        let mut bad = good.clone();
        bad[n - 80] ^= 0x01;
        let p4 = dir.join("badmagic.panpak");
        fs::write(&p4, &bad).unwrap();
        let mut f = File::open(&p4).unwrap();
        assert_eq!(code(read_footer(&mut f).unwrap_err()), ErrorCode::E0001);
        // A stub that differs from the signed one.
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        let (man, _) = verify(&mut f, &ft).unwrap();
        let e = panpak_core::verify_stub_range(&mut f, 8, &man).unwrap_err();
        assert_eq!(code(e), ErrorCode::E1002);
        // A chunk that does not match its hash.
        let c = Chunk { hash: [1u8; 32], off: 0, comp: 1, raw: 3 };
        assert_eq!(code(panpak_core::verify_chunk(&c, b"abc").unwrap_err()), ErrorCode::E0005);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_rejects_corrupt_chunk() {
        let dir = tmpdir("mat-bad");
        let data = b"hello-panpak-contents-longer-for-cdc";
        let (path, root, chunks, entries) = fake_panpak_with_file(&dir, data);
        // Corrupts one byte of the compressed chunk: hash or zstd must fail.
        let mut bytes = fs::read(&path).unwrap();
        let off = chunks[0].off as usize;
        bytes[off + 2] ^= 0xFF;
        fs::write(&path, &bytes).unwrap();
        let mut f = File::open(&path).unwrap();
        assert!(materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_rejects_absolute_symlink() {
        let dir = tmpdir("link-abs");
        let (path, root, chunks, entries) = fake_panpak_with_links(&dir, "a.panpak", &[("evil", "/etc/passwd")]);
        let mut f = File::open(&path).unwrap();
        assert!(materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_rejects_parent_symlink() {
        let dir = tmpdir("link-dotdot");
        let (path, root, chunks, entries) =
            fake_panpak_with_links(&dir, "d.panpak", &[("sub/evil", "../../x")]);
        let mut f = File::open(&path).unwrap();
        assert!(materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_rejects_composite_symlink_chain() {
        // Individually harmless ("b/passwd", ".."), together they escape the root.
        let dir = tmpdir("link-chain");
        let (path, root, chunks, entries) =
            fake_panpak_with_links(&dir, "c.panpak", &[("a", "b/passwd"), ("b", "..")]);
        let mut f = File::open(&path).unwrap();
        assert!(materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_accepts_relative_symlink() {
        let dir = tmpdir("link-ok");
        let (path, root, chunks, entries) =
            fake_panpak_with_links(&dir, "o.panpak", &[("latest", "bin/hello")]);
        let mut f = File::open(&path).unwrap();
        let tree = materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).unwrap();
        assert_eq!(fs::read_link(tree.join("latest")).unwrap(), PathBuf::from("bin/hello"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_accepts_inner_parent_symlink() {
        // Leading "../" staying inside the root (real Kdenlive case):
        // usr/translations/x -> ../share/... == usr/share/...
        let dir = tmpdir("link-inside");
        let (path, root, chunks, entries) = fake_panpak_with_links(
            &dir,
            "i.panpak",
            &[("usr/translations/karchive6_qt.qm", "../share/locale/ru/LC_MESSAGES/karchive6_qt.qm")],
        );
        let mut f = File::open(&path).unwrap();
        let tree = materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).unwrap();
        assert_eq!(
            fs::read_link(tree.join("usr/translations/karchive6_qt.qm")).unwrap(),
            PathBuf::from("../share/locale/ru/LC_MESSAGES/karchive6_qt.qm")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn abi_parse_ldconfig_and_glob() {
        let sample = "ldconfig mock\n\tlibgtk-3.so.0 (libc6,x86-64) => /lib/x86_64-linux-gnu/libgtk-3.so.0\n\tlibc.so.6 (libc6,x86-64) => /lib/x86_64-linux-gnu/libc.so.6\n";
        let m = abi::parse_ldconfig(sample);
        assert_eq!(m["libgtk-3.so.0"], PathBuf::from("/lib/x86_64-linux-gnu/libgtk-3.so.0"));
        assert!(abi::glob_match("libGL*", "libGL.so.1"));
        assert!(!abi::glob_match("libGL*", "libEGL.so.1"));
        assert!(!abi::glob_match("*vulkan*", "libvulkan.so.1")); // Prefix/suffix only.
        assert!(abi::glob_match("libc.so.6", "libc.so.6"));
    }

    #[test]
    fn abi_decide_host_bundle_prefer() {
        use abi::{decide, Source};
        use std::collections::HashMap;
        let host: HashMap<String, PathBuf> =
            [("a.so", "/lib/a.so"), ("gl.so", "/lib/gl.so")].into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        let bund: HashMap<String, PathBuf> =
            [("a.so", "lib/a.so"), ("b.so", "lib/b.so"), ("gl.so", "lib/gl.so")]
                .into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        // The included copy wins when the package has one; the system copy when it does not.
        let d = decide(&["a.so".into(), "b.so".into(), "c.so".into()], &[], &[], &host_c(&host), &bund).unwrap();
        assert!(d.contains(&("a.so".into(), Source::Bundled("lib/a.so".into()))));
        assert!(d.contains(&("b.so".into(), Source::Bundled("lib/b.so".into()))));
        assert!(d.contains(&("c.so".into(), Source::Host("/lib/c.so".into()))));
        // prefer_host present even in the bundle: stays host.
        let d = decide(&["gl.so".into()], &["gl*".to_string()], &[], &host, &bund).unwrap();
        assert!(d.contains(&("gl.so".into(), Source::Host("/lib/gl.so".into()))));
        // prefer_host missing on the host: error, never substituted.
        assert!(decide(&["missing.so".into()], &["missing*".to_string()], &[], &host, &bund).is_err());
        // needs missing everywhere: error.
        assert!(decide(&["nope.so".into()], &[], &[], &host, &bund).is_err());
        // Explicit allow_missing: skipped, not an error.
        let d = decide(&["nope.so".into()], &[], &["nope*".into()], &host, &bund).unwrap();
        assert!(d.is_empty());
    }

    /// `host` plus a library `c.so` that the bundle does not have.
    fn host_c(host: &std::collections::HashMap<String, PathBuf>) -> std::collections::HashMap<String, PathBuf> {
        let mut h = host.clone();
        h.insert("c.so".into(), "/lib/c.so".into());
        h
    }

    /// A table of cases for the bundle-first rule: (libraries needed, in the system, in the bundle,
    /// expected source of the first one). `H` system, `B` bundled.
    #[test]
    fn abi_bundle_first_table() {
        use abi::{decide, decide_with, Source};
        use std::collections::HashMap;
        let map = |names: &[&str], dir: &str| -> HashMap<String, PathBuf> {
            names.iter().map(|n| (n.to_string(), PathBuf::from(format!("{dir}/{n}")))).collect()
        };
        let who = |name: &str, host: &[&str], bund: &[&str]| -> char {
            let d = decide(&[name.to_string()], &[], &[], &map(host, "/usr/lib"), &map(bund, "lib")).unwrap();
            match &d[0].1 {
                Source::Host(_) => 'H',
                Source::Bundled(_) => 'B',
            }
        };
        // (name, in the system, in the bundle, expected)
        let table: &[(&str, &[&str], &[&str], char)] = &[
            ("libSoundTouch.so.1", &["libSoundTouch.so.1"], &["libSoundTouch.so.1"], 'B'),
            ("libfoo.so.1", &["libfoo.so.1"], &[], 'H'),
            ("libfoo.so.1", &[], &["libfoo.so.1"], 'B'),
            // glibc: the system wins even when the bundle has the library but no libc.
            ("libm.so.6", &["libm.so.6"], &["libm.so.6"], 'H'),
            ("libpthread.so.0", &["libpthread.so.0"], &["libpthread.so.0"], 'H'),
            ("libnss_files.so.2", &["libnss_files.so.2"], &["libnss_files.so.2"], 'H'),
            ("libm.so.6", &[], &["libm.so.6"], 'B'),
            // Graphics stack: the system wins, and the bundle is the fallback.
            ("libGL.so.1", &["libGL.so.1"], &["libGL.so.1"], 'H'),
            ("libGLX.so.0", &["libGLX.so.0"], &["libGLX.so.0"], 'H'),
            ("libEGL.so.1", &["libEGL.so.1"], &["libEGL.so.1"], 'H'),
            ("libGLESv2.so.2", &["libGLESv2.so.2"], &["libGLESv2.so.2"], 'H'),
            ("libvulkan.so.1", &["libvulkan.so.1"], &["libvulkan.so.1"], 'H'),
            ("libgbm.so.1", &["libgbm.so.1"], &["libgbm.so.1"], 'H'),
            ("libdrm.so.2", &["libdrm.so.2"], &["libdrm.so.2"], 'H'),
            ("libdrm_amdgpu.so.1", &["libdrm_amdgpu.so.1"], &["libdrm_amdgpu.so.1"], 'H'),
            ("libnvidia-glcore.so.550", &["libnvidia-glcore.so.550"], &["libnvidia-glcore.so.550"], 'H'),
            ("libGL.so.1", &[], &["libGL.so.1"], 'B'),
            // Not on the list: the included copy wins.
            ("libasound.so.2", &["libasound.so.2"], &["libasound.so.2"], 'B'),
            ("libfreetype.so.6", &["libfreetype.so.6"], &["libfreetype.so.6"], 'B'),
        ];
        for (name, host, bund, want) in table {
            assert_eq!(who(name, host, bund), *want, "{name} host={host:?} bundle={bund:?}");
        }
        // libstdc++ and libgcc_s follow the choice made from the symbol versions; the bundled
        // copy without a choice; the only copy when there is just one.
        let both = |prefs: &HashMap<String, bool>, n: &str| -> char {
            let d = decide_with(&[n.to_string()], &[], &[], &map(&[n], "/usr/lib"), &map(&[n], "lib"), prefs).unwrap();
            if matches!(d[0].1, Source::Host(_)) { 'H' } else { 'B' }
        };
        let none = HashMap::new();
        let host_pref = HashMap::from([("libstdc++.so.6".to_string(), true), ("libgcc_s.so.1".to_string(), false)]);
        assert_eq!(both(&none, "libstdc++.so.6"), 'B');
        assert_eq!(both(&host_pref, "libstdc++.so.6"), 'H');
        assert_eq!(both(&host_pref, "libgcc_s.so.1"), 'B');
        assert_eq!(who("libstdc++.so.6", &["libstdc++.so.6"], &[]), 'H');
        assert_eq!(who("libstdc++.so.6", &[], &["libstdc++.so.6"]), 'B');
        // allow_missing: skipped even if both copies exist. prefer_host: always the system.
        assert!(decide(&["libSoundTouch.so.1".into()], &[], &["libSoundTouch*".into()], &map(&["libSoundTouch.so.1"], "/usr/lib"), &map(&["libSoundTouch.so.1"], "lib")).unwrap().is_empty());
        let d = decide(&["libSoundTouch.so.1".into()], &["libSoundTouch*".into()], &[], &map(&["libSoundTouch.so.1"], "/usr/lib"), &map(&["libSoundTouch.so.1"], "lib")).unwrap();
        assert!(matches!(d[0].1, Source::Host(_)));
        // A bundle with its own glibc (B-world) keeps everything in the bundle, graphics included
        // (it is the case of a package for musl or an old system; prefer_host opts out).
        let d = decide(
            &["libc.so.6".into(), "libGL.so.1".into(), "libz.so.1".into()],
            &[], &[],
            &map(&["libc.so.6", "libGL.so.1", "libz.so.1"], "/usr/lib"),
            &map(&["libc.so.6", "libGL.so.1", "libz.so.1"], "lib"),
        ).unwrap();
        assert!(d.iter().all(|(_, s)| matches!(s, Source::Bundled(_))), "{d:?}");
    }

    /// GLib, GTK, GStreamer and FFmpeg move whole: all included or all from the system.
    #[test]
    fn abi_atomic_families_table() {
        use abi::{decide, family_of, Source};
        use std::collections::HashMap;
        let map = |names: &[&str], dir: &str| -> HashMap<String, PathBuf> {
            names.iter().map(|n| (n.to_string(), PathBuf::from(format!("{dir}/{n}")))).collect()
        };
        let needs = |names: &[&str]| -> Vec<String> { names.iter().map(|n| n.to_string()).collect() };
        let sources = |names: &[&str], host: &[&str], bund: &[&str]| -> String {
            decide(&needs(names), &[], &[], &map(host, "/usr/lib"), &map(bund, "lib"))
                .unwrap()
                .iter()
                .map(|(_, s)| if matches!(s, Source::Host(_)) { 'H' } else { 'B' })
                .collect()
        };
        let glib = ["libglib-2.0.so.0", "libgobject-2.0.so.0", "libgio-2.0.so.0"];
        // The bundle has every GLib library the app needs: the bundle.
        assert_eq!(sources(&glib, &glib, &glib), "BBB");
        // The bundle has only libglib (older) and the system all three: the whole family from the system.
        assert_eq!(sources(&glib, &glib, &glib[..1]), "HHH");
        // Neither side has the whole family: per library, the included one first (mix only if unavoidable).
        assert_eq!(sources(&glib, &glib[1..], &glib[..1]), "BHH");
        // Only the system has them: the system.
        assert_eq!(sources(&glib, &glib, &[]), "HHH");
        // GTK with GDK, GdkPixbuf, ATK, Pango, Cairo.
        let gtk = ["libgtk-3.so.0", "libgdk-3.so.0", "libgdk_pixbuf-2.0.so.0", "libatk-1.0.so.0", "libpango-1.0.so.0", "libcairo.so.2"];
        assert_eq!(sources(&gtk, &gtk, &gtk), "BBBBBB");
        assert_eq!(sources(&gtk, &gtk, &gtk[..3]), "HHHHHH");
        let gtk4 = ["libgtk-4.so.1", "libgdk_pixbuf-2.0.so.0"];
        assert_eq!(sources(&gtk4, &gtk4, &gtk4[1..]), "HH");
        // GStreamer: libgst*.
        let gst = ["libgstreamer-1.0.so.0", "libgstbase-1.0.so.0", "libgstvideo-1.0.so.0"];
        assert_eq!(sources(&gst, &gst, &gst[..2]), "HHH");
        assert_eq!(sources(&gst, &gst, &gst), "BBB");
        // FFmpeg: all or nothing.
        let ff = ["libavcodec.so.60", "libavformat.so.60", "libavutil.so.58", "libswscale.so.7"];
        assert_eq!(sources(&ff, &ff, &ff[..3]), "HHHH");
        assert_eq!(sources(&ff, &ff, &ff), "BBBB");
        // A library of no family is not tied to them.
        assert_eq!(sources(&["libz.so.1", "libglib-2.0.so.0"], &["libz.so.1", "libglib-2.0.so.0"], &["libz.so.1"]), "BH");
        // Membership.
        for (n, fam) in [
            ("libglib-2.0.so.0", Some("GLib")), ("libgmodule-2.0.so.0", Some("GLib")), ("libgtk-x11-2.0.so.0", Some("GTK")),
            ("libgdk-3.so.0", Some("GTK")), ("libatspi.so.0", Some("GTK")), ("libpangocairo-1.0.so.0", Some("GTK")),
            ("libgstapp-1.0.so.0", Some("GStreamer")), ("libavfilter.so.9", Some("FFmpeg")), ("libswresample.so.4", Some("FFmpeg")),
            ("libQt6Core.so.6", Some("Qt6")), ("libKF5Auth.so.5", Some("Qt5")), ("libz.so.1", None), ("libcairo.so.1", None),
        ] {
            assert_eq!(family_of(n), fam, "{n}");
        }
    }

    /// A choice from the symbol check: it moves a library that has two copies, but never the ones the
    /// system must win, and not in a bundle with its own glibc.
    #[test]
    fn abi_symbol_check_choices_are_applied_with_limits() {
        use abi::{decide_with, Source};
        use std::collections::HashMap;
        let map = |names: &[&str], dir: &str| -> HashMap<String, PathBuf> {
            names.iter().map(|n| (n.to_string(), PathBuf::from(format!("{dir}/{n}")))).collect()
        };
        let both = ["libfoo.so.1", "libGL.so.1", "libm.so.6"];
        let needs: Vec<String> = both.iter().map(|n| n.to_string()).collect();
        let prefs: HashMap<String, bool> = both.iter().map(|n| (n.to_string(), true)).collect();
        let d = decide_with(&needs, &[], &[], &map(&both, "/usr/lib"), &map(&both, "lib"), &prefs).unwrap();
        // libfoo follows the choice (system); libGL and libm are the system's anyway.
        assert!(d.iter().all(|(_, s)| matches!(s, Source::Host(_))));
        let back: HashMap<String, bool> = both.iter().map(|n| (n.to_string(), false)).collect();
        let d = decide_with(&needs, &[], &[], &map(&both, "/usr/lib"), &map(&both, "lib"), &back).unwrap();
        // The choice "bundled" is honoured for libfoo only: libGL and libm stay with the system.
        assert!(matches!(d[0].1, Source::Bundled(_)) && matches!(d[1].1, Source::Host(_)) && matches!(d[2].1, Source::Host(_)), "{d:?}");
        // prefer_host and allow_missing are stronger than a choice.
        let d = decide_with(&needs, &["libfoo*".into()], &[], &map(&both, "/usr/lib"), &map(&both, "lib"), &back).unwrap();
        assert!(matches!(d[0].1, Source::Host(_)));
        let d = decide_with(&needs, &[], &["libfoo*".into()], &map(&both, "/usr/lib"), &map(&both, "lib"), &prefs).unwrap();
        assert!(d.iter().all(|(n, _)| n != "libfoo.so.1"));
        // A bundle with its own glibc ignores the choices: everything is the bundle's.
        let w = ["libc.so.6", "libfoo.so.1"];
        let wn: Vec<String> = w.iter().map(|n| n.to_string()).collect();
        let d = decide_with(&wn, &[], &[], &map(&w, "/usr/lib"), &map(&w, "lib"), &prefs).unwrap();
        assert!(d.iter().all(|(_, s)| matches!(s, Source::Bundled(_))), "{d:?}");
    }

    #[test]
    fn abi_glibc_family_is_atomic() {
        use abi::{decide, Source};
        use std::collections::HashMap;
        // Bundle with complete 2.36 glibc, host with 2.39: the whole family goes
        // from the bundle (loader included), never mixed.
        let host: HashMap<String, PathBuf> =
            [("libc.so.6", "/lib/libc.so.6"), ("libm.so.6", "/lib/libm.so.6"), ("gtk.so", "/lib/gtk.so")]
                .into_iter().map(|(k, v)| (k.to_string(), PathBuf::from(v))).collect();
        let bund: HashMap<String, PathBuf> =
            [("libc.so.6", "lib/libc.so.6"), ("libm.so.6", "lib/libm.so.6"), ("gtk.so", "lib/gtk.so")]
                .into_iter().map(|(k, v)| (k.to_string(), PathBuf::from(v))).collect();
        let d = decide(&["libc.so.6".into(), "libm.so.6".into(), "gtk.so".into()], &[], &[], &host, &bund).unwrap();
        assert!(d.contains(&("libc.so.6".into(), Source::Bundled("lib/libc.so.6".into()))));
        assert!(d.contains(&("libm.so.6".into(), Source::Bundled("lib/libm.so.6".into()))));
        // B-world: gtk too (in the bundle) comes from the bundle, not the host.
        assert!(d.contains(&("gtk.so".into(), Source::Bundled("lib/gtk.so".into()))));
        // Incomplete family in the bundle: explicit error, no silent mix.
        let mut incomplete = bund.clone();
        incomplete.remove("libm.so.6");
        assert!(decide(&["libm.so.6".into()], &[], &[], &host, &incomplete).is_err());
        // Without libc in the bundle: normal behavior (host).
        let mut no_libc = bund.clone();
        no_libc.remove("libc.so.6");
        no_libc.remove("libm.so.6");
        let d = decide(&["libc.so.6".into()], &[], &[], &host, &no_libc).unwrap();
        assert!(d.contains(&("libc.so.6".into(), Source::Host("/lib/libc.so.6".into()))));
    }

    #[test]
    fn abi_qt_family_is_atomic() {
        use abi::{decide, Source};
        use std::collections::HashMap;
        // Bundle with complete Qt6, host with partial/old Qt6: the whole family goes
        // from the bundle, never mixed (private symbols are not
        // compatible across builds: Qt_PRIVATE_API).
        let host: HashMap<String, PathBuf> =
            [("libQt6Core.so.6", "/lib/libQt6Core.so.6"), ("libQt6Xml.so.6", "/lib/libQt6Xml.so.6")]
                .into_iter().map(|(k, v)| (k.to_string(), PathBuf::from(v))).collect();
        let bund: HashMap<String, PathBuf> =
            [("libQt6Core.so.6", "lib/libQt6Core.so.6"), ("libQt6Xml.so.6", "lib/libQt6Xml.so.6")]
                .into_iter().map(|(k, v)| (k.to_string(), PathBuf::from(v))).collect();
        let d = decide(&["libQt6Core.so.6".into(), "libQt6Xml.so.6".into()], &[], &[], &host, &bund).unwrap();
        assert!(d.contains(&("libQt6Core.so.6".into(), Source::Bundled("lib/libQt6Core.so.6".into()))));
        assert!(d.contains(&("libQt6Xml.so.6".into(), Source::Bundled("lib/libQt6Xml.so.6".into()))));
        // Bundle without QtCore (thin bundle on system Qt): everything from the host.
        let thin: HashMap<String, PathBuf> =
            [("libQt6Xml.so.6", "lib/libQt6Xml.so.6")].into_iter().map(|(k, v)| (k.to_string(), PathBuf::from(v))).collect();
        let d = decide(&["libQt6Core.so.6".into(), "libQt6Xml.so.6".into()], &[], &[], &host, &thin).unwrap();
        assert!(d.contains(&("libQt6Core.so.6".into(), Source::Host("/lib/libQt6Core.so.6".into()))));
        assert!(d.contains(&("libQt6Xml.so.6".into(), Source::Host("/lib/libQt6Xml.so.6".into()))));
        // prefer_host stays supreme even inside the family.
        let d = decide(&["libQt6Core.so.6".into()], &["libQt6Core*".to_string()], &[], &host, &bund).unwrap();
        assert!(d.contains(&("libQt6Core.so.6".into(), Source::Host("/lib/libQt6Core.so.6".into()))));
        // Incomplete bundle (a KF member missing) but complete host:
        // the whole family from the host, never mixed.
        let mut partial = bund.clone();
        partial.remove("libQt6Xml.so.6");
        let d = decide(&["libQt6Core.so.6".into(), "libQt6Xml.so.6".into()], &[], &[], &host, &partial).unwrap();
        assert!(d.contains(&("libQt6Core.so.6".into(), Source::Host("/lib/libQt6Core.so.6".into()))));
        assert!(d.contains(&("libQt6Xml.so.6".into(), Source::Host("/lib/libQt6Xml.so.6".into()))));
    }

    #[test]
    fn split_stub_flags_open() {
        let dir = tmpdir("flags");
        let file = dir.join("a.txt");
        fs::write(&file, b"x").unwrap();
        let canon = fs::canonicalize(&file).unwrap();
        let args = vec![
            "--network-unfiltered".to_string(),
            "--panpak-open".to_string(),
            file.to_string_lossy().into_owned(),
            "--panpak-open-write".to_string(),
            "--".to_string(),
            "x".to_string(),
        ];
        let (unfiltered, open, app) = split_stub_flags(args).unwrap();
        assert!(unfiltered);
        assert_eq!(open, Some((canon, OpenGrant { write: true, rename: false })));
        assert_eq!(app, vec!["--".to_string(), "x".to_string()]);
        assert!(split_stub_flags(vec!["--panpak-open".to_string()]).is_err());
        assert!(split_stub_flags(vec!["--panpak-open-write".to_string()]).is_err());
        assert!(split_stub_flags(vec!["--panpak-open-rename".to_string()]).is_err());
        // Usage messages: say what is missing, no internal traces.
        let e = split_stub_flags(vec!["--panpak-open".to_string()]).unwrap_err();
        assert_eq!(panpak_core::code_of(&e, ErrorCode::E2000), ErrorCode::E5002, "open");
        let e = split_stub_flags(vec!["--panpak-open-write".to_string()]).unwrap_err();
        assert_eq!(panpak_core::code_of(&e, ErrorCode::E2000), ErrorCode::E5001, "write");
        let m = panpak_core::render(&e, ErrorCode::E2000);
        assert!(!m.contains("Os {") && !m.contains("panicked"), "internal trace: {m}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sandbox_open_file_only_that_one() {
        if !crate::testutil::alone() {
            return;
        }
        // Fixture outside /tmp/appdata/tree: the `open` rule must
        // cover only the exact file, even in the same folder.
        let (dir, panpak, tree, appdata) = fixture_tree("open-ro", "");
        let outside = outside_tmpdir("open-ro");
        let docs = outside.join("docs");
        fs::create_dir_all(&docs).unwrap();
        // The granted file contains spaces and special characters.
        let note = docs.join("spaces and (parens) [1].txt");
        fs::write(&note, b"content\n").unwrap();
        let sibling = docs.join("secret.txt");
        fs::write(&sibling, b"top-secret\n").unwrap();
        // Symlink to a non-granted path (same folder, other file).
        std::os::unix::fs::symlink(&sibling, docs.join("link-outside")).unwrap();
        // Hardlink to a NON-granted inode (same file as the sibling, other path):
        // stays denied like the target.
        std::fs::hard_link(&sibling, docs.join("secret-copy")).unwrap();
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, Some((&note, OpenGrant { write: false, rename: false })))?;
            let ok = |p: &std::path::Path| std::fs::read(p).is_ok();
            let no = |p: &std::path::Path| std::fs::read(p).is_err();
            res.push(format!("open_granted={}\n", if ok(&note) { "ok" } else { "FAIL" }));
            denied(res, "open_sibling", no(&sibling));
            denied(res, "open_symlink_out", no(&docs.join("link-outside")));
            denied(res, "open_hardlink_alias", no(&docs.join("secret-copy")));
            // Write denied (RO): both on the granted file and elsewhere.
            let no_w = std::fs::OpenOptions::new().write(true).open(&note).is_err();
            denied(res, "write_granted_ro", no_w);
            let no_create = std::fs::File::create(docs.join("new.txt")).is_err();
            denied(res, "create_sibling", no_create);
            Ok(())
        });
        assert!(res.contains("open_granted=ok"), "granted file with spaces/special characters unreadable:\n{res}");
        for op in ["open_sibling", "open_symlink_out", "open_hardlink_alias", "write_granted_ro", "create_sibling"] {
            assert!(res.contains(&format!("{op}=deny")), "{op} NOT blocked:\n{res}");
        }
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn sandbox_open_file_writable_with_flag() {
        if !crate::testutil::alone() {
            return;
        }
        use std::io::Write as _;
        let (dir, panpak, tree, appdata) = fixture_tree("open-rw", "");
        let outside = outside_tmpdir("open-rw");
        let target = outside.join("written.txt");
        fs::write(&target, b"v0\n").unwrap();
        let elsewhere = outside.join("elsewhere.txt");
        fs::write(&elsewhere, b"x\n").unwrap();
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, Some((&target, OpenGrant { write: true, rename: false })))?;
            let ok_w = std::fs::OpenOptions::new().write(true).truncate(true).open(&target)
                .and_then(|mut o| o.write_all(b"v1\n").map(|_| ()))
                .is_ok();
            res.push(format!("write_granted={}\n", if ok_w { "ok" } else { "FAIL" }));
            let no_elsewhere =
                std::fs::OpenOptions::new().write(true).truncate(true).open(&elsewhere).is_err();
            denied(res, "write_elsewhere", no_elsewhere);
            denied(res, "read_elsewhere", std::fs::read(&elsewhere).is_err());
            Ok(())
        });
        assert!(res.contains("write_granted=ok"), "granted write failed:\n{res}");
        assert!(res.contains("write_elsewhere=deny"), "write elsewhere allowed:\n{res}");
        assert!(res.contains("read_elsewhere=deny"), "read elsewhere allowed:\n{res}");
        assert_eq!(std::fs::read(&target).unwrap(), b"v1\n");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn sandbox_open_rename_without_reading_neighbours() {
        if !crate::testutil::alone() {
            return;
        }
        use std::io::Write as _;
        // Atomic-save pattern (temp+rename): granted only with rename,
        // no reading/listing of neighbors.
        let (dir, panpak, tree, appdata) = fixture_tree("open-rename", "");
        let outside = outside_tmpdir("open-rename");
        let target = outside.join("doc.txt");
        fs::write(&target, b"v0\n").unwrap();
        let sibling = outside.join("neighbour.txt");
        fs::write(&sibling, b"private\n").unwrap();
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, Some((&target, OpenGrant { write: false, rename: true })))?;
            // Atomic save on the granted file: create tmp, write, rename.
            let tmp = outside.join("doc.txt.tmp");
            let created = std::fs::File::create(&tmp).is_ok();
            let written = created
                && std::fs::OpenOptions::new().write(true).open(&tmp)
                    .and_then(|mut o| o.write_all(b"v1\n").map(|_| ()))
                    .is_ok();
            let renamed = written && std::fs::rename(&tmp, &target).is_ok();
            res.push(format!("atomic_save={}\n", if renamed { "ok" } else { "FAIL" }));
            denied(res, "sib_read", std::fs::read(&sibling).is_err());
            denied(res, "sib_list", std::fs::read_dir(&outside).is_err());
            Ok(())
        });
        assert!(res.contains("atomic_save=ok"), "atomic save failed:\n{res}");
        assert!(res.contains("sib_read=deny"), "neighbor readable:\n{res}");
        assert!(res.contains("sib_list=deny"), "directory can be listed:\n{res}");
        assert_eq!(std::fs::read(&target).unwrap(), b"v1\n");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn sandbox_open_rename_denied_without_flag() {
        if !crate::testutil::alone() {
            return;
        }
        // Without rename, the temp+rename pattern is denied (fail-closed).
        let (dir, panpak, tree, appdata) = fixture_tree("open-norename", "");
        let outside = outside_tmpdir("open-norename");
        let target = outside.join("doc.txt");
        fs::write(&target, b"v0\n").unwrap();
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, Some((&target, OpenGrant { write: true, rename: false })))?;
            let tmp = outside.join("doc.txt.tmp");
            denied(res, "tmp_create", std::fs::File::create(&tmp).is_err());
            Ok(())
        });
        assert!(res.contains("tmp_create=deny"), "temp can be created without rename:\n{res}");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn private_tmp_writable_and_var_tmp_denied() {
        if !crate::testutil::alone() {
            return;
        }
        use std::io::Write as _;
        // Under sandbox: SQLite-style (O_EXCL 0600 like etilqs_*) ok in the
        // private tmp folder, denied in /var/tmp (default-deny).
        let (dir, panpak, tree, appdata) = fixture_tree("private-tmp-sandbox", "");
        let private_tmp = appdata.join("tmp");
        fs::create_dir_all(&private_tmp).unwrap();
        let results = appdata.join("results.txt");
        let res = fork_lines(&results, |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, None)?;
            let probe = private_tmp.join("etilqs_probe");
            let ok_tmp = fs::OpenOptions::new().read(true).write(true).create_new(true)
                .open(&probe)
                .and_then(|mut o| o.write_all(b"x").map(|_| ()))
                .is_ok();
            res.push(format!("privatetmp={}\n", if ok_tmp { "ok" } else { "FAIL" }));
            denied(res, "vartmp", File::create(format!("/var/tmp/panpak-privatetmp-{}", std::process::id())).is_err());
            Ok(())
        });
        assert!(res.contains("privatetmp=ok"), "private tmp not writable:\n{res}");
        assert!(res.contains("vartmp=deny"), "/var/tmp writable in sandbox:\n{res}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sandbox_tree_readonly_app_writable() {
        if !crate::testutil::alone() {
            return;
        }
        use std::io::Write as _;
        // Fixture: materialized tree (with write-bit strip) + appdata.
        let (dir, panpak, tree, appdata) = fixture_tree("sandbox-ro", "");
        let blob_before = fs::read(
            fs::read_dir(dir.join("store/blobs")).unwrap().next().unwrap().unwrap().path(),
        )
        .unwrap();
        let target = tree.join("bin/hello");
        let results = appdata.join("results.txt");

        let res = fork_lines(&results, |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, None)?;
            // 1-2: writing and truncating the tree file.
            denied(res, "write", fs::OpenOptions::new().write(true).open(&target).is_err());
            denied(res, "truncate", fs::OpenOptions::new().write(true).truncate(true).open(&target).is_err());
            // 3: creating a new file in the tree.
            denied(res, "create", File::create(tree.join("new")).is_err());
            // 4-5: renaming and deleting.
            denied(res, "rename", fs::rename(&target, tree.join("moved")).is_err());
            denied(res, "unlink", fs::remove_file(&target).is_err());
            // 6: hardlink to a tree file (REFER denied).
            denied(res, "hardlink", fs::hard_link(&target, appdata.join("hl")).is_err());
            // 6b: $HOME (outside appdata) unlistable: default-deny.
            let home_deny = std::env::var("HOME")
                .map(|h| fs::read_dir(h).is_err())
                .unwrap_or(false);
            denied(res, "homedir", home_deny);
            // 7-9: positive — app data and /tmp stay writable,
            // same-dir rename (atomic-save) and /dev/null ok.
            let ok_data = File::create(appdata.join("out.txt"))
                .and_then(|mut o| o.write_all(b"x").map(|_| ()))
                .is_ok();
            res.push(format!("appdata={}\n", if ok_data { "ok" } else { "FAIL" }));
            let ok_rename_same = fs::rename(appdata.join("out.txt"), appdata.join("out2.txt")).is_ok();
            res.push(format!("rename_same={}\n", if ok_rename_same { "ok" } else { "FAIL" }));
            let ok_null = fs::OpenOptions::new().read(true).write(true).open("/dev/null").is_ok();
            res.push(format!("devnull={}\n", if ok_null { "ok" } else { "FAIL" }));
            let tmpf = std::env::temp_dir().join(format!("panpak-ob1-{}", std::process::id()));
            let ok_tmp = File::create(&tmpf).and_then(|mut o| o.write_all(b"x").map(|_| ())).is_ok();
            let _ = fs::remove_file(&tmpf);
            res.push(format!("tmp={}\n", if ok_tmp { "ok" } else { "FAIL" }));
            Ok(())
        });
        for op in ["write", "truncate", "create", "rename", "unlink", "hardlink", "homedir"] {
            assert!(res.contains(&format!("{op}=deny")), "operation {op} NOT blocked:\n{res}");
        }
        assert!(res.contains("appdata=ok"), "app data not writable:\n{res}");
        assert!(res.contains("tmp=ok"), "/tmp not writable:\n{res}");
        assert!(res.contains("rename_same=ok"), "same-directory rename failed:\n{res}");
        assert!(res.contains("devnull=ok"), "cannot open /dev/null:\n{res}");
        // Defense in depth: blobs unchanged and write-bit-less, marker present.
        let blob_after = fs::read(
            fs::read_dir(dir.join("store/blobs")).unwrap().next().unwrap().unwrap().path(),
        )
        .unwrap();
        assert_eq!(blob_before, blob_after, "shared blob modified");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::symlink_metadata(&target).unwrap().permissions().mode() & 0o222, 0);
        assert!(tree.with_extension("readonly").exists(), "missing readonly marker");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_dedup_hardlink_across_trees() {
        use std::os::unix::fs::MetadataExt;
        let dir = tmpdir("dedup");
        let data = b"shared-content";
        let (path, _, chunks, entries) = fake_panpak_with_file(&dir, data);
        let store = dir.join("store");
        let root_a = [0xAAu8; 32];
        let root_b = [0xBBu8; 32];
        let mut f = File::open(&path).unwrap();
        let ta = materialize(&mut f, &store, &root_a, &chunks, &entries).unwrap();
        let mut f = File::open(&path).unwrap();
        let tb = materialize(&mut f, &store, &root_b, &chunks, &entries).unwrap();
        let (ia, ib) = (
            fs::metadata(ta.join("bin/hello")).unwrap().ino(),
            fs::metadata(tb.join("bin/hello")).unwrap().ino(),
        );
        assert_eq!(ia, ib, "same inode across different trees");
        assert!(fs::metadata(ta.join("bin/hello")).unwrap().nlink() >= 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn decide_net_all_combinations() {
        use NetDecision::*;
        // New ABI: always enforce, the flag is irrelevant.
        assert_eq!(decide_net(true, false, false), Enforce);
        assert_eq!(decide_net(true, true, false), Enforce);
        assert_eq!(decide_net(true, false, true), Enforce);
        // Old kernel + network=true: always refuse (not even with the flag).
        assert!(matches!(decide_net(false, true, false), Refuse(_)));
        assert!(matches!(decide_net(false, true, true), Refuse(_)));
        // Old kernel + network=false: default refusal...
        assert!(matches!(decide_net(false, false, false), Refuse(_)));
        // ...opt-out only explicit, with warning.
        assert_eq!(decide_net(false, false, true), UnfilteredWarn);
    }

    /// A fake `gsettings` printing the given values (and sleeping `delay` seconds first).
    fn fake_gsettings(tag: &str, scheme: &str, theme: &str, delay: &str, exit: i32) -> PathBuf {
        let dir = tmpdir(tag);
        let path = dir.join("gsettings");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nsleep {delay}\ncase \"$3\" in color-scheme) echo \"{scheme}\";; gtk-theme) echo \"{theme}\";; esac\nexit {exit}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn dark_theme_from_gsettings() {
        // A script written and then run fails with ETXTBSY if another thread forks while it is open for writing.
        if !crate::testutil::alone() {
            return;
        }
        // GTK_THEME may be set in the environment of the test run: the function respects it.
        if std::env::var_os("GTK_THEME").is_some() {
            eprintln!("SKIP: GTK_THEME is set");
            return;
        }
        let t = std::time::Duration::from_secs(10);
        let g = fake_gsettings("gs-dark", "'prefer-dark'", "'Yaru'", "0", 0);
        assert_eq!(gtk_dark_theme(Some(&g), t).as_deref(), Some("Yaru:dark"));
        let g = fake_gsettings("gs-empty", "'prefer-dark'", "''", "0", 0);
        assert_eq!(gtk_dark_theme(Some(&g), t).as_deref(), Some("Adwaita:dark"));
        let g = fake_gsettings("gs-light", "'default'", "'Yaru'", "0", 0);
        assert_eq!(gtk_dark_theme(Some(&g), t), None);
        let g = fake_gsettings("gs-odd", "'prefer-dark'", "'a;b'", "0", 0);
        assert_eq!(gtk_dark_theme(Some(&g), t), None, "theme names with odd characters are not passed on");
        let g = fake_gsettings("gs-fail", "'prefer-dark'", "'Yaru'", "0", 1);
        assert_eq!(gtk_dark_theme(Some(&g), t), None);
        assert_eq!(gtk_dark_theme(None, t), None);
        assert_eq!(gtk_dark_theme(Some(Path::new("/nonexistent/gsettings")), t), None);
    }

    #[test]
    fn slow_gsettings_is_stopped_at_the_deadline() {
        // A script written and then run fails with ETXTBSY if another thread forks while it is open for writing.
        if !crate::testutil::alone() {
            return;
        }
        if std::env::var_os("GTK_THEME").is_some() {
            return;
        }
        let g = fake_gsettings("gs-slow", "'prefer-dark'", "'Yaru'", "5", 0);
        let t0 = std::time::Instant::now();
        assert_eq!(gtk_dark_theme(Some(&g), GSETTINGS_TIMEOUT), None);
        let took = t0.elapsed();
        assert!(took < std::time::Duration::from_millis(900), "waited {took:?}");
    }

    #[test]
    fn local_sockets_are_hidden_from_the_environment() {
        use std::ffi::OsStr;
        let state = |cmd: &Command, name: &str| cmd.get_envs().find(|(k, _)| *k == OsStr::new(name)).map(|(_, v)| v.map(|v| v.to_owned()));
        let hidden = Some(Some("disabled:".into()));
        let mut cmd = Command::new("true");
        hide_local_sockets(&mut cmd, false, false);
        for v in ["SSH_AUTH_SOCK", "GPG_AGENT_INFO", "SSH_AGENT_PID"] {
            assert_eq!(state(&cmd, v), Some(None), "{v} must be removed");
        }
        assert_eq!(state(&cmd, "DBUS_SESSION_BUS_ADDRESS"), hidden);
        assert_eq!(state(&cmd, "DBUS_SYSTEM_BUS_ADDRESS"), hidden);
        // Each bus is kept only when the manifest lists it; the agents always go.
        let mut cmd = Command::new("true");
        hide_local_sockets(&mut cmd, true, false);
        assert_eq!(state(&cmd, "DBUS_SESSION_BUS_ADDRESS"), None);
        assert_eq!(state(&cmd, "DBUS_SYSTEM_BUS_ADDRESS"), hidden);
        let mut cmd = Command::new("true");
        hide_local_sockets(&mut cmd, false, true);
        assert_eq!(state(&cmd, "DBUS_SESSION_BUS_ADDRESS"), hidden);
        assert_eq!(state(&cmd, "DBUS_SYSTEM_BUS_ADDRESS"), None);
        assert_eq!(state(&cmd, "SSH_AUTH_SOCK"), Some(None));
    }

    #[test]
    fn gsettings_is_only_looked_for_in_system_paths() {
        // The lookup reads no environment variable: the candidates are the whole input.
        assert!(GSETTINGS_PATHS.iter().all(|p| Path::new(p).is_absolute() && (p.starts_with("/usr/") || p.starts_with("/bin/"))));
        assert!(find_gsettings().is_none_or(|p| GSETTINGS_PATHS.iter().any(|g| Path::new(g) == p)));
        // The end-to-end check, with a program planted in PATH, is in tests/app_dirs.py.
    }

    #[test]
    fn landlock_abi_detected() {
        let abi = panpak_core::detect_abi().expect("Landlock unavailable on this kernel");
        assert!(abi >= landlock::ABI::V1);
        // Visible with --nocapture: needed for the report on the obtained ABI.
        eprintln!("Detected Landlock ABI: {abi:?}");
    }

    #[test]
    fn sockets_allow_requires_v9() {
        // On kernels < V9 the allowlist must fail hard, never pass unnoticed.
        let abi = panpak_core::detect_abi().unwrap();
        if abi >= landlock::ABI::V9 {
            eprintln!("SKIP: kernel supports V9, guard does not trigger here");
            return;
        }
        let (_dir, panpak, tree, appdata) = fixture_tree("ux-guard", "");
        let mut f = File::open(&panpak).unwrap();
        let ft = read_footer(&mut f).unwrap();
        let (mut man, _) = verify(&mut f, &ft).unwrap();
        man.permissions = Some(panpak_core::Permissions {
            x11: None,
            audio: None,
            xdg_dirs: None,
            network: None,
            network_allow_tcp: None,
            network_bind_tcp: None,
            sockets_allow: Some(vec!["/run/dbus.sock".into()]),
            home_paths: None,
            portal: None,
            notifications: None,
            tray: None,
            dbus_own: None,
        });
        let access = Access { rw: vec![appdata.clone()], ..Access::default() };
        let err = build_ruleset(&tree, &access, &man, abi, true, false, None).unwrap_err();
        assert!(format!("{err:?}").contains("V9"), "missing V9 guard: {err:?}");
        let _ = (_dir, panpak, tree, appdata);
    }
}
