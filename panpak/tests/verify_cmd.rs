// SPDX-License-Identifier: MIT OR Apache-2.0
// `panpak verify <app-id>`: every file of the installed version against the signed index, on request, read only. A file that changed, a file that is missing and a file
// that nobody signed are all named; `repair` makes it right again.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const ID: &str = "org.example.verifycmd";

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_panpak"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env_remove("PANPAK_SECRET_FILE")
        .env("PANPAK_TEST_LINK_STORE", "1")
        .env_remove("LANGUAGE")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

struct Lab {
    home: PathBuf,
}

impl Drop for Lab {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = Command::new("chmod").args(["-R", "u+w"]).arg(&self.home).output();
            let _ = fs::remove_dir_all(&self.home);
        }
    }
}

fn lab_with_app(name: &str) -> (Lab, PathBuf) {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("verify-{name}-{}", std::process::id()));
    let _ = Command::new("chmod").args(["-R", "u+w"]).arg(&home).output();
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join("run")).unwrap();
    fs::set_permissions(home.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
    let lab = Lab { home: home.clone() };
    let key = home.join("secret.key");
    let o = cli(&home, &["keygen", "-o", key.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let pubkey = text(&o).lines().find_map(|l| l.strip_prefix("key:    ")).unwrap().trim().to_string();
    let real = Path::new(env!("CARGO_BIN_EXE_panpak")).with_file_name("panpak-stub");
    assert!(real.is_file(), "{} is missing: build it first (cargo build -p panpak-stub, with the same --target)", real.display());
    let stub = home.join("stub");
    fs::write(&stub, "#!/bin/sh\necho 1\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let tree = home.join("tree");
    fs::create_dir_all(tree.join("bin")).unwrap();
    fs::create_dir_all(tree.join("share/data")).unwrap();
    fs::write(tree.join("bin/hello"), "#!/bin/sh\necho hello\n").unwrap();
    fs::set_permissions(tree.join("bin/hello"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(tree.join("share/data/one.txt"), "one\n").unwrap();
    fs::write(tree.join("share/data/two.txt"), "two\n").unwrap();
    std::os::unix::fs::symlink("one.txt", tree.join("share/data/link")).unwrap();
    fs::write(tree.join("panpak.toml"), format!("[app]\nid = \"{ID}\"\nname = \"V\"\nversion = \"1\"\nentry = \"bin/hello\"\n[update]\nkey = \"{pubkey}\"\n")).unwrap();
    let pkg = home.join("v1.panpak");
    let o = cli(&home, &["pack", tree.to_str().unwrap(), "--stub", stub.to_str().unwrap(), "--secret-file", key.to_str().unwrap(), "-o", pkg.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let o = cli(&home, &["install", "--yes", "--trust-first-key", pkg.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    (lab, pkg)
}

/// The tree of the installed version in the store (the one folder of `trees`).
fn tree_of(home: &Path) -> PathBuf {
    let trees = home.join(".local/share/panpak/trees");
    let mut v: Vec<PathBuf> = fs::read_dir(&trees).unwrap().flatten().map(|e| e.path()).filter(|p| p.join(".complete").exists()).collect();
    assert_eq!(v.len(), 1, "{v:?}");
    v.remove(0)
}

fn writable(p: &Path) {
    let _ = Command::new("chmod").arg("u+w").arg(p).output();
}

#[test]
fn verify_says_ok_for_a_good_tree_and_names_a_changed_a_missing_and_an_unsigned_file() {
    let (l, pkg) = lab_with_app("flow");
    let home = &l.home;
    let o = cli(home, &["verify", ID]);
    assert!(o.status.success() && text(&o).contains("match the signed index"), "{}", text(&o));
    assert!(text(&o).contains("does not prove that they stay as they are until the app runs"), "{}", text(&o));
    let j = text(&cli(home, &["verify", "--json", ID]));
    assert!(j.contains("\"ok\":true") && j.contains("\"modified\":[]") && j.contains("\"extra\":[]"), "{j}");
    let tree = tree_of(home);
    // A changed file (the same size, so only the content tells), a missing one, and one that nobody signed.
    writable(&tree.join("share/data"));
    writable(&tree.join("share/data/one.txt"));
    fs::write(tree.join("share/data/one.txt"), "ONE\n").unwrap();
    fs::remove_file(tree.join("share/data/two.txt")).unwrap();
    writable(&tree.join("bin"));
    fs::write(tree.join("bin/extra.sh"), "#!/bin/sh\n").unwrap();
    let o = cli(home, &["verify", ID]);
    let t = text(&o);
    assert!(!o.status.success() && t.contains("PANPAK-E2007") && t.contains("3 file(s)") && t.contains(&format!("panpak repair {ID}")), "{t}");
    assert!(t.contains("share/data/one.txt") && t.contains("share/data/two.txt") && t.contains("bin/extra.sh"), "{t}");
    let j = text(&cli(home, &["verify", "--json", ID]));
    assert!(j.contains("\"ok\":false") && j.contains("\"modified\":[") && j.contains("share/data/one.txt") && j.contains("\"extra\":[\"bin/extra.sh\"]"), "{j}");
    // It changed nothing: the damage is still there, and `repair` makes it right (it makes the tree again from the index).
    assert_eq!(fs::read_to_string(tree.join("share/data/one.txt")).unwrap(), "ONE\n");
    assert!(!tree.join("share/data/two.txt").exists());
    let o = cli(home, &["repair", "--from", pkg.to_str().unwrap(), ID]);
    assert!(o.status.success(), "{}", text(&o));
    // The tree is made again from the signed index: what it signed is right, and the file that nobody signed is gone.
    let o = cli(home, &["verify", "--json", ID]);
    assert!(text(&o).contains("\"ok\":true"), "{}", text(&o));
}

#[test]
fn verify_of_an_app_that_is_not_installed_says_so() {
    let (l, _pkg) = lab_with_app("absent");
    let o = cli(&l.home, &["verify", "org.example.nothere"]);
    assert!(!o.status.success() && text(&o).contains("is not installed"), "{}", text(&o));
    let o = cli(&l.home, &["verify"]);
    assert!(!o.status.success() && text(&o).contains("verify [--json] <app-id>"), "{}", text(&o));
}
