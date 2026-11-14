// SPDX-License-Identifier: MIT OR Apache-2.0
//! `panpak build`: from a recipe to a signed package. The steps: the lock, the memory plan, the image, the source, the build in the image
//! (no network; Zig fetches its packages on the first run when the recipe says so), the collection, the manifest, `pack`.
use super::{collect, fetch, image, lock, memory, recipe::Recipe, sandbox};
use anyhow::{bail, ensure, Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub struct BuildArgs<'a> {
    pub dir: &'a Path,
    pub secret: &'a str,
    pub stub: &'a Path,
    pub out: Option<&'a Path>,
    pub update_lock: bool,
}

/// The steps of a build system, as lists of arguments. `jobs` is the number of parallel jobs.
pub fn steps(r: &Recipe, jobs: usize, src: &str) -> Vec<Vec<String>> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    let with = |mut base: Vec<String>, extra: &[String]| {
        base.extend(extra.iter().cloned());
        base
    };
    let j = jobs.to_string();
    match r.build.system.as_str() {
        "meson" => vec![with(s(&["meson", "setup", "/work/build", src]), &r.build.configure), s(&["ninja", "-C", "/work/build", "-j", &j]), s(&["ninja", "-C", "/work/build", "install"])],
        "cmake" => vec![with(s(&["cmake", "-S", src, "-B", "/work/build", "-G", "Ninja"]), &r.build.configure), s(&["ninja", "-C", "/work/build", "-j", &j]), s(&["ninja", "-C", "/work/build", "install"])],
        // Zig takes the number of jobs as one argument, `-j3`.
        "zig" => vec![with(s(&["zig", "build"]), &r.build.configure).into_iter().chain(s(&["--summary", "all", &format!("-j{j}")])).collect()],
        _ => r.build.steps.clone(),
    }
}

fn run_step(root: &Path, shim: &Path, argv: &[String], env: &[(&str, String)], mounts: &[(&Path, &str, bool)], cwd: &str, net: bool) -> sandbox::Run {
    let a: Vec<&str> = argv.iter().map(String::as_str).collect();
    let mut r = sandbox::Run::new(root, shim, &a);
    for (s, d, rw) in mounts {
        r = r.bind(s, d, *rw);
    }
    for (k, v) in env {
        r = r.env(k, v);
    }
    r.cwd = Some(cwd.to_string());
    r.net = net;
    r
}

pub fn cmd_build(a: BuildArgs) -> Result<PathBuf> {
    let t0 = std::time::Instant::now();
    let r = Recipe::load(a.dir)?;
    super::say(&format!("{} {} ({})", r.app.name, r.app.version, r.app.id));
    // 1. The lock.
    let (lk, wrote) = lock::resolve(a.dir, r.image(), a.update_lock)?;
    if wrote {
        super::say(&format!("wrote panpak.lock: {} {} snapshot {}", lk.image.name, &lk.image.manifest_digest[..19], lk.image.snapshot));
    }
    // 2. Memory.
    let avail = memory::available();
    let plan = memory::plan(r.build.jobs, r.build.memory.as_deref().map(memory::parse_size).transpose()?, r.build.memory_per_job.as_deref().map(memory::parse_size).transpose()?, avail)?;
    for n in &plan.notes {
        super::say(&format!("NOTE: {n}"));
    }
    let scoped = memory::scope_available();
    if !scoped {
        super::say("WARNING: no systemd user scope here: the build runs without a memory cap (a build that uses all the memory can freeze the machine)");
    }
    // 3. The image and the source.
    let cache = super::cache_dir()?;
    let t = std::time::Instant::now();
    let ens = image::ensure(&cache, &lk, &r.build.packages)?;
    let root = ens.root;
    // The patched packages are inside the making of the image: they are told apart, and the rest is the image.
    super::said_phase("image", t.elapsed().saturating_sub(ens.patch), ens.kept);
    super::said_phase("patch", ens.patch, ens.patch_kept);
    let shim = sandbox::shim_in(&cache)?;
    let t = std::time::Instant::now();
    let src = fetch::fetch(&cache, &r.source)?;
    super::said_phase("source", t.elapsed(), false);
    let epoch = src.epoch.to_string();
    // 4. The workspace.
    let key = image::image_key(&lk, &r.build.packages);
    let ws = cache.join("work").join(format!("{}-{}", r.app.id, &key[..8]));
    let _ = image::chmod_all(&ws);
    let _ = std::fs::remove_dir_all(&ws);
    let (work, out, tree) = (ws.join("work"), ws.join("out"), ws.join("tree"));
    for d in [&work, &out] {
        std::fs::create_dir_all(d)?;
    }
    let mut srcmount = src.dir.clone();
    let mut srcrw = false;
    if r.build.writable_source || r.build.system == "zig" {
        // The build writes into its source (a generated header, the Zig cache): it works on a copy.
        let copy = ws.join("src");
        collect::copy_tree(&src.dir, &copy)?;
        srcmount = copy;
        srcrw = true;
    }
    let mut env: Vec<(&str, String)> = vec![("SOURCE_DATE_EPOCH", epoch.clone()), ("TZ", "UTC".into()), ("DESTDIR", "/out".into())];
    let mut mounts: Vec<(PathBuf, String, bool)> = vec![(srcmount.clone(), "/src".into(), srcrw), (work.clone(), "/work".into(), true), (out.clone(), "/out".into(), true)];
    let mut net = false;
    let mut cwd = "/src".to_string();
    // 5. Zig: a pinned compiler, and a persistent cache of its packages.
    let mut marker: Option<PathBuf> = None;
    if let Some(z) = &r.build.zig {
        let tz = std::time::Instant::now();
        let zdir = ensure_zig(&cache, &root, &shim, &z.version, &z.sha256)?;
        super::said_phase("zig", tz.elapsed(), false);
        mounts.push((zdir, "/opt/zig".into(), false));
        let zc = cache.join("zigcache").join(format!("{}-{}", r.app.id, &key[..8]));
        std::fs::create_dir_all(&zc)?;
        mounts.push((zc.clone(), "/zigcache".into(), true));
        env.push(("ZIG_GLOBAL_CACHE_DIR", "/zigcache".into()));
        env.push(("PATH", "/opt/zig:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()));
        cwd = "/src".into();
        let m = zc.join(".fetched");
        if r.network_fetch() && !m.exists() {
            net = true;
            marker = Some(m);
            super::say("the first build of this recipe fetches its Zig packages (every package is checked by its hash); later builds need no network");
        }
    }
    // 6. The build.
    super::say(&format!("building with {} job(s), memory cap {}", plan.jobs, memory::fmt_size(plan.cap)));
    let swap = if avail.swap > 0 { avail.swap.min(6 << 30) } else { 0 };
    let mut peak = 0u64;
    let logs = ws.join("logs");
    std::fs::create_dir_all(&logs)?;
    let mt: Vec<(&Path, &str, bool)> = mounts.iter().map(|(s, d, rw)| (s.as_path(), d.as_str(), *rw)).collect();
    let tc = std::time::Instant::now();
    for (n, step) in steps(&r, plan.jobs, "/src").iter().enumerate() {
        let log = logs.join(format!("step-{n}.log"));
        let run = run_step(&root, &shim, step, &env, &mt, &cwd, net);
        let mut argv: Vec<OsString> = vec!["bwrap".into()];
        argv.extend(run.args());
        let _ = std::fs::write(&log, "");
        let (st, p) = memory::run_capped(&argv, plan.cap, swap, &log, scoped)?;
        peak = peak.max(p.unwrap_or(0));
        if !st.success() {
            let oom = p.is_some_and(|p| p + (64 << 20) >= plan.cap);
            bail!(
                "build step {} ({}) failed ({st}){}; the end of {}:\n{}",
                n + 1,
                step.first().map(String::as_str).unwrap_or(""),
                if oom { ": it reached the memory cap, so it was probably closed for lack of memory (raise build.memory, or lower build.jobs)" } else { "" },
                log.display(),
                sandbox::tail(&log, 30)
            );
        }
    }
    if let Some(m) = marker {
        std::fs::write(m, "fetched\n")?;
    }
    super::said_phase("compile", tc.elapsed(), false);
    // 7. Collect.
    super::say("collecting the libraries, plugins and data");
    let tl = std::time::Instant::now();
    let log = logs.join("collect.log");
    let col = collect::collect(&collect::Inputs { recipe: &r, root: &root, destdir: &out, source: &src.dir, tree: &tree, shim: &shim, log: &log })?;
    for n in &col.notes {
        super::say(&format!("NOTE: {n}"));
    }
    super::said_phase("collect", tl.elapsed(), false);
    // 8. The manifest and the package.
    let tp = std::time::Instant::now();
    let secret_key = crate::parse_secret(a.secret)?;
    let pubkey = format!("ed25519:{}", panpak_core::hex(secret_key.verifying_key().as_bytes()));
    let mut deps = Vec::new();
    for d in &r.build.image_deps {
        let v = image::package_version(&root, &d.package).with_context(|| format!("image dependency {}: package {} is not installed in the image", d.name, d.package))?;
        deps.push(panpak_core::ImageDep { name: d.name.clone(), package: d.package.clone(), version: v });
    }
    // The peak memory is not recorded in the package: it changes from one run to the next, and the same inputs must give the same bytes. It is
    // shown at the end of the build, for the recipe's `build.memory`.
    let prov = panpak_core::Provenance {
        source: Some(src.describe.clone()),
        image: Some(format!("{} {} snapshot {}{}", lk.image.name, lk.image.manifest_digest, lk.image.snapshot, lk.image.patches.iter().map(|p| format!(" patch {p}")).collect::<String>())),
        image_deps: (!deps.is_empty()).then_some(deps),
    };
    let toml_text = manifest_toml(&r, &col, &pubkey, &prov)?;
    std::fs::write(tree.join("panpak.toml"), toml_text)?;
    let outp = match a.out {
        Some(p) => p.to_path_buf(),
        None => a.dir.join(format!("{}.panpak", r.app.id)),
    };
    let pkg = crate::cmd_pack(&tree, a.stub, a.secret, None, Some(&outp), Vec::new())?;
    super::said_phase("pack", tp.elapsed(), false);
    // 9. What was measured.
    let secs = t0.elapsed().as_secs();
    super::say(&format!("{}: {} libraries ({} MB), built in {secs} s", pkg.display(), col.libs, col.bytes >> 20));
    if peak > 0 {
        let declared = r.build.memory.as_deref().map(memory::parse_size).transpose()?;
        let suggest = memory::fmt_size(peak + peak / 10);
        match declared {
            None => super::say(&format!("the build used {} at its peak: write build.memory = \"{suggest}\" (and build.jobs = {}) in the recipe", memory::fmt_size(peak), plan.jobs)),
            Some(d) if peak > d => super::say(&format!("WARNING: the build used {} at its peak, more than build.memory = {}: write build.memory = \"{suggest}\"", memory::fmt_size(peak), r.build.memory.as_deref().unwrap_or(""))),
            Some(_) => super::say(&format!("peak memory {} (build.memory is {})", memory::fmt_size(peak), r.build.memory.as_deref().unwrap_or(""))),
        }
    }
    Ok(pkg)
}

/// The `panpak.toml` that `pack` reads, made from the recipe and what was collected.
pub fn manifest_toml(r: &Recipe, col: &collect::Collected, pubkey: &str, prov: &panpak_core::Provenance) -> Result<String> {
    #[derive(serde::Serialize)]
    struct Desktop {
        #[serde(skip_serializing_if = "Option::is_none")]
        wm_class: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        schemes: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        mime_types: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        icons: Vec<String>,
    }
    #[derive(serde::Serialize)]
    struct Abi {
        #[serde(skip_serializing_if = "Vec::is_empty")]
        needs: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        prefer_host: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        allow_missing: Vec<String>,
    }
    #[derive(serde::Serialize)]
    struct Upd {
        key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    }
    #[derive(serde::Serialize)]
    struct M<'a> {
        app: &'a panpak_core::App,
        #[serde(skip_serializing_if = "Option::is_none")]
        abi: Option<Abi>,
        #[serde(skip_serializing_if = "Option::is_none")]
        permissions: Option<&'a panpak_core::Permissions>,
        #[serde(skip_serializing_if = "Option::is_none")]
        launch: Option<&'a panpak_core::Launch>,
        desktop: Desktop,
        #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        env: &'a std::collections::BTreeMap<String, String>,
        update: Upd,
        provenance: &'a panpak_core::Provenance,
    }
    let c = r.collect.as_ref();
    let abi = (!col.needs.is_empty() || c.is_some_and(|c| !c.prefer_host.is_empty() || !c.allow_missing.is_empty())).then(|| Abi {
        needs: col.needs.clone(),
        prefer_host: c.map(|c| c.prefer_host.clone()).unwrap_or_default(),
        allow_missing: c.map(|c| c.allow_missing.clone()).unwrap_or_default(),
    });
    let d = r.desktop.as_ref();
    let m = M {
        app: &r.app,
        abi,
        permissions: r.permissions.as_ref(),
        launch: r.launch.as_ref(),
        desktop: Desktop { wm_class: d.and_then(|d| d.wm_class.clone()), schemes: d.and_then(|d| d.schemes.clone()), mime_types: col.mime_types.clone(), icons: col.icons.clone() },
        env: &col.env,
        update: Upd { key: pubkey.to_string(), url: r.update.as_ref().and_then(|u| u.url.clone()) },
        provenance: prov,
    };
    toml::to_string(&m).context("write panpak.toml")
}

/// A pinned Zig, unpacked in the cache (by the image's `tar`). Returns the folder.
fn ensure_zig(cache: &Path, root: &Path, shim: &Path, version: &str, sha256: &str) -> Result<PathBuf> {
    let dir = cache.join("zig").join(format!("{version}-{}", &sha256[..12]));
    if dir.join(".complete").exists() {
        return Ok(dir);
    }
    super::say(&format!("fetching Zig {version}"));
    let dl = cache.join("zig-dl");
    std::fs::create_dir_all(&dl)?;
    let file = dl.join(format!("zig-{version}.tar.xz"));
    image::download(&format!("https://ziglang.org/download/{version}/zig-x86_64-linux-{version}.tar.xz"), &[], Some(sha256), &file, 1 << 29)?;
    let _ = image::chmod_all(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let log = dl.join("untar.log");
    sandbox::Run::new(root, shim, &["tar", "-xJf", &format!("/dl/zig-{version}.tar.xz"), "-C", "/zig", "--strip-components=1"]).bind(&dl, "/dl", false).bind(&dir, "/zig", true).run_logged(&log, "unpacking Zig")?;
    ensure!(dir.join("zig").is_file(), "the Zig archive has no zig program");
    std::fs::write(dir.join(".complete"), sha256)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe(system: &str, extra: &str) -> Recipe {
        Recipe::parse(&format!("[app]\nid = \"org.example.A\"\nname = \"A\"\nversion = \"1\"\nentry = \"bin/a\"\n[source]\ngit = \"https://e.org/a.git\"\ncommit = \"{}\"\n[build]\nsystem = \"{system}\"\n{extra}\n", "ab".repeat(20))).unwrap()
    }

    #[test]
    fn the_steps_of_the_build_systems() {
        let m = steps(&recipe("meson", "configure = [\"--prefix=/usr\"]"), 4, "/src");
        assert_eq!(m[0], ["meson", "setup", "/work/build", "/src", "--prefix=/usr"]);
        assert_eq!(m[1], ["ninja", "-C", "/work/build", "-j", "4"]);
        assert_eq!(m[2].last().unwrap(), "install");
        let c = steps(&recipe("cmake", "configure = [\"-DX=1\"]"), 2, "/work/src");
        assert_eq!(c[0], ["cmake", "-S", "/work/src", "-B", "/work/build", "-G", "Ninja", "-DX=1"]);
        let z = steps(&recipe("zig", "zig = { version = \"0.15.2\", sha256 = \"00000000000000000000000000000000000000000000000000000000000000aa\" }\nconfigure = [\"-Doptimize=ReleaseFast\"]"), 3, "/src");
        assert_eq!(z.len(), 1);
        assert_eq!(z[0], ["zig", "build", "-Doptimize=ReleaseFast", "--summary", "all", "-j3"]);
        let cu = steps(&recipe("custom", "steps = [[\"make\", \"-j\", \"2\"], [\"make\", \"install\"]]"), 2, "/src");
        assert_eq!(cu.len(), 2);
    }

    #[test]
    fn the_manifest_is_made_of_data_and_passes_pack_rules() {
        let r = Recipe::parse(&format!(
            "[app]\nid = \"org.example.A\"\nname = \"A\"\nversion = \"1\"\nentry = \"bin/a\"\n[source]\ngit = \"https://e.org/a.git\"\ncommit = \"{}\"\n[build]\nsystem = \"meson\"\n[desktop]\nwm_class = \"org.example.A\"\n[permissions]\ndbus_own = [\"org.example.A\"]\n[launch]\nargs = [\"--x\"]\n",
            "ab".repeat(20)
        ))
        .unwrap();
        let col = collect::Collected { env: [("GSETTINGS_BACKEND".to_string(), "keyfile".to_string())].into(), needs: vec!["libpixbufloader_svg.so".into()], icons: vec!["share/icons/hicolor/48x48/apps/org.example.A.png".into()], mime_types: vec!["text/plain".into()], libs: 1, bytes: 1, notes: vec![] };
        let prov = panpak_core::Provenance { source: Some("git x".into()), image: Some("debian-13 sha256:x snapshot y".into()), image_deps: Some(vec![panpak_core::ImageDep { name: "g".into(), package: "p".into(), version: "1".into() }]) };
        let t = manifest_toml(&r, &col, &format!("ed25519:{}", "0".repeat(64)), &prov).unwrap();
        panpak_core::check_panpak_toml_schema(&t).unwrap();
        let m: panpak_core::Manifest = toml::from_str(&t).unwrap();
        panpak_core::validate_manifest(&m).unwrap();
        assert_eq!(m.abi.unwrap().needs.unwrap(), ["libpixbufloader_svg.so"]);
        assert_eq!(m.env.unwrap()["GSETTINGS_BACKEND"], "keyfile");
        assert_eq!(m.provenance.unwrap().image_deps.unwrap()[0].package, "p");
        assert_eq!(m.desktop.unwrap().wm_class.as_deref(), Some("org.example.A"));
    }
}
