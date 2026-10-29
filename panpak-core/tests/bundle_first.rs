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

/// A small version of the QOwnNotes case. The system has `libglib` (with `g_old` and `g_new`),
/// `libgobject` (needs `g_new`) and `libpx` (needs `libgobject`); the package has an older `libglib`
/// with `g_old` only, and no `libgobject`. The program calls `px_fn` and `g_old`.
struct Glib {
    dir: PathBuf,
}

impl Glib {
    /// `bundled_glib_defines`: what the included `libglib` defines; `system_glib_defines`: the system's.
    fn new(tag: &str, bundled_glib: &str, system_glib: &str, extra_call: &str) -> Option<Glib> {
        let dir = std::env::temp_dir().join(format!("panpak-glib-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for d in ["host", "tree/lib", "tree/bin", "store"] {
            fs::create_dir_all(dir.join(d)).ok()?;
        }
        let w = |f: &str, t: &str| fs::write(dir.join(f), t).ok();
        w("bglib.c", bundled_glib)?;
        w("hglib.c", system_glib)?;
        w("gobj.c", "int g_new(void);\nint gobj_fn(void) { return g_new() + 10; }\n")?;
        w("px.c", "int gobj_fn(void);\nint px_fn(void) { return gobj_fn() + 100; }\n")?;
        w(
            "main.c",
            &format!(
                "#include <stdio.h>\nint px_fn(void);\nint g_old(void);\n{extra_call}\nint main(void) {{ printf(\"%d\\n\", px_fn() + g_old()); return 0; }}\n"
            ),
        )?;
        let so = ["-shared", "-fPIC", "-Wl,-z,now"];
        let ok = zig_cc(&dir, &[so.as_slice(), &["-Wl,-soname,libglib-2.0.so.0", "-o", "tree/lib/libglib-2.0.so.0", "bglib.c"]].concat())
            && zig_cc(&dir, &[so.as_slice(), &["-Wl,-soname,libglib-2.0.so.0", "-o", "host/libglib-2.0.so.0", "hglib.c"]].concat())
            && zig_cc(&dir, &[so.as_slice(), &["-Wl,-soname,libgobject-2.0.so.0", "-o", "host/libgobject-2.0.so.0", "gobj.c", "host/libglib-2.0.so.0"]].concat())
            && zig_cc(&dir, &[so.as_slice(), &["-Wl,-soname,libpx.so.1", "-o", "host/libpx.so.1", "px.c", "host/libgobject-2.0.so.0"]].concat())
            && zig_cc(&dir, &["-o", "tree/bin/app", "main.c", "host/libpx.so.1", "tree/lib/libglib-2.0.so.0", "-Wl,-z,now"]);
        ok.then_some(Glib { dir })
    }

    fn host(&self) -> HashMap<String, PathBuf> {
        ["libglib-2.0.so.0", "libgobject-2.0.so.0", "libpx.so.1"]
            .iter()
            .map(|n| (n.to_string(), self.dir.join("host").join(n)))
            .chain([
                ("libc.so.6".to_string(), PathBuf::from("/lib/x86_64-linux-gnu/libc.so.6")),
                ("ld-linux-x86-64.so.2".to_string(), PathBuf::from("/lib64/ld-linux-x86-64.so.2")),
            ])
            .collect()
    }

    fn bundled(&self) -> HashMap<String, PathBuf> {
        HashMap::from([("libglib-2.0.so.0".to_string(), PathBuf::from("lib/libglib-2.0.so.0"))])
    }

    fn needs() -> Vec<String> {
        vec!["libglib-2.0.so.0".to_string(), "libpx.so.1".to_string()]
    }

    fn resolve(&self) -> panpak_core::abi::Resolution {
        panpak_core::abi::resolve_consistent(&self.dir.join("tree"), "bin/app", &Self::needs(), &[], &[], &self.host(), &self.bundled()).unwrap()
    }

    fn run(&self, decisions: &[(String, Source)]) -> (bool, String) {
        let plan = apply(&self.dir.join("store"), "root", &self.dir.join("tree"), decisions, &self.bundled()).unwrap();
        let mut dirs = plan.ld_path.clone();
        dirs.push(self.dir.join("host")); // the system folder: where the loader finds libgobject and libpx
        let out = Command::new(self.dir.join("tree/bin/app"))
            .env_clear()
            .env("LD_LIBRARY_PATH", std::env::join_paths(dirs.iter()).unwrap())
            .output()
            .unwrap();
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_string())
    }
}

impl Drop for Glib {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

const NEW_GLIB: &str = "int g_old(void) { return 1; }\nint g_new(void) { return 2; }\n";
const OLD_GLIB: &str = "int g_old(void) { return 1; }\n";

#[test]
fn a_system_library_that_needs_newer_symbols_moves_the_included_one_to_the_system() {
    let Some(g) = Glib::new("fix", OLD_GLIB, NEW_GLIB, "") else {
        eprintln!("skipped: Zig cannot be run (set PANPAK_ZIG)");
        return;
    };
    if !Path::new("/lib/x86_64-linux-gnu/libc.so.6").exists() || !Path::new("/lib64/ld-linux-x86-64.so.2").exists() {
        eprintln!("skipped: no glibc at the usual place");
        return;
    }
    // The rules alone take the included libglib; libgobject, which nothing in `needs` names, is the system's.
    let rules = decide(&Glib::needs(), &[], &[], &g.host(), &g.bundled()).unwrap();
    assert!(rules.contains(&("libglib-2.0.so.0".to_string(), Source::Bundled(PathBuf::from("lib/libglib-2.0.so.0")))));
    let (ok, text) = g.run(&rules);
    assert!(!ok && text.contains("g_new"), "expected a failure on g_new before the symbol check: {text}");
    let r = g.resolve();
    assert_eq!(r.changed, vec!["libglib-2.0.so.0".to_string()]);
    assert!(r.diagnosis.missing.is_empty(), "{:?}", r.diagnosis);
    assert_eq!(r.steps, 1);
    assert!(r.decisions.contains(&("libglib-2.0.so.0".to_string(), Source::Host(g.dir.join("host/libglib-2.0.so.0")))));
    let (ok, text) = g.run(&r.decisions);
    assert!(ok, "the program does not start after the symbol check: {text}");
    assert_eq!(text, "113");
}

#[test]
fn a_symbol_that_nothing_defines_stays_reported_and_nothing_changes() {
    let Some(g) = Glib::new("none", OLD_GLIB, OLD_GLIB, "int nowhere_fn(void);") else {
        eprintln!("skipped: Zig cannot be run (set PANPAK_ZIG)");
        return;
    };
    // Neither copy of libglib has `g_new`: libgobject (system) asks for a symbol no copy defines.
    let r = g.resolve();
    assert!(r.changed.is_empty(), "{:?}", r.changed);
    assert!(r.steps <= panpak_core::abi::MAX_CONSISTENCY_STEPS);
    assert!(r.diagnosis.missing.iter().any(|(_, s)| s == "g_new"), "{:?}", r.diagnosis);
    // The decisions are those of the rules: the included libglib.
    assert!(r.decisions.contains(&("libglib-2.0.so.0".to_string(), Source::Bundled(PathBuf::from("lib/libglib-2.0.so.0")))));
}

#[test]
fn a_step_that_does_not_help_is_undone_and_the_loop_ends() {
    // The system libglib has `g_new` but not `g_old`: moving to it fixes one symbol and breaks another.
    let Some(g) = Glib::new("loop", OLD_GLIB, "int g_new(void) { return 2; }\n", "") else {
        eprintln!("skipped: Zig cannot be run (set PANPAK_ZIG)");
        return;
    };
    let r = g.resolve();
    assert!(r.changed.is_empty(), "a step that does not lower the count must be undone: {:?}", r.changed);
    assert!(r.steps >= 1 && r.steps <= panpak_core::abi::MAX_CONSISTENCY_STEPS, "steps {}", r.steps);
    assert!(r.decisions.contains(&("libglib-2.0.so.0".to_string(), Source::Bundled(PathBuf::from("lib/libglib-2.0.so.0")))));
    assert!(!r.diagnosis.missing.is_empty());
}
