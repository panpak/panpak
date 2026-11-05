// SPDX-License-Identifier: MIT OR Apache-2.0
// panpak CLI: pack a tree into an executable .panpak file (stub + payload).

// `pack` allocates and frees a buffer for every chunk (chunker output, reads);
// the musl allocator returns each large block to the system, which costs an
// mmap/munmap pair and fresh page faults per chunk. mimalloc reuses the memory
// (CLI only; the stub keeps the default allocator).
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
    "usage:\n  panpak keygen -o <secret-file>\n  panpak licenses [package]\n  (secret key: --secret-file <f> or PANPAK_SECRET_FILE=<f>; file mode 0600)\n  panpak pack <folder> --stub <stub> [--secret-file <f>] [--rotate-from-file <f>] [-o <out.panpak>]\n  panpak convert <app.AppImage> --id <reverse-dns> --stub <stub> [--secret-file <f>] [--name N] [--version V] [--entry E] [--allow-missing <glob>] [-o <out.panpak>]\n  panpak install-runtime <stub> --allow-unsigned      (your own build; the file, not a folder)
  panpak install-runtime <release.tar.gz> --release-key <ed25519:...>
  panpak release --version V [--secret-file <f>] [--target T] [--out-dir D]\n  panpak install [--yes] [--accept-new-permissions] [--runtime <stub>] <package>\n  panpak update [--yes] [--accept-new-permissions] [--allow-downgrade] [--allow-insecure-http] [--timeout N] <app-id>\n  panpak data path <app-id>\n  panpak data clear [--yes] <app-id>\n  panpak data unlock <app-id>\n  panpak rollback <app-id>
  panpak open <app-id> [file] [--write | --write-rename]\n  panpak doctor [package]\n  panpak dbus-rules <package>\n  panpak desktop-integration install|remove\n  panpak dump-index <package> <out.raw>\n  panpak dump-manifest <package> <out.toml>"
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
fn cmd_update(app_id: &str, yes: bool, accept_new: bool, allow_downgrade: bool, allow_insecure: bool, timeout_secs: u64) -> Result<PathBuf> {
    use std::collections::HashMap;
    panpak_core::validate_app_id(app_id)?;
    let store = store_dir()?;
    let appdir = store.join("apps").join(app_id);
    // Refuse before downloading if the launcher cannot be replaced.
    let launcher = dirs::home_dir().context("no home dir")?.join(".local/bin").join(app_id);
    check_replaceable(&launcher, "launcher", &|s| launcher_is_ours(s, app_id))?;
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
    panpak_core::validate_manifest(&man)?;
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
    // New permissions need a yes before the update is published (current stays on a no).
    print_permissions(&vman);
    let added = panpak_core::added_permissions(&old_man, &vman);
    if let Err(e) = accept_new_permissions(&added, yes, accept_new, "update") {
        let _ = fs::remove_file(&tmp);
        return Err(e);
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
    write_replacing(&launcher, &launcher_sh(app_id, &runtime, &current_link)?, 0o755, "launcher", &|s| launcher_is_ours(s, app_id))?;
    // The user folders and the URL schemes of the new version.
    let (xdg_found, xdg_problems) = resolve_xdg_dirs(&man);
    print_xdg_dirs(&xdg_found, &xdg_problems);
    write_xdg_pin(&appdir, &xdg_found)?;
    let desktop = desktop_path(app_id)?;
    let schemes = manifest_schemes(&man);
    if desktop.exists() || legacy_desktop_path(app_id)?.exists() || !schemes.is_empty() {
        fs::create_dir_all(desktop.parent().unwrap())?;
        write_replacing(&desktop, &desktop_file(app_id, &man.app.name, &launcher, &schemes, manifest_wm_class(&man))?, 0o644, "desktop entry", &|s| desktop_is_ours(s, app_id, &launcher))?;
        let moved = remove_legacy_desktop(app_id, &launcher)?;
        if !schemes.is_empty() || moved {
            refresh_desktop_database(desktop.parent().unwrap());
        }
    }
    println!("updated {app_id}: {} -> {} (reused chunks {n_reused}, fetched chunks {n_fetched}, runtime {best_abi})",
        old_man.app.version, man.app.version);
    println!("downloaded bytes: {downloaded}");
    Ok(dest)
}

fn cmd_rollback(app_id: &str) -> Result<PathBuf> {
    panpak_core::validate_app_id(app_id)?;
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
    panpak_core::validate_app_id(app_id)?;
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
    panpak_core::validate_manifest(&man)?;
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

    // The manifest is complete here (the libraries found in the files are in `abi.needs`): check it with
    // the rules `install` applies, before anything is written or signed. A package that install refuses
    // must not exist.
    panpak_core::validate_manifest(&man).context("the manifest that would be signed is invalid; nothing was written")?;

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
    install_file_atomically(&stub, &dest.join("panpak-stub"), 0o755)?;
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

/// Copy `src` next to `dest` and rename it over `dest`. Writing into a runtime that apps are executing fails with
/// "Text file busy"; a rename leaves the running processes on the old file and gives new starts the new one.
fn install_file_atomically(src: &Path, dest: &Path, mode: u32) -> Result<()> {
    let dir = dest.parent().context("destination without a directory")?;
    let name = dest.file_name().context("destination without a file name")?.to_string_lossy();
    let tmp = dir.join(format!(".{name}.panpak-tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let res = (|| -> Result<()> {
        fs::copy(src, &tmp)?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        fs::File::open(&tmp)?.sync_all()?;
        fs::rename(&tmp, dest)?;
        Ok(())
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res.with_context(|| format!("install {}", dest.display()))
}

fn install_runtime_stub(stub: &Path, store: &Path) -> Result<()> {
    let abi = stub_abi(stub)?;
    let dest = store.join("runtimes").join(abi.to_string());
    fs::create_dir_all(&dest)?;
    install_file_atomically(stub, &dest.join("panpak-stub"), 0o755)?;
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
    panpak_core::validate_app_id(id)?;
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
    let mut dwm: Option<String> = None;
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
            } else if let Some(v) = line.strip_prefix("StartupWMClass=") {
                dwm = Some(v.trim().to_string());
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
            let wrap = format!("#!/bin/sh\n# Generated by `panpak convert`: fixed Exec arguments.\nHERE=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\nTREE=${{HERE%/bin}}\nexec \"$TREE\"/{} {} \"$@\"\n", sh_quote(&entry), quoted.join(" "));
            fs::create_dir_all(tree.join("bin"))?;
            fs::write(tree.join("bin/.panpak-exec"), &wrap)?;
            #[cfg(unix)]
            fs::set_permissions(tree.join("bin/.panpak-exec"), fs::Permissions::from_mode(0o755))?;
            entry = "bin/.panpak-exec".to_string();
        } else if !is_elf {
            let wrap = format!("#!/bin/sh\n# Generated by `panpak convert`: emulates the AppImage runtime environment.\nHERE=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\nTREE=${{HERE%/bin}}\nexport APPDIR=\"$TREE\" APPIMAGE=\"$TREE\" ARGV0=\"$0\"\nif [ -z \"$OWD\" ]; then export OWD=\"$PWD\"; fi\nexec \"$TREE\"/{} \"$@\"\n", sh_quote(&entry));
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
    // Strings from the AppImage (names, versions, paths) go through the TOML
    // serializer: a quote or a newline in them must not add keys or sections.
    let q = toml_str;
    let mut toml = format!("[app]\nid = {}\nname = {}\nversion = {}\nentry = {}\n", q(id), q(&name), q(&version), q(&entry));
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
            toml.push_str(&q(g));
        }
        toml.push_str("]\n");
    }
    // The window class the app sets, from the AppImage's own desktop file: the desktop groups windows by it.
    match dwm.as_deref() {
        Some(w) if panpak_core::valid_wm_class(w) => {
            toml.push_str(&format!("[desktop]\nwm_class = {}\n", q(w)));
        }
        Some(w) => eprintln!("panpak convert: WARNING: StartupWMClass {w:?} in the desktop file is not a plain window class; not used"),
        None => eprintln!("panpak convert: WARNING: the desktop file has no StartupWMClass; the desktop may not group the app's windows under its entry (set [desktop] wm_class in panpak.toml if you know the class)"),
    }
    toml.push_str(&format!("[origin]\nconverted_from = {}\nversion = {}\nsha256 = {}\nsigned_by_upstream = false\n", q(&src_name), q(&version), q(&src_sha256)));
    toml.push_str(&format!("[update]\nkey = {}\n", q(&key)));
    fs::write(tree.join("panpak.toml"), &toml)?;
    let r = cmd_pack(&tree, stub, secret, None, out);
    let _ = fs::remove_dir_all(&tree);
    r
}

/// A TOML basic string, quoted and escaped.
fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
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

/// Marker that tells `install` and `update` that panpak wrote a launcher or a
/// `.desktop` file for this app (and may therefore replace it).
const LAUNCHER_MARKER: &str = "# X-Panpak-App-Id: ";
const DESKTOP_MARKER: &str = "X-Panpak-App-Id=";

/// POSIX shell single quoting.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn path_text(p: &Path) -> Result<&str> {
    let s = p.to_str().context("path is not valid UTF-8")?;
    ensure!(!s.chars().any(char::is_control), "control character in path {s:?}");
    Ok(s)
}

fn launcher_sh(app_id: &str, runtime: &Path, current_link: &Path) -> Result<String> {
    let (rt, cur) = (sh_quote(path_text(runtime)?), sh_quote(path_text(current_link)?));
    Ok(format!(
        "#!/bin/sh\n# Generated by `panpak install`: do not edit.\n{LAUNCHER_MARKER}{app_id}\n# Points to apps/<id>/current; `panpak update`/`rollback` move it.\n# With one existing file (double click): opens ONLY that file\n# (--panpak-open, read-only): use `panpak open --write` for writes.\nif [ \"$#\" -eq 1 ] && [ -f \"$1\" ]; then\n  case \"$1\" in /*) _f=\"$1\";; *) _f=\"$PWD/$1\";; esac\n  exec {rt} run {cur} --panpak-open \"$_f\" \"$_f\"\nfi\nexec {rt} run {cur} \"$@\"\n"
    ))
}

/// A launcher written by panpak for `app_id`: it carries the marker, or it is
/// one written before the marker existed (same two first lines).
fn launcher_is_ours(content: &str, app_id: &str) -> bool {
    content.lines().any(|l| l.strip_prefix(LAUNCHER_MARKER) == Some(app_id))
        || content.starts_with("#!/bin/sh\n# Generated by `panpak install`: do not edit.\n# Points to apps/<id>/current")
}

/// Value of a Desktop Entry `string` key (Desktop Entry Specification, "Value
/// types"): backslash, newline, carriage return and tab are escaped, other
/// control characters are refused, so a value can never start a new line.
fn desktop_string(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => bail!("control character in a desktop entry value"),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// One `Exec` argument (Desktop Entry Specification, "The Exec key"): always
/// double-quoted, with `"`, `` ` ``, `$` and `\` escaped and `%` doubled, then
/// string-escaped like any other value.
fn desktop_exec_arg(p: &str) -> Result<String> {
    ensure!(!p.chars().any(char::is_control), "control character in the launcher path");
    let mut q = String::from("\"");
    for c in p.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                q.push('\\');
                q.push(c);
            }
            '%' => q.push_str("%%"),
            c => q.push(c),
        }
    }
    q.push('"');
    desktop_string(&q)
}

/// The entry declares no MimeType: the manifest has no field for MIME types, and
/// `application/x-panpak` is the type of the packages themselves, not of the
/// files an app opens.
fn desktop_file(app_id: &str, name: &str, launcher: &Path, schemes: &[String], wm_class: Option<&str>) -> Result<String> {
    // The URL schemes the manifest declares; every name is checked again here, whatever the caller did.
    let mut mime = String::new();
    for sc in schemes {
        panpak_core::check_scheme_name(sc).map_err(|why| anyhow::anyhow!("scheme {sc}: {why}"))?;
        mime += &format!("x-scheme-handler/{sc};");
    }
    let mime = if mime.is_empty() { String::new() } else { format!("MimeType={mime}\n") };
    // The window class the app sets, so that the desktop groups its windows under this entry.
    let class = match wm_class {
        Some(w) => {
            ensure!(panpak_core::valid_wm_class(w), "wm_class {w:?} is not a plain window class");
            format!("StartupWMClass={w}\n")
        }
        None => String::new(),
    };
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName={}\nExec={} %U\nTerminal=false\nCategories=Utility;\n{mime}{class}{DESKTOP_MARKER}{}\n",
        desktop_string(name)?,
        desktop_exec_arg(path_text(launcher)?)?,
        desktop_string(app_id)?
    ))
}

/// The `.desktop` file of an app: `panpak.<id>.desktop`. The portals know the app as `panpak.<id>` (the
/// name of its systemd scope), and look for the file of that name; the prefix also keeps the entry
/// apart from the Flatpak or distribution entry of an app with the same upstream id.
fn desktop_path(app_id: &str) -> Result<PathBuf> {
    Ok(dirs::data_dir().context("no data dir")?.join("applications").join(format!("panpak.{app_id}.desktop")))
}

/// The name this entry had before the prefix: `<id>.desktop`.
fn legacy_desktop_path(app_id: &str) -> Result<PathBuf> {
    Ok(dirs::data_dir().context("no data dir")?.join("applications").join(format!("{app_id}.desktop")))
}

/// Remove the entry of the old name if panpak wrote it. One that is not ours stays. Returns whether one was removed.
fn remove_legacy_desktop(app_id: &str, launcher: &Path) -> Result<bool> {
    let old = legacy_desktop_path(app_id)?;
    match fs::symlink_metadata(&old) {
        Ok(m) if m.file_type().is_file() && m.len() <= 1 << 20 => {
            let ours = fs::read(&old).ok().and_then(|b| String::from_utf8(b).ok()).is_some_and(|s| desktop_is_ours(&s, app_id, launcher));
            if ours {
                fs::remove_file(&old).with_context(|| format!("remove {}", old.display()))?;
            }
            Ok(ours)
        }
        _ => Ok(false),
    }
}

/// The window class of the manifest.
fn manifest_wm_class(man: &panpak_core::Manifest) -> Option<&str> {
    man.desktop.as_ref().and_then(|d| d.wm_class.as_deref())
}

/// The URL schemes the manifest declares.
fn manifest_schemes(man: &panpak_core::Manifest) -> Vec<String> {
    man.desktop.as_ref().and_then(|d| d.schemes.clone()).unwrap_or_default()
}

/// A user folder asked for by the manifest and found in the user's `user-dirs.dirs`.
struct ResolvedDir {
    name: String,
    rel: String,
    access: String,
}

/// Find the user folders the manifest asks for, from the real `user-dirs.dirs` (outside the sandbox).
/// A folder that cannot be found, or fails the rules, is left out with the reason.
fn resolve_xdg_dirs(man: &panpak_core::Manifest) -> (Vec<ResolvedDir>, Vec<String>) {
    let (mut ok, mut why) = (Vec::new(), Vec::new());
    let wanted = man.permissions.as_ref().and_then(|p| p.xdg_dirs.as_deref()).unwrap_or_default();
    if wanted.is_empty() {
        return (ok, why);
    }
    let Some(home) = dirs::home_dir() else { return (ok, vec!["no home folder".to_string()]) };
    let text = match dirs::config_dir().map(|c| c.join("user-dirs.dirs")).map(fs::read_to_string) {
        Some(Ok(t)) => Some(t),
        Some(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => None,
        Some(Err(e)) => return (ok, vec![format!("cannot read user-dirs.dirs: {e}")]),
        None => None,
    };
    for d in wanted {
        match panpak_core::resolve_xdg_dir(&d.name, &home, text.as_deref()).and_then(|rel| panpak_core::no_links(&home, &rel).map(|()| rel)) {
            Ok(rel) => ok.push(ResolvedDir { name: d.name.clone(), rel, access: d.access.clone() }),
            Err(e) => why.push(format!("{}: not granted: {}", d.name, panpak_core::err_text(&e))),
        }
    }
    (ok, why)
}

/// Print what was found: `music: ~/Music (rw)`.
fn print_xdg_dirs(found: &[ResolvedDir], problems: &[String]) {
    for d in found {
        println!("  {}: ~/{} ({})", d.name, d.rel, d.access);
    }
    for p in problems {
        println!("WARNING: {p}");
    }
}

/// Fix the folders found in the store, next to the pinned key: the stub grants only what is there and
/// still matches `user-dirs.dirs` when the app starts.
fn write_xdg_pin(appdir: &Path, found: &[ResolvedDir]) -> Result<()> {
    let path = appdir.join("xdg-dirs");
    if found.is_empty() {
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    let pairs: Vec<(String, String)> = found.iter().map(|d| (d.name.clone(), d.rel.clone())).collect();
    let tmp = appdir.join(format!(".xdg-dirs.{}", std::process::id()));
    fs::write(&tmp, panpak_core::format_pin(&pairs))?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Tell the desktop about the URL schemes of the entry (`update-desktop-database`, fixed path). A
/// failure only means that the desktop learns about the schemes later.
fn refresh_desktop_database(dir: &Path) {
    for tool in ["/usr/bin/update-desktop-database", "/bin/update-desktop-database"] {
        if Path::new(tool).is_file() {
            let ok = std::process::Command::new(tool).arg(dir).output().is_ok_and(|o| o.status.success());
            if !ok {
                println!("NOTE: update-desktop-database failed; the URL schemes may be known to the desktop only after a new login");
            }
            return;
        }
    }
}

/// A `.desktop` file written by panpak for `app_id`: it has the marker key, or
/// it is one written before the marker existed (panpak's own `Exec` line).
fn desktop_is_ours(content: &str, app_id: &str, launcher: &Path) -> bool {
    content.lines().any(|l| l.strip_prefix(DESKTOP_MARKER) == Some(app_id))
        || (content.starts_with("[Desktop Entry]\nType=Application\n")
            && content.lines().any(|l| l == format!("Exec={} %U", launcher.display())))
}

/// Refuse a destination that is a symbolic link, is not a regular file, or is a
/// file panpak did not write. A missing destination is fine.
fn check_replaceable(path: &Path, what: &str, is_ours: &dyn Fn(&str) -> bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            bail_code!(ErrorCode::E2001, "refusing to replace {what} {}: it is a symbolic link", path.display())
        }
        Ok(m) if !m.file_type().is_file() => {
            bail_code!(ErrorCode::E2001, "refusing to replace {what} {}: not a regular file", path.display())
        }
        Ok(m) => {
            let ours = m.len() <= 1 << 20
                && fs::read(path).ok().and_then(|b| String::from_utf8(b).ok()).is_some_and(|s| is_ours(&s));
            ensure_code!(
                ours,
                ErrorCode::E2001,
                "refusing to replace {what} {}: panpak did not create it (move or remove it, then retry)",
                path.display()
            );
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("inspect {}", path.display())),
    }
}

/// Write `content` to a temporary file in the same directory (created with
/// O_EXCL and O_NOFOLLOW), then rename it over `path`. The rename replaces a
/// link itself and never follows it. Refuses what `check_replaceable` refuses.
fn write_replacing(path: &Path, content: &str, mode: u32, what: &str, is_ours: &dyn Fn(&str) -> bool) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    check_replaceable(path, what, is_ours)?;
    let dir = path.parent().context("destination without a directory")?;
    let name = path.file_name().context("destination without a file name")?.to_string_lossy();
    let tmp = dir.join(format!(".{name}.panpak-tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let res = (|| -> Result<()> {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(mode).custom_flags(libc::O_NOFOLLOW).open(&tmp)?;
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res.with_context(|| format!("write {what} {}", path.display()))
}

/// shared-mime-info definition of `application/x-panpak`.
const MIME_XML: &str = include_str!("../../data/mime/panpak.xml");
/// `remove` deletes the installed file only while it still contains this text.
const MIME_MARKER: &str = "Installed by `panpak desktop-integration install`";
const MIME_FILE: &str = "panpak.xml";

/// Runs `update-mime-database` on `mime_dir`. Warns, without failing, if the
/// program is not installed.
fn update_mime_database(mime_dir: &Path) -> Result<()> {
    match std::process::Command::new("update-mime-database").arg(mime_dir).status() {
        Ok(st) if st.success() => Ok(()),
        Ok(st) => anyhow::bail!("update-mime-database failed ({st}) on {}", mime_dir.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("panpak: update-mime-database not found; run it on {} to refresh the MIME database (package shared-mime-info)", mime_dir.display());
            Ok(())
        }
        Err(e) => Err(e).context("run update-mime-database"),
    }
}

/// `panpak desktop-integration install|remove`: per-user MIME type definition
/// under `$XDG_DATA_HOME/mime/packages`. Never writes outside the user data dir.
fn cmd_desktop_integration(install: bool) -> Result<()> {
    let mime_dir = dirs::data_dir().context("no data dir")?.join("mime");
    let file = mime_dir.join("packages").join(MIME_FILE);
    if install {
        fs::create_dir_all(file.parent().unwrap())?;
        if fs::read_to_string(&file).ok().as_deref() == Some(MIME_XML) {
            println!("already installed: {}", file.display());
        } else {
            fs::write(&file, MIME_XML).with_context(|| format!("write {}", file.display()))?;
            println!("installed: {}", file.display());
        }
        update_mime_database(&mime_dir)?;
        return Ok(());
    }
    match fs::read_to_string(&file) {
        Ok(s) if s.contains(MIME_MARKER) => {
            fs::remove_file(&file).with_context(|| format!("remove {}", file.display()))?;
            println!("removed: {}", file.display());
            update_mime_database(&mime_dir)?;
        }
        Ok(_) => println!("left alone (not installed by panpak): {}", file.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("not installed: {}", file.display()),
        Err(e) => return Err(e).with_context(|| format!("read {}", file.display())),
    }
    Ok(())
}

/// `panpak install`: verify, TOFU+pin, copy package, launcher on the installed runtime.
fn tofu_confirmation(input: &str) -> bool {
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Ask a yes/no question on the terminal. Without a terminal the caller needs `--yes`.
fn confirm(question: &str, yes: bool, cancelled: ErrorCode, what: &str) -> Result<()> {
    if yes {
        return Ok(());
    }
    ensure_code!(
        std::io::IsTerminal::is_terminal(&std::io::stdin()),
        ErrorCode::E5004,
        "non-interactive stdin: pass --yes after checking the {what}"
    );
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut ans = String::new();
    std::io::stdin().read_line(&mut ans)?;
    ensure_code!(tofu_confirmation(&ans), cancelled, "{what} not accepted: cancelled");
    Ok(())
}

/// What the app gets as a session bus on this machine, in one line, for `install` and `update`.
fn bus_filter_state(view: bool, perms: Option<&panpak_core::Permissions>) -> String {
    use panpak_core::dbus::ProxyStatus::*;
    if panpak_core::allows_session_bus(perms) {
        return "the whole bus (the manifest lists its socket)".to_string();
    }
    if !view {
        return "none (no isolated view)".to_string();
    }
    let user_bus = std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|rt| Path::new(&rt).join("bus").exists());
    match panpak_core::dbus::proxy_status() {
        Usable { .. } if user_bus => "filtered (xdg-dbus-proxy): appearance and inhibit portals, and what the permissions above ask for".to_string(),
        Usable { .. } => "none (no systemd user bus at $XDG_RUNTIME_DIR/bus)".to_string(),
        NotFound => "none (xdg-dbus-proxy is not installed: no portals, notifications or tray icon)".to_string(),
        TooOld { version, .. } => format!("none (xdg-dbus-proxy {}.{}.{} is older than 0.1.5)", version.0, version.1, version.2),
        Broken { .. } => "none (xdg-dbus-proxy does not work; see `panpak doctor`)".to_string(),
    }
}

/// Print what the package may do: permissions and extra start arguments or variables.
fn print_permissions(man: &panpak_core::Manifest) {
    let lines = panpak_core::describe_permissions(man);
    if lines.is_empty() {
        println!("permissions: none beyond the private app folders");
    } else {
        println!("permissions:");
        for l in &lines {
            // Warnings stand out: no indent.
            if l.starts_with("WARNING:") {
                println!("{l}");
            } else {
                println!("  {l}");
            }
        }
    }
    let view = panpak_core::view_status();
    let iso = panpak_core::socket_isolation(panpak_core::detect_abi().ok(), man.permissions.as_ref(), view.usable());
    println!("isolation layers: {}", iso.layers());
    if let panpak_core::ViewStatus::Unusable(why) = &view {
        println!("NOTE: no isolated view: {}; to get it, {}", why.reason(), why.remedy());
    }
    if let Some(w) = iso.level.warning() {
        println!("{w}");
    }
    println!("session bus: {}", bus_filter_state(view.usable(), man.permissions.as_ref()));
    if let Some(l) = &man.launch {
        let args = l.args.as_deref().unwrap_or_default();
        if !args.is_empty() {
            println!("extra start arguments: {}", args.join(" "));
        }
        for (k, v) in l.env.iter().flatten() {
            println!("extra environment: {k}={v}");
        }
    }
}

/// New permissions are never accepted by `--yes` (that flag is for the key of a new app).
/// They need an answer at a terminal, or `--accept-new-permissions`.
fn accept_new_permissions(added: &[String], yes: bool, accept: bool, what: &str) -> Result<()> {
    if added.is_empty() {
        return Ok(());
    }
    println!("WARNING: this {what} asks for permissions the installed version did not have:");
    for a in added {
        println!("  {a}");
    }
    if accept {
        return Ok(());
    }
    let terminal = std::io::IsTerminal::is_terminal(&std::io::stdin());
    ensure_code!(
        terminal && !yes,
        ErrorCode::E5004,
        "new permissions need your confirmation: --yes does not accept them. Run the command at a terminal and answer the question, or pass --accept-new-permissions after reading the list above"
    );
    confirm("Accept them?", false, ErrorCode::E1003, "new permissions")
}

/// The manifest of the version installed now, if there is one.
fn installed_manifest(appdir: &Path) -> Option<panpak_core::Manifest> {
    let pkg = fs::read_link(appdir.join("current")).ok()?;
    let mut f = File::open(pkg).ok()?;
    let ft = panpak_core::read_footer(&mut f).ok()?;
    panpak_core::verify(&mut f, &ft).ok().map(|(m, _)| m)
}

/// `panpak data path|clear <app-id>`: the private folders of an app.
fn cmd_data(action: &str, app_id: &str, yes: bool) -> Result<()> {
    let dirs = panpak_core::app_dirs(app_id)?;
    match action {
        "path" => {
            println!("data:  {}", dirs.root.display());
            println!("cache: {}", dirs.cache.display());
            Ok(())
        }
        _ => {
            println!("This deletes the settings and data of {app_id}:");
            for root in dirs.roots() {
                println!("  {}", root.display());
            }
            confirm("Delete them?", yes, ErrorCode::E1003, "deletion")?;
            let removed = dirs.clear()?;
            println!("deleted {} folder(s)", removed.len());
            Ok(())
        }
    }
}

fn cmd_install(pkg: &Path, yes: bool, accept_new: bool, runtime_opt: Option<&Path>) -> Result<PathBuf> {
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
    // Check the launcher and desktop entry destinations before touching the store.
    let launcher = dirs::home_dir().context("no home dir")?.join(".local/bin").join(&man.app.id);
    let desktop = desktop_path(&man.app.id)?;
    check_replaceable(&launcher, "launcher", &|s| launcher_is_ours(s, &man.app.id))?;
    check_replaceable(&desktop, "desktop entry", &|s| desktop_is_ours(s, &man.app.id, &launcher))?;
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
    // The user folders the manifest asks for, found now in the real user-dirs.dirs.
    let (xdg_found, xdg_problems) = resolve_xdg_dirs(&man);
    let pin = appdir.join("pinned-key");
    if pin.exists() {
        let old = fs::read_to_string(&pin)?;
        ensure_code!(old.trim() == key.trim(), ErrorCode::E1004, "key DIFFERS from TOFU pin: possible attack or changed author; manual reinstall required");
        print_permissions(&man);
        print_xdg_dirs(&xdg_found, &xdg_problems);
        // Installing over an installed app is an update: new permissions need a yes.
        if let Some(old) = installed_manifest(&appdir) {
            accept_new_permissions(&panpak_core::added_permissions(&old, &man), yes, accept_new, "package")?;
        }
    } else {
        println!("app: {} {} ({})", man.app.id, man.app.name, man.app.version);
        print_permissions(&man);
        print_xdg_dirs(&xdg_found, &xdg_problems);
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
    write_xdg_pin(&appdir, &xdg_found)?;
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

    fs::create_dir_all(launcher.parent().unwrap())?;
    write_replacing(&launcher, &launcher_sh(&man.app.id, &runtime, &current)?, 0o755, "launcher", &|s| launcher_is_ours(s, &man.app.id))?;
    fs::create_dir_all(desktop.parent().unwrap())?;
    let schemes = manifest_schemes(&man);
    write_replacing(&desktop, &desktop_file(&man.app.id, &man.app.name, &launcher, &schemes, manifest_wm_class(&man))?, 0o644, "desktop entry", &|s| desktop_is_ours(s, &man.app.id, &launcher))?;
    // An entry written under the old name by an earlier version is replaced by this one.
    let moved = remove_legacy_desktop(&man.app.id, &launcher)?;
    if moved {
        println!("note: the desktop entry is now panpak.{}.desktop; the old {}.desktop was removed", man.app.id, man.app.id);
    }
    if !schemes.is_empty() || moved {
        refresh_desktop_database(desktop.parent().unwrap());
    }

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
    // Local sockets: D-Bus, ssh-agent, gpg-agent (see THREAT-MODEL.md). Two layers that work apart:
    // the isolated view (bubblewrap) and Landlock V9.
    let view = panpak_core::view_status();
    match &view {
        panpak_core::ViewStatus::Usable => {
            let v = panpak_core::bwrap_version().unwrap_or_else(|| "version unknown".to_string());
            println!("OK   isolated view: {v} at {} works (private /tmp and /run, only allowed sockets)", panpak_core::BWRAP);
        }
        panpak_core::ViewStatus::Unusable(why) => {
            println!("NOTE isolated view: not available, {}", why.reason());
            println!("     to get it: {}", why.remedy());
        }
    }
    let iso = panpak_core::socket_isolation(panpak_core::detect_abi().ok(), None, view.usable());
    println!("{} local sockets: active layers: {}", if iso.level == panpak_core::SocketIsolation::Isolated { "OK  " } else { "NOTE" }, iso.layers());
    if !iso.landlock_paths {
        println!("     Landlock V9 (Linux 7.1) would also deny path sockets that the manifest does not list");
    }
    if let Some(w) = iso.level.warning() {
        println!("     {w}");
    }
    // The filtered session bus (portals, notifications): needs the view, a systemd user manager and xdg-dbus-proxy.
    {
        use panpak_core::dbus::ProxyStatus::*;
        match panpak_core::dbus::proxy_status() {
            Usable { path, version } if view.usable() => {
                let user_bus = std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|rt| Path::new(&rt).join("bus").exists());
                if user_bus {
                    println!("OK   D-Bus filter: xdg-dbus-proxy {}.{}.{} at {}; apps get portals (settings, inhibit) and the notifications and tray they ask for", version.0, version.1, version.2, path.display());
                } else {
                    println!("NOTE D-Bus filter: no user bus ($XDG_RUNTIME_DIR/bus), so no systemd user manager to make the scope; apps get no session bus");
                }
            }
            Usable { .. } => println!("NOTE D-Bus filter: xdg-dbus-proxy is there but the isolated view is not available; apps get no filtered session bus"),
            NotFound => println!("NOTE D-Bus filter: xdg-dbus-proxy not found ({}); apps get no session bus (no portals, notifications or tray)", panpak_core::dbus::PROXY_PATHS.join(", ")),
            TooOld { path, version } => println!("NOTE D-Bus filter: {} is {}.{}.{}, {}.{}.{} or newer is needed; apps get no session bus", path.display(), version.0, version.1, version.2, panpak_core::dbus::MIN_PROXY.0, panpak_core::dbus::MIN_PROXY.1, panpak_core::dbus::MIN_PROXY.2),
            Broken { path, why } => println!("NOTE D-Bus filter: {} does not work ({why}); apps get no session bus", path.display()),
        }
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
    // The paths the sandbox opens for its rules: one that exists but cannot be opened is left out of the
    // rules by the stub (with a warning at each start) and stays denied.
    {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute());
        let wayland = std::env::var("WAYLAND_DISPLAY").ok();
        let mut paths: Vec<PathBuf> = panpak_core::SYSTEM_RULE_PATHS.iter().map(PathBuf::from).collect();
        paths.extend(panpak_core::implied_sockets(None, runtime.as_deref(), wayland.as_deref()));
        let bad = panpak_core::unopenable_rule_paths(paths.iter().map(PathBuf::as_path));
        if bad.is_empty() {
            line(true, "sandbox rule paths: all that exist can be opened");
        } else {
            for (path, e) in bad {
                line(false, &format!("sandbox rule path {} cannot be opened ({e}); the stub leaves its rule out", path.display()));
            }
        }
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
                if let Ok(locks) = panpak_core::stalelocks::find_for_app(&man.app.id) {
                    if !locks.is_empty() {
                        println!("NOTE {} stale lock file(s) of {} (left by a copy that was killed; the app may refuse to start): run `panpak data unlock {}`", locks.len(), man.app.id, man.app.id);
                    }
                }
                let need: Vec<String> = man.abi.as_ref().and_then(|a| a.needs.clone()).unwrap_or_default();
                let prefer: Vec<String> = man.abi.as_ref().and_then(|a| a.prefer_host.clone()).unwrap_or_default();
                let allowed: Vec<String> = man.abi.as_ref().and_then(|a| a.allow_missing.clone()).unwrap_or_default();
                let tree = store.join("trees").join(panpak_core::hex(&ft.root));
                let bundled = panpak_core::abi::bundled_libs(&tree);
                let host = panpak_core::abi::host_libs();
                // The same resolution as the stub does: the rules, then the symbol check. Without the
                // tree (the app is not installed yet) only the rules can be applied.
                let installed = tree.join(&man.app.entry).exists();
                let resolved = if installed {
                    panpak_core::abi::resolve_consistent(&mut panpak_core::abi::InfoCache::default(), &tree, &man.app.entry, &need, &prefer, &allowed, &host, &bundled).map(Some)
                } else {
                    Ok(None)
                };
                let chosen = match resolved {
                    Ok(Some(r)) => Ok((r.decisions.clone(), Some(r))),
                    Ok(None) => panpak_core::abi::decide(&need, &prefer, &allowed, &host, &bundled).map(|d| (d, None)),
                    Err(e) => Err(e),
                };
                match chosen {
                    Ok((ds, resolution)) => {
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
                        // The symbol check of the resolution: what the app and the chosen libraries need.
                        if let Some(r) = &resolution {
                            if !r.changed.is_empty() {
                                println!("NOTE libraries taken from their other copy so that every symbol is defined: {}", r.changed.join(", "));
                            }
                            let d = &r.diagnosis;
                            if d.missing.is_empty() {
                                line(true, &format!("symbols: every symbol that the app and the chosen libraries need is defined ({} files read)", d.modules));
                            } else {
                                line(false, &format!("symbols: {} needed by the app or the chosen libraries are defined by none of them ({} files read); the loader will fail if they are used", d.missing.len(), d.modules));
                                // Grouped by file: the first few files, with a few of their symbols.
                                let mut by_file: Vec<(&str, Vec<&str>)> = Vec::new();
                                for (module, sym) in &d.missing {
                                    match by_file.last_mut() {
                                        Some((m, v)) if *m == module.as_str() => v.push(sym.as_str()),
                                        _ => by_file.push((module.as_str(), vec![sym.as_str()])),
                                    }
                                }
                                for (module, syms) in by_file.iter().take(8) {
                                    let shown: Vec<&str> = syms.iter().take(3).copied().collect();
                                    println!("       {module}: {} symbols, for example {}", syms.len(), shown.join(", "));
                                }
                                if by_file.len() > 8 {
                                    println!("       ... and {} more files", by_file.len() - 8);
                                }
                            }
                        } else {
                            println!("NOTE symbols: not checked (the app is not installed yet, so its files are not on disk)");
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
            Err(e) => line(false, &format!("INVALID package: {}", panpak_core::render(&e, generic_code(Some("doctor"))))),
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
        Some("pack" | "convert" | "dump-index" | "dump-manifest" | "dbus-rules") => ErrorCode::E0000,
        Some("keygen" | "release" | "install-runtime") => ErrorCode::E1000,
        Some("install" | "rollback" | "open" | "doctor" | "desktop-integration" | "data") => ErrorCode::E2000,
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
            // The file may come before or after the flags.
            let mut src: Option<&String> = None;
            let mut key = None;
            let mut allow_unsigned = false;
            let mut i = 2;
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
                    x if x.starts_with("--") => bail_code!(ErrorCode::E5001, "unknown flag {x}\n{usage}", usage = usage()),
                    _ if src.is_none() => {
                        src = Some(&args[i]);
                        i += 1;
                    }
                    _ => bail_code!(ErrorCode::E5002, "install-runtime takes one file"),
                }
            }
            let src = src.ok_or_else(|| coded(ErrorCode::E5002, "install-runtime <file> (a stub or a release .tar.gz, not a folder)"))?;
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
        Some("desktop-integration") => match (args.get(2).map(|s| s.as_str()), args.len()) {
            (Some("install"), 3) => cmd_desktop_integration(true),
            (Some("remove"), 3) => cmd_desktop_integration(false),
            (Some("install" | "remove"), _) => bail_code!(ErrorCode::E5001, "unexpected argument\n{usage}", usage = usage()),
            (Some(x), _) => bail_code!(ErrorCode::E5001, "unknown action {x}\n{usage}", usage = usage()),
            (None, _) => bail_code!(ErrorCode::E5002, "desktop-integration install|remove"),
        },
        Some("doctor") => {
            let pkg = args.get(2).map(PathBuf::from);
            let rc = cmd_doctor(pkg.as_deref())?;
            if !rc {
                std::process::exit(1);
            }
            Ok(())
        }
        Some("data") => {
            let mut yes = false;
            let mut words: Vec<&str> = Vec::new();
            for a in &args[2..] {
                match a.as_str() {
                    "--yes" => yes = true,
                    x if x.starts_with('-') => bail_code!(ErrorCode::E5001, "unknown flag {x}\n{usage}", usage = usage()),
                    x => words.push(x),
                }
            }
            match words.as_slice() {
                [action @ ("path" | "clear"), id] => {
                    ensure_code!(yes <= (*action == "clear"), ErrorCode::E5001, "--yes only goes with `data clear`");
                    cmd_data(action, id, yes)
                }
                ["unlock", id] => {
                    ensure_code!(!yes, ErrorCode::E5001, "--yes only goes with `data clear`");
                    let gone = panpak_core::stalelocks::unlock_app(id).map_err(|e| panpak_core::coded(ErrorCode::E2000, format!("{e:#}")))?;
                    for g in &gone {
                        println!("removed {}", g.display());
                    }
                    println!("{} stale lock file(s) removed", gone.len());
                    Ok(())
                }
                [] | ["path" | "clear" | "unlock"] => bail_code!(ErrorCode::E5002, "data path <app-id> | data clear [--yes] <app-id> | data unlock <app-id>"),
                _ => bail_code!(ErrorCode::E5001, "unexpected argument\n{usage}", usage = usage()),
            }
        }
        Some("update") => {
            let mut yes = false;
            let mut accept_new = false;
            let mut allow = false;
            let mut insecure = false;
            let mut timeout = 30u64;
            let mut id = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--yes" => {
                        yes = true;
                        i += 1;
                    }
                    "--accept-new-permissions" => {
                        accept_new = true;
                        i += 1;
                    }
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
            cmd_update(id.ok_or_else(|| coded(ErrorCode::E5002, "update <app-id>"))?, yes, accept_new, allow, insecure, timeout)?;
            Ok(())
        }
        Some("dbus-rules") => {
            // The arguments the stub gives xdg-dbus-proxy for this package, one per line. For tests and review.
            let pkg = args.get(2).ok_or_else(|| coded(ErrorCode::E5002, "dbus-rules <package>"))?;
            let mut f = File::open(pkg)?;
            let ft = panpak_core::read_footer(&mut f)?;
            let (man, _) = panpak_core::verify(&mut f, &ft)?;
            for a in panpak_core::dbus::proxy_args(&panpak_core::dbus::BusNeeds { app_id: &man.app.id, app_name: &man.app.name, entry: &man.app.entry, perms: man.permissions.as_ref() }) {
                println!("{a}");
            }
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
            let mut accept_new = false;
            let mut runtime = None;
            let mut pkg = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--yes" => {
                        yes = true;
                        i += 1;
                    }
                    "--accept-new-permissions" => {
                        accept_new = true;
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
                accept_new,
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
        // open: an invalid id has its own code, a missing app is an untyped store error.
        let c = code(cmd_open("non-un-id", Some("/tmp/x"), false, false).map(|_| ()), ErrorCode::E2000);
        assert_eq!(c, ErrorCode::E0006);
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
    fn pack_refuses_a_manifest_that_install_would_refuse() {
        // The FreeCAD case: a library name found in a file of the tree is not a valid name. It is added to
        // `abi.needs` after the first check of the manifest, so only the check before signing sees it.
        let dir = std::env::temp_dir().join(format!("panpak-badneed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let tree = dir.join("tree");
        fs::create_dir_all(tree.join("bin")).unwrap();
        let mut elf = tiny_elf();
        elf[241..250].copy_from_slice(b"lib c.so6"); // a space is not allowed in a library name
        fs::write(tree.join("bin/app"), &elf).unwrap();
        fs::set_permissions(tree.join("bin/app"), fs::Permissions::from_mode(0o755)).unwrap();
        let secret = "29".repeat(32);
        let sk = panpak_core::parse_secret(&secret).unwrap();
        let key = format!("ed25519:{}", panpak_core::hex(sk.verifying_key().as_bytes()));
        fs::write(
            tree.join("panpak.toml"),
            format!("[app]\nid=\"org.example.bad\"\nname=\"B\"\nversion=\"1\"\nentry=\"bin/app\"\n[update]\nkey=\"{key}\"\n"),
        )
        .unwrap();
        let stub = dir.join("stub");
        fs::write(&stub, b"FAKE-STUB").unwrap();
        let out = dir.join("a.panpak");
        let e = cmd_pack(&tree, &stub, &secret, None, Some(&out)).unwrap_err();
        let text = panpak_core::render(&e, ErrorCode::E0000);
        assert!(text.contains("invalid") && text.contains("abi.needs") && text.contains("nothing was written"), "{text}");
        assert!(!out.exists(), "a file was written for an invalid manifest");
        // The same tree with a valid name is packed.
        elf[241..250].copy_from_slice(b"libc.so.6");
        fs::write(tree.join("bin/app"), &elf).unwrap();
        cmd_pack(&tree, &stub, &secret, None, Some(&out)).unwrap();
        assert!(out.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generic_code_per_subcommand() {
        assert_eq!(generic_code(Some("pack")), ErrorCode::E0000);
        assert_eq!(generic_code(Some("release")), ErrorCode::E1000);
        assert_eq!(generic_code(Some("install")), ErrorCode::E2000);
        assert_eq!(generic_code(Some("desktop-integration")), ErrorCode::E2000);
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
        let sh = launcher_sh("org.x", rt, cur).unwrap();
        assert!(sh.starts_with("#!/bin/sh\n"));
        assert!(sh.contains(" run "));
        assert!(sh.contains("--panpak-open"));
        assert!(sh.contains("[ -f \"$1\" ]"));
        assert!(sh.contains("apps/org.x/current"));
        assert!(sh.contains("\n# X-Panpak-App-Id: org.x\n"));
        assert!(sh.ends_with("\"$@\"\n"));
        let d = desktop_file("org.x", "X App", Path::new("/home/u/.local/bin/org.x"), &[], None).unwrap();
        assert!(d.contains("Name=X App\n"));
        assert!(d.contains("Exec=\"/home/u/.local/bin/org.x\" %U\n"));
        assert!(d.contains("\nX-Panpak-App-Id=org.x\n"));
        assert!(!d.contains("MimeType"), "an app must not claim the package type: {d}");
    }
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-unit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn desktop_entry_values_cannot_start_a_line() {
        let l = Path::new("/home/u/.local/bin/org.x");
        let d = desktop_file("org.x", "Evil\nExec=/bin/sh\r\nTerminal=true", l, &[], None).unwrap();
        assert_eq!(d.lines().count(), 7, "{d}");
        assert_eq!(d.lines().filter(|l| l.starts_with("Exec=")).count(), 1, "{d}");
        assert!(d.contains("Name=Evil\\nExec=/bin/sh\\r\\nTerminal=true\n"), "{d}");
        // Backslash is escaped, other controls are refused.
        assert!(desktop_file("org.x", "A\\B", l, &[], None).unwrap().contains("Name=A\\\\B\n"));
        for bad in ["a\u{1}b", "a\u{7f}b", "a\u{9b}b"] {
            assert!(desktop_file("org.x", bad, l, &[], None).is_err(), "{bad:?}");
        }
        assert!(desktop_file("org.x", "N", Path::new("/home/u\n/x"), &[], None).is_err());
    }

    #[test]
    fn desktop_exec_quotes_reserved_characters() {
        let q = |p: &str| desktop_exec_arg(p).unwrap();
        assert_eq!(q("/home/a b/x"), "\"/home/a b/x\"");
        assert_eq!(q("/h/100%/x"), "\"/h/100%%/x\"");
        // `"`, backtick, `$` and backslash get a backslash, which is doubled by string escaping.
        assert_eq!(q("/h/a\"b"), "\"/h/a\\\\\"b\"");
        assert_eq!(q("/h/$x"), "\"/h/\\\\$x\"");
        assert_eq!(q("/h/`x`"), "\"/h/\\\\`x\\\\`\"");
        assert_eq!(q("/h/a\\b"), "\"/h/a\\\\\\\\b\"");
    }

    #[test]
    fn launcher_quotes_paths_for_the_shell() {
        let sh = launcher_sh("org.x", Path::new("/h/it's $(id)/rt"), Path::new("/h/`x`/cur")).unwrap();
        assert!(sh.contains("exec '/h/it'\\''s $(id)/rt' run '/h/`x`/cur' \"$@\""), "{sh}");
        assert!(launcher_sh("org.x", Path::new("/h/a\nb"), Path::new("/c")).is_err());
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn ownership_markers() {
        let l = Path::new("/home/u/.local/bin/org.x");
        let sh = launcher_sh("org.x", Path::new("/rt"), Path::new("/cur")).unwrap();
        assert!(launcher_is_ours(&sh, "org.x"));
        assert!(!launcher_is_ours(&sh, "org.y"));
        assert!(!launcher_is_ours("#!/bin/sh\necho mine\n", "org.x"));
        // Written before the marker existed.
        assert!(launcher_is_ours("#!/bin/sh\n# Generated by `panpak install`: do not edit.\n# Points to apps/<id>/current; x\n", "org.x"));
        let d = desktop_file("org.x", "X", l, &[], None).unwrap();
        assert!(desktop_is_ours(&d, "org.x", l));
        assert!(!desktop_is_ours(&d, "org.y", l));
        assert!(!desktop_is_ours("[Desktop Entry]\nType=Application\nName=Mine\nExec=/bin/mine\n", "org.x", l));
        let old = "[Desktop Entry]\nType=Application\nName=X\nExec=/home/u/.local/bin/org.x %U\nTerminal=false\nCategories=Utility;\nMimeType=application/x-panpak;\n";
        assert!(desktop_is_ours(old, "org.x", l));
    }

    #[test]
    fn a_runtime_in_use_is_replaced_by_a_rename() {
        let d = scratch("atomic");
        let dest = d.join("panpak-stub");
        fs::copy("/bin/sleep", &dest).unwrap();
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
        let mut running = std::process::Command::new(&dest).arg("30").spawn().unwrap();
        // Writing into the file in use fails; the rename does not.
        assert!(fs::OpenOptions::new().write(true).open(&dest).is_err(), "the test needs a busy file");
        let new = d.join("new");
        fs::write(&new, "#!/bin/sh\necho 2\n").unwrap();
        install_file_atomically(&new, &dest, 0o755).unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "#!/bin/sh\necho 2\n");
        assert_eq!(fs::metadata(&dest).unwrap().permissions().mode() & 0o777, 0o755);
        assert!(running.try_wait().unwrap().is_none(), "the running copy goes on");
        assert_eq!(fs::read_dir(&d).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains("panpak-tmp")).count(), 0);
        running.kill().unwrap();
        running.wait().unwrap();
    }

    #[test]
    fn write_replacing_refuses_links_and_foreign_files() {
        let d = scratch("write");
        let ours = |s: &str| s.contains("ours");
        let f = d.join("f");
        // New file, then an update of a file we wrote.
        write_replacing(&f, "ours 1\n", 0o755, "launcher", &ours).unwrap();
        write_replacing(&f, "ours 2\n", 0o755, "launcher", &ours).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "ours 2\n");
        assert_eq!(fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o755);
        // A file we did not write.
        fs::write(&f, "mine\n").unwrap();
        let e = write_replacing(&f, "ours 3\n", 0o755, "launcher", &ours).unwrap_err();
        assert_eq!(panpak_core::code_of(&e, ErrorCode::E0000), ErrorCode::E2001);
        assert_eq!(fs::read_to_string(&f).unwrap(), "mine\n");
        // A link, to a file or to nothing: refused, the target is untouched.
        let target = d.join("target");
        fs::write(&target, "ours target\n").unwrap();
        let l = d.join("l");
        std::os::unix::fs::symlink(&target, &l).unwrap();
        let e = write_replacing(&l, "new\n", 0o755, "launcher", &ours).unwrap_err();
        assert_eq!(panpak_core::code_of(&e, ErrorCode::E0000), ErrorCode::E2001);
        assert_eq!(fs::read_to_string(&target).unwrap(), "ours target\n");
        assert!(fs::symlink_metadata(&l).unwrap().file_type().is_symlink());
        let dangling = d.join("dangling");
        std::os::unix::fs::symlink(d.join("nowhere"), &dangling).unwrap();
        assert!(write_replacing(&dangling, "new\n", 0o755, "launcher", &ours).is_err());
        assert!(!d.join("nowhere").exists());
        // A directory.
        fs::create_dir(d.join("dir")).unwrap();
        assert!(write_replacing(&d.join("dir"), "new\n", 0o755, "launcher", &ours).is_err());
        // No temporary file is left behind.
        assert!(fs::read_dir(&d).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains("panpak-tmp")));
        let _ = fs::remove_dir_all(&d);
    }
    #[test]
    fn convert_strings_cannot_add_keys_or_sections() {
        let evil = "x\"\n[permissions]\nnetwork = true\n#";
        let doc = format!("[app]\nname = {}\nversion = {}\n", toml_str(evil), toml_str("1\\\"2"));
        let v: toml::Value = toml::from_str(&doc).unwrap();
        assert_eq!(v["app"]["name"].as_str(), Some(evil));
        assert_eq!(v["app"]["version"].as_str(), Some("1\\\"2"));
        assert!(v.get("permissions").is_none());
    }
}
