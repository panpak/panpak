// SPDX-License-Identifier: MIT OR Apache-2.0
//! A library included in the package wins over a system library with the same SONAME.
//!
//! Two shared libraries are built with Zig in a temporary folder: a "system" one that has only
//! `foo_old`, and an "included" one that has only `foo_new`, both named `libfoo.so.1`. A program
//! that calls `foo_new` is run with the library folders `apply` makes from the decisions of
//! `decide`. With the system library the loader stops with "symbol lookup error". Nothing is
//! installed and nothing outside the temporary folder is touched. The test is skipped, with a
//! message, when Zig cannot be run (set PANPAK_ZIG).
use panpak_core::abi::{apply, decide, Source};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn zig_cc(dir: &Path, args: &[&str]) -> bool {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    Command::new(root.join("scripts/zig-cc"))
        .env("PANPAK_ZIG_TARGET", "x86_64-linux-gnu.2.28")
        .current_dir(dir)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

struct Lab {
    dir: PathBuf,
}

impl Lab {
    fn new() -> Option<Lab> {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("panpak-bundle-first-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for d in ["sys", "tree/lib", "tree/bin", "store"] {
            fs::create_dir_all(dir.join(d)).ok()?;
        }
        fs::write(dir.join("old.c"), "int foo_old(void) { return 1; }\n").ok()?;
        fs::write(dir.join("new.c"), "int foo_new(void) { return 2; }\n").ok()?;
        fs::write(
            dir.join("main.c"),
            "#include <stdio.h>\nint foo_new(void);\nint main(void) { printf(\"%d\\n\", foo_new()); return 0; }\n",
        )
        .ok()?;
        let ok = zig_cc(&dir, &["-shared", "-fPIC", "-Wl,-soname,libfoo.so.1", "-o", "sys/libfoo.so.1", "old.c"])
            && zig_cc(&dir, &["-shared", "-fPIC", "-Wl,-soname,libfoo.so.1", "-o", "tree/lib/libfoo.so.1", "new.c"])
            && zig_cc(&dir, &["-o", "tree/bin/app", "main.c", "tree/lib/libfoo.so.1", "-Wl,-z,now"]);
        ok.then_some(Lab { dir })
    }

    fn host(&self) -> HashMap<String, PathBuf> {
        HashMap::from([("libfoo.so.1".to_string(), self.dir.join("sys/libfoo.so.1"))])
    }

    fn bundled(&self) -> HashMap<String, PathBuf> {
        HashMap::from([("libfoo.so.1".to_string(), PathBuf::from("lib/libfoo.so.1"))])
    }

    /// Run the program with the library folders of these decisions.
    fn run(&self, decisions: &[(String, Source)]) -> (bool, String) {
        let plan = apply(&self.dir.join("store"), "root", &self.dir.join("tree"), decisions, &self.bundled()).unwrap();
        let path = std::env::join_paths(plan.ld_path.iter()).unwrap();
        let out = Command::new(self.dir.join("tree/bin/app")).env_clear().env("LD_LIBRARY_PATH", path).output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text.trim().to_string())
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn the_system_library_alone_gives_a_symbol_lookup_error() {
    let Some(lab) = Lab::new() else {
        eprintln!("skipped: Zig cannot be run (set PANPAK_ZIG)");
        return;
    };
    let system = vec![("libfoo.so.1".to_string(), Source::Host(lab.dir.join("sys/libfoo.so.1")))];
    let (ok, text) = lab.run(&system);
    assert!(!ok, "{text}");
    assert!(text.contains("symbol lookup error") || text.contains("undefined symbol"), "{text}");
}

#[test]
fn the_included_library_wins_and_the_program_starts() {
    let Some(lab) = Lab::new() else {
        eprintln!("skipped: Zig cannot be run (set PANPAK_ZIG)");
        return;
    };
    let needs = ["libfoo.so.1".to_string()];
    let decisions = decide(&needs, &[], &[], &lab.host(), &lab.bundled()).unwrap();
    let (ok, text) = lab.run(&decisions);
    assert!(ok, "the program does not start with {decisions:?}: {text}");
    assert_eq!(text, "2");
    assert_eq!(decisions, vec![("libfoo.so.1".to_string(), Source::Bundled(PathBuf::from("lib/libfoo.so.1")))]);
}

/// Build `dir/libstdc++.so.6` that defines `GLIBCXX_<v>` for each version in `versions`
/// (one symbol per version, `sym_<i>`).
fn cxx_lib(lab: &Path, dir: &str, versions: &[&str]) -> bool {
    fs::create_dir_all(lab.join(dir)).unwrap();
    let mut c = String::new();
    let mut script = String::new();
    let mut prev = String::new();
    for (i, v) in versions.iter().enumerate() {
        c.push_str(&format!("int sym_{i}(void) {{ return {i}; }}\n"));
        script.push_str(&format!("GLIBCXX_{v} {{ global: sym_{i}; }}{prev};\n"));
        prev = format!(" GLIBCXX_{v}");
    }
    fs::write(lab.join(format!("{dir}.c")), c).unwrap();
    fs::write(lab.join(format!("{dir}.map")), script).unwrap();
    zig_cc(
        lab,
        &[
            "-shared", "-fPIC", "-Wl,-soname,libstdc++.so.6",
            &format!("-Wl,--version-script={dir}.map"),
            "-o", &format!("{dir}/libstdc++.so.6"), &format!("{dir}.c"),
        ],
    )
}

#[test]
fn the_cxx_runtime_copy_is_chosen_by_symbol_versions() {
    use panpak_core::abi::cxx_runtime_prefs;
    let dir = std::env::temp_dir().join(format!("panpak-cxx-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // old: 3.4.20; new: 3.4.20 and 3.4.30; newer: up to 3.4.31.
    let built = cxx_lib(&dir, "old", &["3.4.20"])
        && cxx_lib(&dir, "new", &["3.4.20", "3.4.30"])
        && cxx_lib(&dir, "newer", &["3.4.20", "3.4.30", "3.4.31"]);
    if !built {
        eprintln!("skipped: Zig cannot be run (set PANPAK_ZIG)");
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    // A program that needs `sym_1`, which is in GLIBCXX_3.4.30.
    fs::write(dir.join("app.c"), "int sym_1(void);\nint main(void) { return sym_1() - 1; }\n").unwrap();
    assert!(zig_cc(&dir, &["-o", "app", "app.c", "new/libstdc++.so.6"]));
    let app = dir.join("app");
    let map = |d: &str| HashMap::from([("libstdc++.so.6".to_string(), PathBuf::from(d))]);
    // The system copy is `host`, the one in the package is `bundled` (a path relative to the tree).
    let choose = |host: &str, bundled: &str| -> Option<bool> {
        let host = HashMap::from([("libstdc++.so.6".to_string(), dir.join(host).join("libstdc++.so.6"))]);
        let prefs = cxx_runtime_prefs(std::slice::from_ref(&app), &host, &map(&format!("{bundled}/libstdc++.so.6")), &dir);
        prefs.get("libstdc++.so.6").copied()
    };
    // Only one copy has GLIBCXX_3.4.30: that one, wherever it is.
    assert_eq!(choose("old", "new"), Some(false), "bundled has the version");
    assert_eq!(choose("new", "old"), Some(true), "the system copy has the version");
    // Both have it: the newer one wins.
    assert_eq!(choose("newer", "new"), Some(true));
    assert_eq!(choose("new", "newer"), Some(false));
    // Neither has it: the one with more versions, and the bundled one on a tie.
    assert_eq!(choose("old", "old"), Some(false));
    // No preference without both copies.
    let none = cxx_runtime_prefs(&[app], &HashMap::new(), &map("new/libstdc++.so.6"), &dir);
    assert!(none.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn doctor_diagnosis_reports_a_symbol_that_no_chosen_library_defines() {
    use panpak_core::abi::diagnose;
    let Some(lab) = Lab::new() else {
        eprintln!("skipped: Zig cannot be run (set PANPAK_ZIG)");
        return;
    };
    let tree = lab.dir.join("tree");
    let system = vec![("libfoo.so.1".to_string(), Source::Host(lab.dir.join("sys/libfoo.so.1")))];
    // The system libraries the program needs besides libfoo (libc) are found as the loader finds them.
    let host = panpak_core::abi::host_libs();
    if !host.contains_key("libc.so.6") {
        eprintln!("skipped: no glibc on this machine");
        return;
    }
    let d = diagnose(&tree, "bin/app", &system, None, &host, &[]);
    assert!(d.modules >= 3, "{d:?}");
    assert_eq!(d.missing, vec![("bin/app".to_string(), "foo_new".to_string())]);
    let included = vec![("libfoo.so.1".to_string(), Source::Bundled(PathBuf::from("lib/libfoo.so.1")))];
    assert!(diagnose(&tree, "bin/app", &included, None, &host, &[]).missing.is_empty());
}
