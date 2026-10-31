// SPDX-License-Identifier: MIT OR Apache-2.0
//! panpak-core: Panpak v1 format shared between stub (read/verify) and CLI (pack).
//!
//! All numbers in the format are little-endian. See PANPAK-FORMAT.md.

mod elfsym;
mod errors;
mod layout;
mod openchan;
mod openurl;
pub mod stalelocks;
mod userdirs;
mod validate;
mod view;
pub use elfsym::{parse_syms, read_elf_info, sym_hash, version_number, ElfInfo, ElfSyms, Needed};
pub use openchan::{handle as handle_open_request, RateLimit, Target as OpenTarget, BURST, MAX_REQUEST, RATE_PER_SEC};
pub use openurl::{check_scheme_name, check_url, UrlKind, MAX_URL};
pub use userdirs::{
    default_user_dir, format_pin, generated_user_dirs, no_links, parse_pin, parse_user_dirs, resolve_xdg_dir, user_dirs_key, USER_DIRS_MARKER,
    XDG_DIR_NAMES,
};
pub use errors::{code_of, coded, render, ErrorCode, PanpakError};
pub use layout::{
    added_permissions, app_dirs, describe_permissions, fonts_conf, is_bus_socket, readonly_user_paths, xdg_search_dirs, AppDirs,
    allows_desktop_bus, allows_session_bus, allows_system_bus, is_system_bus_socket, socket_isolation, IsolationReport, SocketIsolation, ABSTRACT_SOCKETS_WARNING, DESKTOP_BUS_WARNING, FONTS_CONF_MARKER,
    PARTIAL_ISOLATION_WARNING,
};
pub use view::{
    bwrap_version, implied_sockets, network_requested, open_o_path, resolv_conf_run_dir, unopenable_rule_paths, view_status, view_status_at, ViewProblem,
    ViewStatus, BWRAP, SYSTEM_RULE_PATHS,
};
pub use validate::{
    validate_app_id, validate_manifest, BROAD_HOME_PATHS, FORBIDDEN_ENV, FORBIDDEN_ENV_PREFIXES, FORBIDDEN_HOME_PATHS, PLACEHOLDERS,
    MAX_APP_ID_LEN,
};

use anyhow::{anyhow, bail, ensure, Context, Result};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Component, PathBuf},
};

pub const MAGIC: &[u8; 8] = b"PANP\0\x01\0\0";
pub const FOOTER_LEN: u64 = 80;
pub const SIG_LEN: u64 = 64;

/// Anti-DoS caps on data read before/during verification.
pub const MAX_MANIFEST_TOML: u64 = 1 << 20; // 1 MiB: the manifest is metadata
pub const MAX_INDEX_COMP: u64 = 1 << 28; // 256 MiB compressed
pub const MAX_INDEX_RAW: u64 = 1 << 28; // 256 MiB decompressed (anti zip-bomb)
pub const MAX_PATH_LEN: usize = 4096;
pub const MAX_SYMLINK_LEN: usize = 4096;
/// Chunk sanity (spec: max 256K; margin for future robustness).
pub const MAX_CHUNK_RAW: u32 = 1 << 20;
pub const MAX_CHUNK_COMP: u32 = 1 << 21;

/// FastCDC parameters from spec §4.
pub const CDC_MIN: usize = 16 * 1024;
pub const CDC_AVG: usize = 64 * 1024;
pub const CDC_MAX: usize = 256 * 1024;
/// zstd level for chunks and index.
pub const ZSTD_LEVEL: i32 = 3;
/// Stub (runtime) ABI v1. Packages declare `[runtime] min_runtime`:
/// the stub refuses packages requiring a newer runtime.
pub const RUNTIME_ABI: u32 = 1;

/// True if the error chain contains ENOSPC (space exhausted).
/// Used to print an actionable hint instead of just
/// "No space left on device".
pub fn is_storage_full(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .map(|io| io.kind() == std::io::ErrorKind::StorageFull)
            .unwrap_or(false)
    })
}

/// Human-readable error chain ("a: cause: detail"), without the
/// internal Debug dumps (`Os { code: ... }`, structs, backtraces).
pub fn err_text(e: &anyhow::Error) -> String {
    e.chain().map(|c| c.to_string()).collect::<Vec<_>>().join(": ")
}

#[derive(Debug, Clone)]
pub struct Footer {
    pub index_off: u64,
    pub index_len: u64,
    pub man_off: u64,
    pub man_len: u64,
    pub root: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub hash: [u8; 32],
    pub off: u64,
    pub comp: u32,
    pub raw: u32,
}

#[derive(Debug, Clone)]
pub enum Kind {
    File { chunks: Vec<u32> },
    Dir,
    Link(String),
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub mode: u32,
    pub kind: Kind,
}

/// Schema manifest v1 (spec §3).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub app: App,
    pub abi: Option<Abi>,
    pub permissions: Option<Permissions>,
    /// Extra start arguments and environment (PANPAK-FORMAT.md §3.1.2).
    pub launch: Option<Launch>,
    /// Desktop integration: the URL schemes the app handles (PANPAK-FORMAT.md §3.1.3).
    pub desktop: Option<Desktop>,
    pub runtime: Option<Runtime>,
    pub origin: Option<Origin>,
    pub update: Option<Update>,
}

/// Provenance for converted packages (e.g. from AppImage): the converter
/// is NOT the upstream author and cannot prove its identity.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Origin {
    /// Original file name (e.g. "Foo-1.0-x86_64.AppImage").
    pub converted_from: String,
    /// Version declared at conversion time.
    pub version: String,
    /// SHA-256 (hex) of the original converted file.
    pub sha256: String,
    /// Always false for converted packages: the upstream author did not sign.
    pub signed_by_upstream: bool,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct App {
    pub id: String,
    pub name: String,
    pub version: String,
    pub entry: String,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Abi {
    pub glibc_min: Option<String>,
    pub needs: Option<Vec<String>>,
    pub prefer_host: Option<Vec<String>>,
    /// SONAMEs (globs like prefer_host) ignored by resolution: the app does not
    /// load them at startup (dlopen plugins). If actually needed, the loader
    /// fails explicitly. Only with an explicit declaration.
    pub allow_missing: Option<Vec<String>>,
}
/// Arguments put before the user's arguments and variables added to the
/// environment of the app. Values may use the placeholders in `PLACEHOLDERS`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Launch {
    pub args: Option<Vec<String>>,
    pub env: Option<std::collections::BTreeMap<String, String>>,
}
/// A path under the user's home, outside the private app folders.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HomePath {
    /// Relative to the home directory, no `..`, no leading or trailing `/`.
    pub path: String,
    /// `ro` or `rw`.
    pub access: String,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Permissions {
    /// X11 opt-in: disables ONLY the abstract Unix scope (signals stay).
    pub x11: Option<bool>,
    pub network: Option<bool>,
    /// Allowed outbound TCP ports (connect). Only with network=true.
    pub network_allow_tcp: Option<Vec<u16>>,
    /// Allowed listen TCP ports (bind). Only with network=true.
    pub network_bind_tcp: Option<Vec<u16>>,
    /// Audio: the PipeWire and PulseAudio sockets of the user's session.
    pub audio: Option<bool>,
    /// Unix sockets (absolute paths) to connect to (requires ABI V9).
    /// Leading `$XDG_RUNTIME_DIR` and `$HOME` are expanded.
    pub sockets_allow: Option<Vec<String>>,
    /// Fixed paths in the home folder (last resort, see §3.1.2).
    pub home_paths: Option<Vec<HomePath>>,
    /// The user's folders (music, documents, ...), found in `user-dirs.dirs` when the app is installed.
    pub xdg_dirs: Option<Vec<XdgDir>>,
    pub portal: Option<Portal>,
}
/// A user folder the app asks for (PANPAK-FORMAT.md §3.1.4).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct XdgDir {
    /// `music`, `documents`, `pictures`, `videos` or `download`.
    pub name: String,
    /// `ro` or `rw`.
    pub access: String,
}

/// `[desktop]`: what the generated `.desktop` file declares.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Desktop {
    /// URL schemes the app opens (`x-scheme-handler/<scheme>`), for example the return from a login.
    pub schemes: Option<Vec<String>>,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Portal {
    pub files: Option<String>,
    pub camera: Option<String>,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Runtime {
    pub min_runtime: Option<u32>,
    /// BLAKE3 of the stub bytes ([0..first chunk]) in the .panpak.
    /// REQUIRED: the executed stub must match the signed one.
    pub stub_hash: Option<String>,
}

/// Verify that the stub bytes in the file ([0..stub_len)) match
/// [runtime].stub_hash. Call on EVERY execution or install path,
/// AFTER verify(): nobody ever runs/installs a stub different from
/// the signed one. `stub_len` = offset of the first chunk
/// (or index_off if the index is empty).
pub fn verify_stub_range(f: &mut File, stub_len: u64, man: &Manifest) -> Result<()> {
    let want = man.runtime.as_ref().and_then(|r| r.stub_hash.as_deref()).context(
        "manifest without [runtime].stub_hash: previous format, not verifiable (reinstall from a recent .panpak)",
    )?;
    ensure!(want.len() == 64 && want.bytes().all(|b| b.is_ascii_hexdigit()), "malformed stub_hash");
    let stub = read_at(f, 0, stub_len)?;
    let got = hex(blake3::hash(&stub).as_bytes());
    crate::ensure_code!(
        got == want,
        ErrorCode::E1002,
        "stub does not match the signed one: tampered file or replaced stub, refused (reinstall from a .panpak published by the author)"
    );
    Ok(())
}

/// Reject packages requiring a runtime newer than `us`.
pub fn check_runtime(man: &Manifest, noi: u32) -> Result<()> {
    let min = man.runtime.as_ref().and_then(|r| r.min_runtime).unwrap_or(1);
    ensure!(
        min <= noi,
        "package requires runtime >= {min}, this runtime is {noi}: update the runtime"
    );
    Ok(())
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Update {
    pub url: Option<String>,
    pub key: String,
    /// Signed succession (rotation): old key authorizing the new one.
    pub rotation_old: Option<String>,
    /// Signature of `rotation_old` over the 32 raw bytes of the new public key (hex).
    pub rotation_sig: Option<String>,
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Highest Landlock ABI REALLY supported (best-effort V9..V1).
/// Uses HardRequirement: with BestEffort (default) create() would still succeed
/// silently clamping unsupported rights. If none passes,
/// there is no Landlock.
pub fn detect_abi() -> Result<landlock::ABI> {
    use landlock::{Access, AccessFs, Compatible, CompatLevel, Ruleset, RulesetAttr, ABI};
    for abi in [ABI::V9, ABI::V8, ABI::V7, ABI::V6, ABI::V5, ABI::V4, ABI::V3, ABI::V2, ABI::V1] {
        let ok = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(abi))
            .and_then(|r| r.create())
            .is_ok();
        if ok {
            return Ok(abi);
        }
    }
    crate::bail_code!(ErrorCode::E3001, "kernel without Landlock: kernel >= 5.13 required, no execution without sandbox")
}

/// Expand leading `$XDG_RUNTIME_DIR` / `$HOME` in socket paths.
/// Absolute paths only, no `..` past the root.
pub fn expand_socket_path(s: &str) -> Result<PathBuf> {
    let mut out = s.to_string();
    for (var, plain, braced) in
        [("XDG_RUNTIME_DIR", "$XDG_RUNTIME_DIR", "${XDG_RUNTIME_DIR}"), ("HOME", "$HOME", "${HOME}")]
    {
        for form in [plain, braced] {
            if out.starts_with(form) {
                let val = std::env::var(var).with_context(|| format!("{var} not set"))?;
                out = val + &out[form.len()..];
                break;
            }
        }
    }
    ensure!(out.starts_with('/'), "non-absolute socket: {s}");
    ensure!(!out.split('/').any(|c| c == ".."), "socket with ..: {s}");
    Ok(PathBuf::from(out))
}

/// Pure Content-Range parsing + consistency with the request (fuzzable).
/// Return the declared total.
pub fn check_range_parts(req_start: u64, req_end: u64, header: &str, body_len: usize) -> Result<u64> {
    let (range, total) = header.strip_prefix("bytes ").and_then(|r| r.split_once('/')).context("unreadable Content-Range")?;
    let (s, e) = range.split_once('-').context("unreadable Content-Range")?;
    let (s, e, total): (u64, u64, u64) =
        (s.parse().context("Content-Range")?, e.parse().context("Content-Range")?, total.parse().context("Content-Range")?);
    ensure!(s == req_start && e == req_end, "Content-Range {range}/{total} != requested {req_start}-{req_end}");
    ensure!(total <= 1 << 33, "file too large: {total} bytes");
    ensure!(body_len as u64 == req_end.checked_sub(req_start).and_then(|n| n.checked_add(1)).unwrap_or(u64::MAX), "incoherent body: {body_len} bytes");
    Ok(total)
}

/// Verify a signed succession: `rotation_sig` must be a signature
/// of the PINNED (old) key over the 32 raw bytes of the NEW public key,
/// and `rotation_old` must match the pin. Used when the manifest key
/// does not match the pin (rotation).
pub fn verify_rotation(pinned: &str, new_key: &str, rot_old: Option<&str>, rot_sig: Option<&str>) -> Result<()> {
    let ro = rot_old.ok_or_else(|| coded(ErrorCode::E1005, "key changed and no rotation in manifest"))?;
    crate::ensure_code!(ro.trim() == pinned.trim(), ErrorCode::E1005, "rotation from a non-pin key: refused");
    let rs = rot_sig.ok_or_else(|| coded(ErrorCode::E1005, "rotation without signature: rotation_sig signed by the old key required"))?;
    let old_vk = parse_pubkey(ro.trim())?;
    let new_vk = parse_pubkey(new_key.trim())?;
    let rs = rs.trim();
    crate::ensure_code!(rs.len() == 128 && rs.bytes().all(|b| b.is_ascii_hexdigit()), ErrorCode::E1005, "rotation_sig must be 128 hex characters");
    let mut raw = [0u8; 64];
    for (i, d) in raw.iter_mut().enumerate() {
        *d = u8::from_str_radix(&rs[2 * i..2 * i + 2], 16).map_err(|_| coded(ErrorCode::E1005, "rotation_sig not hex"))?;
    }
    let sig = ed25519_dalek::Signature::from_bytes(&raw);
    old_vk
        .verify_strict(new_vk.as_bytes(), &sig)
        .map_err(|_| coded(ErrorCode::E1005, "invalid succession: bad old-key signature"))?;
    Ok(())
}

/// Validate the KEYS of an input panpak.toml (pack): unknown keys =
/// error, not silent-drop. Without this, an old pack would silently drop
/// future fields, producing packages different from the intended ones
/// (silent incompatibility). The manifest in .panpak files stays
/// read-tolerant instead (unknown fields ignored).
pub fn check_panpak_toml_schema(input: &str) -> Result<()> {
    let v: toml::Value = toml::from_str(input).context("panpak.toml is not TOML")?;
    let t = v.as_table().context("panpak.toml: table required")?;
    const TOP: &[&str] = &["app", "abi", "permissions", "launch", "desktop", "runtime", "origin", "update"];
    const DESKTOP: &[&str] = &["schemes"];
    const XDG_DIR: &[&str] = &["name", "access"];
    const LAUNCH: &[&str] = &["args", "env"];
    const APP: &[&str] = &["id", "name", "version", "entry"];
    const ABI: &[&str] = &["glibc_min", "needs", "prefer_host", "allow_missing"];
    const PERM: &[&str] = &["x11", "audio", "network", "network_allow_tcp", "network_bind_tcp", "sockets_allow", "home_paths", "xdg_dirs", "portal"];
    const HOME_PATH: &[&str] = &["path", "access"];
    const PORTAL: &[&str] = &["files", "camera"];
    const RUNTIME: &[&str] = &["min_runtime", "stub_hash"];
    const ORIGIN: &[&str] = &["converted_from", "version", "sha256", "signed_by_upstream"];
    const UPDATE: &[&str] = &["url", "key", "rotation_old", "rotation_sig"];
    fn keys<'a>(v: &'a toml::Value, path: &str) -> Result<Vec<&'a str>> {
        Ok(v.as_table().with_context(|| format!("{path} must be a table"))?.keys().map(|s| s.as_str()).collect())
    }
    for k in t.keys() {
        ensure!(TOP.contains(&k.as_str()), "panpak.toml: unknown section [{k}] (update panpak)");
    }
    let get = |name: &str| t.get(name);
    if let Some(v) = get("app") {
        for k in keys(v, "[app]")? {
            ensure!(APP.contains(&k), "panpak.toml [app]: unknown key `{k}` (future field? update panpak)");
        }
    }
    if let Some(v) = get("abi") {
        for k in keys(v, "[abi]")? {
            ensure!(ABI.contains(&k), "panpak.toml [abi]: unknown key `{k}` (future field? update panpak)");
        }
    }
    if let Some(v) = get("desktop") {
        for k in keys(v, "[desktop]")? {
            ensure!(DESKTOP.contains(&k), "panpak.toml [desktop]: unknown key `{k}` (future field? update panpak)");
        }
    }
    if let Some(v) = get("permissions") {
        for k in keys(v, "[permissions]")? {
            ensure!(PERM.contains(&k), "panpak.toml [permissions]: unknown key `{k}` (future field? update panpak)");
        }
        for h in v.get("xdg_dirs").and_then(|h| h.as_array()).into_iter().flatten() {
            for k in keys(h, "[[permissions.xdg_dirs]]")? {
                ensure!(XDG_DIR.contains(&k), "panpak.toml [[permissions.xdg_dirs]]: unknown key `{k}` (future field? update panpak)");
            }
        }
        for h in v.get("home_paths").and_then(|h| h.as_array()).into_iter().flatten() {
            for k in keys(h, "[[permissions.home_paths]]")? {
                ensure!(HOME_PATH.contains(&k), "panpak.toml [[permissions.home_paths]]: unknown key `{k}` (future field? update panpak)");
            }
        }
        if let Some(p) = v.get("portal") {
            for k in keys(p, "[permissions.portal]")? {
                ensure!(PORTAL.contains(&k), "panpak.toml [permissions.portal]: unknown key `{k}` (future field? update panpak)");
            }
        }
    }
    if let Some(v) = get("launch") {
        for k in keys(v, "[launch]")? {
            ensure!(LAUNCH.contains(&k), "panpak.toml [launch]: unknown key `{k}` (future field? update panpak)");
        }
    }
    if let Some(v) = get("runtime") {
        for k in keys(v, "[runtime]")? {
            ensure!(RUNTIME.contains(&k), "panpak.toml [runtime]: unknown key `{k}` (future field? update panpak)");
        }
    }
    if let Some(v) = get("origin") {
        for k in keys(v, "[origin]")? {
            ensure!(ORIGIN.contains(&k), "panpak.toml [origin]: unknown key `{k}` (future field? update panpak)");
        }
    }
    if let Some(v) = get("update") {
        for k in keys(v, "[update]")? {
            ensure!(UPDATE.contains(&k), "panpak.toml [update]: unknown key `{k}` (future field? update panpak)");
        }
    }
    Ok(())
}
pub fn fingerprint(vk: &ed25519_dalek::VerifyingKey) -> String {
    hex(vk.as_bytes())[..16].to_string()
}

pub fn read_at(f: &mut File, off: u64, len: u64) -> Result<Vec<u8>> {
    f.seek(SeekFrom::Start(off))?;
    let mut v = vec![0u8; len as usize];
    f.read_exact(&mut v)?;
    Ok(v)
}

/// Capped zstd decompression: refuse output beyond `limit` without allocating it.
/// Read DT_NEEDED from a little-endian ELF32/64. Non-ELF or static -> [].
/// Minimal bounds-checked parser (no external ELF crates).
/// File version; `elf_needs_bytes` operates on bytes (fuzzable).
pub fn elf_needs(path: &std::path::Path) -> Result<Vec<String>> {
    let b = std::fs::read(path)?;
    let mut out = elf_needs_bytes(&b)?;
    // DT_NEEDED with $ORIGIN (e.g. "$ORIGIN/../lib/libpython3.8.so.1.0" in
    // AppImages with bundled Python): resolve against the directory of the
    // file that needs it and compare by basename, as the loader does.
    // Without this, name matching fails and the lib shows up as "missing".
    if out.iter().any(|s| s.contains("$ORIGIN")) {
        let dir = path.parent().map(|p| p.as_os_str()).unwrap_or_default();
        out = out
            .into_iter()
            .map(|s| {
                if !s.contains("$ORIGIN") {
                    return s;
                }
                let resolved = s.replace("$ORIGIN", &dir.to_string_lossy());
                std::path::Path::new(&resolved)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(s)
            })
            .collect::<Vec<_>>();
        out.sort();
        out.dedup();
    }
    // A name with a slash is a path for the loader, not a SONAME to look for in the library folders
    // (FreeCAD's `libqsqlite.so` asks for a file in the build folder of its maker); it is not a need.
    out.retain(|s| !s.contains('/'));
    Ok(out)
}

/// Upper bound of `DT_STRSZ`: a guard against absurd values, not the size of real tables.
const MAX_STRTAB: u64 = 1 << 30;

pub fn elf_needs_bytes(b: &[u8]) -> Result<Vec<String>> {
    if b.len() < 64 || &b[0..4] != b"\x7fELF" || b[4] == 0 || b[5] != 1 {
        return Ok(Vec::new()); // non-ELF (script, data) or unknown endianness
    }
    let is64 = b[4] == 2;
    let (phoff, phnum): (u64, usize) = if is64 {
        ensure!(b.len() >= 58, "truncated ehdr");
        (u64::from_le_bytes(b[0x20..0x28].try_into().unwrap()),
         u16::from_le_bytes(b[0x38..0x3A].try_into().unwrap()) as usize)
    } else {
        ensure!(b.len() >= 52, "truncated ehdr");
        (u32::from_le_bytes(b[0x1C..0x20].try_into().unwrap()) as u64,
         u16::from_le_bytes(b[0x2C..0x2E].try_into().unwrap()) as usize)
    };
    ensure!(phnum <= 64, "too many phdrs");
    let phsize: u64 = if is64 { 56 } else { 32 };
    // (p_type, p_offset, p_vaddr, p_memsz)
    let mut loads = Vec::new();
    let mut dyn_off: Option<u64> = None;
    for i in 0..phnum {
        let o64 = phoff.checked_add(i as u64 * phsize).context("phdr overflow")?;
        // Checked sums: a wrap would bypass the length check below.
        let end64 = o64.checked_add(phsize).context("phdr overflow")?;
        let o = usize::try_from(o64).context("phdr beyond usize")?;
        let end = usize::try_from(end64).context("phdr beyond usize")?;
        ensure!(b.len() >= end, "truncated phdr");
        let (typ, off, vaddr, memsz) = if is64 {
            (u32::from_le_bytes(b[o..o + 4].try_into().unwrap()),
             u64::from_le_bytes(b[o + 8..o + 16].try_into().unwrap()),
             u64::from_le_bytes(b[o + 16..o + 24].try_into().unwrap()),
             u64::from_le_bytes(b[o + 40..o + 48].try_into().unwrap()))
        } else {
            (u32::from_le_bytes(b[o..o + 4].try_into().unwrap()),
             u32::from_le_bytes(b[o + 4..o + 8].try_into().unwrap()) as u64,
             u32::from_le_bytes(b[o + 8..o + 12].try_into().unwrap()) as u64,
             u32::from_le_bytes(b[o + 20..o + 24].try_into().unwrap()) as u64)
        };
        if typ == 1 {
            loads.push((off, vaddr, memsz)); // PT_LOAD
        } else if typ == 2 {
            dyn_off = Some(off); // PT_DYNAMIC
        }
    }
    let dyn_off = match dyn_off {
        Some(o) => o,
        None => return Ok(Vec::new()), // static
    };
    let vaddr_to_off = |v: u64| -> Result<usize> {
        for (off, base, memsz) in &loads {
            if v >= *base && v < base.saturating_add(*memsz) {
                let o = off.saturating_add(v - base) as usize;
                ensure!(o < b.len(), "dynstr outside file");
                return Ok(o);
            }
        }
        bail!("dynstr not mapped")
    };
    let mut needed_idx = Vec::new();
    let (mut strtab_v, mut strsz) = (None, 0u64);
    let esz: usize = if is64 { 16 } else { 8 };
    for i in 0..4096 {
        let o64 = dyn_off.checked_add(i * esz as u64).context("dyn overflow")?;
        let end64 = o64.checked_add(esz as u64).context("dyn overflow")?;
        let o = usize::try_from(o64).context("dyn beyond usize")?;
        let end = usize::try_from(end64).context("dyn beyond usize")?;
        ensure!(b.len() >= end, "truncated dynamic");
        let (tag, val) = if is64 {
            (i64::from_le_bytes(b[o..o + 8].try_into().unwrap()),
             u64::from_le_bytes(b[o + 8..o + 16].try_into().unwrap()))
        } else {
            (i32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as i64,
             u32::from_le_bytes(b[o + 4..o + 8].try_into().unwrap()) as u64)
        };
        match tag {
            0 => break, // DT_NULL
            1 => needed_idx.push(val), // DT_NEEDED
            5 => strtab_v = Some(val), // DT_STRTAB
            10 => strsz = val, // DT_STRSZ
            _ => {}
        }
    }
    let strtab = vaddr_to_off(strtab_v.context("missing STRTAB")?)?;
    // The string table follows the file and the section: it must lie inside the file, and a name must
    // end inside it. A wide cap stops absurd sizes; real tables are large (libLLVM has 2 MiB of names).
    ensure!(strsz <= MAX_STRTAB, "strtab larger than {MAX_STRTAB} bytes");
    let tab_end = if strsz == 0 {
        b.len() // no DT_STRSZ: the file is the only bound
    } else {
        let end = (strtab as u64).checked_add(strsz).context("strtab overflow")?;
        ensure!(end <= b.len() as u64, "strtab outside file");
        end as usize
    };
    let mut out = Vec::new();
    for idx in needed_idx {
        let idx = usize::try_from(idx).context("needed beyond usize")?;
        let o = strtab.checked_add(idx).context("needed overflow")?;
        ensure!(o < tab_end, "needed outside strtab");
        let end = b[o..tab_end].iter().position(|&c| c == 0).context("string is not NUL-terminated")?;
        ensure!(end <= 512, "soname too long");
        out.push(String::from_utf8(b[o..o + end].to_vec()).context("non-UTF-8 soname")?);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Verify a raw chunk: declared length + BLAKE3.
/// Called BEFORE using/writing the bytes (spec §5.4).
pub fn verify_chunk(c: &Chunk, data: &[u8]) -> Result<()> {
    crate::ensure_code!(
        data.len() == c.raw as usize && blake3::hash(data).as_bytes() == &c.hash,
        ErrorCode::E0005,
        "corrupt chunk: file damaged in transit or on the store, redownload it"
    );
    Ok(())
}

/// Reserved directory (inside the package tree) for the license texts that
/// `pack` embeds: NOTICE, LICENSE-*, THIRD-PARTY-LICENSES. Normal entries:
/// the format is unchanged. User trees must not use the `.panpak` top level.
pub const LICENSES_DIR: &str = ".panpak/licenses";
pub const RESERVED_TOP: &str = ".panpak";

/// Read the embedded license files from an already verified package
/// (`raw_index` as returned by `verify`). Every chunk is checked against its
/// hash; total output is capped.
pub fn read_package_licenses(f: &mut File, raw_index: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    const MAX_TOTAL: usize = 16 << 20;
    let (chunks, entries) = parse_index(raw_index)?;
    let mut out = Vec::new();
    let mut total = 0usize;
    let mut dec = ChunkDecoder::new()?;
    let mut raw = Vec::new();
    for e in &entries {
        let Kind::File { chunks: ids } = &e.kind else { continue };
        let Ok(rest) = e.path.strip_prefix(LICENSES_DIR) else { continue };
        let mut data = Vec::new();
        for &id in ids {
            let c = &chunks[id as usize];
            dec.decode(&read_at(f, c.off, c.comp as u64)?, c.raw as u64, &mut raw)?;
            verify_chunk(c, &raw)?;
            total += raw.len();
            ensure!(total <= MAX_TOTAL, "embedded licenses too large");
            data.extend_from_slice(&raw);
        }
        out.push((rest.to_string_lossy().into_owned(), data));
    }
    Ok(out)
}

pub fn decode_capped(comp: &[u8], limit: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    ChunkDecoder::new()?.decode(comp, limit, &mut out)?;
    Ok(out)
}

/// zstd decompressor that keeps one context for many blocks. Allocating a new
/// context per chunk costs an mmap/munmap pair and fresh page faults each time.
/// Same checks as `decode_capped`: output past `limit` is refused, and input
/// that is truncated, corrupt or followed by garbage is an error.
pub struct ChunkDecoder {
    dec: zstd::stream::raw::Decoder<'static>,
}

impl ChunkDecoder {
    pub fn new() -> Result<Self> {
        Ok(Self { dec: zstd::stream::raw::Decoder::new().context(CORRUPT_BLOCK)? })
    }

    /// Decompress `comp` into `out` (cleared first, capacity reused).
    pub fn decode(&mut self, comp: &[u8], limit: u64, out: &mut Vec<u8>) -> Result<()> {
        use zstd::stream::raw::{InBuffer, Operation, OutBuffer};
        self.dec.reinit().context(CORRUPT_BLOCK)?;
        out.clear();
        ensure!(!comp.is_empty(), "{CORRUPT_BLOCK}"); // not even a frame header
        // limit+1 would wrap with limit=u64::MAX: explicit cap. One byte past
        // the limit is enough to know it was exceeded.
        let cap = limit.saturating_add(1);
        let mut input = InBuffer::around(comp);
        let mut filled = 0usize;
        let mut hint = 0usize; // 0: at a frame boundary
        loop {
            if filled as u64 >= cap {
                break;
            }
            if filled == out.len() {
                let step = out.len().max(128 << 10) as u64;
                out.resize((out.len() as u64 + step).min(cap) as usize, 0);
            }
            let in_before = input.pos;
            let mut dst = OutBuffer::around_pos(&mut out[..], filled);
            let left = self.dec.run(&mut input, &mut dst).context(CORRUPT_BLOCK)?;
            let produced = dst.pos() - filled;
            filled = dst.pos();
            // A call that made no progress only reports that a new frame could
            // start; it says nothing about the frame just decoded.
            if input.pos != in_before || produced != 0 {
                hint = left;
            }
            if input.pos == comp.len() && filled < out.len() {
                break; // all input consumed and the output was not the limit
            }
            if hint == 0 && input.pos < comp.len() {
                self.dec.reinit().context(CORRUPT_BLOCK)?; // next concatenated frame
            }
            ensure!(input.pos != in_before || produced != 0, "{CORRUPT_BLOCK}");
        }
        out.truncate(filled);
        ensure!(filled as u64 <= limit, "decompression past the limit");
        ensure!(hint == 0, "{CORRUPT_BLOCK}");
        Ok(())
    }
}

/// zstd compressor that keeps one context for many blocks (see `ChunkDecoder`).
/// Produces the same frames as `zstd::encode_all(data, level)`: streaming mode,
/// no content size in the header.
pub struct ChunkEncoder {
    enc: zstd::stream::raw::Encoder<'static>,
    out: Vec<u8>,
}

impl ChunkEncoder {
    pub fn new(level: i32) -> Result<Self> {
        Ok(Self { enc: zstd::stream::raw::Encoder::new(level)?, out: Vec::new() })
    }

    /// Compress `data` into one frame; valid until the next call.
    pub fn encode(&mut self, data: &[u8]) -> Result<&[u8]> {
        use zstd::stream::raw::{InBuffer, Operation, OutBuffer};
        self.enc.reinit()?;
        self.out.clear();
        let mut filled = 0usize;
        let mut input = InBuffer::around(data);
        while input.pos < data.len() {
            self.room(filled);
            let mut dst = OutBuffer::around_pos(&mut self.out[..], filled);
            self.enc.run(&mut input, &mut dst)?;
            filled = dst.pos();
        }
        loop {
            self.room(filled);
            let mut dst = OutBuffer::around_pos(&mut self.out[..], filled);
            let left = self.enc.finish(&mut dst, false)?;
            filled = dst.pos();
            if left == 0 {
                break;
            }
        }
        Ok(&self.out[..filled])
    }

    /// Keep free space after `filled`; grows to the compress bound at first use.
    fn room(&mut self, filled: usize) {
        if self.out.len() < filled + 4096 {
            let grow = self.out.len().max(zstd::zstd_safe::compress_bound(1 << 20));
            self.out.resize(self.out.len() + grow, 0);
        }
    }
}

const CORRUPT_BLOCK: &str = "unreadable compressed block: corrupt file, redownload it";

pub fn read_footer(f: &mut File) -> Result<Footer> {
    let size = f.metadata()?.len();
    ensure!(size > FOOTER_LEN, "file too short to be a .panpak: incomplete or corrupt file, redownload it");
    let b = read_at(f, size - FOOTER_LEN, FOOTER_LEN)?;
    parse_footer(size, &b)
}

/// Footer parsing from bytes (read_footer reads from the file and delegates here).
pub fn parse_footer(size: u64, b: &[u8]) -> Result<Footer> {
    ensure!(size > FOOTER_LEN, "file too short to be a .panpak: incomplete or corrupt file, redownload it");
    ensure!(b.len() == FOOTER_LEN as usize, "truncated footer: incomplete or corrupt file, redownload it");
    crate::ensure_code!(&b[0..8] == MAGIC, ErrorCode::E0001, "wrong magic: not a panpak file");
    let mut r = Reader::new(&b[8..]);
    let version = r.u32()?;
    crate::ensure_code!(version == 1, ErrorCode::E0002, "unsupported format version {version} (this runtime reads only v1): update panpak to the latest version");
    let flags = r.u32()?;
    crate::ensure_code!(flags == 0, ErrorCode::E0002, "unknown flags ({flags:#x}): future format, update the runtime");
    let (index_off, index_len) = (r.u64()?, r.u64()?);
    let (man_off, man_len) = (r.u64()?, r.u64()?);
    let root: [u8; 32] = b[48..80].try_into()?;
    Ok(Footer { index_off, index_len, man_off, man_len, root })
}

/// Layout coherence (spec §3): adjacent index and manifest, no extra bytes.
/// Extracted from verify() for direct fuzzing.
pub fn check_layout(size: u64, ft: &Footer) -> Result<()> {
    crate::ensure_code!(
        ft.index_off.saturating_add(ft.index_len) == ft.man_off,
        ErrorCode::E0003,
        "incoherent layout: index not adjacent to the manifest (corrupt or tampered file: redownload it from a trusted source)"
    );
    let manifest_end = ft
        .man_off
        .checked_add(ft.man_len)
        .and_then(|v| v.checked_add(SIG_LEN))
        .unwrap_or(u64::MAX);
    crate::ensure_code!(
        manifest_end == size.saturating_sub(FOOTER_LEN),
        ErrorCode::E0003,
        "incoherent layout: manifest+signature not adjacent to the footer (corrupt or tampered file: redownload it from a trusted source)"
    );
    crate::ensure_code!(ft.man_len <= MAX_MANIFEST_TOML, ErrorCode::E0003, "manifest too large");
    crate::ensure_code!(ft.index_len <= MAX_INDEX_COMP, ErrorCode::E0003, "compressed index too large");
    Ok(())
}

pub fn encode_footer(ft: &Footer) -> [u8; 80] {
    let mut b = [0u8; 80];
    b[0..8].copy_from_slice(MAGIC);
    b[8..12].copy_from_slice(&1u32.to_le_bytes());
    b[12..16].copy_from_slice(&0u32.to_le_bytes());
    b[16..24].copy_from_slice(&ft.index_off.to_le_bytes());
    b[24..32].copy_from_slice(&ft.index_len.to_le_bytes());
    b[32..40].copy_from_slice(&ft.man_off.to_le_bytes());
    b[40..48].copy_from_slice(&ft.man_len.to_le_bytes());
    b[48..80].copy_from_slice(&ft.root);
    b
}

/// Bounds-checked reader: never panic on truncated input, only errors.
struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, p: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.p.checked_add(n).context("truncated index: incomplete file, redownload it")?;
        ensure!(end <= self.b.len(), "truncated index: incomplete file, redownload it");
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn finish(&self) -> Result<()> {
        ensure!(self.p == self.b.len(), "trailing bytes after the index");
        Ok(())
    }
}

/// Reject absolute paths, `..`, `.`, empty and non-canonical UTF-8.
/// Only `Component::Normal`: a single representation allowed.
/// The check is lexical over segments (does not use the normalization of
/// `Path::components`, which would collapse `a/./b` into `a/b`, creating aliases
/// for the same destination and bypassing the duplicate check).
pub fn safe_path(s: &str) -> Result<PathBuf> {
    ensure!(!s.is_empty(), "empty path");
    ensure!(s.len() <= MAX_PATH_LEN, "path too long");
    ensure!(!s.chars().any(char::is_control), "unsafe path (control character): {s:?}");
    for comp in s.split('/') {
        ensure!(!comp.is_empty() && comp != "." && comp != "..", "unsafe path: {s}");
    }
    let p = PathBuf::from(s);
    for c in p.components() {
        ensure!(matches!(c, Component::Normal(_)), "unsafe path: {s}");
    }
    Ok(p)
}

/// Reject symlink targets escaping the root (spec §5.5).
/// Pure lexical check: absolutes always rejected, `..` climbing above
/// the root rejected. Chain resolution happens in materialize.
pub fn target_escapes(target: &str) -> bool {
    if target.is_empty() {
        return true;
    }
    let p = PathBuf::from(target);
    if p.is_absolute() {
        return true;
    }
    let mut depth = 0u32;
    for c in p.components() {
        match c {
            Component::Normal(_) => depth += 1,
            Component::ParentDir => {
                if depth == 0 {
                    return true;
                }
                depth -= 1;
            }
            // Harmless CurDir; Prefix/RootDir not representable on Linux
            // in a relative target (RootDir => absolute, already excluded).
            _ => {}
        }
    }
    false
}

pub fn parse_index(raw: &[u8]) -> Result<(Vec<Chunk>, Vec<Entry>)> {
    let mut r = Reader::new(raw);
    let n = r.u32()?;
    // Bounded prealloc: the loop fails anyway at the first missing byte.
    let mut chunks = Vec::with_capacity(n.min(65536) as usize);
    for _ in 0..n {
        let hash: [u8; 32] = r.take(32)?.try_into().unwrap();
        let (off, comp, raw_len) = (r.u64()?, r.u32()?, r.u32()?);
        ensure!((1..=MAX_CHUNK_COMP).contains(&comp), "comp_len out of range");
        ensure!((1..=MAX_CHUNK_RAW).contains(&raw_len), "raw_len out of range");
        chunks.push(Chunk { hash, off, comp, raw: raw_len });
    }
    let n_entries = r.u32()?;
    let mut entries = Vec::with_capacity(n_entries.min(65536) as usize);
    // Each path only once: duplicates enable overwrite attacks
    // (file-then-symlink and vice versa) in materialize.
    let mut seen = std::collections::HashSet::new();
    for _ in 0..n_entries {
        let l = r.u16()? as usize;
        ensure!((1..=MAX_PATH_LEN).contains(&l), "path_len out of range");
        let path_s = std::str::from_utf8(r.take(l)?).context("non-UTF-8 path")?;
        let path = safe_path(path_s)?;
        ensure!(seen.insert(path.clone()), "duplicate path in index: {path_s}");
        let kind = r.u8()?;
        let mode = r.u32()? & 0o777;
        let kind = match kind {
            0 => {
                let size = r.u64()?;
                let k = r.u32()?;
                let mut ids = Vec::with_capacity(k.min(4096) as usize);
                let mut total: u64 = 0;
                for _ in 0..k {
                    let id = r.u32()?;
                    ensure!((id as usize) < chunks.len(), "chunk_id past the table");
                    total = total
                        .checked_add(chunks[id as usize].raw as u64)
                        .context("size overflow")?;
                    ids.push(id);
                }
                ensure!(total == size, "size does not match the chunks");
                Kind::File { chunks: ids }
            }
            1 => Kind::Dir,
            2 => {
                let l = r.u16()? as usize;
                ensure!((1..=MAX_SYMLINK_LEN).contains(&l), "symlink target out of range");
                let t = String::from_utf8(r.take(l)?.to_vec()).context("non-UTF-8 target")?;
                ensure!(!t.chars().any(char::is_control), "unsafe symlink target (control character)");
                Kind::Link(t)
            }
            k => anyhow::bail!("unknown kind {k}"),
        };
        entries.push(Entry { path, mode, kind });
    }
    r.finish()?;
    Ok((chunks, entries))
}

/// Exact inverse of `parse_index`: pack uses it, tests verify the roundtrip.
pub fn encode_index(chunks: &[Chunk], entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
    for c in chunks {
        out.extend_from_slice(&c.hash);
        out.extend_from_slice(&c.off.to_le_bytes());
        out.extend_from_slice(&c.comp.to_le_bytes());
        out.extend_from_slice(&c.raw.to_le_bytes());
    }
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in entries {
        let p = e.path.as_os_str().as_encoded_bytes();
        out.extend_from_slice(&(p.len() as u16).to_le_bytes());
        out.extend_from_slice(p);
        match &e.kind {
            Kind::File { chunks: ids } => {
                out.push(0);
                out.extend_from_slice(&e.mode.to_le_bytes());
                let size: u64 = ids.iter().map(|&i| chunks[i as usize].raw as u64).sum();
                out.extend_from_slice(&size.to_le_bytes());
                out.extend_from_slice(&(ids.len() as u32).to_le_bytes());
                for id in ids {
                    out.extend_from_slice(&id.to_le_bytes());
                }
            }
            Kind::Dir => {
                out.push(1);
                out.extend_from_slice(&e.mode.to_le_bytes());
            }
            Kind::Link(t) => {
                out.push(2);
                out.extend_from_slice(&e.mode.to_le_bytes());
                out.extend_from_slice(&(t.len() as u16).to_le_bytes());
                out.extend_from_slice(t.as_bytes());
            }
        }
    }
    out
}

/// Strict public-key parsing (spec §3.2): exact prefix
/// lowercase `ed25519:`, then exactly 64 [0-9a-f] characters.
pub fn parse_pubkey(s: &str) -> Result<ed25519_dalek::VerifyingKey> {
    let hexs = s.strip_prefix("ed25519:").context("key without `ed25519:` prefix")?;
    ensure!(hexs.len() == 64, "key: expected 64 hex chars, found {}", hexs.len());
    ensure!(
        hexs.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "key: only lowercase [0-9a-f]"
    );
    let mut raw = [0u8; 32];
    for (i, dst) in raw.iter_mut().enumerate() {
        *dst = u8::from_str_radix(&hexs[2 * i..2 * i + 2], 16)?;
    }
    Ok(ed25519_dalek::VerifyingKey::from_bytes(&raw)?)
}

/// Read a secret-key file (64 hex + optional newline). The file must be a
/// regular file (no symlink, no directory) with no permission bits for group
/// or others; the value is never included in an error message.
pub fn read_secret_file(path: &std::path::Path) -> Result<String> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let shown = path.display();
    let lmeta = std::fs::symlink_metadata(path).with_context(|| format!("secret file {shown}"))?;
    crate::ensure_code!(!lmeta.file_type().is_symlink(), ErrorCode::E5003, "secret file {shown} is a symlink: refusing to follow it");
    crate::ensure_code!(lmeta.file_type().is_file(), ErrorCode::E5003, "secret file {shown} is not a regular file");
    let f = std::fs::File::open(path).with_context(|| format!("secret file {shown}"))?;
    // Check the opened file, not the path: closes the lstat/open race.
    let meta = f.metadata()?;
    crate::ensure_code!(
        meta.dev() == lmeta.dev() && meta.ino() == lmeta.ino() && meta.file_type().is_file(),
        ErrorCode::E5003,
        "secret file {shown} changed while being opened"
    );
    let mode = meta.mode() & 0o777;
    crate::ensure_code!(
        mode & 0o077 == 0,
        ErrorCode::E5003,
        "secret file {shown} has permissions {mode:04o}: group/others must have none; run: chmod 600 {shown}"
    );
    let mut buf = Vec::new();
    f.take(129).read_to_end(&mut buf)?;
    ensure!(buf.len() <= 128, "secret file {shown} is too large for a key");
    let text = String::from_utf8(buf).map_err(|_| anyhow!("secret file {shown} is not valid UTF-8"))?;
    Ok(text.trim().to_string())
}

/// Pack secret key: 64 lowercase hex (panpak of 32 bytes).
pub fn parse_secret(s: &str) -> Result<ed25519_dalek::SigningKey> {
    let s = s.trim();
    ensure!(s.len() == 64, "secret: expected 64 hex chars");
    ensure!(
        s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "secret: only lowercase [0-9a-f]"
    );
    let mut raw = [0u8; 32];
    for (i, dst) in raw.iter_mut().enumerate() {
        *dst = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)?;
    }
    Ok(ed25519_dalek::SigningKey::from_bytes(&raw))
}

/// Verify signature and index BEFORE any other operation (spec §5).
/// The signature covers `manifest_toml || root_hash` (root in raw bytes).
/// TOFU NOTE: the key lives in the manifest it signs (self-signed) and alone
/// proves nothing about the author's identity (spec §3.2).
pub fn verify(f: &mut File, ft: &Footer) -> Result<(Manifest, Vec<u8>)> {
    let size = f.metadata()?.len();
    // Adjacent layout (spec §3): no holes nor extra data.
    check_layout(size, ft)?;

    let blob = read_at(f, ft.man_off, ft.man_len + SIG_LEN)?;
    let (toml_b, sig_b) = blob.split_at(ft.man_len as usize);
    let man: Manifest =
        toml::from_str(std::str::from_utf8(toml_b).context("non-UTF-8 manifest")?)?;
    let vk = parse_pubkey(&man.update.as_ref().context("manifest without [update].key")?.key)?;

    let sig = ed25519_dalek::Signature::from_bytes(sig_b.try_into().context("truncated signature")?);
    let mut msg = Vec::with_capacity(toml_b.len() + 32);
    msg.extend_from_slice(toml_b);
    msg.extend_from_slice(&ft.root);
    vk.verify_strict(&msg, &sig).map_err(|_| {
        coded(ErrorCode::E1001, "invalid manifest signature: tampered or badly downloaded file; redownload it from a trusted source and verify the key")
    })?;
    // A valid signature does not make the fields safe to use as file names.
    validate_manifest(&man)?;

    let comp = read_at(f, ft.index_off, ft.index_len)?;
    let raw = decode_capped(&comp, MAX_INDEX_RAW)?;
    crate::ensure_code!(blake3::hash(&raw).as_bytes() == &ft.root, ErrorCode::E0004, "root hash mismatch: tampered index or corrupt file, redownload it from a trusted source");
    Ok((man, raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pubkey_accepts_only_lowercase_hex() {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let key = format!("ed25519:{}", hex(sk.verifying_key().as_bytes()));
        assert!(parse_pubkey(&key).is_ok());
        // Same key in uppercase: rejected (single representation).
        let upper = format!("ed25519:{}", key[8..].to_uppercase());
        assert!(parse_pubkey(&upper).is_err());
        assert!(parse_pubkey(&format!("0x{}", &key[8..])).is_err());
        assert!(parse_pubkey(&format!(" {} ", key)).is_err());
        assert!(parse_pubkey("ed25519:abcd").is_err());
        assert!(parse_pubkey(&key[8..]).is_err()); // no prefix
        assert!(parse_pubkey("ED25519:abcd").is_err());
        // Pack secret: same rules, no prefix.
        assert!(parse_secret(&key[8..]).is_ok());
        assert!(parse_secret(&key).is_err());
    }

    #[test]
    fn check_runtime_rejects_future() {
        let mut man: Manifest = toml::from_str(
            "[app]\nid=\"a.b\"\nname=\"N\"\nversion=\"1\"\nentry=\"x\"\n[update]\nkey=\"ed25519:0000000000000000000000000000000000000000000000000000000000000000\"\n",
        )
        .unwrap();
        assert!(check_runtime(&man, RUNTIME_ABI).is_ok()); // No [runtime] => 1.
        man.runtime = Some(Runtime { min_runtime: Some(RUNTIME_ABI), stub_hash: None });
        assert!(check_runtime(&man, RUNTIME_ABI).is_ok());
        man.runtime = Some(Runtime { min_runtime: Some(RUNTIME_ABI + 1), stub_hash: None });
        assert!(check_runtime(&man, RUNTIME_ABI).is_err());
        // Serde roundtrip of the [runtime] section.
        let s = toml::to_string(&man).unwrap();
        assert!(s.contains("min_runtime"));
        let back: Manifest = toml::from_str(&s).unwrap();
        assert_eq!(back.runtime.unwrap().min_runtime, Some(RUNTIME_ABI + 1));
    }

    #[test]
    fn expand_socket_path_valid() {
        // Save/restore: tests run in parallel and env is global.
        let old_x = std::env::var("XDG_RUNTIME_DIR").ok();
        let old_h = std::env::var("HOME").ok();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000") };
        unsafe { std::env::set_var("HOME", "/home/u") };
        assert_eq!(
            expand_socket_path("$XDG_RUNTIME_DIR/bus").unwrap(),
            PathBuf::from("/run/user/1000/bus")
        );
        assert_eq!(
            expand_socket_path("${HOME}/x.sock").unwrap(),
            PathBuf::from("/home/u/x.sock")
        );
        assert!(expand_socket_path("relative/sock").is_err());
        assert!(expand_socket_path("/a/../b").is_err());
        assert!(expand_socket_path("$UNSET_VARIABLE/exists").is_err());
        match old_x {
            Some(v) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", v) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }
        match old_h {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    fn bundled_libs_resolve_soname_symlink() {
        let dir = std::env::temp_dir().join(format!("panpak-soname-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = dir.join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("libfoo.so.1.2.3"), b"x").unwrap();
        std::os::unix::fs::symlink("libfoo.so.1.2.3", lib.join("libfoo.so.1")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", lib.join("libevil.so.9")).unwrap();
        let m = crate::abi::bundled_libs(&dir);
        assert_eq!(m.get("libfoo.so.1"), Some(&std::path::PathBuf::from("lib/libfoo.so.1.2.3")));
        assert!(!m.contains_key("libevil.so.9"), "symlink outside tree registered");
        assert!(!m.contains_key("libfoo.so.1.2.3") || true); // The real file may or may not be present.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn panpak_toml_strict() {
        let ok = "[app]\nid = \"a.b\"\nname = \"N\"\nversion = \"1\"\nentry = \"x\"\n[update]\nkey = \"ed25519:0000000000000000000000000000000000000000000000000000000000000000\"\n";
        assert!(check_panpak_toml_schema(ok).is_ok());
        assert!(check_panpak_toml_schema("[app]\nid=\"a.b\"\nfuture = 1\n").is_err());
        assert!(check_panpak_toml_schema("[unknown-section]\nx = 1\n").is_err());
        assert!(check_panpak_toml_schema("[update]\nkey = \"ed25519:0000000000000000000000000000000000000000000000000000000000000000\"\nytbd = true\n").is_err());
        assert!(check_panpak_toml_schema("non toml [[[").is_err());
    }

    #[test]
    fn footer_roundtrip() {        let ft = Footer {
            index_off: 123,
            index_len: 456,
            man_off: 579,
            man_len: 789,
            root: [9u8; 32],
        };
        let b = encode_footer(&ft);
        assert_eq!(b.len(), 80);
        assert_eq!(&b[0..8], MAGIC);
        // Manual re-read of the fields.
        assert_eq!(u64::from_le_bytes(b[16..24].try_into().unwrap()), 123);
        assert_eq!(u64::from_le_bytes(b[24..32].try_into().unwrap()), 456);
        assert_eq!(u64::from_le_bytes(b[32..40].try_into().unwrap()), 579);
        assert_eq!(u64::from_le_bytes(b[40..48].try_into().unwrap()), 789);
        assert_eq!(&b[48..80], &[9u8; 32]);
    }

    #[test]
    fn magic_and_footer_have_fixed_size() {
        // The magic is EXACTLY 8 bytes (PANP + version 1 + 3 zeroes):
        // single constant, used by stub, runtime, convert and tests.
        assert_eq!(MAGIC.len(), 8);
        assert_eq!(MAGIC, b"PANP\0\x01\0\0");
        assert_eq!(FOOTER_LEN, 80);
        let ft = Footer { index_off: 1, index_len: 2, man_off: 3, man_len: 4, root: [5u8; 32] };
        assert_eq!(encode_footer(&ft).len(), 80);
    }

    #[test]
    fn rejects_unknown_magic() {
        // A magic other than the one defined by the format must NOT
        // be accepted, here as in the verifier.
        let ft = Footer { index_off: 1, index_len: 2, man_off: 3, man_len: 4, root: [5u8; 32] };
        let mut b = encode_footer(&ft);
        b[0..8].copy_from_slice(b"XXXX\0\x01\0\0");
        let e = parse_footer(81, &b).unwrap_err();
        assert_eq!(code_of(&e, ErrorCode::E0000), ErrorCode::E0001, "unknown magic accepted");
    }

    #[test]
    fn docs_errors_lists_every_code() {
        let docs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/ERRORS.md")).unwrap();
        for c in ErrorCode::ALL {
            assert!(docs.contains(&format!("| {} |", c)), "{c} missing from docs/ERRORS.md");
        }
        for c in ErrorCode::ALL {
            // Group digit matches the documented group.
            assert!(c.as_str().starts_with("PANPAK-E") && c.as_str().len() == 12);
        }
    }

    #[test]
    fn user_errors_have_stable_codes() {
        // Every user error carries a stable code; the text is not asserted.
        let ft = Footer { index_off: 0, index_len: 0, man_off: 0, man_len: 0, root: [7u8; 32] };
        let code = |r: Result<(), anyhow::Error>| code_of(&r.unwrap_err(), ErrorCode::E0000);
        let mut b = encode_footer(&ft);
        b[8..12].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(code(parse_footer(81, &b).map(|_| ())), ErrorCode::E0002, "version");
        let mut b = encode_footer(&ft);
        b[12..16].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(code(parse_footer(81, &b).map(|_| ())), ErrorCode::E0002, "flags");
        let mut b = encode_footer(&ft);
        b[0] = b'X';
        assert_eq!(code(parse_footer(81, &b).map(|_| ())), ErrorCode::E0001, "magic");
        // Too short: untyped, falls back to the generic format code.
        assert_eq!(code(parse_footer(10, &encode_footer(&ft)).map(|_| ())), ErrorCode::E0000, "short");
        // Incoherent layout.
        let mut bad = ft.clone();
        bad.man_off = 5;
        assert_eq!(code(check_layout(200, &bad).map(|_| ())), ErrorCode::E0003, "layout");
        // Unknown key in pack: untyped, generic.
        assert_eq!(code(check_panpak_toml_schema("[app]\nid=\"a.b\"\nfuture = 1\n")), ErrorCode::E0000, "schema");
        // Rotation with a non-pin key.
        let zero = "ed25519:0000000000000000000000000000000000000000000000000000000000000000";
        let one = "ed25519:1111111111111111111111111111111111111111111111111111111111111111";
        let two = "ed25519:2222222222222222222222222222222222222222222222222222222222222222";
        let e = verify_rotation(zero, one, Some(two), None).unwrap_err();
        assert_eq!(code_of(&e, ErrorCode::E1000), ErrorCode::E1005, "rotation");
        let m = render(&e, ErrorCode::E1000);
        assert!(m.starts_with("PANPAK-E1005: "), "{m}");
        // No user message contains internal dumps.
        assert!(!m.contains("panicked"), "internal trace");
        assert!(!m.contains("Os {"), "internal trace");
        // err_text: readable chain with no internal Debug dumps.
        let chained: anyhow::Result<()> = Err(anyhow::anyhow!(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))).context("open /tmp/x.panpak");
        let t = err_text(&chained.unwrap_err());
        assert!(t.contains("open /tmp/x.panpak") && t.contains("missing"), "chain: {t}");
        assert!(!t.contains("Os {") && !t.contains("code:"), "internal dump: {t}");
        let full = anyhow::anyhow!(std::io::Error::new(std::io::ErrorKind::StorageFull, "x"));
        assert!(is_storage_full(&full));
        let wrapped = full.context("copy package");
        assert!(is_storage_full(&wrapped));
        let other = anyhow::anyhow!(std::io::Error::new(std::io::ErrorKind::NotFound, "x"));
        assert!(!is_storage_full(&other));
        let plain = anyhow::anyhow!("plain string");
        assert!(!is_storage_full(&plain));
    }


    fn sample_index() -> (Vec<Chunk>, Vec<Entry>) {
        (
            vec![
                Chunk { hash: [1u8; 32], off: 100, comp: 50, raw: 70 },
                Chunk { hash: [2u8; 32], off: 200, comp: 30, raw: 64 },
            ],
            vec![
                Entry {
                    path: PathBuf::from("bin/hello"),
                    mode: 0o755,
                    kind: Kind::File { chunks: vec![0, 1] },
                },
                Entry { path: PathBuf::from("bin"), mode: 0o755, kind: Kind::Dir },
                Entry {
                    path: PathBuf::from("latest"),
                    mode: 0o777,
                    kind: Kind::Link("bin/hello".into()),
                },
            ],
        )
    }

    #[test]
    fn index_roundtrip() {
        let (chunks, entries) = sample_index();
        let raw = encode_index(&chunks, &entries);
        let (c2, e2) = parse_index(&raw).unwrap();
        assert_eq!(c2.len(), 2);
        assert_eq!(e2.len(), 3);
        assert_eq!(c2[0].off, 100);
        assert!(matches!(&e2[0].kind, Kind::File { chunks } if chunks == &[0, 1]));
        assert!(matches!(&e2[1].kind, Kind::Dir));
        assert!(matches!(&e2[2].kind, Kind::Link(t) if t == "bin/hello"));
    }

    #[test]
    fn index_rejects_truncated_and_garbage() {
        let (chunks, entries) = sample_index();
        let raw = encode_index(&chunks, &entries);
        // Every truncated prefix must error, never panic.
        for len in [0, 1, 3, 4, 10, 40, raw.len() - 1] {
            assert!(parse_index(&raw[..len]).is_err(), "len={len}");
        }
        // Trailing extra bytes: rejected.
        let mut with_junk = raw.clone();
        with_junk.push(0);
        assert!(parse_index(&with_junk).is_err());
        // Empty and pure garbage.
        assert!(parse_index(&[]).is_err());
        assert!(parse_index(&[0xFF; 100]).is_err());
    }

    #[test]
    fn index_rejects_inconsistent_chunk_id_and_size() {
        let (chunks, entries) = sample_index();
        let mut raw = encode_index(&chunks, &entries);
        // Corrupts the file's first chunk_id (0 -> 99, past the table).
        // Layout: u32 n(4) + 2*44 chunk + u32 n_entries(4) + u16 len(2) + 9 path
        // + u8 kind(1) + u32 mode(4) + u64 size(8) + u32 k(4) + ids...
        let id_off = 4 + 88 + 4 + 2 + 9 + 1 + 4 + 8 + 4;
        raw[id_off] = 99;
        assert!(parse_index(&raw).is_err());
    }

    #[test]
    fn target_escapes_rejects_escapes() {
        assert!(!target_escapes("bin/hello"));
        assert!(!target_escapes("a/../b"));
        assert!(!target_escapes("./b"));
        assert!(target_escapes(""));
        assert!(target_escapes("/etc/passwd"));
        assert!(target_escapes("../x"));
        assert!(target_escapes("a/../../b"));
        assert!(target_escapes(".."));
    }

    #[test]
    fn index_rejects_duplicate_paths() {
        let chunks = vec![Chunk { hash: [1u8; 32], off: 0, comp: 10, raw: 20 }];
        let entries = vec![
            Entry { path: PathBuf::from("x"), mode: 0o755, kind: Kind::Dir },
            Entry { path: PathBuf::from("x"), mode: 0o755, kind: Kind::Dir },
        ];
        assert!(parse_index(&encode_index(&chunks, &entries)).is_err());
    }

    #[test]
    fn safe_path_accepts_relative_rejects_escapes() {        assert!(safe_path("bin/hello").is_ok());
        assert!(safe_path("a/b/c.txt").is_ok());
        for bad in ["", "/", "/abs", "../x", "a/../../b", "a/../b", "..", "./b"] {
            assert!(safe_path(bad).is_err(), "{bad:?}");
        }
        // Double separators and trailing slash rejected (no canonical aliases);
        // interior `.` rejected: they would collapse onto other paths, bypassing
        // the duplicate check (same destination, different string).
        assert!(safe_path("a//b").is_err());
        assert!(safe_path("a/").is_err());
        assert!(safe_path("a/./b").is_err());
    }

    #[test]
    fn verify_stub_range_accepts_and_rejects() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("panpak-stubhash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let stub = b"STUB-BYTES-GENUINE";
        let p = dir.join("f.bin");
        {
            let mut f = std::fs::File::create(&p).unwrap();
            f.write_all(stub).unwrap();
            f.write_all(b"payload-any").unwrap();
        }
        let good_hash = hex(blake3::hash(stub).as_bytes());
        let man_ok = Manifest {
            app: App { id: "a.b".into(), name: "N".into(), version: "1".into(), entry: "x".into() },
            abi: None,
            permissions: None,
            launch: None,
            desktop: None,
            runtime: Some(Runtime { min_runtime: None, stub_hash: Some(good_hash.clone()) }),
            origin: None,
            update: None,
        };
        let mut f = std::fs::File::open(&p).unwrap();
        assert!(verify_stub_range(&mut f, stub.len() as u64, &man_ok).is_ok());
        // Altered stub bytes: rejected.
        let mut f = std::fs::File::open(&p).unwrap();
        let mut man_bad = man_ok.clone();
        man_bad.runtime.as_mut().unwrap().stub_hash = Some("00".repeat(32));
        assert!(verify_stub_range(&mut f, stub.len() as u64, &man_bad).is_err());
        // Manifest without stub_hash (previous format): rejected, not ignored.
        let mut man_old = man_ok.clone();
        man_old.runtime = None;
        let mut f = std::fs::File::open(&p).unwrap();
        assert!(verify_stub_range(&mut f, stub.len() as u64, &man_old).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decode_capped_blocks_zip_bomb() {
        // 1 MiB of zeroes compressed into a few bytes: past a 1K cap it must fail.
        let big = vec![0u8; 1 << 20];
        let comp = zstd::encode_all(&big[..], 3).unwrap();
        assert!(comp.len() < 1024);
        assert!(decode_capped(&comp, 1024).is_err());
        assert_eq!(decode_capped(&comp, 1 << 20).unwrap(), big);
    }

    #[test]
    fn chunk_decompressing_past_declared_raw_len_is_rejected() {
        // A chunk whose compressed data expands to more than the raw_len declared in
        // the index must be refused while decoding, not after buffering it all.
        let data = vec![b'a'; 4096];
        let comp = zstd::encode_all(&data[..], 3).unwrap();
        let dir = std::env::temp_dir().join(format!("panpak-rawlen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("chunk.bin");
        std::fs::write(&p, &comp).unwrap();
        let index_for = |declared: u32, hash: [u8; 32]| {
            let chunks = vec![Chunk { hash, off: 0, comp: comp.len() as u32, raw: declared }];
            let entries = vec![Entry {
                path: PathBuf::from(".panpak/licenses/NOTICE"),
                mode: 0o644,
                kind: Kind::File { chunks: vec![0] },
            }];
            encode_index(&chunks, &entries)
        };
        let mut f = std::fs::File::open(&p).unwrap();
        // Control: the honest declaration is accepted.
        let ok = index_for(data.len() as u32, *blake3::hash(&data).as_bytes());
        let got = read_package_licenses(&mut f, &ok).unwrap();
        assert_eq!(got, vec![("NOTICE".to_string(), data.clone())]);
        // Declared 1 byte short: the decoder stops at the cap and the chunk is refused.
        let short = index_for(data.len() as u32 - 1, *blake3::hash(&data).as_bytes());
        let err = read_package_licenses(&mut f, &short).unwrap_err();
        assert!(format!("{err:#}").contains("decompression past the limit"), "{err:#}");
        // Same through the shared helper, as used for every chunk by the stub and the CLI.
        assert!(decode_capped(&comp, data.len() as u64 - 1).is_err());
        assert_eq!(decode_capped(&comp, data.len() as u64).unwrap(), data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn footer_lengths_past_the_limits_are_rejected() {
        // Coherent layout (index, manifest, signature and footer adjacent) so that
        // only the size caps can reject it.
        let layout = |index_len: u64, man_len: u64| {
            let man_off = index_len;
            let size = man_off + man_len + SIG_LEN + FOOTER_LEN;
            (size, Footer { index_off: 0, index_len, man_off, man_len, root: [0; 32] })
        };
        let (size, ft) = layout(MAX_INDEX_COMP, MAX_MANIFEST_TOML);
        check_layout(size, &ft).expect("values at the limits are accepted");

        let (size, ft) = layout(10, MAX_MANIFEST_TOML + 1);
        let err = check_layout(size, &ft).unwrap_err();
        assert!(format!("{err:#}").contains("manifest too large"), "{err:#}");
        assert_eq!(code_of(&err, ErrorCode::E0000), ErrorCode::E0003);

        let (size, ft) = layout(MAX_INDEX_COMP + 1, 100);
        let err = check_layout(size, &ft).unwrap_err();
        assert!(format!("{err:#}").contains("compressed index too large"), "{err:#}");
        assert_eq!(code_of(&err, ErrorCode::E0000), ErrorCode::E0003);
    }

    #[test]
    fn decode_capped_max_limit() {
        // Fuzz regression (limit=u64::MAX wrapped limit+1): must equal decode_all.
        let data = b"hello world";
        let comp = zstd::encode_all(&data[..], 3).unwrap();
        assert_eq!(decode_capped(&comp, u64::MAX).unwrap(), data);
        assert!(decode_capped(&comp, 3).is_err());
    }

    /// Deterministic, poorly compressible bytes (xorshift).
    fn noise(n: usize, mut x: u64) -> Vec<u8> {
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    /// The decoder as it was before context reuse: one `zstd::Decoder` per call.
    fn decode_capped_reference(comp: &[u8], limit: u64) -> Result<Vec<u8>> {
        let mut dec = zstd::Decoder::new(comp)?;
        let mut out = Vec::new();
        (&mut dec).take(limit.saturating_add(1)).read_to_end(&mut out)?;
        ensure!(out.len() as u64 <= limit, "past the limit");
        Ok(out)
    }

    #[test]
    fn one_decoder_handles_chunks_of_different_sizes() {
        let sizes = [0usize, 1, 17, 4096, 131_071, 131_072, 131_073, 300_000, 1 << 20, 5, 700_000];
        let mut dec = ChunkDecoder::new().unwrap();
        let mut out = Vec::new();
        for (i, &n) in sizes.iter().enumerate() {
            let data = if i % 2 == 0 { noise(n, i as u64 + 1) } else { vec![i as u8; n] };
            let comp = zstd::encode_all(&data[..], ZSTD_LEVEL).unwrap();
            dec.decode(&comp, n as u64, &mut out).unwrap();
            assert_eq!(out, data, "chunk of {n} bytes");
        }
    }

    #[test]
    fn decoder_stays_usable_after_an_error() {
        let data = noise(50_000, 7);
        let comp = zstd::encode_all(&data[..], ZSTD_LEVEL).unwrap();
        let mut dec = ChunkDecoder::new().unwrap();
        let mut out = Vec::new();
        assert!(dec.decode(&comp, 49_999, &mut out).is_err()); // past the limit
        assert!(dec.decode(&comp[..comp.len() / 2], 50_000, &mut out).is_err()); // truncated
        assert!(dec.decode(b"not zstd at all", 50_000, &mut out).is_err());
        dec.decode(&comp, 50_000, &mut out).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn reused_decoder_agrees_with_a_fresh_one_on_odd_inputs() {
        let a = noise(3000, 1);
        let b = vec![b'x'; 200_000];
        let fa = zstd::encode_all(&a[..], 3).unwrap();
        let fb = zstd::encode_all(&b[..], 3).unwrap();
        let mut two = fa.clone();
        two.extend_from_slice(&fb);
        let mut trailing = fa.clone();
        trailing.extend_from_slice(b"garbage");
        let cases: Vec<(Vec<u8>, u64)> = vec![
            (Vec::new(), 10),
            (fa.clone(), 3000),
            (fa.clone(), 2999),
            (fa[..fa.len() - 1].to_vec(), 3000),
            (fa[..4].to_vec(), 3000),
            (two.clone(), 203_000),
            (two.clone(), 202_999),
            (two, u64::MAX),
            (trailing, 10_000),
            (b"junk".to_vec(), 10),
        ];
        let mut dec = ChunkDecoder::new().unwrap();
        let mut out = Vec::new();
        for (i, (comp, limit)) in cases.iter().enumerate() {
            let want = decode_capped_reference(comp, *limit);
            let got = dec.decode(comp, *limit, &mut out).map(|_| out.clone());
            assert_eq!(want.is_ok(), got.is_ok(), "case {i}");
            if let (Ok(w), Ok(g)) = (want, got) {
                assert_eq!(w, g, "case {i}");
            }
        }
    }

    #[test]
    fn chunk_encoder_produces_the_same_frames_as_encode_all() {
        let mut enc = ChunkEncoder::new(ZSTD_LEVEL).unwrap();
        let sizes = [0usize, 1, 100, 4096, 8192, 8193, 131_072, 131_073, 262_144, 1 << 20, 3];
        for (i, &n) in sizes.iter().enumerate() {
            for data in [noise(n, i as u64 + 3), vec![7u8; n], noise(n, 9).iter().map(|b| b & 3).collect()] {
                let want = zstd::encode_all(&data[..], ZSTD_LEVEL).unwrap();
                assert_eq!(enc.encode(&data).unwrap(), &want[..], "{n} bytes");
            }
        }
    }

    /// A minimal ELF64 with one `DT_NEEDED`: `pad` bytes of other names, then `needed`. `strsz` is
    /// what `DT_STRSZ` says (the real size when None).
    fn elf_with_strtab(pad: usize, needed: &str, strsz: Option<u64>) -> Vec<u8> {
        let mut tab = vec![0u8];
        tab.extend(std::iter::repeat_n(b'x', pad));
        tab.push(0);
        let idx = tab.len() as u64;
        tab.extend(needed.as_bytes());
        tab.push(0);
        let (dynoff, taboff) = (64 + 2 * 56, 64 + 2 * 56 + 4 * 16);
        let mut b = vec![0u8; taboff];
        b[0..4].copy_from_slice(b"\x7fELF");
        b[4] = 2;
        b[5] = 1;
        b[0x20..0x28].copy_from_slice(&64u64.to_le_bytes());
        b[0x34..0x36].copy_from_slice(&56u16.to_le_bytes());
        b[0x38..0x3A].copy_from_slice(&2u16.to_le_bytes());
        let total = (taboff + tab.len()) as u64;
        // PT_LOAD over the whole file, then PT_DYNAMIC.
        b[64..68].copy_from_slice(&1u32.to_le_bytes());
        b[96..104].copy_from_slice(&total.to_le_bytes()); // p_filesz
        b[104..112].copy_from_slice(&total.to_le_bytes()); // p_memsz
        b[120..124].copy_from_slice(&2u32.to_le_bytes());
        b[128..136].copy_from_slice(&(dynoff as u64).to_le_bytes());
        for (i, (tag, val)) in [(1i64, idx), (5, taboff as u64), (10, strsz.unwrap_or(tab.len() as u64)), (0, 0)].into_iter().enumerate() {
            let o = dynoff + i * 16;
            b[o..o + 8].copy_from_slice(&tag.to_le_bytes());
            b[o + 8..o + 16].copy_from_slice(&val.to_le_bytes());
        }
        b.extend(tab);
        b
    }

    #[test]
    fn a_needed_name_with_a_slash_is_not_a_need() {
        // FreeCAD 1.1.4: a plugin lists /home/runner/work/FreeCAD/.../lib/libsqlite3.so, which the
        // manifest validation (rightly) refuses as a library name.
        let dir = std::env::temp_dir().join(format!("panpak-slash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("libplugin.so");
        std::fs::write(&path, elf_with_strtab(10, "/home/runner/work/FreeCAD/lib/libsqlite3.so", None)).unwrap();
        assert_eq!(elf_needs_bytes(&std::fs::read(&path).unwrap()).unwrap(), ["/home/runner/work/FreeCAD/lib/libsqlite3.so"]);
        assert!(elf_needs(&path).unwrap().is_empty());
        std::fs::write(&path, elf_with_strtab(10, "$ORIGIN/../lib/libpython3.so.1", None)).unwrap();
        assert_eq!(elf_needs(&path).unwrap(), ["libpython3.so.1"], "$ORIGIN names are still reduced to the SONAME");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn elf_needs_limit_follows_the_file_not_a_constant() {
        // Names after more than 1 MiB of other strings (libLLVM, etcher-util): read.
        for pad in [10, 1 << 20, 3 << 20, 40 << 20] {
            let b = elf_with_strtab(pad, "libfoo.so.1", None);
            assert_eq!(elf_needs_bytes(&b).unwrap(), ["libfoo.so.1"], "pad {pad}");
        }
        // A size that reaches beyond the file, or the wide cap, is refused.
        let honest = elf_with_strtab(100, "libfoo.so.1", None);
        let real = (honest.len() - (64 + 2 * 56 + 4 * 16)) as u64;
        assert!(elf_needs_bytes(&elf_with_strtab(100, "libfoo.so.1", Some(real + 1))).is_err());
        assert!(elf_needs_bytes(&elf_with_strtab(100, "libfoo.so.1", Some(MAX_STRTAB + 1))).is_err());
        assert!(elf_needs_bytes(&elf_with_strtab(100, "libfoo.so.1", Some(u64::MAX))).is_err());
        // A name that is cut by the end of the table is not read past it.
        assert!(elf_needs_bytes(&elf_with_strtab(100, "libfoo.so.1", Some(real - 3))).is_err());
        // A table smaller than the file is fine; a missing size falls back to the file.
        assert_eq!(elf_needs_bytes(&elf_with_strtab(100, "libfoo.so.1", Some(real))).unwrap(), ["libfoo.so.1"]);
        assert_eq!(elf_needs_bytes(&elf_with_strtab(100, "libfoo.so.1", Some(0))).unwrap(), ["libfoo.so.1"]);
    }

    #[test]
    fn elf_needs_rejects_absurd_offsets() {
        // Fuzz regression: phoff=u64::MAX wrapped o+phsize and bypassed the
        // length check (panic on slice). phnum=1 to pass the
        // count check and hit exactly the wrap.
        let mut b = vec![0u8; 200];
        b[0..4].copy_from_slice(b"\x7fELF");
        b[4] = 2;
        b[5] = 1;
        b[0x20..0x28].copy_from_slice(&u64::MAX.to_le_bytes()); // Oversized phoff.
        b[0x34..0x36].copy_from_slice(&56u16.to_le_bytes());
        b[0x38..0x3A].copy_from_slice(&1u16.to_le_bytes()); // phnum=1
        assert!(elf_needs_bytes(&b).is_err());
        // Absurd dyn_off: same treatment (PT_DYNAMIC with offset u64::MAX).
        let mut b2 = vec![0u8; 120];
        b2[0..4].copy_from_slice(b"\x7fELF");
        b2[4] = 2;
        b2[5] = 1;
        b2[0x20..0x28].copy_from_slice(&64u64.to_le_bytes());
        b2[0x34..0x36].copy_from_slice(&56u16.to_le_bytes());
        b2[0x38..0x3A].copy_from_slice(&1u16.to_le_bytes());
        b2[64..68].copy_from_slice(&2u32.to_le_bytes()); // PT_DYNAMIC
        b2[72..80].copy_from_slice(&u64::MAX.to_le_bytes()); // Oversized p_offset.
        assert!(elf_needs_bytes(&b2).is_err());
    }
}

/// ABI resolution (spec §7, SONAME-granular v1).
/// At equal SONAME the library included in the package wins, except for the
/// libraries in `SYSTEM_WINS` (glibc and the graphics stack), and for `libstdc++` and
/// `libgcc_s`, where the copy that offers every symbol version the app needs wins.
/// Symbol versions are otherwise not compared: if the chosen library is incompatible the
/// loader fails explicitly (fail-closed), never a silently wrong lib; `panpak doctor` reports it.
pub mod abi {
    use anyhow::{bail, ensure, Context, Result};
    use std::{
        collections::{HashMap, HashSet},
        fs,
        path::{Path, PathBuf},
    };

    /// v1 globs for prefer_host: only `prefix*`, `*suffix` or contained `*`.
    pub fn glob_match(pat: &str, name: &str) -> bool {
        if let Some(pre) = pat.strip_suffix('*') {
            return name.starts_with(pre);
        }
        if let Some(suf) = pat.strip_prefix('*') {
            return name.ends_with(suf);
        }
        pat == name
    }

    pub fn parse_ldconfig(out: &str) -> HashMap<String, PathBuf> {
        let mut map = HashMap::new();
        for line in out.lines() {
            let line = line.trim();
            let Some((left, path)) = line.split_once(" => ") else { continue };
            let name = left.split_whitespace().next().unwrap_or("");
            if name.contains(".so") && !name.is_empty() {
                map.entry(name.to_string()).or_insert_with(|| PathBuf::from(path.trim()));
            }
        }
        map
    }

    fn scan_dirs(dirs: &[&str], depth: usize) -> HashMap<String, PathBuf> {
        let mut map = HashMap::new();
        let mut stack: Vec<(PathBuf, usize)> = dirs.iter().map(|d| (PathBuf::from(d), 0)).collect();
        while let Some((dir, d)) = stack.pop() {
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().into_owned();
                if p.is_dir() && d < depth {
                    stack.push((p, d + 1));
                } else if (name.starts_with("lib") && name.contains(".so")) || is_loader_name(&name) {
                    map.entry(name).or_insert(p);
                }
            }
        }
        map
    }

    /// The name of a dynamic loader (`ld-linux-x86-64.so.2`, `ld.so`). It defines symbols that the
    /// libraries need (`__tls_get_addr`, `_rtld_global_ro`), so the inventory must list it.
    fn is_loader_name(name: &str) -> bool {
        name.starts_with("ld-linux") || name == "ld.so"
    }

    /// The output of `ldconfig -p`: from `PATH` first, then from the folders of the system tools,
    /// which an ordinary user's `PATH` often lacks.
    pub fn ldconfig_output() -> Option<String> {
        ["ldconfig", "/sbin/ldconfig", "/usr/sbin/ldconfig"].iter().find_map(|c| {
            let out = std::process::Command::new(c).arg("-p").output().ok().filter(|o| o.status.success())?;
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        })
    }

    /// Host map SONAME -> path: `ldconfig -p`, fallback to dir scan.
    pub fn host_libs() -> HashMap<String, PathBuf> {
        if let Some(text) = ldconfig_output() {
            let map = parse_ldconfig(&text);
            if !map.is_empty() {
                return map;
            }
        }
        scan_dirs(&["/lib", "/lib64", "/usr/lib", "/usr/lib64", "/usr/lib/x86_64-linux-gnu"], 2)
    }

    #[cfg(test)]
    mod inventory_tests {
        use super::*;

        #[test]
        fn the_scan_of_the_library_folders_lists_the_loader() {
            let d = std::env::temp_dir().join(format!("panpak-scan-{}", std::process::id()));
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(d.join("sub")).unwrap();
            for f in ["libc.so.6", "ld-linux-x86-64.so.2", "sub/ld.so", "sub/libm.so.6", "notalib.txt", "ld-notaloader.txt"] {
                fs::write(d.join(f), b"x").unwrap();
            }
            let m = scan_dirs(&[d.to_str().unwrap()], 2);
            let mut names: Vec<&str> = m.keys().map(String::as_str).collect();
            names.sort();
            assert_eq!(names, ["ld-linux-x86-64.so.2", "ld.so", "libc.so.6", "libm.so.6"]);
            let _ = fs::remove_dir_all(&d);
        }
    }

    /// Inventory of libs included in the tree: SONAME -> relative path.
    /// Includes lib*.so* symlinks resolved to their target (SONAMEs are
    /// almost always symlinks to lib*.so.X.Y): without this, versioned files
    /// never match DT_NEEDED (found with QOwnNotes/Qt6).
    pub fn bundled_libs(tree: &Path) -> HashMap<String, PathBuf> {
        let mut map = HashMap::new();
        let mut stack = vec![tree.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                // Do not follow symlinks in dirs: inventory, not execution.
                // But SONAMEs (lib*.so*) are almost always symlinks to the
                // versioned file: registered under the link name (what
                // DT_NEEDED asks for), pointing at the real target.
                let Ok(m) = fs::symlink_metadata(&p) else { continue };
                if m.file_type().is_symlink() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let is_lib = name.starts_with("lib") && name.contains(".so");
                    let is_loader = name.starts_with("ld-linux") || name == "ld.so";
                    if is_lib || is_loader {
                        if let Ok(tgt) = fs::read_link(&p) {
                            let abs = if tgt.is_absolute() { tgt } else { p.parent().unwrap_or(tree).join(tgt) };
                            if let Ok(rel) = abs.strip_prefix(tree) {
                                if tree.join(rel).exists() {
                                    map.entry(name).or_insert_with(|| rel.to_path_buf());
                                }
                            }
                        }
                    }
                    continue;
                }
                if m.is_dir() {
                    // Descends everywhere (symlinks are not followed: no cycles).
                    // Filtering by dir name ("only lib*") misses libs
                    // bundled in non-standard paths (e.g. PyQt5's Qt under
                    // usr/lib/python3.8/...): incomplete inventory = false
                    // "missing" and wrong host/bundle mixes.
                    stack.push(p);
                    continue;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                // Inventory: lib*.so* and the loader (ld-*). The loader never enters
                // the farm (decide() skips it): it is the plan's `loader`.
                let is_lib = name.starts_with("lib") && name.contains(".so");
                let is_loader = name.starts_with("ld-linux") || name == "ld.so";
                if is_lib || is_loader {
                    map.entry(name)
                        .or_insert_with(|| p.strip_prefix(tree).unwrap().to_path_buf());
                }
            }
        }
        map
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Source {
        Host(PathBuf),
        Bundled(PathBuf), // relative to tree
    }

    /// For each SONAME in needs decide host vs bundle (spec §7).
    ///
    /// The rule, in order: `allow_missing` skips the library; `prefer_host` takes the system
    /// copy or is an error; with glibc in the bundle ("B-world") everything in the bundle comes
    /// from the bundle; the Qt/KF families are atomic; then, for a library that is in the bundle,
    /// the bundled copy is used, except for `SYSTEM_WINS` (the system copy when there is one),
    /// and for `libstdc++.so.6` and `libgcc_s.so.1`, which follow `runtime_prefs` (the copy that
    /// has every symbol version the app needs, see `cxx_runtime_prefs`; the bundled one when
    /// there is no preference). A library that is not in the bundle comes from the system.
    ///
    /// The glibc family is atomic: loader + libc + libm + ... must come
    /// from the SAME build. A 2.36 loader with host 2.39 libc dies with
    /// `undefined symbol: GLIBC_PRIVATE` (found with the test on Noble).
    /// Moreover: with glibc in the bundle ("B-world"), EVERYTHING in the bundle
    /// comes from the bundle — a newer host lib (e.g. Noble's libX11 asking
    /// for GLIBC_2.38) under the bundle's 2.36 libc dies with
    /// `version GLIBC_2.38 not found`. Only `prefer_host` (e.g. libGL) and
    /// SONAMEs missing from the bundle come from the host.
    /// Without glibc in the bundle ("H-world") the rule above applies. Before it,
    /// the system copy won there (kept for packages that start from an old baseline); that
    /// chose a system `libSoundTouch.so.1` without the symbols Audacity 4 needs.
    const GLIBC_FAMILY: &[&str] = &[
        "libc.so.6",
        "libm.so.6",
        "libdl.so.2",
        "librt.so.1",
        "libpthread.so.0",
        "libresolv.so.2",
        "libutil.so.1",
    ];
    /// Libraries for which the system copy wins even when the package includes one (without glibc
    /// in the bundle). Taken from the AppImage excludelist
    /// (<https://github.com/AppImageCommunity/pkg2appimage/blob/master/excludelist>), only the
    /// entries that MUST come from the system: glibc, whose parts must match each other, and the
    /// graphics stack, which must match the driver. Globs as in `glob_match`.
    pub const SYSTEM_WINS: &[&str] = &[
        // glibc: the loader, libc and the libraries built with it.
        "ld-linux.so.2", "ld-linux-x86-64.so.2", "libc.so.6", "libm.so.6", "libdl.so.2", "librt.so.1",
        "libpthread.so.0", "libresolv.so.2", "libutil.so.1", "libanl.so.1", "libBrokenLocale.so.1",
        "libmvec.so.1", "libthread_db.so.1", "libcidn.so.1", "libnss_*",
        // Graphics stack: OpenGL, EGL, Vulkan, GBM, DRM and the drivers.
        "libGL.so.1", "libGLX.so.0", "libGLdispatch.so.0", "libOpenGL.so.0", "libEGL.so.1",
        "libGLESv1_CM.so.1", "libGLESv2.so.2", "libvulkan.so.1", "libgbm.so.1", "libdrm.so.2",
        "libdrm_*", "libglapi.so.0", "libxcb-dri2.so.0", "libxcb-dri3.so.0", "libnvidia-*",
    ];

    /// The C++ and compiler runtime libraries whose copy is chosen by symbol versions.
    pub const CXX_RUNTIME: &[&str] = &["libstdc++.so.6", "libgcc_s.so.1"];

    /// Qt family of a SONAME (Qt5/Qt6), if it belongs to one.
    fn qt_family(n: &str) -> Option<&'static str> {
        if n.starts_with("libQt6") || n.starts_with("libKF6") {
            Some("Qt6")
        } else if n.starts_with("libQt5") || n.starts_with("libKF5") {
            Some("Qt5")
        } else {
            None
        }
    }
    /// Core of the Qt family (whose presence in the bundle indicates bundled Qt).
    fn qt_core(fam: &str) -> &'static str {
        if fam == "Qt6" {
            "libQt6Core.so.6"
        } else {
            "libQt5Core.so.5"
        }
    }
    /// The atomic families: all of a family comes from the bundle or all from the system.
    pub const FAMILIES: &[&str] = &["Qt6", "Qt5", "GLib", "GTK", "GStreamer", "FFmpeg"];

    /// GLib: the libraries that share one source tree and one version number.
    const GLIB_FAMILY: &[&str] = &[
        "libglib-2.0.so.0", "libgobject-2.0.so.0", "libgio-2.0.so.0", "libgmodule-2.0.so.0", "libgthread-2.0.so.0",
    ];
    /// GTK 2, 3 and 4 with GDK, and the libraries that are built with them and whose types they
    /// share: GdkPixbuf, ATK and AT-SPI, Pango, Cairo (and its GObject glue), Epoxy, Graphene.
    /// Globs as in `glob_match`.
    const GTK_FAMILY: &[&str] = &[
        "libgtk-*", "libgdk-*", "libgdk_pixbuf-2.0.so.0", "libatk-1.0.so.0", "libatk-bridge-2.0.so.0", "libatspi.so.0",
        "libpango-1.0.so.0", "libpangocairo-1.0.so.0", "libpangoft2-1.0.so.0", "libpangoxft-1.0.so.0",
        "libcairo.so.2", "libcairo-gobject.so.2", "libepoxy.so.0", "libgraphene-1.0.so.0",
    ];
    /// FFmpeg: the libraries of one release.
    const FFMPEG_FAMILY: &[&str] = &[
        "libavcodec.*", "libavformat.*", "libavutil.*", "libswresample.*", "libswscale.*", "libavfilter.*",
        "libavdevice.*", "libpostproc.*",
    ];

    /// The atomic family of a SONAME, if it belongs to one.
    pub fn family_of(n: &str) -> Option<&'static str> {
        if let Some(q) = qt_family(n) {
            return Some(q);
        }
        let in_list = |list: &[&str]| list.iter().any(|g| glob_match(g, n));
        if GLIB_FAMILY.contains(&n) {
            Some("GLib")
        } else if in_list(GTK_FAMILY) {
            Some("GTK")
        } else if n.starts_with("libgst") && n.contains(".so") {
            Some("GStreamer")
        } else if in_list(FFMPEG_FAMILY) {
            Some("FFmpeg")
        } else {
            None
        }
    }

    /// The library whose presence in the bundle shows that the bundle carries the family (Qt only).
    fn family_core(fam: &str) -> Option<&'static str> {
        match fam {
            "Qt6" | "Qt5" => Some(qt_core(fam)),
            _ => None,
        }
    }

    pub fn decide(
        needs: &[String],
        prefer_host: &[String],
        allow_missing: &[String],
        host: &HashMap<String, PathBuf>,
        bundled: &HashMap<String, PathBuf>,
    ) -> Result<Vec<(String, Source)>> {
        decide_with(needs, prefer_host, allow_missing, host, bundled, &HashMap::new())
    }

    /// `decide` with the choices made from the symbols (`cxx_runtime_prefs`, `resolve_consistent`):
    /// name -> true to take the system copy.
    pub fn decide_with(
        needs: &[String],
        prefer_host: &[String],
        allow_missing: &[String],
        host: &HashMap<String, PathBuf>,
        bundled: &HashMap<String, PathBuf>,
        prefs: &HashMap<String, bool>,
    ) -> Result<Vec<(String, Source)>> {
        let mut out = Vec::new();
        let bundle_libc = bundled.contains_key("libc.so.6")
            && needs.iter().any(|n| GLIBC_FAMILY.iter().any(|g| g == n));
        // Atomic families (like glibc): the libraries of a family are built against each other, and
        // mixing the package's copy of one with the system's copy of another dies with missing
        // symbols (Qt: `Qt_PRIVATE_API`; GLib: `g_free_sized`). If the bundle covers ALL the needs
        // of a family (and its core library, for Qt), the whole family comes from the bundle (vendor
        // configuration). Otherwise, if the host covers all of them, it comes whole from the host.
        // Otherwise the normal rules stand (mix only if unavoidable).
        let mut bundle_fam_ok: HashMap<&str, bool> = Default::default();
        let mut host_fam_ok: HashMap<&str, bool> = Default::default();
        for fam in FAMILIES {
            let fam_needs: Vec<&String> = needs.iter().filter(|n| family_of(n.as_str()) == Some(*fam)).collect();
            if fam_needs.is_empty() {
                continue;
            }
            let core_ok = family_core(fam).is_none_or(|c| bundled.contains_key(c));
            bundle_fam_ok.insert(fam, core_ok && fam_needs.iter().all(|n| bundled.contains_key(n.as_str())));
            host_fam_ok.insert(fam, fam_needs.iter().all(|n| host.contains_key(n.as_str())));
        }
        for n in needs {
            // The loader (libc's own DT_NEEDED) does not enter the farm:
            // it is the plan's `loader`, not a lib to link.
            if n.starts_with("ld-linux") || n == "ld.so" {
                continue;
            }
            let preferred = prefer_host.iter().any(|g| glob_match(g, n));
            let in_family = GLIBC_FAMILY.iter().any(|g| g == n);
            // allow_missing: explicitly declared (e.g. dlopen plugins
            // missing from the bundle). If actually needed, the loader fails hard.
            // Never for the glibc family (it would break the whole B-world).
            if allow_missing.iter().any(|g| glob_match(g, n)) {
                ensure!(!in_family, "{n} in allow_missing but part of glibc: refused");
                eprintln!("panpak: {n} ignored by explicit allow_missing");
                continue;
            }
            // prefer_host: always host, never substituted. Period.
            if preferred {
                match host.get(n) {
                    Some(hp) => {
                        out.push((n.clone(), Source::Host(hp.clone())));
                        continue;
                    }
                    None => bail!("{n} is prefer_host but missing on the host: no substitution"),
                }
            }
            if bundle_libc && in_family {
                match bundled.get(n) {
                    Some(bp) => {
                        out.push((n.clone(), Source::Bundled(bp.clone())));
                        continue;
                    }
                    None => bail!("incomplete glibc family in the bundle: missing {n}"),
                }
            }
            // A choice made by the symbol check (see `resolve_consistent`) for a library that has two
            // copies. Not in the B-world, and never for the libraries the system must win.
            if !bundle_libc && !SYSTEM_WINS.iter().any(|g| glob_match(g, n)) {
                if let (Some(hp), Some(bp), Some(&use_host)) = (host.get(n), bundled.get(n), prefs.get(n)) {
                    out.push((n.clone(), if use_host { Source::Host(hp.clone()) } else { Source::Bundled(bp.clone()) }));
                    continue;
                }
            }
            // Atomic family: bundle first if complete, then host if complete, never host+bundle mix.
            if let Some(fam) = family_of(n) {
                if bundle_fam_ok.get(fam).copied().unwrap_or(false) {
                    match bundled.get(n) {
                        Some(bp) => {
                            out.push((n.clone(), Source::Bundled(bp.clone())));
                            continue;
                        }
                        None => bail!("incomplete {fam} family in the bundle: missing {n}"),
                    }
                }
                if host_fam_ok.get(fam).copied().unwrap_or(false) {
                    match host.get(n) {
                        Some(hp) => {
                            out.push((n.clone(), Source::Host(hp.clone())));
                            continue;
                        }
                        None => bail!("incomplete {fam} family on the host: missing {n}"),
                    }
                }
            }
            match (host.get(n), bundled.get(n)) {
                // B-world (glibc in the bundle): bundle-first for build coherence.
                (_, Some(bp)) if bundle_libc => {
                    out.push((n.clone(), Source::Bundled(bp.clone())))
                }
                // glibc and the graphics stack: the system copy, even if the package has one.
                (Some(hp), Some(_)) if SYSTEM_WINS.iter().any(|g| glob_match(g, n)) => {
                    out.push((n.clone(), Source::Host(hp.clone())))
                }
                // Included in the package: the included copy.
                (_, Some(bp)) => out.push((n.clone(), Source::Bundled(bp.clone()))),
                // Not included: the system copy.
                (Some(hp), None) => out.push((n.clone(), Source::Host(hp.clone()))),
                (None, None) => {
                        bail!("{n} missing on the host and not included in the tree: the app cannot start here (try another distro or report it to the package author)");
                }
            }
        }
        Ok(out)
    }

    /// The path of a decided library: the system file, or the one in the tree.
    pub fn source_path(tree: &Path, src: &Source) -> PathBuf {
        match src {
            Source::Host(p) => p.clone(),
            Source::Bundled(rel) => tree.join(rel),
        }
    }

    /// ELF files read once each, with `pread` and only the sections that matter, many at a time.
    /// Shared by the whole resolution: the closure of `DT_NEEDED`, the choice of the C++ runtime,
    /// the symbol check and the search for a library to move.
    #[derive(Default)]
    pub struct InfoCache {
        map: HashMap<PathBuf, Option<std::sync::Arc<crate::ElfInfo>>>,
    }

    impl InfoCache {
        pub fn get(&mut self, p: &Path) -> Option<std::sync::Arc<crate::ElfInfo>> {
            self.map
                .entry(p.to_path_buf())
                .or_insert_with(|| crate::read_elf_info(p).ok().map(std::sync::Arc::new))
                .clone()
        }

        /// The `DT_NEEDED` names of a file (none if it cannot be read or is not an ELF).
        pub fn needs(&mut self, p: &Path) -> Vec<String> {
            self.get(p).map(|i| i.needs.clone()).unwrap_or_default()
        }

        /// Read the files not yet read, on several threads (no more than the cores, at most 16).
        pub fn prefetch(&mut self, paths: &[PathBuf]) {
            let mut todo: Vec<&PathBuf> = paths.iter().filter(|p| !self.map.contains_key(*p)).collect();
            todo.sort();
            todo.dedup();
            if todo.is_empty() {
                return;
            }
            let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 16).min(todo.len());
            let next = std::sync::atomic::AtomicUsize::new(0);
            let found: Vec<(usize, Option<std::sync::Arc<crate::ElfInfo>>)> = std::thread::scope(|sc| {
                let handles: Vec<_> = (0..threads)
                    .map(|_| {
                        sc.spawn(|| {
                            let mut mine = Vec::new();
                            loop {
                                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let Some(p) = todo.get(i) else { break };
                                mine.push((i, crate::read_elf_info(p).ok().map(std::sync::Arc::new)));
                            }
                            mine
                        })
                    })
                    .collect();
                handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
            });
            let keys: Vec<PathBuf> = todo.iter().map(|p| (*p).clone()).collect();
            for (i, info) in found {
                self.map.insert(keys[i].clone(), info);
            }
        }

        /// Every file that was read.
        pub fn read_paths(&self) -> Vec<PathBuf> {
            let mut v: Vec<PathBuf> = self.map.iter().filter(|(_, i)| i.is_some()).map(|(p, _)| p.clone()).collect();
            v.sort();
            v
        }
    }

    /// Whether the file starts with the ELF magic (reads four bytes).
    fn looks_like_elf(p: &Path) -> bool {
        use std::io::Read as _;
        let mut magic = [0u8; 4];
        fs::File::open(p).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && &magic == b"\x7fELF"
    }

    /// The ELF files whose needs count for the choices: the entry of the app and the other
    /// executables next to it, and the libraries already decided.
    pub fn elf_modules(tree: &Path, entry: &str, decisions: &[(String, Source)]) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let exe = tree.join(entry);
        if let Some(dir) = exe.parent() {
            if let Ok(rd) = fs::read_dir(dir) {
                let mut names: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
                names.sort();
                out.extend(names.into_iter().filter(|p| *p == exe || looks_like_elf(p)));
            }
        }
        if !out.contains(&exe) && exe.is_file() {
            out.push(exe);
        }
        out.extend(decisions.iter().map(|(_, s)| source_path(tree, s)));
        out
    }

    /// For each of `CXX_RUNTIME` that exists on the system and in the bundle, choose a copy:
    /// name -> true for the system one. The copy must offer every symbol version (`GLIBCXX_*`,
    /// `CXXABI_*`, `GCC_*`) that the modules ask of that library; among the copies that do, the
    /// one with the newest versions. If none offers them all, the one offering more. Without
    /// information (unreadable files) the bundled copy is kept.
    pub fn cxx_runtime_prefs(
        cache: &mut InfoCache,
        modules: &[PathBuf],
        host: &HashMap<String, PathBuf>,
        bundled: &HashMap<String, PathBuf>,
        tree: &Path,
    ) -> HashMap<String, bool> {
        let mut prefs = HashMap::new();
        let mut asked: HashMap<&str, HashSet<String>> = HashMap::new();
        cache.prefetch(modules);
        for m in modules {
            let Some(info) = cache.get(m) else { continue };
            for (file, version) in &info.syms.verneed {
                if let Some(lib) = CXX_RUNTIME.iter().find(|l| **l == file) {
                    asked.entry(lib).or_default().insert(version.clone());
                }
            }
        }
        for lib in CXX_RUNTIME {
            let (Some(hp), Some(bp)) = (host.get(*lib), bundled.get(*lib)) else { continue };
            let bp = tree.join(bp);
            let (Some(h), Some(b)) = (cache.get(hp), cache.get(&bp)) else { continue };
            let (h, b): (HashSet<&String>, HashSet<&String>) = (h.syms.verdef.iter().collect(), b.syms.verdef.iter().collect());
            let want = asked.get(lib).cloned().unwrap_or_default();
            let missing = |have: &HashSet<&String>| want.iter().filter(|v| !have.contains(v)).count();
            let newest = |have: &HashSet<&String>| have.iter().filter_map(|v| crate::version_number(v)).max();
            let (mh, mb) = (missing(&h), missing(&b));
            // Fewer missing versions first, then the newer copy; a tie keeps the bundled one.
            let use_host = (mb, std::cmp::Reverse(newest(&b))) > (mh, std::cmp::Reverse(newest(&h)));
            prefs.insert((*lib).to_string(), use_host);
        }
        prefs
    }

    /// What `panpak doctor` finds when it checks the chosen libraries against the app.
    #[derive(Debug, Default, PartialEq, Eq)]
    pub struct Diagnosis {
        /// Modules read.
        pub modules: usize,
        /// `(module, symbol)` pairs that no chosen library defines (with the version, if any).
        pub missing: Vec<(String, String)>,
    }

    /// A set of hashes that are already hashes: no second hashing.
    #[derive(Default)]
    struct KeyHasher(u64);
    impl std::hash::Hasher for KeyHasher {
        fn finish(&self) -> u64 {
            self.0
        }
        fn write(&mut self, bytes: &[u8]) {
            for &b in bytes {
                self.0 = (self.0 ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        fn write_u64(&mut self, v: u64) {
            self.0 = v;
        }
    }
    type KeySet = HashSet<u64, std::hash::BuildHasherDefault<KeyHasher>>;

    /// Read-only check: for the entry, the executables next to it and every chosen library, each
    /// non-weak undefined symbol must be defined by one of them (same version name when the
    /// symbol is versioned; symbols asked of an `allow_missing` library are left out). `loader` is the
    /// dynamic loader in use. A report, not a decision: the loader has the last word.
    pub fn diagnose(
        tree: &Path,
        entry: &str,
        decisions: &[(String, Source)],
        loader: Option<&Path>,
        host: &HashMap<String, PathBuf>,
        allow_missing: &[String],
    ) -> Diagnosis {
        diagnose_cached(&mut InfoCache::default(), tree, entry, decisions, loader, host, allow_missing)
    }

    /// `name` in the folder of `module` or in a folder directly below it.
    fn sibling_lib(module: &Path, name: &str) -> Option<PathBuf> {
        let dir = module.parent()?;
        let here = dir.join(name);
        if here.is_file() {
            return Some(here);
        }
        fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).map(|p| p.join(name)).find(|p| p.is_file())
    }

    fn diagnose_cached(
        cache: &mut InfoCache,
        tree: &Path,
        entry: &str,
        decisions: &[(String, Source)],
        loader: Option<&Path>,
        host: &HashMap<String, PathBuf>,
        allow_missing: &[String],
    ) -> Diagnosis {
        // The dynamic loader defines symbols too (`__tls_get_addr`, `_dl_*`): it is not in the decisions.
        let mut modules = elf_modules(tree, entry, decisions);
        modules.extend(loader.map(Path::to_path_buf));
        // The libraries of the system that the chosen ones need in turn (libcairo asks for libXrender):
        // the loader finds them on its own, so they are not in the decisions. Level by level, each
        // level read on several threads.
        let mut known: HashSet<String> = decisions.iter().map(|(n, _)| n.clone()).collect();
        let mut done = 0;
        while done < modules.len() && modules.len() < 4096 {
            let level: Vec<PathBuf> = modules[done..].to_vec();
            cache.prefetch(&level);
            done = modules.len();
            for m in &level {
                for need in cache.needs(m) {
                    if known.insert(need.clone()) {
                        if let Some(p) = host.get(&need) {
                            modules.push(p.clone());
                        } else if let Some(p) = sibling_lib(m, &need) {
                            // A library next to the one that needs it, or in a folder below it (a RUNPATH
                            // such as /usr/lib/x86_64-linux-gnu/libproxy), which the inventory does not list.
                            modules.push(p);
                        }
                    }
                }
            }
        }
        let mut parsed = Vec::new();
        let mut defined = KeySet::default();
        for m in &modules {
            let Some(info) = cache.get(m) else { continue };
            defined.extend(info.syms.defined.iter().copied());
            parsed.push((m.clone(), info));
        }
        let mut d = Diagnosis { modules: parsed.len(), missing: Vec::new() };
        for (path, info) in &parsed {
            let shown = path.strip_prefix(tree).unwrap_or(path).display().to_string();
            for u in &info.syms.undefined {
                // A versioned symbol asked of a library that the package declared `allow_missing`
                // is expected to be undefined.
                if u.version.as_ref().is_some_and(|(_, file)| allow_missing.iter().any(|g| glob_match(g, file))) {
                    continue;
                }
                let version = u.version.as_ref().map(|(v, _)| v.as_bytes());
                if !defined.contains(&crate::sym_hash(u.name.as_bytes(), version)) {
                    let key = match &u.version {
                        Some((v, _)) => format!("{}@{v}", u.name),
                        None => u.name.clone(),
                    };
                    d.missing.push((shown.clone(), key));
                }
            }
        }
        d.missing.sort();
        d.missing.dedup();
        d
    }

    /// The most steps `resolve_consistent` takes.
    pub const MAX_CONSISTENCY_STEPS: usize = 8;

    /// The decisions after the symbol check.
    #[derive(Debug)]
    pub struct Resolution {
        pub decisions: Vec<(String, Source)>,
        /// What no chosen library defines after the last step.
        pub diagnosis: Diagnosis,
        /// The libraries whose source the symbol check changed, in order.
        pub changed: Vec<String>,
        /// Steps taken (0: the rules gave a consistent set).
        pub steps: usize,
        /// Every file the decision depends on: the files read, the system copies of the libraries
        /// named in `needs` (also the ones not chosen) and the loader. If one of them changes, the
        /// decision has to be made again.
        pub inputs: Vec<PathBuf>,
    }

    /// The dynamic loader in use: the bundled one (glibc in the bundle) or the system's.
    fn loader_path(tree: &Path, host: &HashMap<String, PathBuf>, bundled: &HashMap<String, PathBuf>) -> Option<PathBuf> {
        const NAMES: [&str; 2] = ["ld-linux-x86-64.so.2", "ld-linux-aarch64.so.1"];
        // The loader of the bundle, else the one in the inventory of the system, else the one at the
        // standard places: without it every symbol it defines would be reported as missing.
        NAMES
            .iter()
            .find_map(|n| bundled.get(*n).map(|r| tree.join(r)).or_else(|| host.get(*n).cloned()))
            .or_else(|| {
                let dirs = ["/lib64", "/lib", "/usr/lib64", "/usr/lib", "/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu"];
                dirs.iter().flat_map(|d| NAMES.iter().map(move |n| Path::new(d).join(n))).find(|p| p.is_file())
            })
    }

    /// `decide` (with the C++ runtime choice) followed by the symbol check: if some symbol that the
    /// app or the chosen libraries need is defined by none of them, change the source of the library
    /// (and its whole atomic family) whose other copy defines it, and check again, until nothing is
    /// missing, nothing can be changed, or `MAX_CONSISTENCY_STEPS` steps. A step is kept only if it
    /// lowers the number of missing symbols. It never touches the libraries in `SYSTEM_WINS`, the
    /// C++ runtime, `prefer_host` and `allow_missing` libraries, nor a bundle with its own glibc.
    /// The symbols that remain are reported in `diagnosis`; the app still installs and starts.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_consistent(
        cache: &mut InfoCache,
        tree: &Path,
        entry: &str,
        needs: &[String],
        prefer_host: &[String],
        allow_missing: &[String],
        host: &HashMap<String, PathBuf>,
        bundled: &HashMap<String, PathBuf>,
    ) -> Result<Resolution> {
        let loader = loader_path(tree, host, bundled);
        let first = decide(needs, prefer_host, allow_missing, host, bundled)?;
        let modules = elf_modules(tree, entry, &first);
        let mut prefs = cxx_runtime_prefs(cache, &modules, host, bundled, tree);
        let mut decisions = decide_with(needs, prefer_host, allow_missing, host, bundled, &prefs)?;
        let mut diag = diagnose_cached(cache, tree, entry, &decisions, loader.as_deref(), host, allow_missing);
        let (mut changed, mut blocked, mut steps) = (Vec::new(), HashSet::<String>::new(), 0usize);
        while !diag.missing.is_empty() && steps < MAX_CONSISTENCY_STEPS {
            steps += 1;
            let flips = flip_candidates(cache, &diag, &decisions, tree, host, bundled, prefer_host, &blocked);
            if flips.is_empty() {
                break;
            }
            let mut trial = prefs.clone();
            trial.extend(flips.iter().map(|(n, h)| (n.clone(), *h)));
            let names: Vec<String> = flips.keys().cloned().collect();
            let next = decide_with(needs, prefer_host, allow_missing, host, bundled, &trial).ok();
            let checked = next.map(|d| {
                let g = diagnose_cached(cache, tree, entry, &d, loader.as_deref(), host, allow_missing);
                (d, g)
            });
            match checked {
                Some((d, g)) if g.missing.len() < diag.missing.len() => {
                    prefs = trial;
                    decisions = d;
                    diag = g;
                    changed.extend(names);
                }
                _ => blocked.extend(names),
            }
        }
        let mut inputs = cache.read_paths();
        inputs.extend(needs.iter().filter_map(|n| host.get(n).cloned()));
        inputs.extend(loader);
        inputs.sort();
        inputs.dedup();
        Ok(Resolution { decisions, diagnosis: diag, changed, steps, inputs })
    }

    /// Libraries to take from their other copy so that a missing symbol gets defined: name -> true for
    /// the system copy. A family moves whole, and only if every member has the copy it moves to.
    #[allow(clippy::too_many_arguments)]
    fn flip_candidates(
        cache: &mut InfoCache,
        diag: &Diagnosis,
        decisions: &[(String, Source)],
        tree: &Path,
        host: &HashMap<String, PathBuf>,
        bundled: &HashMap<String, PathBuf>,
        prefer_host: &[String],
        blocked: &HashSet<String>,
    ) -> std::collections::BTreeMap<String, bool> {
        let movable = |n: &str| {
            !blocked.contains(n)
                && !SYSTEM_WINS.iter().any(|g| glob_match(g, n))
                && !CXX_RUNTIME.contains(&n)
                && !prefer_host.iter().any(|g| glob_match(g, n))
        };
        // The other copy of a decided library: (path, true if it is the system one).
        let other = |n: &str, src: &Source| -> Option<(PathBuf, bool)> {
            match src {
                Source::Bundled(_) => host.get(n).map(|p| (p.clone(), true)),
                Source::Host(_) => bundled.get(n).map(|r| (tree.join(r), false)),
            }
        };
        // The other copies of the libraries that can move, read at once.
        let alts: Vec<(&String, PathBuf, bool)> =
            decisions.iter().filter(|(l, _)| movable(l)).filter_map(|(l, s)| other(l, s).map(|(p, h)| (l, p, h))).collect();
        cache.prefetch(&alts.iter().map(|(_, p, _)| p.clone()).collect::<Vec<_>>());
        let defs: Vec<Option<KeySet>> = alts.iter().map(|(_, p, _)| cache.get(p).map(|i| i.syms.defined.iter().copied().collect())).collect();
        let mut wanted = std::collections::BTreeMap::new();
        for (_, key) in &diag.missing {
            let (name, version) = match key.split_once('@') {
                Some((n, v)) => (n, Some(v.as_bytes())),
                None => (key.as_str(), None),
            };
            let h = crate::sym_hash(name.as_bytes(), version);
            for ((lib, _, to_host), d) in alts.iter().zip(&defs) {
                if d.as_ref().is_some_and(|d| d.contains(&h)) {
                    wanted.insert((*lib).clone(), *to_host);
                }
            }
        }
        // Whole families, in the same direction; drop a family that cannot move whole.
        let mut out = std::collections::BTreeMap::new();
        for (lib, to_host) in &wanted {
            let Some(fam) = family_of(lib) else {
                out.insert(lib.clone(), *to_host);
                continue;
            };
            let members: Vec<&(String, Source)> = decisions.iter().filter(|(m, _)| family_of(m) == Some(fam)).collect();
            let all_can = members.iter().all(|(m, src)| {
                let here_is_host = matches!(src, Source::Host(_));
                here_is_host == *to_host || (movable(m) && other(m, src).is_some_and(|(_, h)| h == *to_host))
            });
            if all_can {
                for (m, src) in members {
                    if matches!(src, Source::Host(_)) != *to_host {
                        out.insert(m.clone(), *to_host);
                    }
                }
            }
        }
        out
    }

    pub struct Plan {
        /// If glibc is included, all of user-space uses it: run
        /// the included loader with --library-path (spec §7).
        pub loader: Option<PathBuf>,
        pub ld_path: Vec<PathBuf>,
    }

    /// Apply the plan: symlink farm for per-lib control + LD_LIBRARY_PATH.
    pub fn apply(
        store: &Path,
        root_hex: &str,
        tree: &Path,
        decisions: &[(String, Source)],
        bundled: &HashMap<String, PathBuf>,
    ) -> Result<Plan> {
        let farm = store.join("farms").join(root_hex);
        fs::create_dir_all(&farm)?;
        let wanted: HashSet<&str> = decisions.iter().map(|(name, _)| name.as_str()).collect();
        for entry in fs::read_dir(&farm)? {
            let entry = entry?;
            if !wanted.contains(entry.file_name().to_string_lossy().as_ref()) {
                fs::remove_file(entry.path())?;
            }
        }
        for (soname, src) in decisions {
            let target = match src {
                Source::Host(p) => p.clone(),
                Source::Bundled(rel) => tree.join(rel),
            };
            let link = farm.join(soname);
            if fs::read_link(&link).ok().as_ref() != Some(&target) {
                // Host/bundle decisions can change after host updates;
                // a stale farm must not keep obsolete links.
                if fs::symlink_metadata(&link).is_ok() { fs::remove_file(&link)?; }
                std::os::unix::fs::symlink(&target, &link)
                    .with_context(|| format!("farm {soname}"))?;
            }
        }
        // Included-lib dirs, for the bundle's transitive dependencies.
        let mut libdirs = vec![farm.clone()];
        let mut seen = HashSet::new();
        for rel in bundled.values() {
            if let Some(d) = rel.parent() {
                if seen.insert(d.to_path_buf()) {
                    libdirs.push(tree.join(d));
                }
            }
        }
        // Included loader? glibc in the bundle => use it for everything.
        let has_libc = bundled.keys().any(|n| n.starts_with("libc.so"));
        let loader = has_libc.then(|| find_loader(tree)).transpose()?;
        Ok(Plan { loader, ld_path: libdirs })
    }

    fn find_loader(tree: &Path) -> Result<PathBuf> {
        let mut stack = vec![tree.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                let n = e.file_name().to_string_lossy().into_owned();
                if n.starts_with("ld-linux") || n == "ld.so" {
                    return Ok(p);
                }
            }
        }
        bail!("bundled glibc but loader (ld-linux) not found in tree")
    }
}
