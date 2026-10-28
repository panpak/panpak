// SPDX-License-Identifier: MIT OR Apache-2.0
// `panpak desktop-integration install|remove`: per-user MIME type definition.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn work(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("desktop-integration-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Runs the CLI with `HOME` and `XDG_DATA_HOME` inside `dir` and `PATH` limited
/// to `path_dir`, so the host's `update-mime-database` is never used.
fn cli(dir: &Path, path_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_panpak"))
        .args(args)
        .env("HOME", dir)
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("PATH", path_dir)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// A stand-in `update-mime-database` that appends its arguments to `calls`.
fn fake_update(dir: &Path, exit: i32) -> PathBuf {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("update-mime-database");
    fs::write(&script, format!("#!/bin/sh\necho \"$@\" >> \"{}\"\nexit {exit}\n", dir.join("calls").display())).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[test]
fn install_and_remove_are_idempotent() {
    let dir = work("idem");
    let bin = fake_update(&dir, 0);
    let xml = dir.join("data/mime/packages/panpak.xml");

    for round in 0..2 {
        let o = cli(&dir, &bin, &["desktop-integration", "install"]);
        assert!(o.status.success(), "install {round}: {}", text(&o));
        let s = fs::read_to_string(&xml).unwrap();
        assert!(s.contains("application/x-panpak") && s.contains("*.panpak") && s.contains("*.pan\""), "{s}");
        assert!(!s.contains("x-executable\"/>") && !s.contains("sub-class-of"), "must not be an executable subclass");
    }
    for round in 0..2 {
        let o = cli(&dir, &bin, &["desktop-integration", "remove"]);
        assert!(o.status.success(), "remove {round}: {}", text(&o));
        assert!(!xml.exists());
    }
    // The database is refreshed on the user's mime directory after every install
    // and after a removal that deleted the file: 2 + 1 calls.
    let calls = fs::read_to_string(dir.join("calls")).unwrap();
    let mime = dir.join("data/mime");
    assert!(calls.lines().all(|l| l == mime.to_str().unwrap()), "{calls}");
    assert_eq!(calls.lines().count(), 3, "{calls}");
}

#[test]
fn missing_update_mime_database_only_warns() {
    let dir = work("nodb");
    let empty = dir.join("empty-path");
    fs::create_dir_all(&empty).unwrap();
    let o = cli(&dir, &empty, &["desktop-integration", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("update-mime-database not found"), "{}", text(&o));
    assert!(dir.join("data/mime/packages/panpak.xml").is_file());
    let o = cli(&dir, &empty, &["desktop-integration", "remove"]);
    assert!(o.status.success(), "{}", text(&o));
}

#[test]
fn failing_update_mime_database_is_an_error() {
    let dir = work("faildb");
    let bin = fake_update(&dir, 1);
    let o = cli(&dir, &bin, &["desktop-integration", "install"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("PANPAK-E2000") && text(&o).contains("update-mime-database failed"), "{}", text(&o));
}

#[test]
fn remove_leaves_a_file_it_did_not_install() {
    let dir = work("foreign");
    let bin = fake_update(&dir, 0);
    let xml = dir.join("data/mime/packages/panpak.xml");
    fs::create_dir_all(xml.parent().unwrap()).unwrap();
    fs::write(&xml, "<mime-info/>\n").unwrap();
    let o = cli(&dir, &bin, &["desktop-integration", "remove"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("left alone"), "{}", text(&o));
    assert_eq!(fs::read_to_string(&xml).unwrap(), "<mime-info/>\n");
    assert!(!dir.join("calls").exists(), "nothing changed, so no database refresh");
}

#[test]
fn nothing_is_written_outside_the_user_data_dir() {
    let dir = work("scope");
    let bin = fake_update(&dir, 0);
    assert!(cli(&dir, &bin, &["desktop-integration", "install"]).status.success());
    let mut found = Vec::new();
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                found.push(p.strip_prefix(&dir).unwrap().to_path_buf());
            }
        }
    }
    found.sort();
    let expected: Vec<PathBuf> = ["bin/update-mime-database", "calls", "data/mime/packages/panpak.xml"].iter().map(PathBuf::from).collect();
    assert_eq!(found, expected);
}

#[test]
fn usage_errors_are_coded() {
    let dir = work("usage");
    let empty = dir.join("p");
    fs::create_dir_all(&empty).unwrap();
    for (args, code) in [
        (vec!["desktop-integration"], "PANPAK-E5002"),
        (vec!["desktop-integration", "enable"], "PANPAK-E5001"),
        (vec!["desktop-integration", "install", "extra"], "PANPAK-E5001"),
    ] {
        let o = cli(&dir, &empty, &args);
        assert!(!o.status.success() && text(&o).contains(code), "{args:?}: {}", text(&o));
    }
}
