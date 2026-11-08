// SPDX-License-Identifier: MIT OR Apache-2.0
//! The build image: a Debian root file system made without root. The base is the image of the registry at the digest of `panpak.lock`
//! (the manifest digest and every layer digest are checked); the packages come from the dated snapshot of the lock (the main archive and
//! the security archive of that day), so that two builds a year apart use the same packages. The result is kept in the cache, one folder
//! per (digest, snapshot, packages).
use super::{lock::Lock, sandbox};
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// An image name that the tool knows, and what it takes to make it.
#[derive(Debug)]
pub struct Known {
    pub name: &'static str,
    /// The repository on Docker Hub.
    pub repo: &'static str,
    /// The Debian suite.
    pub suite: &'static str,
    /// What a new lock gets today.
    pub manifest_digest: &'static str,
    pub snapshot: &'static str,
    /// Patches to source packages of the image, applied by rebuilding those packages once per image (see `patched_debs`).
    pub patches: &'static [Patch],
}

/// A patch to a Debian source package.
#[derive(Debug)]
pub struct Patch {
    pub package: &'static str,
    pub name: &'static str,
    pub text: &'static str,
}

impl Patch {
    /// How the lock names it: the package, the file name and the hash of the text.
    pub fn entry(&self) -> String {
        format!("{} {} sha256:{}", self.package, self.name, hex(&Sha256::digest(self.text.as_bytes())))
    }
}

pub const KNOWN: &[Known] = &[Known {
    name: "debian-13",
    repo: "library/debian",
    suite: "trixie",
    // debian:trixie-slim for linux/amd64, 2026-11-05.
    manifest_digest: "sha256:7378260c0a098d27ac206225db5e7d92b8c7c84ea7afccc86eeb513756bbd392",
    snapshot: "20261107T000000Z",
    // GTK 4.18 crashes at the start on a compositor that offers zwp_linux_dmabuf_v1 version 6 (KWin 6.8): defect 54, docs/PRIVATE-BUS.md.
    patches: &[Patch { package: "gtk4", name: "gtk4-dmabuf-v5.patch", text: include_str!("../../data/patches/gtk4-dmabuf-v5.patch") }],
}];

/// The tools every build image has (the compilers and the things a source package asks for first).
pub const BASE_PACKAGES: &[&str] = &["ca-certificates", "curl", "git", "build-essential", "pkg-config", "xz-utils", "python3", "gettext", "desktop-file-utils", "file", "binutils", "librsvg2-bin", "libglib2.0-dev-bin", "gtk-update-icon-cache"];

pub fn known(name: &str) -> Result<&'static Known> {
    KNOWN.iter().find(|k| k.name == name).with_context(|| format!("unknown build image {name:?}; this panpak knows: {}", KNOWN.iter().map(|k| k.name).collect::<Vec<_>>().join(", ")))
}

fn http() -> ureq::Agent {
    // The addresses of a host are tried the way Happy Eyeballs does it (see `happy`): an unreachable IPv6 address does not stall a download.
    crate::happy::agent(900)
}

/// Download `url` into `dest`, checking the SHA-256 on the way. `headers` are added to the request.
pub fn download(url: &str, headers: &[(&str, String)], sha256: Option<&str>, dest: &Path, max: u64) -> Result<String> {
    let mut req = http().get(url);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let resp = req.call().map_err(|e| anyhow::anyhow!("GET {url} failed ({e}): check the connection, then retry"))?;
    let mut body = resp.into_body().into_reader().take(max + 1);
    let tmp = dest.with_extension("part");
    let mut f = std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    let mut h = Sha256::new();
    let (mut buf, mut n) = (vec![0u8; 1 << 16], 0u64);
    loop {
        let k = body.read(&mut buf).context("read the download")?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        f.write_all(&buf[..k])?;
        n += k as u64;
    }
    drop(f);
    let got = hex(&h.finalize());
    ensure!(n <= max, "{url} is larger than {max} bytes: refused");
    if let Some(want) = sha256 {
        if got != want {
            let _ = std::fs::remove_file(&tmp);
            bail!("{url}: SHA-256 is {got}, the recipe or the lock says {want}: refused");
        }
    }
    std::fs::rename(&tmp, dest)?;
    Ok(got)
}

/// The string values of a key in a JSON text (`"key":"value"`), in order. The files read this way are small, and their bytes are checked
/// against a digest before they are read (the manifest) or come from the registry over HTTPS with a token for one repository.
fn json_strings(text: &str, key: &str) -> Vec<String> {
    let pat = format!("\"{key}\"");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(&pat) {
        rest = &rest[i + pat.len()..];
        let t = rest.trim_start();
        if let Some(t) = t.strip_prefix(':') {
            if let Some(t) = t.trim_start().strip_prefix('"') {
                if let Some(end) = t.find('"') {
                    out.push(t[..end].to_string());
                    rest = &t[end..];
                }
            }
        }
    }
    out
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Unpack the base image into `root`: manifest and layers checked against their digests. Device nodes are skipped (the sandbox has its own
/// `/dev`), owners are not set (one user id), and every folder stays writable by us.
fn pull_base(lock: &Lock, k: &Known, root: &Path, scratch: &Path) -> Result<()> {
    let repo = k.repo;
    let mut tok = Vec::new();
    http().get(&format!("https://auth.docker.io/token?service=registry.docker.io&scope=repository:{repo}:pull")).call().map_err(|e| anyhow::anyhow!("registry token: {e}"))?.into_body().into_reader().take(1 << 20).read_to_end(&mut tok)?;
    let token = json_strings(&String::from_utf8_lossy(&tok), "token").into_iter().next().context("registry token")?;
    let auth = format!("Bearer {token}");
    let mpath = scratch.join("manifest.json");
    let digest = &lock.image.manifest_digest;
    let want = digest.strip_prefix("sha256:").unwrap();
    download(&format!("https://registry-1.docker.io/v2/{repo}/manifests/{digest}"), &[("Authorization", auth.clone()), ("Accept", "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json".into())], Some(want), &mpath, 1 << 20)?;
    // The manifest bytes are checked against the digest above; the layers are the digests after `"layers"`.
    let text = String::from_utf8(std::fs::read(&mpath)?).context("image manifest")?;
    let from = text.find("\"layers\"").context("the image manifest has no layers")?;
    let layers = json_strings(&text[from..], "digest");
    ensure!(!layers.is_empty() && layers.len() <= 16, "unexpected number of layers: {}", layers.len());
    std::fs::create_dir_all(root)?;
    for d in &layers {
        ensure!(d.len() == 71 && d.starts_with("sha256:") && d[7..].bytes().all(|b| b.is_ascii_hexdigit()), "layer digest {d:?} is not sha256:<hex>");
        let lp = scratch.join("layer.tar.gz");
        download(&format!("https://registry-1.docker.io/v2/{repo}/blobs/{d}"), &[("Authorization", auth.clone())], d.strip_prefix("sha256:"), &lp, 1 << 30)?;
        unpack_layer(&lp, root)?;
        let _ = std::fs::remove_file(&lp);
    }
    Ok(())
}

/// Unpack a gzip tar layer: regular files, folders, links; nothing else.
fn unpack_layer(layer: &Path, root: &Path) -> Result<()> {
    std::fs::create_dir_all(root)?;
    let f = std::fs::File::open(layer)?;
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
    ar.set_preserve_permissions(true);
    ar.set_preserve_ownerships(false);
    ar.set_unpack_xattrs(false);
    for e in ar.entries()? {
        let mut e = e?;
        let t = e.header().entry_type();
        if !(t.is_file() || t.is_dir() || t.is_symlink() || t.is_hard_link()) {
            continue; // device nodes, fifos
        }
        let path = e.path()?.into_owned();
        ensure!(!path.is_absolute() && !path.components().any(|c| matches!(c, std::path::Component::ParentDir)), "a layer entry leaves the image: {}", path.display());
        // `unpack_in` refuses what would land outside `root`, links included.
        e.unpack_in(root).with_context(|| format!("unpack {}", path.display()))?;
    }
    make_owner_writable(root);
    Ok(())
}

/// Folders and files of the image that the owner could not write (the owner is us): make them so, so that `apt` can replace them.
fn make_owner_writable(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.path().symlink_metadata() else { continue };
            if m.is_dir() {
                let mode = m.permissions().mode();
                if mode & 0o700 != 0o700 {
                    let _ = std::fs::set_permissions(e.path(), std::fs::Permissions::from_mode(mode | 0o700));
                }
                stack.push(e.path());
            } else if m.is_file() && m.permissions().mode() & 0o200 == 0 {
                let _ = std::fs::set_permissions(e.path(), std::fs::Permissions::from_mode(m.permissions().mode() | 0o200));
            }
        }
    }
}

/// The key of an image in the cache: the lock and the packages (the base tools and the recipe's).
pub fn image_key(lock: &Lock, packages: &[String]) -> String {
    let mut p: Vec<&str> = BASE_PACKAGES.iter().copied().chain(packages.iter().map(String::as_str)).collect();
    p.sort();
    p.dedup();
    let mut h = Sha256::new();
    h.update(format!("panpak-image-v1\n{}\n{}\n{}\n{}\n", lock.image.name, lock.image.manifest_digest, lock.image.snapshot, p.join(" ")));
    // Only an image with patches has this line, so that the key of an image without them does not change.
    if !lock.image.patches.is_empty() {
        h.update(format!("patches {}\n", lock.image.patches.join(";")));
    }
    hex(&h.finalize())[..20].to_string()
}

/// The root file system for a recipe, made if it is not in the cache yet. Returns its folder.
pub fn ensure(cache: &Path, lock: &Lock, packages: &[String]) -> Result<PathBuf> {
    let k = known(&lock.image.name)?;
    let key = image_key(lock, packages);
    let dir = cache.join("images").join(&key);
    let root = dir.join("rootfs");
    if dir.join(".complete").exists() {
        return Ok(root);
    }
    super::say(&format!("making the build image {} (digest {}…, snapshot {}): the first time takes a few minutes", lock.image.name, &lock.image.manifest_digest[7..19], lock.image.snapshot));
    let _ = chmod_all(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let scratch = dir.join("scratch");
    std::fs::create_dir_all(&scratch)?;
    pull_base(lock, k, &root, &scratch)?;
    let shim = sandbox::shim_in(cache)?;
    // The package sources: the main archive and the security archive of the same day. Plain http: the archive is signed (apt checks it with
    // the keyring of the base image) and the image has no certificates yet.
    let etc = root.join("etc/apt");
    let _ = std::fs::remove_dir_all(etc.join("sources.list.d"));
    std::fs::create_dir_all(etc.join("sources.list.d"))?;
    write_sources(&etc, lock, k, false)?;
    std::fs::create_dir_all(etc.join("apt.conf.d"))?;
    std::fs::write(etc.join("apt.conf.d/99panpak"), "APT::Sandbox::User \"root\";\nAcquire::Check-Valid-Until \"false\";\nAPT::Install-Recommends \"false\";\nAcquire::Retries \"3\";\nDPkg::Options { \"--force-unsafe-io\"; };\n")?;
    std::fs::create_dir_all(root.join("opt"))?;
    let mut pk: Vec<String> = BASE_PACKAGES.iter().map(|s| s.to_string()).collect();
    for p in packages {
        if !pk.contains(p) {
            pk.push(p.clone());
        }
    }
    let log = scratch.join("apt.log");
    let mut update = sandbox::Run::new(&root, &shim, &["apt-get", "update", "-q"]);
    update.net = true;
    update.run_logged(&log, "apt-get update")?;
    let mut argv: Vec<&str> = vec!["apt-get", "install", "-y", "-q"];
    argv.extend(pk.iter().map(String::as_str));
    let mut install = sandbox::Run::new(&root, &shim, &argv);
    install.net = true;
    install.run_logged(&log, "apt-get install")?;
    // Record what is in the image (name and version), so that a build can say which version of an image dependency it found.
    let mut tsv = installed(&root, &shim)?;
    // Patched packages: the ones the image has are replaced by the rebuilt ones (built once for all images of this lock).
    if !lock.image.patches.is_empty() {
        let debs = patched_debs(cache, lock, k)?;
        let have: Vec<&str> = tsv.lines().filter_map(|l| l.split('\t').next()).collect();
        let mut names: Vec<String> = Vec::new();
        for e in std::fs::read_dir(&debs)? {
            let f = e?.file_name().to_string_lossy().into_owned();
            if f.ends_with(".deb") && f.split('_').next().is_some_and(|n| have.contains(&n)) {
                names.push(f);
            }
        }
        names.sort();
        if !names.is_empty() {
            super::say(&format!("installing the patched packages: {}", names.join(" ")));
            let paths: Vec<String> = names.iter().map(|n| format!("/opt/patched/{n}")).collect();
            let mut argv: Vec<&str> = vec!["apt-get", "install", "-y", "-q", "--allow-downgrades"];
            argv.extend(paths.iter().map(String::as_str));
            let mut inst = sandbox::Run::new(&root, &shim, &argv).bind(&debs, "/opt/patched", false);
            inst.net = true;
            inst.run_logged(&log, "apt-get install (patched packages)")?;
            tsv = installed(&root, &shim)?;
        }
    }
    std::fs::write(dir.join("packages.tsv"), &tsv)?;
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::write(dir.join(".complete"), format!("{}\n", lock.image.manifest_digest))?;
    Ok(root)
}

/// `name<TAB>version` for every installed package of the image.
fn installed(root: &Path, shim: &Path) -> Result<String> {
    let mut dpkg = sandbox::Run::new(root, shim, &["dpkg-query", "-W", "-f", "${Package}\\t${Version}\\n"]);
    dpkg.net = false;
    let out = dpkg.output()?;
    ensure!(out.status.success(), "dpkg-query failed in the image");
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The package sources: the main archive and the security archive of the snapshot's day, and with `with_src` the source packages too.
fn write_sources(etc: &Path, lock: &Lock, k: &Known, with_src: bool) -> Result<()> {
    let (s, suite) = (&lock.image.snapshot, k.suite);
    let mut t = format!("deb http://snapshot.debian.org/archive/debian/{s}/ {suite} main\ndeb http://snapshot.debian.org/archive/debian-security/{s}/ {suite}-security main\n");
    if with_src {
        t.push_str(&format!("deb-src http://snapshot.debian.org/archive/debian/{s}/ {suite} main\ndeb-src http://snapshot.debian.org/archive/debian-security/{s}/ {suite}-security main\n"));
    }
    std::fs::write(etc.join("sources.list"), t)?;
    Ok(())
}

/// Seconds since 1970 of a snapshot date `YYYYMMDDTHHMMSSZ` (for SOURCE_DATE_EPOCH).
pub fn snapshot_epoch(snapshot: &str) -> Result<i64> {
    let n = |a: usize, b: usize| -> Result<i64> { snapshot.get(a..b).and_then(|t| t.parse::<i64>().ok()).context("snapshot date") };
    let (y, m, d) = (n(0, 4)?, n(4, 6)?, n(6, 8)?);
    let (hh, mm, ss) = (n(9, 11)?, n(11, 13)?, n(13, 15)?);
    // days from civil
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Ok(days * 86400 + hh * 3600 + mm * 60 + ss)
}

/// The rebuilt packages of the lock's patches, in a folder of the cache. They are built once for an image version (the base image, the snapshot and the
/// patches), not once per recipe: the first image that needs them builds them in a separate image with the build dependencies, and every later image
/// installs the same files. The build is deterministic as far as Debian's packaging is (fixed SOURCE_DATE_EPOCH, fixed changelog date, no tests, no
/// documentation); the hash of each file is written to `SHA256SUMS` next to them.
pub fn patched_debs(cache: &Path, lock: &Lock, k: &Known) -> Result<PathBuf> {
    let mut h = Sha256::new();
    h.update(format!("panpak-patched-v1\n{}\n{}\n{}\n{}\n", lock.image.name, lock.image.manifest_digest, lock.image.snapshot, lock.image.patches.join(";")));
    let id = hex(&h.finalize())[..20].to_string();
    let dir = cache.join("patched").join(&id);
    let debs = dir.join("debs");
    if dir.join(".complete").exists() {
        return Ok(debs);
    }
    super::say("building the patched packages of the image once (the build dependencies and the compilation take a while, about half an hour for GTK)");
    let _ = chmod_all(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    let (root, work, out, scratch) = (dir.join("builder"), dir.join("work"), dir.join("out"), dir.join("scratch"));
    for d in [&work, &out, &scratch] {
        std::fs::create_dir_all(d)?;
    }
    pull_base(lock, k, &root, &scratch)?;
    let shim = sandbox::shim_in(cache)?;
    let etc = root.join("etc/apt");
    let _ = std::fs::remove_dir_all(etc.join("sources.list.d"));
    std::fs::create_dir_all(etc.join("sources.list.d"))?;
    write_sources(&etc, lock, k, true)?;
    std::fs::create_dir_all(etc.join("apt.conf.d"))?;
    std::fs::write(etc.join("apt.conf.d/99panpak"), "APT::Sandbox::User \"root\";\nAcquire::Check-Valid-Until \"false\";\nAPT::Install-Recommends \"false\";\nAcquire::Retries \"3\";\nDPkg::Options { \"--force-unsafe-io\"; };\n")?;
    let log = scratch.join("build.log");
    let step = |argv: &[&str], what: &str, mounts: bool| -> Result<()> {
        let mut r = sandbox::Run::new(&root, &shim, argv);
        r.net = true;
        if mounts {
            r = r.bind(&work, "/work", true).bind(&out, "/out", true);
        }
        r.cwd = mounts.then(|| "/work".to_string());
        r.run_logged(&log, what)
    };
    step(&["apt-get", "update", "-q"], "apt-get update", false)?;
    step(&["apt-get", "install", "-y", "-q", "ca-certificates", "build-essential", "dpkg-dev", "fakeroot", "patch"], "apt-get install (build tools)", false)?;
    let jobs = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).min(8);
    let epoch = snapshot_epoch(&lock.image.snapshot)?.to_string();
    for entry in &lock.image.patches {
        let p = k.patches.iter().find(|p| p.entry() == *entry).with_context(|| format!("panpak.lock names a patch this panpak does not have: {entry}"))?;
        std::fs::write(work.join(p.name), p.text)?;
        step(&["apt-get", "build-dep", "-y", "-q", p.package], "apt-get build-dep", false)?;
        let script = format!(
            "set -e\ncd /work\napt-get source -q {pkg}\ncd {pkg}-*/\npatch -p1 < /work/{name}\nV=$(dpkg-parsechangelog -S Version)\n\
             {{ printf '{pkg} (%s+panpak1) {suite}; urgency=medium\\n\\n  * Patch {name}.\\n\\n -- Panpak <noreply@invalid>  %s\\n\\n' \"$V\" \"$(date -u -d @{epoch} -R)\"; cat debian/changelog; }} > /work/changelog.new\n\
             cp /work/changelog.new debian/changelog\n\
             SOURCE_DATE_EPOCH={epoch} TZ=UTC LC_ALL=C DEB_BUILD_OPTIONS='nocheck nodoc noautodbgsym parallel={jobs}' dpkg-buildpackage -b -uc -us\n\
             cp /work/*.deb /out/\n",
            pkg = p.package, name = p.name, suite = k.suite, epoch = epoch, jobs = jobs
        );
        std::fs::write(work.join("build.sh"), script)?;
        step(&["sh", "/work/build.sh"], &format!("build of {} with {}", p.package, p.name), true)?;
    }
    std::fs::create_dir_all(&debs)?;
    let mut sums = String::new();
    let mut files: Vec<_> = std::fs::read_dir(&out)?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|f| f.ends_with(".deb")).collect();
    files.sort();
    ensure!(!files.is_empty(), "the patched build produced no package");
    for f in &files {
        std::fs::copy(out.join(f), debs.join(f))?;
        sums.push_str(&format!("{}  {}\n", hex(&Sha256::digest(std::fs::read(debs.join(f))?)), f));
    }
    std::fs::write(dir.join("SHA256SUMS"), &sums)?;
    super::say(&format!("patched packages built:\n{sums}"));
    // The builder is large and not needed again.
    let _ = chmod_all(&root);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&work);
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::write(dir.join(".complete"), format!("{}\n", lock.image.patches.join("\n")))?;
    Ok(debs)
}

/// The version of an image package, from the list written when the image was made.
pub fn package_version(root: &Path, package: &str) -> Option<String> {
    let t = std::fs::read_to_string(root.parent()?.join("packages.tsv")).ok()?;
    t.lines().find_map(|l| l.split_once('\t').filter(|(n, _)| *n == package).map(|(_, v)| v.to_string()))
}

/// `chmod -R u+rwX`: the cache must be removable.
pub fn chmod_all(dir: &Path) -> Result<()> {
    if dir.exists() {
        let _ = std::process::Command::new("chmod").args(["-R", "u+rwX"]).arg(dir).status();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the patched GTK for real (network, bubblewrap, half an hour): `cargo test -- --ignored builds_the_patched_packages`, with
    /// XDG_CACHE_HOME pointing at a scratch folder. Prints the hashes of the packages.
    #[test]
    #[ignore]
    fn builds_the_patched_packages() {
        let cache = super::super::cache_dir().unwrap();
        let l = Lock::current_for("debian-13").unwrap();
        let debs = patched_debs(&cache, &l, known("debian-13").unwrap()).unwrap();
        let sums = std::fs::read_to_string(debs.parent().unwrap().join("SHA256SUMS")).unwrap();
        eprintln!("{sums}");
        assert!(sums.contains("libgtk-4-1_"));
    }

    #[test]
    fn snapshot_dates_and_patches() {
        assert_eq!(snapshot_epoch("20261107T000000Z").unwrap(), 1794009600);
        assert_eq!(snapshot_epoch("19700101T000001Z").unwrap(), 1);
        assert!(snapshot_epoch("yesterday").is_err());
        // The patch of the image is in the lock a new build writes, and the key of an image with it differs from one without.
        let l = Lock::current_for("debian-13").unwrap();
        assert_eq!(l.image.patches.len(), 1);
        assert!(l.image.patches[0].starts_with("gtk4 gtk4-dmabuf-v5.patch sha256:"));
        let mut bare = l.clone();
        bare.image.patches.clear();
        assert_ne!(image_key(&l, &[]), image_key(&bare, &[]));
        let mut bad = l.clone();
        bad.image.patches = vec!["gtk4 other.patch sha256:0000000000000000000000000000000000000000000000000000000000000000".into()];
        assert!(bad.check().is_err());
    }

    #[test]
    fn known_images_and_keys() {
        assert!(known("debian-13").is_ok());
        assert!(known("arch").unwrap_err().to_string().contains("debian-13"));
        let l = Lock::current_for("debian-13").unwrap();
        l.check().unwrap();
        let a = image_key(&l, &["libgtk-4-dev".to_string()]);
        // The order of the packages and the base tools do not change the key; a package, the snapshot or the digest do.
        assert_eq!(a, image_key(&l, &["libgtk-4-dev".to_string(), "git".to_string()]));
        assert_ne!(a, image_key(&l, &["libgtk-4-dev".to_string(), "meson".to_string()]));
        let mut l2 = l.clone();
        l2.image.snapshot = "20250101T000000Z".into();
        assert_ne!(a, image_key(&l2, &["libgtk-4-dev".to_string()]));
    }

    /// Needs the network: `cargo test -p panpak registry -- --ignored`.
    #[test]
    #[ignore]
    fn registry_manifest_and_layer_download() {
        let d = std::env::temp_dir().join(format!("panpak-registry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let l = Lock::current_for("debian-13").unwrap();
        let t = std::time::Instant::now();
        pull_base(&l, known("debian-13").unwrap(), &d.join("root"), &d).unwrap();
        eprintln!("pulled in {:?}", t.elapsed());
        assert!(d.join("root/etc/debian_version").is_file());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn json_values_by_key() {
        let t = r#"{"schemaVersion":2,"config":{"digest":"sha256:aa","size":1},"layers":[{"digest": "sha256:bb","size":2},{"digest":"sha256:cc"}]}"#;
        assert_eq!(json_strings(&t[t.find("\"layers\"").unwrap()..], "digest"), ["sha256:bb", "sha256:cc"]);
        assert_eq!(json_strings(r#"{"token":"abc.def","expires_in":300}"#, "token"), ["abc.def"]);
        assert!(json_strings("{}", "token").is_empty());
    }

    #[test]
    fn a_layer_cannot_leave_the_image() {
        let d = std::env::temp_dir().join(format!("panpak-layer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(1);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "ok/file", &b"x"[..]).unwrap();
        let tgz = {
            let raw = b.into_inner().unwrap();
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(&raw).unwrap();
            e.finish().unwrap()
        };
        std::fs::write(d.join("l.tar.gz"), &tgz).unwrap();
        unpack_layer(&d.join("l.tar.gz"), &d.join("root")).unwrap();
        assert!(d.join("root/ok/file").is_file());
        // An entry with `..` is refused by the check before it is unpacked.
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(1);
        h.set_mode(0o644);
        h.as_gnu_mut().unwrap().name[..8].copy_from_slice(b"../evil\0");
        h.set_cksum();
        b.append(&h, &b"x"[..]).unwrap();
        let raw = b.into_inner().unwrap();
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(&raw).unwrap();
        std::fs::write(d.join("e.tar.gz"), e.finish().unwrap()).unwrap();
        assert!(unpack_layer(&d.join("e.tar.gz"), &d.join("root2")).is_err());
        assert!(!d.join("evil").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
