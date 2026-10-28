// SPDX-License-Identifier: MIT OR Apache-2.0
// Panpak stub: verify (spec §5) -> materialize -> Landlock sandbox -> exec.
// Build: cargo build --release --target x86_64-unknown-linux-musl
use anyhow::{bail, ensure, Context, Result};
use panpak_core::{bail_code, coded, ensure_code, ErrorCode};
use panpak_core::{hex, ChunkDecoder, parse_index, read_at, read_footer, verify, Chunk, Entry, Kind};
use std::{
    fs::{self, File},
    io::Write,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
};

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
                        let btmp = blob.with_extension("part");
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

fn apply_landlock(
    tree: &Path,
    appdata: &Path,
    man: &panpak_core::Manifest,
    allow_unfiltered: bool,
    open: Option<(&Path, OpenGrant)>,
) -> Result<landlock::RestrictionStatus> {
    let abi = panpak_core::detect_abi().context("Landlock unsupported by kernel")?;
    // /dev single nodes first; if the kernel refuses them (build or
    // restrict), explicit fallback to whole /dev. Never a silent denial.
    let rs = match build_ruleset(tree, appdata, man, abi, true, allow_unfiltered, open) {
        Ok(rs) => rs,
        Err(_) => {
            eprintln!("panpak: cannot build per-node /dev rules on this kernel, using whole /dev");
            build_ruleset(tree, appdata, man, abi, false, allow_unfiltered, open)?
        }
    };
    let res = rs.restrict_self();
    let st = match res {
        Ok(st) => st,
        Err(_) => {
            eprintln!("panpak: kernel rejected per-node /dev rules, using whole /dev (DAC still applies)");
            build_ruleset(tree, appdata, man, abi, false, allow_unfiltered, open)?
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
/// Network (V4+), scope (V6+), Unix sockets (V9): only if the ABI offers them,
/// otherwise error only if the manifest really requires them.
fn build_ruleset(
    tree: &Path,
    appdata: &Path,
    man: &panpak_core::Manifest,
    abi: landlock::ABI,
    fine_dev: bool,
    allow_unfiltered: bool,
    open: Option<(&Path, OpenGrant)>,
) -> Result<landlock::RulesetCreated> {
    use landlock::{
        Access, AccessFs, AccessNet, NetPort, PathBeneath, PathFd, Ruleset, RulesetAttr,
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
        ensure_code!(new_unix, ErrorCode::E3000, "sockets_allow requires Landlock ABI >= V9, this kernel has {abi:?}");
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
        if let Ok(fd) = PathFd::new("/tmp/.X11-unix") {
            let mut access = AccessFs::from_read(abi);
            if new_unix {
                access |= AccessFs::ResolveUnix;
            }
            rs = rs.add_rule(PathBeneath::new(fd, access))?;
        }
    }
    for p in ["/usr", "/lib", "/lib64", "/etc", "/proc", "/sys", "/run"] {
        if let Ok(fd) = PathFd::new(p) {
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
            if let Ok(fd) = PathFd::new(node) {
                let access = if write {
                    AccessFs::ReadFile | AccessFs::WriteFile
                } else {
                    AccessFs::ReadFile.into()
                };
                rs = rs.add_rule(PathBeneath::new(fd, access))?;
            }
        }
        for d in ["/dev/dri", "/dev/shm"] {
            if let Ok(fd) = PathFd::new(d) {
                let rw = AccessFs::from_all(abi) & !AccessFs::Refer;
                rs = rs.add_rule(PathBeneath::new(fd, rw))?;
            }
        }
    } else if let Ok(fd) = PathFd::new("/dev") {
        let rw = AccessFs::from_all(abi) & !AccessFs::Refer;
        rs = rs.add_rule(PathBeneath::new(fd, rw))?;
    }
    // Tree: read + execute, NEVER write (blobs are shared
    // across trees and apps: a write would alter them everywhere).
    let rx = AccessFs::from_read(abi) | AccessFs::Execute;
    rs = rs.add_rule(PathBeneath::new(PathFd::new(tree)?, rx))?;
    // Writable dirs (appdata, tmp, dev): everything EXCEPT Refer. From ABI V2
    // from_all includes Refer (link/rename across different dirs): granting it
    // would allow hardlinks to tree files. Without Refer, same-dir
    // renames stay possible (atomic-save) but no external links.
    let rw = AccessFs::from_all(abi) & !AccessFs::Refer;
    rs = rs.add_rule(PathBeneath::new(PathFd::new(appdata)?, rw))?;
    // Shared /tmp (RW): nearly all apps require it; denying it by
    // default would break even trivial scripts. Future: [permissions]tmp.
    if let Ok(fd) = PathFd::new("/tmp") {
        rs = rs.add_rule(PathBeneath::new(fd, rw))?;
    }
    // Explicitly opened file (`panpak open` / double click): ONLY that path,
    // read by default, write only with explicit declaration.
    // The caller passes the already canonicalized path.
    if let Some((path, grant)) = open {
        let fd = PathFd::new(path)
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
                let fd = PathFd::new(parent).with_context(|| {
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
    // Path Unix sockets (V9+): allowlist from the manifest; under appdata/tmp
    // they already pass via ResolveUnix.
    for s in &socks {
        let path = panpak_core::expand_socket_path(s)?;
        match PathFd::new(&path) {
            Ok(fd) => {
                let access = AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::ResolveUnix;
                rs = rs.add_rule(PathBeneath::new(fd, access))?;
            }
            Err(_) => eprintln!("panpak: socket {s} absent, rule omitted (access remains denied)"),
        }
    }
    Ok(rs)
}

/// Landlock from the manifest permissions (spec §3.1, policy in PANPAK-FORMAT.md §9):
/// - app tree: read + execute, never write;
/// - ~/.local/share/<id> and /tmp: read+write (app data and temp);
/// - /usr /lib /etc /proc /sys /run: read-only (host runtime);
/// - /dev: only null/zero/full/urandom/random/tty nodes + dri/shm dirs
///   (explicit fallback to whole /dev if the kernel refuses per-file rules);
/// - network: denied by default (V4+), allowlist from the manifest;
///   on kernels without V4, explicit --network-unfiltered is required (warning at every start).
fn sandbox(tree: &Path, man: &panpak_core::Manifest, allow_unfiltered: bool, open: Option<(&Path, OpenGrant)>) -> Result<()> {
    // App data from the manifest: ~/.local/share/<id> (RW, created if missing).
    let appdata = dirs::data_dir().context("no data dir")?.join(&man.app.id);
    fs::create_dir_all(&appdata)?;
    let st = apply_landlock(tree, &appdata, man, allow_unfiltered, open)?;
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
    ensure!(
        File::create(tree.join(".panpak-probe")).is_err(),
        "sandbox self-test: tree writes allowed"
    );
    Ok(())
}

/// Private directory for SQLite temp files (`<appdata>/tmp`).
/// SQLite looks in `$SQLITE_TMPDIR`, then `/var/tmp`, `/usr/tmp`,
/// `/tmp`: the first two are outside the sandbox's writable areas
/// (observed: Joplin's FTS indexes failed with CANTOPEN even with
/// a writable DB). Only SQLite reads this variable: minimal blast radius,
/// no global `TMPDIR`. Respected if the user already set it
/// explicitly.
fn sqlite_tmp_dir(appdata: &Path) -> Result<PathBuf> {
    let dir = appdata.join("tmp");
    fs::create_dir_all(&dir)?;
    Ok(dir)
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

fn real_main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    // --abi-version: for `panpak install-runtime` (works on a bare stub too).
    if args.get(1).map(|s| s.as_str()) == Some("--abi-version") {
        println!("{}", panpak_core::RUNTIME_ABI);
        return Ok(());
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

    sandbox(&tree, &man, allow_unfiltered, open_file.as_ref().map(|(p, w)| (p.as_path(), *w)))?;

    // SQLite temp in a private area (see sqlite_tmp_dir): without it, /var/tmp
    // denied breaks operations using temp files (FTS).
    let appdata = dirs::data_dir().context("no data dir")?.join(&man.app.id);
    let sqlite_tmp = if std::env::var_os("SQLITE_TMPDIR").is_none() {
        Some(sqlite_tmp_dir(&appdata)?)
    } else {
        None
    };

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
    if let Some(dir) = &sqlite_tmp {
        cmd.env("SQLITE_TMPDIR", dir);
    }
    let err = cmd
        .args(&app_args)
        .env("PANPAK_APP_ID", &man.app.id)
        .env("LD_LIBRARY_PATH", &ld_path)
        .exec(); // Replaces the process.
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
        let st = apply_landlock(tree, appdata, &man, false, open).map_err(|_| "apply")?;
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
        if !require_abi(landlock::ABI::V9, "unix path") {
            return;
        }
        use std::os::unix::net::UnixListener;
        // Socket under store/ (outside every RW rule): denied without allow,
        // allowed with allow.
        for (tag, listed) in [("deny", false), ("allow", true)] {
            let dir = tmpdir(&format!("ux-{tag}"));
            fs::create_dir_all(dir.join("store")).unwrap();
            let sock = dir.join("store/sock");
            let _listener = UnixListener::bind(&sock).unwrap();
            let extra = if listed {
                format!("[permissions]\nsockets_allow = [\"{}\"]\n", sock.display())
            } else {
                String::new()
            };
            let (panpak, tree, appdata) = fixture_tree_in(&dir, &extra);
            let var = format!("PANPAK_USOCK_{tag}");
            unsafe { std::env::set_var(&var, sock.to_str().unwrap()) };
            let expect = if listed { "ok" } else { "deny" };
            let res = fork_lines(&appdata.join("r.txt"), |res| {
                let _man = child_sandboxed(res, &panpak, &tree, &appdata, None)?;
                let p = std::env::var(&var).unwrap();
                let ok = std::os::unix::net::UnixStream::connect(p).is_ok();
                res.push(format!("ux_{tag}={}\n", if ok { "ok" } else { "deny" }));
                Ok(())
            });
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
        // host wins when available; bundle as fallback.
        let d = decide(&["a.so".into(), "b.so".into()], &[], &[], &host, &bund).unwrap();
        assert!(d.contains(&("a.so".into(), Source::Host("/lib/a.so".into()))));
        assert!(d.contains(&("b.so".into(), Source::Bundled("lib/b.so".into()))));
        // prefer_host present even in the bundle: stays host.
        let d = decide(&["gl.so".into()], &["libGL*".replace("GL", "gl")], &[], &host, &bund).unwrap();
        assert!(d.contains(&("gl.so".into(), Source::Host("/lib/gl.so".into()))));
        // prefer_host missing on the host: error, never substituted.
        assert!(decide(&["missing.so".into()], &["missing*".to_string()], &[], &host, &bund).is_err());
        // needs missing everywhere: error.
        assert!(decide(&["nope.so".into()], &[], &[], &host, &bund).is_err());
        // Explicit allow_missing: skipped, not an error.
        let d = decide(&["nope.so".into()], &[], &["nope*".into()], &host, &bund).unwrap();
        assert!(d.is_empty());
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
    fn sqlite_tmp_private_and_var_tmp_denied() {
        use std::io::Write as _;
        // Helper: <appdata>/tmp created and writable (outside the sandbox).
        let dir = tmpdir("sqlitetmp");
        let appdata = dir.join("appdata");
        fs::create_dir_all(&appdata).unwrap();
        let tmp = sqlite_tmp_dir(&appdata).unwrap();
        assert_eq!(tmp, appdata.join("tmp"));
        assert!(tmp.is_dir());
        fs::write(tmp.join("probe"), b"x").unwrap();
        // Under sandbox: SQLite-style (O_EXCL 0600 like etilqs_*) ok in
        // appdata/tmp, denied in /var/tmp (default-deny).
        let (dir2, panpak, tree, appdata2) = fixture_tree("sqlitetmp-sandbox", "");
        let results = appdata2.join("results.txt");
        let res = fork_lines(&results, |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata2, None)?;
            let tmp = sqlite_tmp_dir(&appdata2).map_err(|_| "mktmp")?;
            let probe = tmp.join("etilqs_probe");
            let ok_tmp = fs::OpenOptions::new().read(true).write(true).create_new(true)
                .open(&probe)
                .and_then(|mut o| o.write_all(b"x").map(|_| ()))
                .is_ok();
            res.push(format!("sqlitetmp={}\n", if ok_tmp { "ok" } else { "FAIL" }));
            denied(res, "vartmp", File::create(format!("/var/tmp/panpak-sqlitetmp-{}", std::process::id())).is_err());
            Ok(())
        });
        assert!(res.contains("sqlitetmp=ok"), "SQLite tmp not writable:\n{res}");
        assert!(res.contains("vartmp=deny"), "/var/tmp writable in sandbox:\n{res}");
        let _ = (dir, dir2);
    }

    #[test]
    fn sandbox_tree_readonly_app_writable() {
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
            network: None,
            network_allow_tcp: None,
            network_bind_tcp: None,
            sockets_allow: Some(vec!["/run/dbus.sock".into()]),
            portal: None,
        });
        let err = build_ruleset(&tree, &appdata, &man, abi, true, false, None).unwrap_err();
        assert!(format!("{err:?}").contains("V9"), "missing V9 guard: {err:?}");
        let _ = (_dir, panpak, tree, appdata);
    }
}
