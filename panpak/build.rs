// SPDX-License-Identifier: MIT OR Apache-2.0
// Builds assets/nochown.c into a small shared library that `panpak build` puts in its build sandbox. The C compiler is the one the
// build already uses (Zig in the musl release builds: CC_<target>, as `.cargo/config.toml` sets it; `cc` otherwise). The library has no
// dependency on a C library, so the same file works for any target.
//
// It also sets `PANPAK_BUILD_EPOCH`, the date of this build's code (`panpak --build-epoch`), so that `install` can tell a newer build of the same version
// from an older one: `SOURCE_DATE_EPOCH` when it is set, else the date of the last commit that touched the code (not the docs, not the reference hashes,
// so a commit that changes no code does not change the binary; the rule is scripts/code-epoch.sh), else nothing.
use std::{env, path::{Path, PathBuf}, process::Command};

/// The date of the build: `SOURCE_DATE_EPOCH`, else what `scripts/code-epoch.sh` says (the one rule, shared with `scripts/repro-build.sh`), else nothing.
fn build_epoch(root: &Path) -> String {
    if let Some(v) = env::var("SOURCE_DATE_EPOCH").ok().filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())) {
        return v;
    }
    let out = Command::new("sh").arg(root.join("scripts/code-epoch.sh")).output();
    match out {
        Ok(o) if o.status.success() => {
            let t = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if t.bytes().all(|b| b.is_ascii_digit()) { t } else { String::new() }
        }
        _ => String::new(),
    }
}

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("..");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-changed=../scripts/code-epoch.sh");
    // Every commit appends a line here; without it a change in panpak-core alone would not run this script again.
    if root.join(".git/logs/HEAD").exists() {
        println!("cargo:rerun-if-changed=../.git/logs/HEAD");
    }
    println!("cargo:rustc-env=PANPAK_BUILD_EPOCH={}", build_epoch(&root));
    println!("cargo:rerun-if-changed=assets/nochown.c");
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("nochown.so");
    let target = env::var("TARGET").unwrap_or_default();
    let cc = env::var(format!("CC_{}", target.replace('-', "_"))).or_else(|_| env::var("CC")).unwrap_or_else(|_| "cc".to_string());
    // CC_<target> can be a relative path (`scripts/zig-cc` in `.cargo/config.toml`, `relative = true`): cargo resolves it for us.
    let status = Command::new(&cc)
        .args(["-shared", "-nostdlib", "-fPIC", "-O2", "-g0", "-fno-stack-protector", "-Wl,--build-id=none", "-Wl,--strip-all", "-Wl,-soname,nochown.so", "-o"])
        .arg(&out)
        .arg("assets/nochown.c")
        .status()
        .unwrap_or_else(|e| panic!("cannot run the C compiler {cc:?} for assets/nochown.c: {e}"));
    assert!(status.success(), "the C compiler failed on assets/nochown.c");
}
