// SPDX-License-Identifier: MIT OR Apache-2.0
//! Precompiled Python (`.pyc`) for a package that carries its own Python (PANPAK-FORMAT.md section 4, "Caches that the writer makes").
//!
//! A tree is read-only at run time, so a Python in it cannot write its `__pycache__`, and every start compiles the standard library and the modules of the app
//! again (3.1 s on the first start of GIMP, measured). `pack` runs the Python of the package once, here, in a sandbox, and puts the result in the package:
//! `compileall --invalidation-mode unchecked-hash`, so a `.pyc` does not depend on the time of its source (the store gives its own times) and the same
//! source gives the same bytes.
//!
//! What runs: the interpreter of the package (found in the files that are being packed) on the files of the package, in `bubblewrap` with no network, no
//! access to the home of the person or the rest of the machine, the package mounted read-only at `/tree` (also the working folder: a Python that asks for its loader by a relative path needs it), a folder for the output, a time limit and no more
//! than the files of the package to read. The host supplies only `/usr` (the loader and libc, as when the app runs without its own).

use anyhow::{ensure, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The most time that the compilation may take.
const LIMIT_SECS: u64 = 600;
/// The most `.pyc` files that are taken (a tree with more is not a Python package that anyone ships).
const MAX_FILES: usize = 200_000;

/// What was made: the files to add to the package (paths relative to its root), and the line that says so.
pub struct Compiled {
    pub files: Vec<(PathBuf, Vec<u8>)>,
    pub note: String,
}

/// The Python of a package: the file `bin/python3.N` (or `python3`) of the paths, an ELF file, with the highest `N`.
fn find_python(root: &Path, paths: &[PathBuf]) -> Option<(PathBuf, u32)> {
    let mut best: Option<(PathBuf, u32)> = None;
    for p in paths {
        let name = p.file_name()?.to_str()?;
        let Some(rest) = name.strip_prefix("python3") else { continue };
        let minor = match rest.strip_prefix('.') {
            Some(m) => match m.parse::<u32>() {
                Ok(m) => m,
                Err(_) => continue,
            },
            None if rest.is_empty() => 0,
            None => continue,
        };
        if p.parent().and_then(|d| d.file_name()).is_none_or(|d| d != "bin") {
            continue;
        }
        let mut magic = [0u8; 4];
        let is_elf = std::fs::symlink_metadata(root.join(p)).is_ok_and(|m| m.file_type().is_file())
            && std::fs::File::open(root.join(p)).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic)).is_ok()
            && &magic == b"\x7fELF";
        if is_elf && best.as_ref().is_none_or(|(_, m)| minor >= *m) {
            best = Some((p.clone(), minor));
        }
    }
    best
}

/// The dynamic loader that the package carries (`ld-linux-x86-64.so.2` of its own glibc), if it has one: the Python of such a package asks for a loader by a path inside the
/// package (`.//usr/lib64/ld-linux-x86-64.so.2` in the Python of GIMP), which only the loader itself, run by path, can be given a library path for.
fn find_loader(root: &Path, paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("ld-linux") && n.contains(".so")) && std::fs::symlink_metadata(root.join(p)).is_ok_and(|m| m.is_file() || m.file_type().is_symlink())).cloned()
}

/// The folders of the package that hold shared libraries, for the library path of the interpreter (relative to the root).
fn library_dirs(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for p in paths {
        if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains(".so")) {
            if let Some(d) = p.parent() {
                dirs.insert(d.to_path_buf());
            }
        }
    }
    dirs.into_iter().take(64).collect()
}

/// Where a `.pyc` that Python wrote under `PYTHONPYCACHEPREFIX=/out` goes in the package: `/out/tree/usr/lib/python3.12/os.cpython-312.pyc` is
/// `usr/lib/python3.12/__pycache__/os.cpython-312.pyc`.
fn package_path(out: &Path, file: &Path) -> Option<PathBuf> {
    let rel = file.strip_prefix(out.join("tree")).ok()?;
    let name = rel.file_name()?;
    let dir = rel.parent()?;
    Some(dir.join("__pycache__").join(name))
}

/// Compile the Python files of a package with its own Python. `paths` are the files that are going into it (relative to `root`, which holds them). `Ok(None)`
/// when there is no Python of the package, no Python source, or nothing to make.
pub fn precompile(root: &Path, paths: &[PathBuf]) -> Result<Option<Compiled>> {
    if std::env::var_os("PANPAK_NO_PYC").is_some() {
        return Ok(None);
    }
    let Some((python, minor)) = find_python(root, paths) else { return Ok(None) };
    let sources = paths.iter().filter(|p| p.extension().is_some_and(|e| e == "py")).count();
    if sources == 0 {
        return Ok(None);
    }
    ensure!(Path::new(panpak_core::BWRAP).exists(), "{} is not here, so the Python of the package is not run", panpak_core::BWRAP);
    let out = OutDir::new()?;
    let libs: Vec<String> = library_dirs(paths).iter().map(|d| format!("/tree/{}", d.display())).collect();
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=10", &LIMIT_SECS.to_string(), panpak_core::BWRAP, "--unshare-all", "--die-with-parent", "--new-session", "--cap-drop", "ALL"]);
    cmd.args(["--ro-bind", "/usr", "/usr", "--symlink", "usr/lib", "/lib", "--symlink", "usr/bin", "/bin"]);
    if Path::new("/usr/lib64").exists() {
        cmd.args(["--symlink", "usr/lib64", "/lib64"]);
    }
    cmd.args(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"]);
    cmd.arg("--ro-bind").arg(root).arg("/tree");
    cmd.arg("--bind").arg(out.path()).arg("/out");
    cmd.args(["--setenv", "PYTHONPYCACHEPREFIX", "/out", "--setenv", "PYTHONHASHSEED", "0", "--setenv", "PYTHONNOUSERSITE", "1", "--setenv", "HOME", "/tmp", "--setenv", "LD_LIBRARY_PATH", &libs.join(":"), "--chdir", "/tree", "--"]);
    match find_loader(root, paths) {
        // The package has its own glibc: its Python runs by its loader, with the libraries of the package.
        Some(loader) => {
            cmd.arg(Path::new("/tree").join(loader)).args(["--library-path", &libs.join(":")]).arg(Path::new("/tree").join(&python));
        }
        None => {
            cmd.arg(Path::new("/tree").join(&python));
        }
    }
    // `-S`: no `site` (a `.pth` file of the package would run code). `PYTHONPYCACHEPREFIX` (in the environment, so the worker processes of `-j 0` have it too; `-X` is not passed
    // to them): the output goes to /out, not beside the sources (the tree is read-only). Not `-I`: it would ignore the environment, `PYTHONHASHSEED` too, which keeps the constants that are sets in a fixed order.
    cmd.args(["-S", "-B", "-m", "compileall", "-q", "-j", "0", "-f", "--invalidation-mode", "unchecked-hash", "/tree"]);
    let started = std::time::Instant::now();
    let r = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).output().context("run the Python of the package")?;
    // A file that does not compile (a template, Python 2) makes compileall exit with 1 and the others are made all the same: only a run that did not start is an error.
    let produced = walk_pyc(out.path());
    if produced.is_empty() {
        let err = String::from_utf8_lossy(&r.stderr);
        anyhow::bail!("the Python of the package made nothing ({}): {}", r.status, err.lines().last().unwrap_or("no message"));
    }
    ensure!(produced.len() <= MAX_FILES, "the Python of the package made {} files: too many", produced.len());
    let mut files = Vec::with_capacity(produced.len());
    let mut bytes = 0u64;
    for f in produced {
        let Some(rel) = package_path(out.path(), &f) else { continue };
        let data = std::fs::read(&f)?;
        bytes += data.len() as u64;
        files.push((rel, data));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let note = format!("made {} .pyc files ({} KiB) with python3.{minor} of the package from {sources} sources, in {:.1} s", files.len(), bytes >> 10, started.elapsed().as_secs_f64());
    Ok(Some(Compiled { files, note }))
}

/// A folder of our own for the output, taken out again at the end (the files go to the package, so nothing stays on the disk).
struct OutDir(PathBuf);

impl OutDir {
    fn new() -> Result<OutDir> {
        use std::os::unix::fs::DirBuilderExt;
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let p = std::env::temp_dir().join(format!("panpak-pyc-{}-{nanos}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&p).context("make the folder of the output")?;
        Ok(OutDir(p))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for OutDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn walk_pyc(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                stack.push(e.path());
            } else if t.is_file() && e.path().extension().is_some_and(|x| x == "pyc") {
                out.push(e.path());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pyc_goes_to_the_pycache_beside_its_source() {
        let out = Path::new("/tmp/o");
        assert_eq!(package_path(out, &out.join("tree/usr/lib/python3.12/os.cpython-312.pyc")).unwrap(), Path::new("usr/lib/python3.12/__pycache__/os.cpython-312.pyc"));
        assert!(package_path(out, Path::new("/elsewhere/x.pyc")).is_none());
    }

    #[test]
    fn the_python_of_a_package_is_the_highest_elf_one_in_a_bin_folder() {
        let d = std::env::temp_dir().join(format!("panpak-pyc-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("usr/bin")).unwrap();
        std::fs::create_dir_all(d.join("usr/lib")).unwrap();
        let elf = |p: &str| std::fs::write(d.join(p), b"\x7fELF....").unwrap();
        elf("usr/bin/python3.11");
        elf("usr/bin/python3.12");
        std::fs::write(d.join("usr/bin/python3.13"), b"#!/bin/sh\n").unwrap();
        elf("usr/lib/python3.99");
        let paths: Vec<PathBuf> = ["usr/bin/python3.11", "usr/bin/python3.12", "usr/bin/python3.13", "usr/lib/python3.99", "usr/bin/python3x"].iter().map(PathBuf::from).collect();
        assert_eq!(find_python(&d, &paths).unwrap(), (PathBuf::from("usr/bin/python3.12"), 12), "a script and a file outside bin/ do not count");
        assert!(find_python(&d, &paths[3..]).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// With the Python of this machine put in a folder as a package would carry it: the `.pyc` come out as unchecked-hash files, the same bytes twice, and Python takes them.
    #[test]
    fn the_python_of_a_package_makes_unchecked_hash_pyc_files() {
        let py = ["/usr/bin/python3"].iter().map(Path::new).find(|p| p.exists());
        let Some(py) = py else { return };
        let Ok(real) = std::fs::canonicalize(py) else { return };
        let ver = real.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("python3.")).and_then(|m| m.parse::<u32>().ok());
        let Some(ver) = ver else { return };
        let lib = PathBuf::from(format!("/usr/lib/python3.{ver}"));
        if !lib.is_dir() || !Path::new(panpak_core::BWRAP).exists() {
            return;
        }
        let d = std::env::temp_dir().join(format!("panpak-pyc-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("usr/bin")).unwrap();
        std::fs::create_dir_all(d.join("usr/lib")).unwrap();
        std::fs::copy(&real, d.join(format!("usr/bin/python3.{ver}"))).unwrap();
        // A small standard library: enough for the interpreter to start and for compileall to run.
        let dest = d.join(format!("usr/lib/python3.{ver}"));
        let status = Command::new("cp").arg("-r").arg(&lib).arg(&dest).status().unwrap();
        assert!(status.success());
        for heavy in ["test", "idlelib", "turtledemo", "site-packages", "ensurepip", "tkinter", "__pycache__"] {
            let _ = std::fs::remove_dir_all(dest.join(heavy));
        }
        std::fs::create_dir_all(d.join("share/app")).unwrap();
        std::fs::write(d.join("share/app/main.py"), "print('hi')\n").unwrap();
        let mut paths = Vec::new();
        let mut stack = vec![d.clone()];
        while let Some(x) = stack.pop() {
            for e in std::fs::read_dir(&x).unwrap().flatten() {
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                } else {
                    paths.push(e.path().strip_prefix(&d).unwrap().to_path_buf());
                }
            }
        }
        let one = precompile(&d, &paths).unwrap().expect("the Python of the package ran");
        let two = precompile(&d, &paths).unwrap().unwrap();
        assert_eq!(one.files, two.files, "the same source gives the same bytes");
        let main = one.files.iter().find(|(p, _)| p == Path::new(&format!("share/app/__pycache__/main.cpython-{}{ver}.pyc", 3))).expect("the file of the app");
        // The header of a `.pyc`: the magic, then the flags; bit 0 is "check source", bit 1 "unchecked": 0b01 is unchecked-hash.
        assert_eq!(&main.1[4..8], &[1, 0, 0, 0], "unchecked-hash, not a time");
        assert!(one.files.iter().any(|(p, _)| p.ends_with(format!("__pycache__/os.cpython-3{ver}.pyc"))), "the standard library too");
        let _ = std::fs::remove_dir_all(&d);
    }
}
