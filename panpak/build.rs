// SPDX-License-Identifier: MIT OR Apache-2.0
// Builds assets/nochown.c into a small shared library that `panpak build` puts in its build sandbox. The C compiler is the one the
// build already uses (Zig in the musl release builds: CC_<target>, as `.cargo/config.toml` sets it; `cc` otherwise). The library has no
// dependency on a C library, so the same file works for any target.
use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/nochown.c");
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("nochown.so");
    let target = env::var("TARGET").unwrap_or_default();
    let cc = env::var(format!("CC_{}", target.replace('-', "_"))).or_else(|_| env::var("CC")).unwrap_or_else(|_| "cc".to_string());
    // CC_<target> can be a relative path (`scripts/zig-cc` in `.cargo/config.toml`, `relative = true`): cargo resolves it for us.
    let status = Command::new(&cc)
        .args(["-shared", "-nostdlib", "-fPIC", "-O2", "-fno-stack-protector", "-Wl,--build-id=none", "-Wl,-soname,nochown.so", "-o"])
        .arg(&out)
        .arg("assets/nochown.c")
        .status()
        .unwrap_or_else(|e| panic!("cannot run the C compiler {cc:?} for assets/nochown.c: {e}"));
    assert!(status.success(), "the C compiler failed on assets/nochown.c");
}
