// SPDX-License-Identifier: MIT OR Apache-2.0
// panpak CLI: pack a tree into an executable .panpak file (stub + payload).

// `pack` allocates and frees a large zstd context for every chunk; the musl
// allocator turns each of those into an mmap/munmap pair and fresh page faults.
// mimalloc reuses the memory (CLI only; the stub keeps the default allocator).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::{bail, ensure, Context, Result};
use panpak_core::{bail_code, coded, ensure_code, ErrorCode};
use panpak_core::{
    encode_footer, encode_index, parse_secret, CDC_AVG, CDC_MAX, CDC_MIN, Chunk, Entry, Footer,
    Kind, Manifest, ZSTD_LEVEL,
};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

fn usage() -> &'static str {
    "usage:\n  panpak keygen -o <secret-file>\n  panpak licenses [package]\n  (secret key: --secret-file <f> or PANPAK_SECRET_FILE=<f>; file mode 0600)\n  panpak pack <folder> --stub <stub> [--secret-file <f>] [--rotate-from-file <f>] [-o <out.panpak>]\n  panpak convert <app.AppImage> --id <reverse-dns> --stub <stub> [--secret-file <f>] [--name N] [--version V] [--entry E] [--allow-missing <glob>] [-o <out.panpak>]\n  panpak install-runtime <stub>
  panpak install-runtime <release.tar.gz> --release-key <ed25519:...>
  panpak release --version V [--secret-file <f>] [--target T] [--out-dir D]\n  panpak install [--yes] [--runtime <stub>] <package>\n  panpak update [--allow-downgrade] [--allow-insecure-http] [--timeout N] <app-id>\n  panpak rollback <app-id>
  panpak open <app-id> [file] [--write | --write-rename]\n  panpak doctor [package]\n  panpak dump-index <package> <out.raw>\n  panpak dump-manifest <package> <out.toml>"
}

fn agent_for(url: &str, allow_insecure: bool, timeout_secs: u64) -> Result<ureq::Agent> {
    let uri: http::Uri = url.parse().context("invalid URL")?;
    match uri.scheme_str() {
        // Always HTTPS anyway; redirects stay in https (ureq) and max 3.
        Some("https") => Ok(ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(3)
            .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
            .build()
            .into()),
        // HTTP only for localhost/test WITH an explicit flag, and with no redirect
        // (max 0: a 3xx is never followed, fail on the expected 206).
        Some("http") if allow_insecure && is_localhost(uri.host().unwrap_or_default()) => {
            Ok(ureq::Agent::config_builder()
                .https_only(false)
                .max_redirects(0)
                .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
                .build()
                .into())
        }
        Some("http") => bail_code!(ErrorCode::E4002, "HTTPS only (localhost/test require --allow-insecure-http)"),
        _ => anyhow::bail!("unsupported scheme (HTTPS only, HTTP on localhost with flag)"),
    }
}

fn is_localhost(host: &str) -> bool {
    matches!(host, "localhost" | "localhost." | "127.0.0.1" | "::1" | "[::1]")
}

fn read_body(body: ureq::Body, expected: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    // Read limited to expected+1: a lying/hostile server cannot make us
    // allocate more than one byte past what was agreed.
    let mut buf = Vec::new();
    body.into_reader().take(expected.saturating_add(1)).read_to_end(&mut buf).context("reading body")?;
    ensure!(buf.len() as u64 == expected, "truncated or excess body: {} vs {expected} bytes", buf.len());
    Ok(buf)
}

/// GET with Range, only 206 with coherent Content-Range (else error).
/// Readable network error: what, why (one line), what to do.
/// Never `{e:?}`: ureq internals do not help the user.
fn net_err(op: &str, url: &str, e: ureq::Error) -> anyhow::Error {
    anyhow::anyhow!("{op} on {url} failed ({e}): check the connection and URL, then retry")
}

fn http_range(agent: &ureq::Agent, url: &str, start: u64, end: u64) -> Result<(Vec<u8>, u64)> {
    let resp = agent.get(url).header("Range", &format!("bytes={start}-{end}")).call().map_err(|e| net_err(&format!("GET Range {start}-{end}"), url, e))?;
    let (body, total) = check_range(resp, start, end, url)?;
    Ok((body, total))
}

/// Suffix-range: last `n` bytes (for the footer).
fn http_suffix(agent: &ureq::Agent, url: &str, n: u64) -> Result<(Vec<u8>, u64)> {
    let resp = agent.get(url).header("Range", &format!("bytes=-{n}")).call().map_err(|e| net_err("GET suffix", url, e))?;
    ensure!(resp.status() == 206, "server without Range support (status {}): the update URL must point to a Range-served file", resp.status());
    let cr = resp.headers().get("content-range").context("missing Content-Range")?.to_str().context("Content-Range")?.to_string();
    let (range, total) = cr.strip_prefix("bytes ").and_then(|r| r.split_once('/')).context("unreadable Content-Range")?;
    let (s, e) = range.split_once('-').context("unreadable Content-Range")?;
    let (s, e, total): (u64, u64, u64) =
        (s.parse().context("Content-Range")?, e.parse().context("Content-Range")?, total.parse().context("Content-Range")?);
    ensure!(e.checked_add(1) == Some(total) && e.checked_sub(s).and_then(|v| v.checked_add(1)) == Some(n), "inconsistent Content-Range {cr} (expected last {n} bytes)");
    ensure!(total <= 1 << 33, "file too large: {total} bytes");
    let body = read_body(resp.into_body(), n)?;
    Ok((body, total))
}


fn check_range(resp: http::Response<ureq::Body>, start: u64, end: u64, url: &str) -> Result<(Vec<u8>, u64)> {
    ensure!(resp.status() == 206, "server without Range support (status {}): the update URL must point to a Range-served file", resp.status());
    let cr = resp.headers().get("content-range").context("missing Content-Range")?.to_str().context("Content-Range")?.to_string();
    let want = end.checked_sub(start).and_then(|n| n.checked_add(1)).unwrap_or(u64::MAX);
    let body = read_body(resp.into_body(), want)?;
    let total = panpak_core::check_range_parts(start, end, &cr, body.len()).with_context(|| format!("for {url}"))?;
    Ok((body, total))
}

/// Half-open intervals, 4 MiB max per response. Coalescing only
/// of adjacent/overlapping intervals: never downloads already-available chunks
/// even if they separate two very close missing chunks.
fn coalesce_ranges(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (mut start, end) in ranges {
        while start < end {
            if let Some(last) = out.last_mut() {
                if start <= last.1 && end.saturating_sub(last.0) <= 4 << 20 {
                    last.1 = last.1.max(end);
                    break;
                }
            }
            let stop = end.min(start.saturating_add(4 << 20));
            out.push((start, stop));
            start = stop;
        }
    }
    out
}

/// Assemble the new .panpak: stub+chunks from remote (Range) and from the old
/// local package (byte-identical), index/manifest/footer from remote.
/// Only missing chunks travel the network. Full verification of the assembled
/// file before publishing it; current stays intact on error.
fn cmd_update(app_id: &str, allow_downgrade: bool, allow_insecure: bool, timeout_secs: u64) -> Result<PathBuf> {
    use std::collections::HashMap;
    ensure!(is_reverse_dns(app_id), "invalid app-id (reverse-DNS required, e.g. org.example.app)");
    let store = store_dir()?;
    let appdir = store.join("apps").join(app_id);
    let pin =
        fs::read_to_string(appdir.join("pinned-key")).context("app not installed (missing pin)")?;
    let pin = pin.trim().to_string();
    let current_link = appdir.join("current");
    let current_pkg = fs::read_link(&current_link).context("missing current: reinstall")?;
    let mut old_f = File::open(&current_pkg).context("unreadable current package")?;
    let old_ft = panpak_core::read_footer(&mut old_f)?;
    let (old_man, old_raw) = panpak_core::verify(&mut old_f, &old_ft)?;
    let (old_chunks0, _) = panpak_core::parse_index(&old_raw)?;
    let old_stub_len = old_chunks0.iter().map(|c| c.off).min().unwrap_or(old_ft.index_off);
    panpak_core::verify_stub_range(&mut old_f, old_stub_len, &old_man)?;
    let url = old_man.update.as_ref().and_then(|u| u.url.clone()).context("manifest without [update].url")?;
    let agent = agent_for(&url, allow_insecure, timeout_secs)?;

    // Footer + manifest + index via Range, with total coherence and caps
    // BEFORE allocating (the footer declares untrusted lengths).
    let (foot, total) = http_suffix(&agent, &url, panpak_core::FOOTER_LEN)?;
    let ft = panpak_core::parse_footer(total, &foot)?;
    ensure!(ft.man_len <= panpak_core::MAX_MANIFEST_TOML, "remote manifest too large");
    ensure!(ft.index_len <= panpak_core::MAX_INDEX_COMP, "remote index too large");
    let (man_b, total2) = http_range(&agent, &url, ft.man_off, ft.man_off + ft.man_len - 1)?;
    ensure!(total2 == total, "file changed during download ({total} vs {total2})");
    let man: panpak_core::Manifest =
        toml::from_str(std::str::from_utf8(&man_b).context("non-UTF-8 manifest")?)?;
    // The key MUST be the pin OR a succession signed by the pin.
    // The pin only updates on verified atomic install (below).
    let new_key = man.update.as_ref().context("manifest without [update].key")?.key.trim().to_string();
    let mut rotated_to: Option<String> = None;
    if new_key != pin {
        let u = man.update.as_ref().unwrap();
        panpak_core::verify_rotation(&pin, &new_key, u.rotation_old.as_deref(), u.rotation_sig.as_deref())
            .context("key differs from the pin and succession invalid")?;
        println!("key rotation accepted: pin updated to {new_key}");
        rotated_to = Some(new_key.clone());
    }
    ensure!(man.app.id == app_id, "remote id differs from the installed app");
    let (sig_b, sig_total) = http_range(&agent, &url, ft.man_off + ft.man_len, ft.man_off + ft.man_len + 63)?;
    ensure!(sig_total == total, "file changed during download");
    let mut signed = man_b.clone();
    signed.extend_from_slice(&ft.root);
    let sig = ed25519_dalek::Signature::from_slice(&sig_b)?;
    panpak_core::parse_pubkey(&new_key)?.verify_strict(&signed, &sig)
        .map_err(|_| coded(ErrorCode::E1001, "invalid remote signature: update refused"))?;
    panpak_core::check_runtime(&man, panpak_core::RUNTIME_ABI)?;
    match cmp_versions(&man.app.version, &old_man.app.version) {
        std::cmp::Ordering::Less => {
            ensure_code!(allow_downgrade, ErrorCode::E4001, "remote version {} older than {}: refused (use --allow-downgrade)", man.app.version, old_man.app.version);
        }
        std::cmp::Ordering::Equal => {
            println!("{} already at version {}", app_id, man.app.version);
            return Ok(current_pkg);
        }
        std::cmp::Ordering::Greater => {}
    }
    let (index_comp, total3) = http_range(&agent, &url, ft.index_off, ft.index_off + ft.index_len - 1)?;
    ensure!(total3 == total, "file changed during download");
    let index_raw = panpak_core::decode_capped(&index_comp, panpak_core::MAX_INDEX_RAW)?;
    ensure_code!(blake3::hash(&index_raw).as_bytes() == &ft.root, ErrorCode::E0004, "remote root hash mismatch: corrupt or tampered download, retry; if it persists do not trust the file");
    let (chunks, entries) = panpak_core::parse_index(&index_raw)?;
    let _ = entries;

    // Map old chunks (hash -> (off, comp, raw)) from the local package.
    let old_index_raw = {
        let comp = panpak_core::read_at(&mut old_f, old_ft.index_off, old_ft.index_len)?;
        panpak_core::decode_capped(&comp, panpak_core::MAX_INDEX_RAW)?
    };
    let (old_chunks, _) = panpak_core::parse_index(&old_index_raw)?;
    let mut old_map: HashMap<[u8; 32], (u64, u32, u32)> = HashMap::new();
    for c in &old_chunks {
        old_map.entry(c.hash).or_insert((c.off, c.comp, c.raw));
    }

    // Sparse assembly into the temp file, CONFINED in packages/
    // (id and version are sanitized; the canonical path must stay inside).
    let safe_ver: String = man.app.version.chars()
        .map(|c| if c.is_ascii_alphanumeric() || ".-_".contains(c) { c } else { '_' })
        .collect();
    ensure!(!safe_ver.is_empty(), "empty version");
    let dest = store.join("packages").join(format!("{app_id}-{safe_ver}.panpak"));
    ensure!(dest.starts_with(store.join("packages")), "path outside staging");
    let tmp = dest.with_extension("panpak.tmp");
    let _ = fs::remove_file(&tmp);
    let mut out = File::create(&tmp)?;
    out.set_len(total)?;
    let mut at = |off: u64, data: &[u8]| -> Result<()> {
        use std::io::{Seek, SeekFrom, Write};
        out.seek(SeekFrom::Start(off))?;
        out.write_all(data)?;
        Ok(())
    };
    let stub_len = chunks.iter().map(|c| c.off).min().unwrap_or(ft.index_off);
    let same_stub = old_stub_len == stub_len
        && old_man.runtime.as_ref().and_then(|r| r.stub_hash.as_ref())
            == man.runtime.as_ref().and_then(|r| r.stub_hash.as_ref());
    let mut missing = Vec::new();
    if same_stub {
        // Old hash already verified and new hash authenticated by the signature.
        for (start, end) in coalesce_ranges(vec![(0, stub_len)]) {
            at(start, &panpak_core::read_at(&mut old_f, start, end - start)?)?;
        }
    } else {
        missing.push((0, stub_len));
    }
    let mut n_fetched = 0u64;
    let mut n_reused = 0u64;
    for c in &chunks {
        let chunk_end = c.off.checked_add(c.comp as u64).context("chunk exceeds u64")?;
        ensure!(chunk_end <= ft.index_off, "chunk outside region: {c:?}");
        match old_map.get(&c.hash) {
            Some(&(o_off, o_comp, o_raw)) if o_comp == c.comp && o_raw == c.raw => {
                let bytes = panpak_core::read_at(&mut old_f, o_off, o_comp as u64)?;
                at(c.off, &bytes)?;
                n_reused += 1;
            }
            _ => {
                missing.push((c.off, chunk_end));
                n_fetched += 1;
            }
        }
    }
    let mut downloaded = 80 + man_b.len() as u64 + sig_b.len() as u64 + index_comp.len() as u64;
    for (start, end) in coalesce_ranges(missing) {
        let (bytes, range_total) = http_range(&agent, &url, start, end - 1)?;
        ensure!(range_total == total, "file changed during download");
        downloaded += bytes.len() as u64;
        at(start, &bytes)?;
    }
    at(ft.index_off, &index_comp)?;
    let mut man_sig = man_b.clone();
    man_sig.extend_from_slice(&sig_b);
    at(ft.man_off, &man_sig)?;
    at(total - panpak_core::FOOTER_LEN, &foot)?;
    drop(out);

    // Full verification BEFORE publishing (current intact on failure).
    // Includes the assembled stub: never publish mismatching stubs.
    let mut vf = File::open(&tmp)?;
    let vft = panpak_core::read_footer(&mut vf)?;
    let (vman, _vraw) = panpak_core::verify(&mut vf, &vft)?;
    let stub_len = chunks.iter().map(|c| c.off).min().unwrap_or(ft.index_off);
    panpak_core::verify_stub_range(&mut vf, stub_len, &vman)?;
    // Reused chunks too: the local store could be damaged.
    let (mut dec, mut raw) = (panpak_core::ChunkDecoder::new()?, Vec::new());
    for c in &chunks {
        let comp = panpak_core::read_at(&mut vf, c.off, c.comp as u64)?;
        dec.decode(&comp, c.raw as u64, &mut raw)?;
        panpak_core::verify_chunk(c, &raw)?;
    }
    fs::rename(&tmp, &dest)?;

    // Atomic switch + previous for rollback + indirection launcher.
    // The rotated pin is written here: verified file but not yet published.
    if let Some(k) = rotated_to {
        fs::write(appdir.join("pinned-key"), format!("{k}\n"))?;
    }
    if current_link.exists() || fs::symlink_metadata(&current_link).is_ok() {
        let prev_target = fs::read_link(&current_link)?;
        symlink_atomic(&prev_target, &appdir.join("previous"))?;
    }
    symlink_atomic(&dest, &current_link)?;
    let (best_abi, runtime) = best_runtime(&store)?;
    let launcher = dirs::home_dir().context("no home dir")?.join(".local/bin").join(app_id);
    fs::write(&launcher, launcher_sh(&runtime, &current_link))?;
    #[cfg(unix)]
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755))?;
    println!("updated {app_id}: {} -> {} (reused chunks {n_reused}, fetched chunks {n_fetched}, runtime {best_abi})",
        old_man.app.version, man.app.version);
    println!("downloaded bytes: {downloaded}");
    Ok(dest)
}

fn cmd_rollback(app_id: &str) -> Result<PathBuf> {
    let appdir = store_dir()?.join("apps").join(app_id);
    let current = appdir.join("current");
    let previous = appdir.join("previous");
    let cur_t = fs::read_link(&current).context("missing current")?;
    let prev_t = fs::read_link(&previous).context("missing previous: no update to roll back")?;
    // Atomic swap: current<->previous.
    let swap_tmp = appdir.join("current.swap");
    let _ = fs::remove_file(&swap_tmp);
    std::os::unix::fs::symlink(&prev_t, &swap_tmp)?;
    fs::rename(&swap_tmp, &current)?;
    symlink_atomic(&cur_t, &previous)?;
    println!("rollback {app_id}: back to {}", prev_t.display());
    Ok(prev_t)
}

/// `panpak open <app> [file] [--write]`: open a file with the app, granting
/// the sandbox ONLY that file (read; write only with --write).
/// Without a file, ask the host picker (outside the sandbox).
fn cmd_open(app_id: &str, file: Option<&str>, write: bool, rename: bool) -> Result<()> {
    use std::os::unix::process::CommandExt;
    ensure!(is_reverse_dns(app_id), "invalid app-id (reverse-DNS required, e.g. org.example.app)");
    let store = store_dir()?;
    let current = store.join("apps").join(app_id).join("current");
    let pkg = fs::read_link(&current).with_context(|| format!("{app_id} not installed: run `panpak install` first"))?;
    let target: PathBuf = match file {
        Some(f) => std::fs::canonicalize(f).context("check the path").with_context(|| format!("unopenable file: {f}"))?,
        None => pick_host_file().context("no file chosen (or no picker available: install zenity or kdialog)")?,
    };
    ensure!(target.is_file(), "open: not a regular file: {}", target.display());
    let (best_abi, runtime) = best_runtime(&store)?;
    let _ = best_abi;
    let mut cmd = std::process::Command::new(&runtime);
    cmd.arg("run").arg(&pkg).arg("--panpak-open").arg(&target);
    if write {
        cmd.arg("--panpak-open-write");
    }
    if rename {
        eprintln!("panpak: WARNING: --write-rename grants create/rename/remove in folder {} (neighbors are not readable nor listable, but are overwritable/deletable)", target.parent().map(|p| p.display().to_string()).unwrap_or_default());
        cmd.arg("--panpak-open-rename");
    }
    cmd.arg(&target);
    let err = cmd.exec();
    anyhow::bail!("exec failed: {err}")
}

/// Host file picker, run OUTSIDE the sandbox (the CLI is not
/// confined). Returns the chosen path or an error if cancelled/missing.
fn pick_host_file() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home dir")?;
    let home_s = home.to_string_lossy().into_owned();
    for (bin, args) in [
        ("zenity", vec!["--file-selection".to_string()]),
        ("kdialog", vec!["--getopenfilename".to_string(), home_s.clone()]),
    ] {
        let out = std::process::Command::new(bin).args(&args).env("HOME", &home).output();
        if let Ok(o) = out {
            if o.status.success() {
                let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if !p.is_empty() {
                    return std::fs::canonicalize(&p).with_context(|| format!("chosen file not openable: {p}"));
                }
                anyhow::bail!("selection cancelled");
            }
        }
    }
    anyhow::bail!("no picker available: install zenity or kdialog, or pass the file")
}

/// Read DT_NEEDED from a little-endian ELF32/64. Non-ELF or static -> [].
/// Minimal bounds-checked parser (no external ELF crates).
fn elf_needs(path: &Path) -> Result<Vec<String>> {
    panpak_core::elf_needs(path)
}

/// Print license texts: the ones built into this binary, or those embedded
/// in a (verified) package.
fn cmd_licenses(pkg: Option<&Path>) -> Result<()> {
    let files: Vec<(String, Vec<u8>)> = match pkg {
        None => LICENSE_FILES.iter().map(|(n, t)| (n.to_string(), t.as_bytes().to_vec())).collect(),
        Some(p) => {
            let mut f = File::open(p).with_context(|| format!("open {}", p.display()))?;
            let ft = panpak_core::read_footer(&mut f)?;
            let (_man, raw_index) = panpak_core::verify(&mut f, &ft)?;
            let files = panpak_core::read_package_licenses(&mut f, &raw_index)?;
            ensure!(!files.is_empty(), "package has no embedded licenses (built by an older panpak?)");
            files
        }
    };
    for (name, data) in files {
        println!("===== {name} =====");
        println!("{}", String::from_utf8_lossy(&data));
    }
    Ok(())
}

fn cmd_keygen(out: &Path) -> Result<()> {
    use ed25519_dalek::SigningKey;
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut panpak = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut panpak)?;
    let sk = SigningKey::from_bytes(&panpak);
    let vk = sk.verifying_key();
    // O_EXCL + 0600 from creation: never overwrites, never world-readable.
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(out)
        .with_context(|| format!("create secret file {} (it must not exist)", out.display()))?;
    writeln!(f, "{}", panpak_core::hex(&panpak))?;
    f.sync_all()?;
    println!("secret key written to: {}", out.display());
    println!("key:    ed25519:{}", panpak_core::hex(vk.as_bytes()));
    println!("fingerprint (TOFU, first 16 hex): {}", panpak_core::fingerprint(&vk));
    Ok(())
}

/// Secret from `--secret-file` or, if absent, `PANPAK_SECRET_FILE` (a path,
/// never the key itself).
fn load_secret(flag: Option<&str>) -> Result<String> {
    let path = match (flag, std::env::var_os("PANPAK_SECRET_FILE")) {
        (Some(f), _) => PathBuf::from(f),
        (None, Some(v)) if !v.is_empty() => PathBuf::from(v),
        _ => bail_code!(ErrorCode::E5002, "--secret-file <f> or PANPAK_SECRET_FILE required\n{usage}", usage = usage()),
    };
    panpak_core::read_secret_file(&path)
}

fn is_reverse_dns(id: &str) -> bool {
    let labels: Vec<&str> = id.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    labels.iter().all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    })
}

struct PayloadEntry {
    entry: Entry,
    source: Option<PathBuf>, // Source file (None for dirs and symlinks).
    embedded: Option<&'static str>, // In-memory content (license texts) instead of a file.
}

/// License texts embedded in every package under `.panpak/licenses/` and
/// shown by `panpak licenses`: the stub is redistributed inside third-party
/// packages, so its notice and dependency licenses travel with it.
const LICENSE_FILES: [(&str, &str); 4] = [
    ("NOTICE", include_str!("../../NOTICE")),
    ("LICENSE-MIT", include_str!("../../LICENSE-MIT")),
    ("LICENSE-APACHE", include_str!("../../LICENSE-APACHE")),
    ("THIRD-PARTY-LICENSES", include_str!("../../THIRD-PARTY-LICENSES")),
];

/// Add `.panpak/licenses/*` to the payload. The `.panpak` top level is
/// reserved: a tree that already uses it is refused.
fn add_license_entries(payload: &mut Vec<PayloadEntry>) -> Result<()> {
    ensure!(
        !payload.iter().any(|p| p.entry.path.starts_with(panpak_core::RESERVED_TOP)),
        "the top-level path {} is reserved for embedded licenses: rename it in the tree",
        panpak_core::RESERVED_TOP
    );
    for dir in [panpak_core::RESERVED_TOP, panpak_core::LICENSES_DIR] {
        payload.push(PayloadEntry {
            entry: Entry { path: PathBuf::from(dir), mode: 0o755, kind: Kind::Dir },
            source: None,
            embedded: None,
        });
    }
    for (name, text) in LICENSE_FILES {
        payload.push(PayloadEntry {
            entry: Entry {
                path: Path::new(panpak_core::LICENSES_DIR).join(name),
                mode: 0o644,
                kind: Kind::File { chunks: Vec::new() },
            },
            source: None,
            embedded: Some(text),
        });
    }
    // Same deterministic order as walk_tree.
    payload.sort_by(|a, b| a.entry.path.cmp(&b.entry.path));
    Ok(())
}

fn walk_tree(root: &Path) -> Result<Vec<PayloadEntry>> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let abs = root.join(&rel);
        let meta = fs::symlink_metadata(&abs)
            .with_context(|| format!("stat {}", abs.display()))?;
        if !rel.as_os_str().is_empty() {
            let rel_s = rel.to_str().context("non-UTF-8 path in tree")?;
            if rel_s == "panpak.toml" {
                continue; // Pack input, not payload: the signed manifest is outside the tree.
            }
            let path = panpak_core::safe_path(rel_s)?;
            let mode = meta.permissions().mode();
            ensure!(mode & 0o6000 == 0, "setuid/setgid not allowed: {}", rel_s);
            if meta.is_dir() {
                out.push(PayloadEntry {
                    entry: Entry { path, mode: mode & 0o777, kind: Kind::Dir },
                    source: None,
                    embedded: None,
                });
            } else if meta.file_type().is_symlink() {
                let t = fs::read_link(&abs)?;
                let t = t.to_str().context("non-UTF-8 symlink target")?.to_string();
                out.push(PayloadEntry {
                    entry: Entry { path, mode: mode & 0o777, kind: Kind::Link(t) },
                    source: None,
                    embedded: None,
                });
                continue;
            } else if meta.is_file() {
                out.push(PayloadEntry {
                    entry: Entry {
                        path,
                        mode: mode & 0o777,
                        kind: Kind::File { chunks: Vec::new() },
                    },
                    source: Some(abs),
                    embedded: None,
                });
                continue;
            } else {
                bail!("unsupported file type: {}", rel_s);
            }
        }
        if meta.is_dir() {
            let mut kids: Vec<PathBuf> = fs::read_dir(&abs)?
                .map(|e| e.map(|x| x.file_name()))
                .collect::<std::io::Result<Vec<_>>>()?
                .into_iter()
                .map(|n| rel.join(n))
                .collect();
            kids.sort();
            for k in kids.into_iter().rev() {
                stack.push(k);
            }
        }
    }
    // Deterministic order for a reproducible index.
    out.sort_by(|a, b| a.entry.path.cmp(&b.entry.path));
    Ok(out)
}

fn cmd_pack(dir: &Path, stub: &Path, secret: &str, rotate_from: Option<&str>, out: Option<&Path>) -> Result<PathBuf> {
    let input_toml = fs::read_to_string(dir.join("panpak.toml")).context("read <folder>/panpak.toml")?;
    // Strict schema in pack: future keys must not silently disappear.
    panpak_core::check_panpak_toml_schema(&input_toml)?;
    let mut man: Manifest = toml::from_str(&input_toml).context("invalid panpak.toml")?;
    ensure!(!man.app.id.is_empty() && is_reverse_dns(&man.app.id), "app.id must be reverse-DNS (e.g. org.example.app)");
    ensure!(!man.app.name.is_empty(), "app.name is required");
    ensure!(!man.app.version.is_empty(), "app.version is required");
    let entry_rel = panpak_core::safe_path(&man.app.entry).context("unsafe app.entry")?;
    // [permissions] coherence: ports without network=true refused (fail-closed
    // in pack, not at runtime); sockets with valid syntax (absolute or $VAR).
    if let Some(perm) = &man.permissions {
        let nets = perm.network.unwrap_or(false);
        ensure!(
            nets || perm.network_allow_tcp.as_ref().map(|v| v.is_empty()).unwrap_or(true),
            "network_allow_tcp without network=true"
        );
        ensure!(
            nets || perm.network_bind_tcp.as_ref().map(|v| v.is_empty()).unwrap_or(true),
            "network_bind_tcp without network=true"
        );
        for s in perm.sockets_allow.as_ref().unwrap_or(&Vec::new()) {
            ensure!(
                s.starts_with('/') || s.starts_with("$XDG_RUNTIME_DIR") || s.starts_with("${XDG_RUNTIME_DIR}")
                    || s.starts_with("$HOME") || s.starts_with("${HOME}"),
                "sockets_allow must be absolute or $VAR: {s}"
            );
        }
    }
    // The key must already be valid here: fail in pack, not at runtime.
    let sk = parse_secret(secret)?;
    let derived = format!("ed25519:{}", panpak_core::hex(sk.verifying_key().as_bytes()));
    ensure!(
        man.update.as_ref().is_some_and(|u| u.key == derived),
        "update.key must match the signing secret"
    );
    // Rotation: prove that the OLD key authorizes the new one (OB3).
    if let Some(old_hex) = rotate_from {
        use ed25519_dalek::Signer;
        let old_sk = parse_secret(old_hex)?;
        let old_pub = format!("ed25519:{}", panpak_core::hex(old_sk.verifying_key().as_bytes()));
        let sig = old_sk.sign(sk.verifying_key().as_bytes()).to_bytes();
        let upd = man.update.as_mut().context("manifest without [update]")?;
        upd.rotation_old = Some(old_pub);
        upd.rotation_sig = Some(panpak_core::hex(&sig));
    }

    let mut payload = walk_tree(dir)?;
    add_license_entries(&mut payload)?;
    ensure!(
        payload.iter().any(|p| p.entry.path == entry_rel
            && matches!(p.entry.kind, Kind::File { .. })),
        "app.entry not found as a file in tree: {}",
        man.app.entry
    );

    // Spec §7: prefer_host + included glibc rejected by pack.
    if man.abi.as_ref().is_some_and(|a| a.prefer_host.as_ref().is_some_and(|v| !v.is_empty())) {
        let has_libc = payload.iter().any(|p| {
            let s = p.entry.path.to_string_lossy();
            (s.starts_with("lib/") || s.starts_with("lib64/") || s.starts_with("usr/lib/"))
                && s.contains("libc.so")
        });
        ensure!(!has_libc, "prefer_host + bundled glibc: combination refused (spec §7)");
    }

    // needs: union of scanned DT_NEEDED + declared ones (spec §7).
    {
        let mut set: std::collections::BTreeSet<String> = man
            .abi
            .as_ref()
            .and_then(|a| a.needs.clone())
            .unwrap_or_default()
            .into_iter()
            .collect();
        for p in payload.iter() {
            if let Some(src) = &p.source {
                set.extend(elf_needs(src).with_context(|| format!("scan {}", src.display()))?);
            }
        }
        if !set.is_empty() {
            let needs: Vec<String> = set.into_iter().collect();
            match &mut man.abi {
                Some(a) => a.needs = Some(needs),
                None => {
                    man.abi = Some(panpak_core::Abi {
                        glibc_min: None,
                        needs: Some(needs),
                        prefer_host: None,
                        allow_missing: None,
                    })
                }
            }
        }
    }

    let out_path = match out {
        Some(p) => p.to_path_buf(),
        None => PathBuf::from(format!("{}.panpak", man.app.id)),
    };

    let mut fout = File::create(&out_path).context("create output")?;
    let stub_bytes = fs::read(stub).context("read stub")?;
    // The stub is part of the signed data ([runtime].stub_hash): anyone
    // replacing the stub invalidates the signature (verified at every use).
    let stub_hash = panpak_core::hex(blake3::hash(&stub_bytes).as_bytes());
    match &mut man.runtime {
        Some(r) => r.stub_hash = Some(stub_hash),
        None => {
            man.runtime = Some(panpak_core::Runtime { min_runtime: None, stub_hash: Some(stub_hash) })
        }
    }
    fout.write_all(&stub_bytes)?;
    let mut file_off = stub_bytes.len() as u64;

    // Per-file chunks in streaming (StreamCDC: internal buffer = max 256K).
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut by_hash: HashMap<[u8; 32], u32> = HashMap::new();
    let mut refs: u64 = 0;
    // One zstd context for every chunk and for the index.
    let mut enc = panpak_core::ChunkEncoder::new(ZSTD_LEVEL)?;
    for p in payload.iter_mut() {
        let reader: Box<dyn std::io::Read> = match (&p.source, p.embedded) {
            (Some(src), _) => Box::new(File::open(src)?),
            (None, Some(text)) => Box::new(text.as_bytes()),
            (None, None) => continue,
        };
        let chunker =
            fastcdc::v2020::StreamCDC::new(reader, CDC_MIN, CDC_AVG, CDC_MAX);
        let mut ids = Vec::new();
        for chunk in chunker {
            let chunk = chunk.map_err(|e| anyhow::anyhow!("cdc: cannot read file to chunk ({e})"))?;
            let raw = chunk.data;
            let hash = *blake3::hash(&raw).as_bytes();
            if let Some(&id) = by_hash.get(&hash) {
                ids.push(id);
                continue;
            }
            let comp = enc.encode(&raw)?;
            let id = chunks.len() as u32;
            fout.write_all(comp)?;
            chunks.push(Chunk {
                hash,
                off: file_off,
                comp: comp.len() as u32,
                raw: raw.len() as u32,
            });
            file_off += comp.len() as u64;
            by_hash.insert(hash, id);
            ids.push(id);
        }
        if let Kind::File { chunks: dst } = &mut p.entry.kind {
            refs += ids.len() as u64;
            *dst = ids;
        }
    }

    let entries: Vec<Entry> = payload.into_iter().map(|p| p.entry).collect();
    let raw_index = encode_index(&chunks, &entries);
    let comp_index = enc.encode(&raw_index)?.to_vec();
    let root = *blake3::hash(&raw_index).as_bytes();

    let index_off = file_off;
    fout.write_all(&comp_index)?;
    let man_off = index_off + comp_index.len() as u64;

    let toml_b = toml::to_string(&man)?.into_bytes();
    let mut msg = Vec::with_capacity(toml_b.len() + 32);
    msg.extend_from_slice(&toml_b);
    msg.extend_from_slice(&root);
    use ed25519_dalek::Signer;
    let sig = sk.sign(&msg).to_bytes();
    fout.write_all(&toml_b)?;
    fout.write_all(&sig)?;

    let ft = Footer {
        index_off,
        index_len: comp_index.len() as u64,
        man_off,
        man_len: toml_b.len() as u64,
        root,
    };
    fout.write_all(&encode_footer(&ft))?;
    fout.flush()?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&out_path, fs::Permissions::from_mode(0o755))?;
    }
    println!(
        "pack: {} references, {} unique chunks, {} entries, root={}",
        refs,
        chunks.len(),
        entries.len(),
        panpak_core::hex(&root)
    );
    Ok(out_path)
}

fn store_dir() -> Result<PathBuf> {
    Ok(dirs::data_dir().context("no data dir")?.join("panpak"))
}

/// Installed runtime ABI by reading `<stub> --abi-version`.
fn stub_abi(stub: &Path) -> Result<u32> {
    let out = std::process::Command::new(stub).arg("--abi-version").output()?;
    ensure!(out.status.success(), "stub does not report --abi-version");
    String::from_utf8(out.stdout)?.trim().parse().context("non-numeric ABI")
}

/// Best installed runtime (max ABI).
fn best_runtime(store: &Path) -> Result<(u32, PathBuf)> {
    let mut best: Option<(u32, PathBuf)> = None;
    if let Ok(rd) = fs::read_dir(store.join("runtimes")) {
        for e in rd.flatten() {
            let stub = e.path().join("panpak-stub");
            if !stub.is_file() {
                continue;
            }
            if let Ok(abi) = stub_abi(&stub) {
                if best.as_ref().is_none_or(|(a, _)| abi > *a) {
                    best = Some((abi, stub));
                }
            }
        }
    }
    best.context("no installed runtime: run `panpak install-runtime <stub>` first")
}

/// Install a runtime. Two forms: release tarball
/// (`panpak-X.Y.Z-<target>.tar.gz` + sidecar .sha256 + .sig), with
/// SHA-256 and Ed25519 signature with the release key (argument or
/// `runtimes/release-key` pin), staging extraction and stub install;
/// or a single binary (local builds only), which requires explicit
/// --allow-unsigned. Paths inside the tarball are validated (no absolutes/..).
fn cmd_install_runtime(src: &Path, release_key: Option<&str>, allow_unsigned: bool) -> Result<()> {
    let store = store_dir()?;
    let magic = {
        use std::io::Read;
        let mut f = File::open(src).with_context(|| format!("open {}", src.display()))?;
        let mut m = [0u8; 2];
        f.read_exact(&mut m).context("file too short")?;
        m
    };
    if magic != [0x1f, 0x8b] {
        ensure!(allow_unsigned, "unsigned runtime: use --allow-unsigned only for local builds (never for downloads)");
        eprintln!("WARNING: installing UNSIGNED runtime (local builds only)");
        return install_runtime_stub(src, &store);
    }
    // Tarball: look for sidecar .sha256 and .sig (same name + suffix).
    let sha_path = {
        let mut s = src.as_os_str().to_owned();
        s.push(".sha256");
        PathBuf::from(s)
    };
    let sig_path = {
        let mut s = src.as_os_str().to_owned();
        s.push(".sig");
        PathBuf::from(s)
    };
    let raw = fs::read(src)?;
    let sha_line = fs::read_to_string(&sha_path)
        .with_context(|| format!("missing {} (sidecar .sha256 required)", sha_path.display()))?;
    let expect = sha_line.split_whitespace().next().context("unreadable .sha256")?;
    let got = panpak_core::hex(blake3::hash(&raw).as_bytes());
    ensure!(got == expect.to_lowercase(), "tarball SHA-256 mismatch: corrupt or tampered download");
    let sig_raw = fs::read(&sig_path)
        .with_context(|| format!("missing {} (sidecar .sig required)", sig_path.display()))?;
    ensure!(sig_raw.len() == 64, "release signature: expected 64 bytes");
    // Release key: argument or pin; never silent TOFU.
    let pin_path = store.join("runtimes").join("release-key");
    let key = match (release_key, pin_path.exists()) {
        (Some(k), _) => k.to_string(),
        (None, true) => fs::read_to_string(&pin_path).context("unreadable release pin")?,
        (None, false) => anyhow::bail!("--release-key <ed25519:...> required (key published on the separate channel, see RELEASE.md)"),
    };
    if pin_path.exists() {
        let pinned = fs::read_to_string(&pin_path)?;
        ensure_code!(pinned.trim() == key.trim(), ErrorCode::E1004, "release key DIFFERS from pin: possible attack (see RELEASE.md for rotation)");
    }
    let vk = panpak_core::parse_pubkey(key.trim()).context("invalid release-key")?;
    let sig = ed25519_dalek::Signature::from_bytes(sig_raw.as_slice().try_into().map_err(|_| anyhow::anyhow!("truncated signature"))?);
    let mut msg = Vec::with_capacity(raw.len());
    msg.extend_from_slice(&raw);
    vk.verify_strict(&msg, &sig).context("invalid release signature")?;
    if !pin_path.exists() {
        fs::create_dir_all(pin_path.parent().unwrap())?;
        fs::write(&pin_path, format!("{}\n", key.trim()))?;
        println!("release key pin registered");
    }
    // Confined extraction into an empty staging dir.
    let stage = store.join("runtimes").join(".stage");
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage)?;
    {
        use flate2::read::GzDecoder;
        let gz = GzDecoder::new(&raw[..]);
        let mut ar = tar::Archive::new(gz);
        for entry in ar.entries().context("unreadable tarball")? {
            let mut entry = entry?;
            let rel = entry.path().context("entry without path")?.into_owned();
            ensure!(!rel.is_absolute() && !rel.components().any(|c| matches!(c, std::path::Component::ParentDir)), "entry outside root: {}", rel.display());
            let dst = store.join("runtimes").join(".stage").join(&rel);
            if let Some(d) = dst.parent() {
                fs::create_dir_all(d)?;
            }
            entry.unpack(&dst).with_context(|| format!("extract {}", rel.display()))?;
        }
    }
    let stub = find_in(&stage, "panpak-stub").context("panpak-stub missing in release")?;
    let abi = stub_abi(&stub)?;
    let dest = store.join("runtimes").join(abi.to_string());
    fs::create_dir_all(&dest)?;
    fs::copy(&stub, dest.join("panpak-stub"))?;
    #[cfg(unix)]
    fs::set_permissions(dest.join("panpak-stub"), fs::Permissions::from_mode(0o755))?;
    let _ = fs::remove_dir_all(&stage);
    println!("runtime {abi} installed from verified release ({})", src.display());
    Ok(())
}

fn find_in(dir: &Path, name: &str) -> Result<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d)?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if e.file_name().to_str() == Some(name) {
                return Ok(p);
            }
        }
    }
    anyhow::bail!("{name} not found in {}", dir.display())
}

fn install_runtime_stub(stub: &Path, store: &Path) -> Result<()> {
    let abi = stub_abi(stub)?;
    let dest = store.join("runtimes").join(abi.to_string());
    fs::create_dir_all(&dest)?;
    fs::copy(stub, dest.join("panpak-stub"))?;
    #[cfg(unix)]
    fs::set_permissions(dest.join("panpak-stub"), fs::Permissions::from_mode(0o755))?;
    println!("runtime {abi} installed from {} (NOT verified)", stub.display());
    Ok(())
}

/// Create a release: tarball panpak-<v>-<target>.tar.gz with panpak+panpak-stub,
/// .sha256 and .sig files (raw Ed25519 signature of the tarball). Binaries must
/// already exist compiled (target/<triple>/release/).
fn cmd_release(version: &str, secret: &str, target: Option<&str>, out_dir: &Path) -> Result<PathBuf> {
    use flate2::{write::GzEncoder, Compression};
    ensure!(version.chars().all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c)) && !version.is_empty(), "invalid version");
    let target = target.map(|s| s.to_string()).unwrap_or_else(|| {
        let m = std::env::consts::ARCH;
        let a = if m == "x86_64" { "x86_64" } else if m == "aarch64" { "aarch64" } else { m };
        format!("{a}-unknown-linux-musl")
    });
    let rel = std::env::current_dir()?.join("target").join(&target).join("release");
    let top = format!("panpak-{version}-{target}");
    let tar_path = out_dir.join(format!("{top}.tar.gz"));
    fs::create_dir_all(out_dir)?;
    // Fixed mtimes for reproducible tarballs (SOURCE_DATE_EPOCH or default).
    let mtime: u64 = std::env::var("SOURCE_DATE_EPOCH").ok().and_then(|s| s.parse().ok()).unwrap_or(1700000000);
    {
        let f = File::create(&tar_path)?;
        let gz = GzEncoder::new(f, Compression::new(9));
        let mut ar = tar::Builder::new(gz);
        for name in ["panpak", "panpak-stub"] {
            let src = rel.join(name);
            let data = fs::read(&src).with_context(|| format!("read {} (build first: cargo build --release --target {target})", src.display()))?;
            let mut hdr = tar::Header::new_gnu();
            hdr.set_size(data.len() as u64);
            hdr.set_mode(0o755);
            hdr.set_mtime(mtime);
            hdr.set_cksum();
            ar.append_data(&mut hdr, format!("{top}/{name}"), &data[..])?;
        }
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(version.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_mtime(mtime);
        hdr.set_cksum();
        ar.append_data(&mut hdr, format!("{top}/VERSION"), version.as_bytes())?;
        ar.into_inner()?.finish()?;
    }
    let tar_bytes = fs::read(&tar_path)?;
    let sha = panpak_core::hex(blake3::hash(&tar_bytes).as_bytes());
    let stem = tar_path.file_name().unwrap().to_string_lossy().into_owned();
    fs::write(out_dir.join(format!("{stem}.sha256")), format!("{sha}  {stem}\n"))?;
    let sk = parse_secret(secret)?;
    use ed25519_dalek::Signer;
    let sig = sk.sign(&tar_bytes);
    fs::write(out_dir.join(format!("{stem}.sig")), sig.to_bytes())?;
    println!("release: {} ({} bytes) + .sha256 + .sig", tar_path.display(), tar_bytes.len());
    println!("release public key: ed25519:{}", panpak_core::hex(sk.verifying_key().as_bytes()));
    Ok(tar_path)
}

/// Atomic symlink (tmp + rename): for current/previous with no broken windows.
fn symlink_atomic(target: &Path, link: &Path) -> Result<()> {
    let tmp = link.with_extension("tmp-link");
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    fs::rename(&tmp, link)?;
    Ok(())
}

/// `panpak convert`: extract squashfs from an AppImage (pure Rust, no FUSE),
/// generate panpak.toml from the .desktop and invoke pack. The id is required (never
/// guessed from a .desktop file); entry auto from the Exec field.
struct ConvertArgs<'a> {
    src: &'a Path,
    id: &'a str,
    stub: &'a Path,
    secret: &'a str,
    name_ov: Option<&'a str>,
    version_ov: Option<&'a str>,
    entry_ov: Option<&'a str>,
    allow_missing: Vec<String>,
    out: Option<&'a Path>,
}

/// Fast check of a squashfs v4 superblock: magic+version+block_size.
fn valid_squashfs_sb(b: &[u8]) -> bool {
    if b.len() < 32 || &b[0..4] != b"hsqs" {
        return false;
    }
    let u16le = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u32le = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    u16le(28) == 4 // s_major
        && matches!(u32le(12), 4096 | 8192 | 16384 | 32768 | 65536 | 131072) // block_size
        && u32le(4) > 0 // inodes
        && u32le(4) < 100_000_000
}

fn cmd_convert(a: ConvertArgs<'_>) -> Result<PathBuf> {
    use std::io::Read;
    let ConvertArgs { src, id, stub, secret, name_ov, version_ov, entry_ov, allow_missing, out } = a;
    ensure!(is_reverse_dns(id), "--id must be reverse-DNS (e.g. org.example.app)");
    let mut f = File::open(src)?;
    // Look for the squashfs v4 magic ("hsqs") in the first 64 MiB, validating the
    // superblock (the ELF runtime contains false positives).
    let mut probe = Vec::new();
    std::io::Read::by_ref(&mut f).take(64 << 20).read_to_end(&mut probe)?;
    let mut off = None;
    let mut i = 0;
    while let Some(k) = probe[i..].windows(4).position(|w| w == b"hsqs") {
        let o = i + k;
        if valid_squashfs_sb(&probe[o..]) {
            off = Some(o as u64);
            break;
        }
        i = o + 1;
    }
    let off = off.context("squashfs v4 superblock not found (v4 only, no FUSE)")?;
    println!("convert: squashfs v4 at offset {off}");
    let reader = std::io::BufReader::new(File::open(src)?);
    let fs = backhand::FilesystemReader::from_reader_with_offset(reader, off)
        .map_err(|e| anyhow::anyhow!("unreadable squashfs: {e}"))?;

    let tree = std::env::temp_dir().join(format!("panpak-convert-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tree);
    fs::create_dir_all(&tree)?;
    let mut desktop: Option<PathBuf> = None;
    for node in fs.files() {
        let rel = node.fullpath.strip_prefix("/").unwrap_or(&node.fullpath);
        if rel.as_os_str().is_empty() {
            continue;
        }
        let dst = tree.join(rel);
        match &node.inner {
            backhand::InnerNode::Dir(_) => {
                fs::create_dir_all(&dst)?;
            }
            backhand::InnerNode::File(h) => {
                if let Some(d) = dst.parent() {
                    fs::create_dir_all(d)?;
                }
                let mut r = fs.file(h).reader();
                let mut o = File::create(&dst)?;
                std::io::copy(&mut r, &mut o)?;
                #[cfg(unix)]
                fs::set_permissions(&dst, fs::Permissions::from_mode(node.header.permissions as u32 & 0o7777))?;
            }
            backhand::InnerNode::Symlink(s) => {
                if let Some(d) = dst.parent() {
                    fs::create_dir_all(d)?;
                }
                std::os::unix::fs::symlink(&s.link, &dst)?;
            }
            _other => eprintln!("convert: skipping special node {}", node.fullpath.display()),
        }
        if desktop.is_none()
            && node.fullpath.extension().map(|e| e == "desktop").unwrap_or(false)
            && node.fullpath.parent().map(|p| p.as_os_str().is_empty() || p == std::path::Path::new("/")).unwrap_or(false)
        {
            desktop = Some(dst);
        }
    }
    // Parse .desktop: Name, Exec, Version.
    let mut dname: Option<String> = None;
    let mut dexec: Option<String> = None;
    let mut dexec_args: Vec<String> = Vec::new();
    let mut dver: Option<String> = None;
    if let Some(d) = &desktop {
        let txt = fs::read_to_string(d).unwrap_or_default();
        let mut in_entry = false;
        for line in txt.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            if let Some(v) = line.strip_prefix("Name=") {
                dname = Some(v.to_string());
            } else if let Some(v) = line.strip_prefix("Exec=") {
                // Exec may have fixed arguments (e.g. `python3.8 -m mu`):
                // the first token is the binary, the rest goes in a wrapper.
                // Freedesktop field codes (%U, %F, ...) are dropped.
                let mut parts = v
                    .split_whitespace()
                    .map(|s| s.trim_matches('"'))
                    .filter(|s| !s.is_empty() && !s.starts_with('%'));
                dexec = Some(parts.next().unwrap_or("").to_string());
                dexec_args = parts.map(|s| s.to_string()).collect();
            } else if let Some(v) = line.strip_prefix("Version=") {
                dver = Some(v.to_string());
            }
        }
    }
    let mut entry = if let Some(e) = entry_ov {
        e.to_string()
    } else if let Some(e) = dexec.filter(|e| !e.is_empty()) {
        // Exec may be absolute (/usr/bin/x), relative or bare (PATH).
        let cand = e.trim_start_matches('/');
        let mut found = None;
        for c in [cand.to_string(), format!("usr/bin/{cand}"), format!("bin/{cand}")] {
            if tree.join(&c).is_file() {
                found = Some(c);
                break;
            }
        }
        found.with_context(|| format!("entry not found for Exec={e} (use --entry)"))?
    } else {
        anyhow::bail!("Exec not found in .desktop (use --entry)");
    };
    // Non-ELF entry (typically AppRun): wrapper emulating the AppImage
    // runtime environment (APPDIR/APPIMAGE/ARGV0/OWD), otherwise the
    // AppRun scripts fail looking for absolute paths.
    // Exec with fixed arguments (e.g. `python3.8 -m mu`): wrapper passing them.
    {
        let ep = tree.join(&entry);
        let mut magic = [0u8; 4];
        let is_elf = std::fs::File::open(&ep).and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut magic).map(|_| magic == *b"\x7fELF")
        }).unwrap_or(false);
        if !dexec_args.is_empty() {
            // Quote arguments for sh (no creative eval).
            let quoted: Vec<String> = dexec_args
                .iter()
                .map(|a| format!("'{}'", a.replace('\'', "'\\''")))
                .collect();
            let wrap = format!("#!/bin/sh\n# Generated by `panpak convert`: fixed Exec arguments.\nHERE=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\nTREE=${{HERE%/bin}}\nexec \"$TREE/{entry}\" {} \"$@\"\n", quoted.join(" "));
            fs::create_dir_all(tree.join("bin"))?;
            fs::write(tree.join("bin/.panpak-exec"), &wrap)?;
            #[cfg(unix)]
            fs::set_permissions(tree.join("bin/.panpak-exec"), fs::Permissions::from_mode(0o755))?;
            entry = "bin/.panpak-exec".to_string();
        } else if !is_elf {
            let wrap = format!("#!/bin/sh\n# Generated by `panpak convert`: emulates the AppImage runtime environment.\nHERE=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\nTREE=${{HERE%/bin}}\nexport APPDIR=\"$TREE\" APPIMAGE=\"$TREE\" ARGV0=\"$0\"\nif [ -z \"$OWD\" ]; then export OWD=\"$PWD\"; fi\nexec \"$TREE/{entry}\" \"$@\"\n");
            fs::create_dir_all(tree.join("bin"))?;
            fs::write(tree.join("bin/.panpak-run"), &wrap)?;
            #[cfg(unix)]
            fs::set_permissions(tree.join("bin/.panpak-run"), fs::Permissions::from_mode(0o755))?;
            entry = "bin/.panpak-run".to_string();
        }
    }
    let x11 = detects_xcb_only(&tree)?;
    let name = name_ov.map(|s| s.to_string()).or(dname).unwrap_or_else(|| id.to_string());
    let version = version_ov.map(|s| s.to_string()).or(dver).unwrap_or_else(|| "1.0".to_string());
    println!("convert: entry={entry} name={name} version={version}");
    let sk = parse_secret(secret)?;
    let key = format!("ed25519:{}", panpak_core::hex(sk.verifying_key().as_bytes()));
    // Provenance: SHA-256 of the original AppImage, streaming (OB2).
    // The converted package is never signed by the upstream author.
    let src_sha256 = {
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        let mut sf = File::open(src)?;
        let mut buf = [0u8; 65536];
        loop {
            use std::io::Read;
            let n = sf.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        panpak_core::hex(&h.finalize())
    };
    let src_name = src.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();
    let mut toml = format!("[app]\nid = \"{id}\"\nname = \"{name}\"\nversion = \"{version}\"\nentry = \"{entry}\"\n");
    if x11 {
        eprintln!("panpak convert: xcb-only app detected: declaring [permissions] x11 = true (verify before distributing)");
        toml.push_str("[permissions]\nx11 = true\n");
    }
    if !allow_missing.is_empty() {
        toml.push_str("[abi]\nallow_missing = [");
        for (i, g) in allow_missing.iter().enumerate() {
            if i > 0 {
                toml.push_str(", ");
            }
            toml.push_str(&format!("\"{g}\""));
        }
        toml.push_str("]\n");
    }
    toml.push_str(&format!("[origin]\nconverted_from = \"{src_name}\"\nversion = \"{version}\"\nsha256 = \"{src_sha256}\"\nsigned_by_upstream = false\n"));
    toml.push_str(&format!("[update]\nkey = \"{key}\"\n"));
    fs::write(tree.join("panpak.toml"), &toml)?;
    let r = cmd_pack(&tree, stub, secret, None, out);
    let _ = fs::remove_dir_all(&tree);
    r
}

/// Conservative evidence: a script forcing QT_QPA_PLATFORM=xcb or
/// xcb plugins with no Wayland plugin at all. Do not infer from mere Qt use.
fn detects_xcb_only(tree: &Path) -> Result<bool> {
    let mut stack = vec![tree.to_path_buf()];
    let (mut xcb, mut wayland) = (false, false);
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() { continue; }
            if kind.is_dir() { stack.push(entry.path()); continue; }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            xcb |= name == "libqxcb.so";
            wayland |= name.starts_with("libqwayland") && name.ends_with(".so");
            if entry.metadata()?.len() <= 65536 {
                let b = fs::read(entry.path())?;
                if let Ok(s) = std::str::from_utf8(&b) {
                    if ["QT_QPA_PLATFORM=xcb", "QT_QPA_PLATFORM=\"xcb\"", "QT_QPA_PLATFORM='xcb'"].iter().any(|v| s.contains(v)) {
                        return Ok(true);
                    }
                }
            }
        }
    }
    Ok(xcb && !wayland)
}


/// Version comparison (anti-downgrade): numbers against numbers, lexical rest.
/// "1.10" > "1.9"; trailing non-numeric suffixes lose against absence.
fn cmp_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let (xn, xs) = split_num(x);
                let (yn, ys) = split_num(y);
                // Release (empty suffix) > prerelease at equal numbers.
                let tail = match (xs.is_empty(), ys.is_empty()) {
                    (true, true) => Ordering::Equal,
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    (false, false) => xs.cmp(ys),
                };
                match xn.cmp(&yn).then(tail) {
                    Ordering::Equal => continue,
                    o => return o,
                }
            }
        }
    }
}

fn split_num(s: &str) -> (u64, &str) {
    let n = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    (s[..n].parse().unwrap_or(0), &s[n..])
}

fn launcher_sh(runtime: &Path, current_link: &Path) -> String {
    format!(
        "#!/bin/sh\n# Generated by `panpak install`: do not edit.\n# Points to apps/<id>/current; `panpak update`/`rollback` move it.\n# With one existing file (double click): opens ONLY that file\n# (--panpak-open, read-only): use `panpak open --write` for writes.\nif [ \"$#\" -eq 1 ] && [ -f \"$1\" ]; then\n  case \"$1\" in /*) _f=\"$1\";; *) _f=\"$PWD/$1\";; esac\n  exec \"{}\" run \"{}\" --panpak-open \"$_f\" \"$_f\"\nfi\nexec \"{}\" run \"{}\" \"$@\"\n",
        runtime.display(),
        current_link.display(),
        runtime.display(),
        current_link.display()
    )
}

fn desktop_file(name: &str, launcher: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={name}\nExec={} %U\nTerminal=false\nCategories=Utility;\nMimeType=application/x-panpak;\n",
        launcher.display()
    )
}

/// `panpak install`: verify, TOFU+pin, copy package, launcher on the installed runtime.
fn tofu_confirmation(input: &str) -> bool {
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn cmd_install(pkg: &Path, yes: bool, runtime_opt: Option<&Path>) -> Result<PathBuf> {
    let store = store_dir()?;
    if let Some(rt) = runtime_opt {
        // --runtime points at a local build: like --allow-unsigned (warning included).
        eprintln!("panpak: --runtime accepts an unverified local build (use only in development)");
        cmd_install_runtime(rt, None, true)?;
    }
    let mut f = File::open(pkg).with_context(|| format!("open {}", pkg.display()))?;
    let ft = panpak_core::read_footer(&mut f)?;
    let (man, raw_index) = panpak_core::verify(&mut f, &ft)?;
    if man.permissions.as_ref().and_then(|p| p.x11).unwrap_or(false) {
        println!("WARNING X11: this app can observe other apps' input and screen. Abstract-socket isolation will be disabled.");
    }
    // Never install/run a stub different from the signed one.
    let (idx_chunks, _) = panpak_core::parse_index(&raw_index)?;
    let stub_len = idx_chunks.iter().map(|c| c.off).min().unwrap_or(ft.index_off);
    panpak_core::verify_stub_range(&mut f, stub_len, &man)?;
    let key = &man.update.as_ref().context("manifest without [update].key")?.key;
    let fp = panpak_core::fingerprint(&panpak_core::parse_pubkey(key)?);

    let (best_abi, runtime) = best_runtime(&store)?;
    let need = man.runtime.as_ref().and_then(|r| r.min_runtime).unwrap_or(1);
    ensure!(
        need <= best_abi,
        "package requires runtime >= {need}, installed {best_abi}: update with `panpak install-runtime`"
    );

    let appdir = store.join("apps").join(&man.app.id);
    fs::create_dir_all(&appdir)?;
    let pin = appdir.join("pinned-key");
    if pin.exists() {
        let old = fs::read_to_string(&pin)?;
        ensure_code!(old.trim() == key.trim(), ErrorCode::E1004, "key DIFFERS from TOFU pin: possible attack or changed author; manual reinstall required");
    } else {
        println!("app: {} {} ({})", man.app.id, man.app.name, man.app.version);
        println!("key fingerprint (TOFU, first 16 hex): {fp}");
        if let Some(o) = man.origin.as_ref() {
            println!("WARNING: THIRD-PARTY CONVERTED package, not verified by the upstream author:");
            println!("  from: {} (version {})", o.converted_from, o.version);
            println!("  sha256: {}", o.sha256);
            println!("  signed by upstream: {}", o.signed_by_upstream);
        }
        if let Some(allow) = man.abi.as_ref().and_then(|a| a.allow_missing.clone()) {
            if !allow.is_empty() {
                println!("WARNING: libraries excluded from resolution (allow_missing declared by the author):");
                for g in &allow {
                    println!("  {g} (if actually needed, the app will fail at startup)");
                }
            }
        }
        println!("pin: {}", pin.display());
        if !yes {
            ensure_code!(
                std::io::IsTerminal::is_terminal(&std::io::stdin()),
                ErrorCode::E5004,
                "non-interactive stdin: pass --yes after verifying the fingerprint"
            );
            print!("Install and trust this key? [y/N] ");
            use std::io::Write;
            std::io::stdout().flush()?;
            let mut ans = String::new();
            std::io::stdin().read_line(&mut ans)?;
            ensure_code!(tofu_confirmation(&ans), ErrorCode::E1003, "installation cancelled: key not trusted");
        }
        fs::write(&pin, format!("{key}\n"))?;
    }
    if let Some(url) = man.update.as_ref().and_then(|u| u.url.clone()) {
        fs::write(appdir.join("update.url"), format!("{url}\n"))?;
    }

    let safe_ver: String = man
        .app
        .version
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || ".-_".contains(c) { c } else { '_' })
        .collect();
    let dest = store.join("packages").join(format!("{}-{safe_ver}.panpak", man.app.id));
    fs::create_dir_all(dest.parent().unwrap())?;
    fs::copy(pkg, &dest)?;
    // current -> package: launchers follow it, update/rollback move it.
    let current = appdir.join("current");
    symlink_atomic(&dest, &current)?;

    let bindir = dirs::home_dir().context("no home dir")?.join(".local/bin");
    fs::create_dir_all(&bindir)?;
    let launcher = bindir.join(&man.app.id);
    fs::write(&launcher, launcher_sh(&runtime, &current))?;
    #[cfg(unix)]
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755))?;
    let apps_dir = dirs::data_dir().context("no data dir")?.join("applications");
    fs::create_dir_all(&apps_dir)?;
    fs::write(
        apps_dir.join(format!("{}.desktop", man.app.id)),
        desktop_file(&man.app.name, &launcher),
    )?;

    println!("installed: {} (runtime {best_abi})", man.app.id);
    println!("launcher: {}", launcher.display());
    Ok(launcher)
}

/// `panpak doctor`: system and optional package status.
/// Returns true if all OK, false if any check fails.
/// Every KO prints the suggested fix; exit code left to the caller.
fn cmd_doctor(pkg: Option<&Path>) -> Result<bool> {
    let mut ok = true;
    let mut line = |good: bool, msg: &str| {
        println!("{} {msg}", if good { "OK  " } else { "KO  " });
        ok &= good;
    };

    // Kernel + Landlock ABI (real probe, not declared version).
    let release = fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_else(|_| "?".into());
    match panpak_core::detect_abi() {
        Ok(abi) => line(true, &format!("kernel {release}, Landlock {abi:?} ({})", abi_features(abi))),
        Err(e) => line(false, &format!("kernel {release}, Landlock unavailable ({}); kernel >= 5.13 with CONFIG_SECURITY_LANDLOCK=y required", panpak_core::err_text(&e))),
    }
    // User namespaces (needed by containers/tooling, not the stub).
    match fs::read_to_string("/proc/sys/user/max_user_namespaces") {
        Ok(v) if v.trim().parse::<u64>().unwrap_or(0) > 0 => line(true, &format!("user namespaces: max {}", v.trim())),
        _ => line(false, "user namespaces disabled; suggestion: sysctl -w user.max_user_namespaces=15000"),
    }
    // Disk space for the store.
    let store = store_dir()?;
    let _ = fs::create_dir_all(&store);
    match disk_free(&store) {
        Some(free) if free > 100 * 1024 * 1024 => line(true, &format!("disk store: {} free", humansize(free))),
        Some(free) => line(false, &format!("disk store almost full ({} free); free some space", humansize(free))),
        None => line(false, "unreadable disk store (statvfs failed)"),
    }
    // GPU.
    match fs::read_dir("/dev/dri") {
        Ok(rd) => {
            let devs: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            line(true, &format!("GPU /dev/dri present: {}", devs.join(",")));
        }
        Err(_) => line(true, "GPU /dev/dri absent (software rendering only)"),
    }
    // Host glibc.
    match host_glibc() {
        Some((kind, ver)) => line(true, &format!("libc host: {kind} {ver}")),
        None => line(false, "host libc not detected (missing ldd?); host resolution may fail"),
    }
    // Installed runtime.
    match best_runtime(&store) {
        Ok((abi, p)) => line(true, &format!("installed runtime: ABI {abi} ({})", p.display())),
        Err(e) => line(false, &panpak_core::err_text(&e)),
    }
    // Package: COMPLETE verification (including every chunk: a doctor that
    // approved corrupt chunks would lie; see conformance/invalid-chunk.panpak).
    if let Some(pkg) = pkg {
        let mut f = File::open(pkg).with_context(|| format!("open {}", pkg.display()))?;
        let checked = panpak_core::read_footer(&mut f).and_then(|ft| {
            panpak_core::verify(&mut f, &ft).and_then(|(man, raw)| {
                panpak_core::parse_index(&raw).and_then(|(chunks, _)| {
                    let stub_len = chunks.iter().map(|c| c.off).min().unwrap_or(ft.index_off);
                    panpak_core::verify_stub_range(&mut f, stub_len, &man)?;
                    let (mut dec, mut raw) = (panpak_core::ChunkDecoder::new()?, Vec::new());
                    for c in &chunks {
                        let comp = panpak_core::read_at(&mut f, c.off, c.comp as u64)?;
                        dec.decode(&comp, c.raw as u64, &mut raw)?;
                        panpak_core::verify_chunk(c, &raw)?;
                    }
                    Ok((ft, man))
                })
            })
        });
        match checked {
            Ok((ft, man)) => {
                line(true, &format!("verified package: {} {} (root {})", man.app.id, man.app.version, &panpak_core::hex(&ft.root)[..16]));
                let need: Vec<String> = man.abi.as_ref().and_then(|a| a.needs.clone()).unwrap_or_default();
                let prefer: Vec<String> = man.abi.as_ref().and_then(|a| a.prefer_host.clone()).unwrap_or_default();
                let allowed: Vec<String> = man.abi.as_ref().and_then(|a| a.allow_missing.clone()).unwrap_or_default();
                let tree = store.join("trees").join(panpak_core::hex(&ft.root));
                let bundled = panpak_core::abi::bundled_libs(&tree);
                match panpak_core::abi::decide(&need, &prefer, &allowed, &panpak_core::abi::host_libs(), &bundled) {
                    Ok(ds) => {
                        for (n, s) in &ds {
                            line(true, &format!("lib {n}: {s:?}"));
                        }
                        let decided: std::collections::HashSet<&str> = ds.iter().map(|(n, _)| n.as_str()).collect();
                        for n in &need {
                            if n.starts_with("ld-linux") || *n == "ld.so" {
                                line(true, &format!("lib {n}: bundle loader (not in farm)"));
                            } else if !decided.contains(n.as_str()) {
                                line(true, &format!("lib {n}: ignored (explicit allow_missing from package author)"));
                            }
                        }
                        if need.is_empty() {
                            line(true, "no dynamic dependencies (static or script)");
                        }
                    }
                    Err(e) => line(false, &format!("unresolved libraries: {}", panpak_core::err_text(&e))),
                }
                if let Some(o) = man.origin.as_ref() {
                    line(true, &format!("provenance: converted from {} v{} (NOT verified by the author)", o.converted_from, o.version));
                    line(true, &format!("original sha256: {}", o.sha256));
                    if !o.signed_by_upstream {
                        line(true, "NOTE: signed by the converter, not upstream");
                    }
                }
            }
            Err(e) => line(false, &format!("INVALID package: {}", panpak_core::err_text(&e))),
        }
    }
    Ok(ok)
}

fn abi_features(abi: landlock::ABI) -> &'static str {
    match abi {
        landlock::ABI::V1 => "basic fs",
        landlock::ABI::V2 => "fs + cross-directory link/rename",
        landlock::ABI::V3 => "fs + truncate",
        landlock::ABI::V4 => "fs + TCP network",
        landlock::ABI::V5 => "fs + network + ioctl",
        _ => "fs + network + ioctl + signal/abstract-socket scope (+Unix paths if V9 supported)",
    }
}

fn humansize(n: u64) -> String {
    const U: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < U.len() {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", U[u])
}

fn disk_free(p: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::mem::zeroed;
    let c = CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    unsafe {
        let mut st: libc::statvfs = zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        st.f_bavail.checked_mul(st.f_frsize as u64)
    }
}

fn host_glibc() -> Option<(String, String)> {
    let out = std::process::Command::new("ldd").arg("--version").output().ok()?;
    let first = String::from_utf8_lossy(&out.stdout).lines().next()?.to_string();
    // "ldd (Ubuntu GLIBC 2.39-...) ..." | "ldd (GNU libc) 2.39" | "musl libc (x86_64) ..."
    if first.contains("musl") {
        return Some(("musl".into(), first));
    }
    let ver = first.split_whitespace().last()?.to_string();
    Some(("glibc".into(), ver))
}

/// Generic code for errors that carry no specific one, by subcommand.
fn generic_code(sub: Option<&str>) -> ErrorCode {
    match sub {
        Some("pack" | "convert" | "dump-index" | "dump-manifest") => ErrorCode::E0000,
        Some("keygen" | "release" | "install-runtime") => ErrorCode::E1000,
        Some("install" | "rollback" | "open" | "doctor") => ErrorCode::E2000,
        Some("update") => ErrorCode::E4000,
        _ => ErrorCode::E5000,
    }
}

fn main() {
    let sub = std::env::args().nth(1);
    if let Err(e) = real_main() {
        if panpak_core::is_storage_full(&e) {
            eprintln!("out of space: free disk space and retry");
        }
        eprintln!("{}", panpak_core::render(&e, generic_code(sub.as_deref())));
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("keygen") => match (args.get(2).map(|s| s.as_str()), args.get(3), args.len()) {
            (Some("-o"), Some(f), 4) => cmd_keygen(Path::new(f)),
            _ => bail_code!(ErrorCode::E5002, "keygen -o <secret-file> (the secret is never printed)\n{usage}", usage = usage()),
        },
        Some("pack") => {
            let dir = args.get(2).ok_or_else(|| coded(ErrorCode::E5002, "pack <folder>"))?;
            let mut stub = None;
            let mut secret_file = None;
            let mut rotate_from = None;
            let mut out = None;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--stub" => {
                        stub = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--stub <file>"))?.as_str());
                        i += 2;
                    }
                    "--secret-file" => {
                        secret_file = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--secret-file <f>"))?.clone());
                        i += 2;
                    }
                    "--rotate-from-file" => {
                        rotate_from = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--rotate-from-file <f>"))?.clone());
                        i += 2;
                    }
                    "-o" => {
                        out = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "-o <out>"))?.as_str());
                        i += 2;
                    }
                    x => bail_code!(ErrorCode::E5001, "unknown flag {x}\n{usage}", usage = usage()),
                }
            }
            let secret = load_secret(secret_file.as_deref())?;
            let out_p = out.map(PathBuf::from);
            let rotate_secret = rotate_from.as_deref().map(|f| panpak_core::read_secret_file(Path::new(f))).transpose()?;
            cmd_pack(Path::new(dir), Path::new(stub.ok_or_else(|| coded(ErrorCode::E5002, "missing --stub"))?), &secret, rotate_secret.as_deref(), out_p.as_deref())?;
            Ok(())
        }
        Some("install-runtime") => {
            let src = args.get(2).ok_or_else(|| coded(ErrorCode::E5002, "install-runtime <file>"))?;
            let mut key = None;
            let mut allow_unsigned = false;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--release-key" => {
                        key = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--release-key <ed25519:...>"))?.clone());
                        i += 2;
                    }
                    "--allow-unsigned" => {
                        allow_unsigned = true;
                        i += 1;
                    }
                    "--yes" => {
                        i += 1; // Accepted for compatibility (no prompt here).
                    }
                    x => bail_code!(ErrorCode::E5001, "unknown flag {x}\n{usage}", usage = usage()),
                }
            }
            cmd_install_runtime(Path::new(src), key.as_deref(), allow_unsigned)?;
            Ok(())
        }
        Some("release") => {
            let mut version = None;
            let mut secret_file = None;
            let mut target = None;
            let mut outdir = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--version" => { version = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--version <v>"))?.as_str()); i += 2; }
                    "--secret-file" => { secret_file = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--secret-file <f>"))?.clone()); i += 2; }
                    "--target" => { target = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--target <t>"))?.as_str()); i += 2; }
                    "--out-dir" => { outdir = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--out-dir <d>"))?.as_str()); i += 2; }
                    x => bail_code!(ErrorCode::E5001, "unknown flag {x}\n{usage}", usage = usage()),
                }
            }
            let outdir = outdir.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
            let secret = load_secret(secret_file.as_deref())?;
            cmd_release(version.ok_or_else(|| coded(ErrorCode::E5002, "missing --version"))?, &secret, target, &outdir)?;
            Ok(())
        }
        Some("licenses") => {
            ensure_code!(args.len() <= 3, ErrorCode::E5001, "licenses [package]\n{usage}", usage = usage());
            cmd_licenses(args.get(2).map(Path::new))
        }
        Some("rollback") => {
            let id = args.get(2).ok_or_else(|| coded(ErrorCode::E5002, "rollback <app-id>"))?;
            cmd_rollback(id)?;
            Ok(())
        }
        Some("open") => {
            let mut write = false;
            let mut rename = false;
            let mut id = None;
            let mut file = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--write" => {
                        write = true;
                        i += 1;
                    }
                    "--write-rename" => {
                        rename = true;
                        i += 1;
                    }
                    x if x.starts_with('-') => bail_code!(ErrorCode::E5001, "unknown flag {x}\n{usage}", usage = usage()),
                    _ if id.is_none() => {
                        id = Some(args[i].as_str());
                        i += 1;
                    }
                    _ if file.is_none() => {
                        file = Some(args[i].as_str());
                        i += 1;
                    }
                    x => bail_code!(ErrorCode::E5001, "unexpected argument: {x}\n{usage}", usage = usage()),
                }
            }
            cmd_open(id.ok_or_else(|| coded(ErrorCode::E5002, "open <app-id> [file]"))?, file, write, rename)?;
            Ok(())
        }
        Some("convert") => {
            let src = args.get(2).ok_or_else(|| coded(ErrorCode::E5002, "convert <app.AppImage>"))?;
            let mut id = None;
            let mut stub = None;
            let mut secret_file = None;
            let mut name = None;
            let mut version = None;
            let mut entry = None;
            let mut allow_missing: Vec<&str> = Vec::new();
            let mut out = None;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--id" => { id = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--id <reverse-dns>"))?.as_str()); i += 2; }
                    "--stub" => { stub = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--stub <file>"))?.as_str()); i += 2; }
                    "--secret-file" => { secret_file = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--secret-file <f>"))?.clone()); i += 2; }
                    "--name" => { name = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--name <n>"))?.as_str()); i += 2; }
                    "--version" => { version = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--version <v>"))?.as_str()); i += 2; }
                    "--entry" => { entry = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--entry <e>"))?.as_str()); i += 2; }
                    "--allow-missing" => { allow_missing.push(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--allow-missing <glob>"))?.as_str()); i += 2; }
                    "-o" => { out = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "-o <out>"))?.as_str()); i += 2; }
                    x => bail_code!(ErrorCode::E5001, "unknown flag {x}\n{usage}", usage = usage()),
                }
            }
            let secret = load_secret(secret_file.as_deref())?;
            cmd_convert(ConvertArgs {
                src: Path::new(src),
                id: id.ok_or_else(|| coded(ErrorCode::E5002, "missing --id"))?,
                stub: Path::new(stub.ok_or_else(|| coded(ErrorCode::E5002, "missing --stub"))?),
                secret: &secret,
                name_ov: name,
                version_ov: version,
                entry_ov: entry,
                allow_missing: allow_missing.into_iter().map(|s| s.to_string()).collect(),
                out: out.map(PathBuf::from).as_deref(),
            })?;
            Ok(())
        }
        Some("doctor") => {
            let pkg = args.get(2).map(PathBuf::from);
            let rc = cmd_doctor(pkg.as_deref())?;
            if !rc {
                std::process::exit(1);
            }
            Ok(())
        }
        Some("update") => {
            let mut allow = false;
            let mut insecure = false;
            let mut timeout = 30u64;
            let mut id = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--allow-downgrade" => {
                        allow = true;
                        i += 1;
                    }
                    "--allow-insecure-http" => {
                        insecure = true;
                        i += 1;
                    }
                    "--timeout" => {
                        timeout = args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--timeout <seconds>"))?.parse().map_err(|_| coded(ErrorCode::E5001, "non-numeric --timeout"))?;
                        ensure_code!((1..=300).contains(&timeout), ErrorCode::E5001, "--timeout must be between 1 and 300");
                        i += 2;
                    }
                    x if x.starts_with('-') => bail_code!(ErrorCode::E5001, "unknown flag {x}\n{usage}", usage = usage()),
                    _ => {
                        id = Some(args[i].as_str());
                        i += 1;
                    }
                }
            }
            cmd_update(id.ok_or_else(|| coded(ErrorCode::E5002, "update <app-id>"))?, allow, insecure, timeout)?;
            Ok(())
        }
        Some("dump-index") | Some("dump-manifest") => {
            let what = args[1].clone();
            let pkg = args.get(2).ok_or_else(|| coded(ErrorCode::E5002, "dump <package> <out>"))?;
            let out = args.get(3).ok_or_else(|| coded(ErrorCode::E5002, "dump <package> <out>"))?;
            let mut f = File::open(pkg)?;
            let ft = panpak_core::read_footer(&mut f)?;
            let (_man, raw_index) = panpak_core::verify(&mut f, &ft)?;
            if what == "dump-index" {
                fs::write(out, &raw_index)?;
            } else {
                let toml_b = panpak_core::read_at(&mut f, ft.man_off, ft.man_len)?;
                fs::write(out, &toml_b)?;
            }
            Ok(())
        }
        Some("install") => {
            let mut yes = false;
            let mut runtime = None;
            let mut pkg = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--yes" => {
                        yes = true;
                        i += 1;
                    }
                    "--runtime" => {
                        runtime = Some(args.get(i + 1).ok_or_else(|| coded(ErrorCode::E5002, "--runtime <stub>"))?.as_str());
                        i += 2;
                    }
                    x if x.starts_with('-') => bail_code!(ErrorCode::E5001, "unknown flag {x}\n{usage}", usage = usage()),
                    _ => {
                        pkg = Some(args[i].as_str());
                        i += 1;
                    }
                }
            }
            cmd_install(
                Path::new(pkg.ok_or_else(|| coded(ErrorCode::E5002, "install <package>"))?),
                yes,
                runtime.map(Path::new),
            )?;
            Ok(())
        }
        _ => {
            eprintln!("{usage}", usage = usage());
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xcb_detection_requires_evidence() {
        let dir = std::env::temp_dir().join(format!("panpak-xcb-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(!detects_xcb_only(&dir).unwrap());
        fs::write(dir.join("libqxcb.so"), b"ELF").unwrap();
        assert!(detects_xcb_only(&dir).unwrap());
        fs::write(dir.join("libqwayland.so"), b"ELF").unwrap();
        assert!(!detects_xcb_only(&dir).unwrap());
        fs::write(dir.join("AppRun"), b"#!/bin/sh\nexport QT_QPA_PLATFORM=xcb\n").unwrap();
        assert!(detects_xcb_only(&dir).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn range_coalescence_preserves_holes_and_caps_memory() {
        assert_eq!(coalesce_ranges(vec![(20, 30), (0, 10), (10, 20), (40, 50)]), vec![(0, 30), (40, 50)]);
        assert_eq!(coalesce_ranges(vec![(0, 9 << 20)]), vec![(0, 4 << 20), (4 << 20, 8 << 20), (8 << 20, 9 << 20)]);
        assert!(coalesce_ranges(vec![(5, 5)]).is_empty());
    }

    /// Minimal hand-built ELF64 with DT_NEEDED libc.so.6.
    fn tiny_elf() -> Vec<u8> {
        let mut b = vec![0u8; 251];
        b[0..4].copy_from_slice(b"\x7fELF");
        b[4] = 2; // 64 bit
        b[5] = 1; // LE
        b[6] = 1; // version
        b[0x10] = 2; // ET_EXEC
        b[0x12] = 62; // x86-64
        b[0x20..0x28].copy_from_slice(&64u64.to_le_bytes()); // phoff
        b[0x34..0x36].copy_from_slice(&56u16.to_le_bytes()); // phentsize
        b[0x38..0x3A].copy_from_slice(&2u16.to_le_bytes()); // phnum
        // PHDR0 PT_LOAD: whole file at vaddr 0x400000.
        b[64..68].copy_from_slice(&1u32.to_le_bytes());
        b[72..80].copy_from_slice(&0u64.to_le_bytes()); // offset
        b[80..88].copy_from_slice(&0x400000u64.to_le_bytes()); // vaddr
        b[104..112].copy_from_slice(&251u64.to_le_bytes()); // filesz/memsz
        // PHDR1 PT_DYNAMIC at offset 176.
        b[120..124].copy_from_slice(&2u32.to_le_bytes());
        b[128..136].copy_from_slice(&176u64.to_le_bytes()); // offset
        b[136..144].copy_from_slice(&(0x400000u64 + 176).to_le_bytes()); // vaddr
        // Dynamic: NEEDED(1) STRTAB(vaddr) STRSZ NULL.
        b[176..184].copy_from_slice(&1i64.to_le_bytes());
        b[184..192].copy_from_slice(&1u64.to_le_bytes());
        b[192..200].copy_from_slice(&5i64.to_le_bytes());
        b[200..208].copy_from_slice(&(0x400000u64 + 240).to_le_bytes());
        b[208..216].copy_from_slice(&10i64.to_le_bytes());
        b[216..224].copy_from_slice(&11u64.to_le_bytes());
        // strtab at 240: "\0libc.so.6\0".
        b[241..250].copy_from_slice(b"libc.so.6");
        b
    }

    #[test]
    fn elf_needs_reads_dt_needed() {
        let dir = std::env::temp_dir().join(format!("panpak-elf-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.out");
        fs::write(&p, tiny_elf()).unwrap();
        assert_eq!(elf_needs(&p).unwrap(), ["libc.so.6"]);
        // Non-ELF -> empty, not an error.
        fs::write(&p, b"#!/bin/sh\n").unwrap();
        assert!(elf_needs(&p).unwrap().is_empty());
        // Truncated ELF -> error, not panic.
        fs::write(&p, &tiny_elf()[..100]).unwrap();
        assert!(elf_needs(&p).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn url_policy() {
        // https always ok (no I/O here, agent policy only).
        assert!(agent_for("https://example.org/x.panpak", false, 5).is_ok());
        // http: refused without flag even on localhost.
        assert!(agent_for("http://127.0.0.1:8471/x.panpak", false, 5).is_err());
        // http: refused toward non-local hosts even with the flag.
        assert!(agent_for("http://example.org/x.panpak", true, 5).is_err());
        // http+flag on localhost: allowed (test).
        assert!(agent_for("http://127.0.0.1:8471/x.panpak", true, 5).is_ok());
        assert!(agent_for("http://localhost:8471/x.panpak", true, 5).is_ok());
        assert!(is_localhost("localhost") && is_localhost("127.0.0.1") && is_localhost("[::1]"));
        assert!(!is_localhost("example.org") && !is_localhost(""));
        // weird schemes: never.
        assert!(agent_for("ftp://example.org/x", true, 5).is_err());
        assert!(agent_for("file:///tmp/x", true, 5).is_err());
    }

    #[test]
    fn user_errors_have_stable_codes() {
        let code = |r: Result<_, anyhow::Error>, generic| panpak_core::code_of(&r.unwrap_err(), generic);
        // Plain http without the flag: insecure transport refused.
        let c = code(agent_for("http://127.0.0.1:1/x.panpak", false, 5).map(|_| ()), ErrorCode::E4000);
        assert_eq!(c, ErrorCode::E4002);
        // Unsupported scheme: untyped, falls back to the generic code.
        let c = code(agent_for("ftp://example.org/x", true, 5).map(|_| ()), ErrorCode::E4000);
        assert_eq!(c, ErrorCode::E4000);
        // open: invalid id and missing app are untyped store errors.
        let c = code(cmd_open("non-un-id", Some("/tmp/x"), false, false).map(|_| ()), ErrorCode::E2000);
        assert_eq!(c, ErrorCode::E2000);
        let e = cmd_open("org.example.never-installed", Some("/tmp/x"), false, false).unwrap_err();
        assert_eq!(panpak_core::code_of(&e, ErrorCode::E2000), ErrorCode::E2000);
        // Printed form: code first, no internal dumps.
        let m = panpak_core::render(&e, ErrorCode::E2000);
        assert!(m.starts_with("PANPAK-E2000: "), "{m}");
        assert!(!m.contains("Os {") && !m.contains("panicked"), "internal trace: {m}");
    }

    #[test]
    fn pack_embeds_licenses_and_reserves_dot_panpak() {
        let dir = std::env::temp_dir().join(format!("panpak-lic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let tree = dir.join("tree");
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("go"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(tree.join("go"), fs::Permissions::from_mode(0o755)).unwrap();
        let secret = "29".repeat(32);
        let sk = panpak_core::parse_secret(&secret).unwrap();
        let key = format!("ed25519:{}", panpak_core::hex(sk.verifying_key().as_bytes()));
        fs::write(
            tree.join("panpak.toml"),
            format!("[app]\nid=\"org.example.lic\"\nname=\"L\"\nversion=\"1\"\nentry=\"go\"\n[update]\nkey=\"{key}\"\n"),
        )
        .unwrap();
        let stub = dir.join("stub");
        fs::write(&stub, b"FAKE-STUB").unwrap();
        let out = dir.join("a.panpak");
        cmd_pack(&tree, &stub, &secret, None, Some(&out)).unwrap();
        let mut f = File::open(&out).unwrap();
        let ft = panpak_core::read_footer(&mut f).unwrap();
        let (_man, raw) = panpak_core::verify(&mut f, &ft).unwrap();
        let files = panpak_core::read_package_licenses(&mut f, &raw).unwrap();
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["LICENSE-APACHE", "LICENSE-MIT", "NOTICE", "THIRD-PARTY-LICENSES"]);
        for (name, data) in &files {
            let want = LICENSE_FILES.iter().find(|(n, _)| n == name).unwrap().1;
            assert_eq!(data, want.as_bytes(), "{name}");
        }
        // Same input -> same package (licenses do not break determinism).
        let out2 = dir.join("b.panpak");
        cmd_pack(&tree, &stub, &secret, None, Some(&out2)).unwrap();
        assert_eq!(fs::read(&out).unwrap(), fs::read(&out2).unwrap());
        // The reserved top level cannot come from the user's tree.
        fs::create_dir_all(tree.join(".panpak")).unwrap();
        fs::write(tree.join(".panpak/x"), "x").unwrap();
        let e = cmd_pack(&tree, &stub, &secret, None, Some(&dir.join("c.panpak"))).unwrap_err();
        assert!(panpak_core::err_text(&e).contains("reserved"), "{e:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generic_code_per_subcommand() {
        assert_eq!(generic_code(Some("pack")), ErrorCode::E0000);
        assert_eq!(generic_code(Some("release")), ErrorCode::E1000);
        assert_eq!(generic_code(Some("install")), ErrorCode::E2000);
        assert_eq!(generic_code(Some("update")), ErrorCode::E4000);
        assert_eq!(generic_code(None), ErrorCode::E5000);
        assert_eq!(generic_code(Some("nonsense")), ErrorCode::E5000);
    }

    #[test]
    fn tofu_confirmation_accepts_only_y_or_yes() {
        assert!(tofu_confirmation("y"));
        assert!(tofu_confirmation("Y"));
        assert!(tofu_confirmation("yes"));
        assert!(tofu_confirmation(" YES\n"));
        assert!(!tofu_confirmation(""));
        assert!(!tofu_confirmation("s"));
        assert!(!tofu_confirmation("yess"));
    }

    #[test]
    fn humansize_formats() {
        assert_eq!(humansize(0), "0.0 B");
        assert_eq!(humansize(1536), "1.5 KiB");
        assert_eq!(humansize(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn rotation_valid_broken_and_replay() {        use ed25519_dalek::Signer;
        let old_sk = ed25519_dalek::SigningKey::from_bytes(&[11u8; 32]);
        let new_sk = ed25519_dalek::SigningKey::from_bytes(&[22u8; 32]);
        let evil_sk = ed25519_dalek::SigningKey::from_bytes(&[33u8; 32]);
        let old_pub = format!("ed25519:{}", panpak_core::hex(old_sk.verifying_key().as_bytes()));
        let new_pub = format!("ed25519:{}", panpak_core::hex(new_sk.verifying_key().as_bytes()));
        let good_sig = panpak_core::hex(&old_sk.sign(new_sk.verifying_key().as_bytes()).to_bytes());
        // Valid chain.
        assert!(panpak_core::verify_rotation(&old_pub, &new_pub, Some(&old_pub), Some(&good_sig)).is_ok());
        // Signed by the wrong key.
        let bad_sig = panpak_core::hex(&evil_sk.sign(new_sk.verifying_key().as_bytes()).to_bytes());
        assert!(panpak_core::verify_rotation(&old_pub, &new_pub, Some(&old_pub), Some(&bad_sig)).is_err());
        // rotation_old different from the pin (someone else's rotation replay).
        let evil_pub = format!("ed25519:{}", panpak_core::hex(evil_sk.verifying_key().as_bytes()));
        assert!(panpak_core::verify_rotation(&old_pub, &new_pub, Some(&evil_pub), Some(&good_sig)).is_err());
        // Tampered signature.
        let mut tampered = good_sig.clone();
        tampered.replace_range(0..2, if &tampered[0..2] == "00" { "ff" } else { "00" });
        assert!(panpak_core::verify_rotation(&old_pub, &new_pub, Some(&old_pub), Some(&tampered)).is_err());
        // Missing fields.
        assert!(panpak_core::verify_rotation(&old_pub, &new_pub, None, Some(&good_sig)).is_err());
        assert!(panpak_core::verify_rotation(&old_pub, &new_pub, Some(&old_pub), None).is_err());
    }

    #[test]
    fn versions_order() {        use std::cmp::Ordering::*;
        assert_eq!(cmp_versions("1.0", "1.0"), Equal);
        assert_eq!(cmp_versions("1.10", "1.9"), Greater);
        assert_eq!(cmp_versions("2.0", "10.0"), Less);
        assert_eq!(cmp_versions("1.0", "1.0.1"), Less);
        assert_eq!(cmp_versions("1.0-rc1", "1.0"), Less);
        assert_eq!(cmp_versions("0.1.0", "0.1.0"), Equal);
    }

    #[test]
    fn find_in_finds_nested() {
        let dir = std::env::temp_dir().join(format!("panpak-find-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("a/b")).unwrap();
        fs::write(dir.join("a/b/panpak-stub"), b"x").unwrap();
        assert_eq!(find_in(&dir, "panpak-stub").unwrap(), dir.join("a/b/panpak-stub"));
        assert!(find_in(&dir, "nonexistent").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn launcher_and_desktop() {
        let rt = Path::new("/home/u/.local/share/panpak/runtimes/1/panpak-stub");
        let cur = Path::new("/home/u/.local/share/panpak/apps/org.x/current");
        let sh = launcher_sh(rt, cur);
        assert!(sh.starts_with("#!/bin/sh\n"));
        assert!(sh.contains(" run "));
        assert!(sh.contains("--panpak-open"));
        assert!(sh.contains("[ -f \"$1\" ]"));
        assert!(sh.contains("apps/org.x/current"));
        assert!(sh.ends_with("\"$@\"\n"));
        let d = desktop_file("X App", Path::new("/home/u/.local/bin/org.x"));
        assert!(d.contains("Name=X App"));
        assert!(d.contains("Exec=/home/u/.local/bin/org.x"));
        assert!(d.contains("MimeType=application/x-panpak;"));
    }
}
