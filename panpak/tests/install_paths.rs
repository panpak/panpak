// SPDX-License-Identifier: MIT OR Apache-2.0
// What `install`, `update` and the other commands write outside the store, and what
// they refuse: hostile manifest fields, links and foreign files in the destinations.
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
};

const ID: &str = "org.example.paths";

fn work(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("install-paths-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_panpak"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env_remove("PANPAK_SECRET_FILE")
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// Every path under `root` (files, directories, links), relative and sorted.
fn tree(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            out.push(p.strip_prefix(root).unwrap().display().to_string());
            if e.file_type().unwrap().is_dir() {
                stack.push(p);
            }
        }
    }
    out.sort();
    out
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../conformance/fixtures").join(name)
}

/// A home with a stand-in runtime and a signed package, ready to install.
struct Env {
    home: PathBuf,
    pkg: PathBuf,
}

fn setup(name: &str) -> Env {
    setup_with(name, "")
}

/// As `setup`, with extra manifest text (sections other than [app] and [update]) in the package.
fn setup_with(name: &str, extra: &str) -> Env {
    let home = work(name);
    let key = home.join("secret.key");
    let o = cli(&home, &["keygen", "-o", key.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let pubkey = text(&o).lines().find_map(|l| l.strip_prefix("key:    ")).unwrap().trim().to_string();
    let tree = home.join("tree");
    fs::create_dir_all(tree.join("bin")).unwrap();
    fs::write(tree.join("bin/hello"), "#!/bin/sh\necho hi\n").unwrap();
    fs::set_permissions(tree.join("bin/hello"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        tree.join("panpak.toml"),
        format!("[app]\nid = \"{ID}\"\nname = \"Paths\"\nversion = \"1\"\nentry = \"bin/hello\"\n{extra}[update]\nkey = \"{pubkey}\"\n"),
    )
    .unwrap();
    let stub = home.join("stub");
    fs::write(&stub, "#!/bin/sh\necho 1\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let o = cli(&home, &["install-runtime", stub.to_str().unwrap(), "--allow-unsigned"]);
    assert!(o.status.success(), "{}", text(&o));
    let pkg = home.join("p.panpak");
    let o = cli(&home, &["pack", tree.to_str().unwrap(), "--stub", stub.to_str().unwrap(), "--secret-file", key.to_str().unwrap(), "-o", pkg.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    Env { home, pkg }
}

impl Env {
    fn launcher(&self) -> PathBuf {
        self.home.join(".local/bin").join(ID)
    }
    fn desktop(&self) -> PathBuf {
        self.home.join(".local/share/applications").join(format!("panpak.{ID}.desktop"))
    }
    fn install(&self) -> Output {
        cli(&self.home, &["install", "--yes", self.pkg.to_str().unwrap()])
    }
}

#[test]
fn hostile_manifest_fields_write_nothing() {
    for (name, code) in [
        ("invalid-id-dotdot", "PANPAK-E0006"),
        ("invalid-id-slash", "PANPAK-E0006"),
        ("invalid-id-newline", "PANPAK-E0006"),
        ("invalid-id-nul", "PANPAK-E0006"),
        ("invalid-id-long", "PANPAK-E0006"),
        ("invalid-name-newline", "PANPAK-E0007"),
        ("invalid-entry-abs", "PANPAK-E0007"),
        ("invalid-abi-name", "PANPAK-E0007"),
    ] {
        let home = work(name);
        let pkg = fixture(&format!("{name}.panpak"));
        for args in [vec!["install", "--yes", pkg.to_str().unwrap()], vec!["doctor", pkg.to_str().unwrap()]] {
            let o = cli(&home, &args);
            let t = text(&o);
            assert!(!o.status.success() || t.contains("INVALID package"), "{name} {args:?}: {t}");
            assert!(t.contains(code), "{name} {args:?}: {t}");
        }
        // Nothing outside the store: no launcher, desktop entry or data directory
        // (`doctor` creates the empty store itself).
        let outside: Vec<String> = tree(&home)
            .into_iter()
            .filter(|p| !matches!(p.as_str(), ".local" | ".local/share") && !p.starts_with(".local/share/panpak"))
            .collect();
        assert_eq!(outside, Vec::<String>::new(), "{name} wrote outside the store");
        assert!(tree(&home.join(".local/share/panpak")).is_empty() || !tree(&home.join(".local/share/panpak")).iter().any(|p| p.starts_with("apps")), "{name} created an app directory");
    }
}

#[test]
fn hostile_app_id_arguments_are_refused() {
    let home = work("args");
    for id in ["../../escape.app", "org/example.app", "org.example.app\nx", "app"] {
        for cmd in [vec!["open", id, "/tmp/x"], vec!["update", id], vec!["rollback", id]] {
            let o = cli(&home, &cmd);
            assert!(!o.status.success() && text(&o).contains("PANPAK-E0006"), "{cmd:?}: {}", text(&o));
        }
    }
    assert_eq!(tree(&home), Vec::<String>::new());
}

#[test]
fn normal_install_and_reinstall_work() {
    let env = setup("reinstall");
    for round in 0..2 {
        let o = env.install();
        assert!(o.status.success(), "install {round}: {}", text(&o));
    }
    let launcher = fs::read_to_string(env.launcher()).unwrap();
    assert!(launcher.contains(&format!("\n# X-Panpak-App-Id: {ID}\n")), "{launcher}");
    assert_eq!(fs::metadata(env.launcher()).unwrap().permissions().mode() & 0o777, 0o755);
    let desktop = fs::read_to_string(env.desktop()).unwrap();
    assert!(desktop.contains(&format!("\nX-Panpak-App-Id={ID}\n")) && desktop.contains("Name=Paths\n"), "{desktop}");
    assert!(!desktop.contains("MimeType"));
    // No temporary file is left next to them.
    for d in [env.home.join(".local/bin"), env.home.join(".local/share/applications")] {
        for e in fs::read_dir(d).unwrap().flatten() {
            assert!(!e.file_name().to_string_lossy().contains("panpak-tmp"), "{:?}", e.path());
        }
    }
}

#[test]
fn launcher_and_desktop_written_by_an_older_panpak_are_replaced() {
    let env = setup("legacy");
    fs::create_dir_all(env.launcher().parent().unwrap()).unwrap();
    fs::create_dir_all(env.desktop().parent().unwrap()).unwrap();
    fs::write(env.launcher(), "#!/bin/sh\n# Generated by `panpak install`: do not edit.\n# Points to apps/<id>/current; `panpak update`/`rollback` move it.\nexec old\n").unwrap();
    fs::write(
        env.desktop(),
        format!("[Desktop Entry]\nType=Application\nName=Paths\nExec={} %U\nTerminal=false\nCategories=Utility;\nMimeType=application/x-panpak;\n", env.launcher().display()),
    )
    .unwrap();
    let o = env.install();
    assert!(o.status.success(), "{}", text(&o));
    assert!(fs::read_to_string(env.launcher()).unwrap().contains("X-Panpak-App-Id"));
    assert!(!fs::read_to_string(env.desktop()).unwrap().contains("MimeType"));
}

#[test]
fn symlink_in_place_of_the_launcher_is_refused() {
    let env = setup("symlink");
    assert!(env.install().status.success());
    let victim = env.home.join("victim");
    fs::write(&victim, "precious\n").unwrap();
    fs::remove_file(env.launcher()).unwrap();
    symlink(&victim, env.launcher()).unwrap();
    let before = tree(&env.home);
    for args in [vec!["install", "--yes", env.pkg.to_str().unwrap()], vec!["update", ID]] {
        let o = cli(&env.home, &args);
        assert!(!o.status.success() && text(&o).contains("PANPAK-E2001"), "{args:?}: {}", text(&o));
    }
    assert_eq!(fs::read_to_string(&victim).unwrap(), "precious\n");
    assert!(fs::symlink_metadata(env.launcher()).unwrap().file_type().is_symlink());
    assert_eq!(tree(&env.home), before, "a refused install changed the home");
}

#[test]
fn symlink_in_place_of_the_desktop_entry_is_refused() {
    let env = setup("symlink-desktop");
    assert!(env.install().status.success());
    let victim = env.home.join("victim");
    fs::write(&victim, "precious\n").unwrap();
    fs::remove_file(env.desktop()).unwrap();
    symlink(&victim, env.desktop()).unwrap();
    let o = env.install();
    assert!(!o.status.success() && text(&o).contains("PANPAK-E2001"), "{}", text(&o));
    assert_eq!(fs::read_to_string(&victim).unwrap(), "precious\n");
}

#[test]
fn foreign_files_are_not_overwritten() {
    let env = setup("foreign");
    assert!(env.install().status.success());
    for (path, content) in [
        (env.desktop(), "[Desktop Entry]\nType=Application\nName=Mine\nExec=/usr/bin/mine\n"),
        (env.launcher(), "#!/bin/sh\necho mine\n"),
    ] {
        fs::write(&path, content).unwrap();
        let o = env.install();
        assert!(!o.status.success() && text(&o).contains("PANPAK-E2001"), "{}: {}", path.display(), text(&o));
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        // Remove the foreign file and the same command works again.
        fs::remove_file(&path).unwrap();
        let o = env.install();
        assert!(o.status.success(), "{}", text(&o));
    }
    // A launcher of another app is not ours either.
    let other = fs::read_to_string(env.launcher()).unwrap().replace(&format!("# X-Panpak-App-Id: {ID}"), "# X-Panpak-App-Id: org.example.other");
    fs::write(env.launcher(), &other).unwrap();
    let o = env.install();
    assert!(!o.status.success() && text(&o).contains("PANPAK-E2001"), "{}", text(&o));
    assert_eq!(fs::read_to_string(env.launcher()).unwrap(), other);
}

#[test]
fn an_entry_under_the_old_name_is_replaced_but_a_foreign_one_stays() {
    let env = setup("legacy-desktop");
    assert!(env.install().status.success());
    let old = env.home.join(".local/share/applications").join(format!("{ID}.desktop"));
    // What an earlier version wrote under the old name.
    let ours = fs::read_to_string(env.desktop()).unwrap();
    fs::write(&old, &ours).unwrap();
    let o = env.install();
    assert!(o.status.success(), "{}", text(&o));
    assert!(!old.exists(), "the old entry is still there");
    assert!(env.desktop().exists());
    assert!(text(&o).contains(&format!("panpak.{ID}.desktop")), "{}", text(&o));
    // An entry of that name that panpak did not write (the distribution's or a Flatpak's) is left alone.
    let theirs = "[Desktop Entry]\nType=Application\nName=Theirs\nExec=/usr/bin/theirs\n";
    fs::write(&old, theirs).unwrap();
    let o = env.install();
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(fs::read_to_string(&old).unwrap(), theirs);
}

#[test]
fn the_window_class_of_the_manifest_goes_into_the_entry() {
    let env = setup_with("wm-class", "[desktop]\nwm_class = \"Audacity\"\n");
    assert!(env.install().status.success());
    let d = fs::read_to_string(env.desktop()).unwrap();
    assert!(d.contains("StartupWMClass=Audacity\n"), "{d}");
    let env = setup("no-wm-class");
    assert!(env.install().status.success());
    assert!(!fs::read_to_string(env.desktop()).unwrap().contains("StartupWMClass"));
}

#[test]
fn install_runtime_takes_the_flag_before_or_after_the_file_and_refuses_a_folder() {
    let env = setup("runtime-args");
    let stub = env.home.join("stub");
    let o = cli(&env.home, &["install-runtime", "--allow-unsigned", stub.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let o = cli(&env.home, &["install-runtime", env.home.to_str().unwrap(), "--allow-unsigned"]);
    assert!(!o.status.success(), "a folder is not a runtime");
    let o = cli(&env.home, &["install-runtime", "--allow-unsigned"]);
    assert!(text(&o).contains("install-runtime <file>"), "{}", text(&o));
}
