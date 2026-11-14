// SPDX-License-Identifier: MIT OR Apache-2.0
// `panpak list` and `panpak remove`: what they take away, what they keep, what they refuse, and that
// a link put in place of any folder they touch never makes them delete anything outside.
use std::{
    fs::{self, File},
    os::unix::{
        fs::{symlink, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn work(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("remove-{name}-{}", std::process::id()));
    let _ = Command::new("chmod").args(["-R", "u+w"]).arg(&d).output();
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("run")).unwrap();
    fs::set_permissions(d.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
    d
}

fn cmd(home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_panpak"));
    c.args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env_remove("PANPAK_SECRET_FILE")
        .env_remove("PANPAK_TEST_KILL_AT")
        // The messages are in English whatever the language of the user who runs the tests.
        .env_remove("LANGUAGE")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null());
    c
}

fn cli(home: &Path, args: &[&str]) -> Output {
    cmd(home, args).output().unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

struct Env {
    home: PathBuf,
    key: PathBuf,
    pubkey: String,
}

/// The scratch folder holds a copy of the `panpak` binary (`install` puts one in the store), which is about 100 MB in a debug
/// build: it is removed when the test ends, unless the test failed (then it stays for a look).
impl Drop for Env {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::process::Command::new("chmod").args(["-R", "u+w"]).arg(&self.home).output();
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }
}

fn env(name: &str) -> Env {
    let home = work(name);
    let key = home.join("secret.key");
    let o = cli(&home, &["keygen", "-o", key.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let pubkey = text(&o).lines().find_map(|l| l.strip_prefix("key:    ")).unwrap().trim().to_string();
    let stub = home.join("stub");
    fs::write(&stub, "#!/bin/sh\necho 1\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let o = cli(&home, &["install-runtime", stub.to_str().unwrap(), "--allow-unsigned"]);
    assert!(o.status.success(), "{}", text(&o));
    Env { home, key, pubkey }
}

impl Env {
    /// Pack an app: same `body` gives the same tree root, so two apps can share a tree.
    fn pack(&self, id: &str, version: &str, body: &str) -> PathBuf {
        let tree = self.home.join(format!("tree-{id}-{version}"));
        fs::create_dir_all(tree.join("bin")).unwrap();
        fs::write(tree.join("bin/hello"), format!("#!/bin/sh\necho {body}\n")).unwrap();
        fs::set_permissions(tree.join("bin/hello"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            tree.join("panpak.toml"),
            format!("[app]\nid = \"{id}\"\nname = \"App {id}\"\nversion = \"{version}\"\nentry = \"bin/hello\"\n[update]\nkey = \"{}\"\n", self.pubkey),
        )
        .unwrap();
        let pkg = self.home.join(format!("{id}-{version}.src.panpak"));
        let stub = self.home.join("stub");
        let o = cli(&self.home, &["pack", tree.to_str().unwrap(), "--stub", stub.to_str().unwrap(), "--secret-file", self.key.to_str().unwrap(), "-o", pkg.to_str().unwrap()]);
        assert!(o.status.success(), "{}", text(&o));
        pkg
    }

    fn install(&self, id: &str, version: &str, body: &str) -> String {
        let pkg = self.pack(id, version, body);
        let o = cli(&self.home, &["install", "--yes", pkg.to_str().unwrap()]);
        assert!(o.status.success(), "{}", text(&o));
        root_of(&pkg)
    }

    fn store(&self) -> PathBuf {
        self.home.join(".local/share/panpak")
    }
    fn launcher(&self, id: &str) -> PathBuf {
        self.home.join(".local/bin").join(id)
    }
    fn desktop(&self, id: &str) -> PathBuf {
        self.home.join(".local/share/applications").join(format!("panpak.{id}.desktop"))
    }
    fn userdata(&self, id: &str) -> PathBuf {
        self.store().join("userdata").join(id)
    }
    fn cache(&self, id: &str) -> PathBuf {
        self.home.join(".cache/panpak").join(id)
    }
    fn remove(&self, args: &[&str]) -> Output {
        cli(&self.home, &[&["remove"], args].concat())
    }
    /// What an app leaves in the store once it has run: an unpacked tree and a farm, read-only like the real ones.
    fn fake_tree(&self, root: &str) {
        let t = self.store().join("trees").join(root);
        fs::create_dir_all(t.join("bin/sub")).unwrap();
        fs::write(t.join("bin/sub/file"), "x").unwrap();
        fs::write(self.store().join("trees").join(format!("{root}.readonly")), "").unwrap();
        for p in ["bin/sub", "bin", ""] {
            fs::set_permissions(t.join(p), fs::Permissions::from_mode(0o555)).unwrap();
        }
        let f = self.store().join("farms").join(root);
        fs::create_dir_all(&f).unwrap();
        fs::write(f.join("lib"), "x").unwrap();
    }
    /// User data and cache with a file each, and the mark "in use".
    fn fake_data(&self, id: &str) {
        for d in [self.userdata(id).join("data"), self.cache(id)] {
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("file"), "user data").unwrap();
        }
        fs::write(self.store().join("userdata").join(format!("{id}.in-use")), "").unwrap();
    }
    fn shared_tmp(&self, id: &str) {
        let d = self.home.join("run/panpak").join(id);
        fs::create_dir_all(d.join("tmp")).unwrap();
        fs::write(d.join("tmp/sock"), "").unwrap();
    }
}

/// The tree root of a package: the last 32 bytes of its footer, as hex.
fn root_of(pkg: &Path) -> String {
    let b = fs::read(pkg).unwrap();
    b[b.len() - 32..].iter().map(|x| format!("{x:02x}")).collect()
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir).map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    v.sort();
    v
}

const A: &str = "org.example.alpha";
const B: &str = "org.example.beta";

#[test]
fn remove_takes_the_entry_out_of_mimeapps_list_and_leaves_the_rest() {
    let e = env("mimeapps");
    let root = e.install(A, "1", "a");
    e.fake_tree(&root);
    let list = e.home.join(".config/mimeapps.list");
    fs::create_dir_all(list.parent().unwrap()).unwrap();
    let mine = format!("panpak.{A}.desktop");
    fs::write(&list, format!("[Default Applications]\ntext/plain={mine};\nimage/png=eog.desktop;{mine};\n[Added Associations]\ntext/html=other.desktop;\n")).unwrap();
    let o = e.remove(&[A]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(fs::read_to_string(&list).unwrap(), "[Default Applications]\nimage/png=eog.desktop;\n[Added Associations]\ntext/html=other.desktop;\n");
    assert!(!e.desktop(A).exists());
}

#[test]
fn remove_takes_the_app_away_and_keeps_the_data_and_a_reinstall_works() {
    let e = env("basic");
    let root = e.install(A, "1", "a");
    e.fake_tree(&root);
    e.fake_data(A);
    e.shared_tmp(A);
    assert!(e.launcher(A).exists() && e.desktop(A).exists());
    let o = e.remove(&[A]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(!e.launcher(A).exists() && !e.desktop(A).exists(), "{t}");
    assert!(!e.store().join("apps").join(A).exists());
    assert!(names(&e.store().join("packages")).is_empty(), "{:?}", names(&e.store().join("packages")));
    assert!(names(&e.store().join("trees")).is_empty() && names(&e.store().join("farms")).is_empty());
    assert!(names(&e.store().join("removing")).is_empty());
    assert!(!e.home.join("run/panpak").join(A).exists(), "the shared /tmp is gone");
    // The data stays, and the output says where and how much.
    assert!(e.userdata(A).join("data/file").exists() && e.cache(A).join("file").exists());
    assert!(t.contains(e.userdata(A).to_str().unwrap()) && t.contains("--purge-data"), "{t}");
    // The runtime stays.
    assert!(e.store().join("runtimes/1/panpak-stub").exists());
    // The same id installs again and finds its data.
    let again = e.install(A, "1", "a");
    assert_eq!(again, root);
    assert!(e.launcher(A).exists() && e.desktop(A).exists());
    assert!(e.userdata(A).join("data/file").exists());
    assert!(text(&cli(&e.home, &["list"])).contains(A));
}

#[test]
fn a_running_copy_stops_the_removal_and_nothing_is_touched() {
    let e = env("alive");
    e.install(A, "1", "a");
    e.fake_data(A);
    let lock_dir = e.home.join("run/panpak").join(A);
    fs::create_dir_all(&lock_dir).unwrap();
    let lock = File::create(lock_dir.join("lock")).unwrap();
    // SAFETY: flock on a descriptor of this test.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) }, 0);
    for args in [&[A][..], &["--purge-data", "--yes", A][..]] {
        let o = e.remove(args);
        let t = text(&o);
        assert!(!o.status.success() && t.contains("PANPAK-E2002") && t.contains("is open. Close it and try again"), "{t}");
    }
    assert!(e.launcher(A).exists() && e.desktop(A).exists() && e.store().join("apps").join(A).join("current").exists());
    assert!(e.userdata(A).join("data/file").exists());
    drop(lock);
    assert!(e.remove(&[A]).status.success());
}

#[test]
fn purge_asks_and_without_an_answer_deletes_nothing() {
    let e = env("purge");
    e.install(A, "1", "a");
    e.fake_data(A);
    // No terminal and no --yes: refused, with the code, before anything is removed.
    let o = e.remove(&["--purge-data", A]);
    let t = text(&o);
    assert!(!o.status.success() && t.contains("PANPAK-E5004"), "{t}");
    assert!(e.launcher(A).exists() && e.userdata(A).join("data/file").exists() && e.cache(A).join("file").exists());
    // With --yes the data and the cache go, and the mark.
    let o = e.remove(&["--purge-data", "--yes", A]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!e.userdata(A).exists() && !e.cache(A).exists());
    assert!(!e.store().join("userdata").join(format!("{A}.in-use")).exists());
    assert!(!e.launcher(A).exists());
}

#[test]
fn a_shared_tree_stays_while_another_package_uses_it() {
    let e = env("shared");
    let ra = e.install(A, "1", "same");
    let rb = e.install(B, "1", "same");
    assert_eq!(ra, rb, "same files, same root");
    e.fake_tree(&ra);
    let o = e.remove(&[A]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(e.store().join("trees").join(&ra).exists() && e.store().join("farms").join(&ra).exists(), "B still uses it");
    assert!(names(&e.store().join("packages")) == vec![format!("{B}-1.panpak")]);
    assert!(e.remove(&[B]).status.success());
    assert!(!e.store().join("trees").join(&ra).exists() && !e.store().join("farms").join(&ra).exists());
}

#[test]
fn an_id_that_is_a_prefix_of_another_does_not_take_its_packages() {
    let e = env("prefix");
    e.install("org.example.app", "1", "a");
    e.install("org.example.app-extra", "1", "b");
    let o = e.remove(&["org.example.app"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(names(&e.store().join("packages")), vec!["org.example.app-extra-1.panpak".to_string()]);
    assert!(e.store().join("apps/org.example.app-extra/current").exists());
}

#[test]
fn old_versions_and_the_previous_link_go_with_the_app() {
    let e = env("versions");
    e.install(A, "1", "a");
    e.install(A, "2", "b");
    assert_eq!(names(&e.store().join("packages")).len(), 2);
    fs::write(e.store().join("apps").join(A).join("update.url"), "https://example.invalid/\n").unwrap();
    symlink(e.store().join("packages").join(format!("{A}-1.panpak")), e.store().join("apps").join(A).join("previous")).unwrap();
    assert!(e.remove(&[A]).status.success());
    assert!(names(&e.store().join("packages")).is_empty());
    assert!(!e.store().join("apps").join(A).exists());
}

#[test]
fn incomplete_and_hand_cleaned_installs_are_finished_without_errors() {
    let e = env("incomplete");
    // Only the key pin is left: no current link, no launcher, no entry, no package.
    fs::create_dir_all(e.store().join("apps").join(A)).unwrap();
    fs::write(e.store().join("apps").join(A).join("pinned-key"), "ed25519:x\n").unwrap();
    let t = text(&cli(&e.home, &["list"]));
    assert!(t.contains(A) && t.contains("incomplete"), "{t}");
    let o = e.remove(&[A]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!e.store().join("apps").join(A).exists());
    // An installed app whose launcher and entry were deleted by hand.
    e.install(B, "1", "b");
    fs::remove_file(e.launcher(B)).unwrap();
    fs::remove_file(e.desktop(B)).unwrap();
    let t = text(&cli(&e.home, &["list"]));
    assert!(t.contains("no launcher"), "{t}");
    let o = e.remove(&[B]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(names(&e.store().join("packages")).is_empty());
    // Only data left behind: removed with --purge-data, and listed as such.
    e.fake_data("org.example.gamma");
    let t = text(&cli(&e.home, &["list"]));
    assert!(t.contains("org.example.gamma") && t.contains("data-only"), "{t}");
    let o = e.remove(&["--purge-data", "--yes", "org.example.gamma"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!e.userdata("org.example.gamma").exists());
    // Nothing at all under an id is an error with a code.
    let o = e.remove(&["org.example.unknown"]);
    assert!(!o.status.success() && text(&o).contains("PANPAK-E2004"), "{}", text(&o));
    let o = e.remove(&["../x"]);
    assert!(!o.status.success());
}

#[test]
fn a_foreign_launcher_and_entry_are_left_alone() {
    let e = env("foreign");
    e.install(A, "1", "a");
    fs::write(e.launcher(A), "#!/bin/sh\necho mine\n").unwrap();
    fs::write(e.desktop(A), "[Desktop Entry]\nName=mine\n").unwrap();
    let legacy = e.home.join(".local/share/applications").join(format!("{A}.desktop"));
    fs::write(&legacy, "[Desktop Entry]\nName=other\n").unwrap();
    let o = e.remove(&[A]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(fs::read_to_string(e.launcher(A)).unwrap().contains("mine"));
    assert!(e.desktop(A).exists() && legacy.exists());
    assert!(t.contains("kept:"), "{t}");
    // An old entry that panpak wrote goes.
    e.install(B, "1", "b");
    let old = e.home.join(".local/share/applications").join(format!("{B}.desktop"));
    fs::write(&old, format!("[Desktop Entry]\nType=Application\nName=old\nX-Panpak-App-Id={B}\n")).unwrap();
    assert!(e.remove(&[B]).status.success());
    assert!(!old.exists());
}

/// Put something precious outside, and make `at` a link to it.
fn plant(e: &Env, at: &Path) -> PathBuf {
    let outside = e.home.join(format!("outside-{}", at.file_name().unwrap().to_string_lossy()));
    fs::create_dir_all(outside.join("deep")).unwrap();
    fs::write(outside.join("precious"), "keep").unwrap();
    fs::write(outside.join("deep/precious"), "keep").unwrap();
    let _ = Command::new("chmod").args(["-R", "u+w"]).arg(at).output();
    let _ = fs::remove_dir_all(at);
    let _ = fs::remove_file(at);
    fs::create_dir_all(at.parent().unwrap()).unwrap();
    symlink(&outside, at).unwrap();
    outside
}

fn intact(outside: &Path) {
    assert_eq!(fs::read_to_string(outside.join("precious")).unwrap(), "keep");
    assert_eq!(fs::read_to_string(outside.join("deep/precious")).unwrap(), "keep");
}

#[test]
fn a_link_in_place_of_any_folder_never_deletes_outside() {
    // Each case: install, make the place a link to a folder with precious files, remove with --purge-data.
    type Place = Box<dyn Fn(&Env) -> PathBuf>;
    let places: Vec<(&str, Place)> = vec![
        ("apps/<id>", Box::new(|e| e.store().join("apps").join(A))),
        ("userdata/<id>", Box::new(|e| e.userdata(A))),
        ("cache/<id>", Box::new(|e| e.cache(A))),
        ("trees/<root>", Box::new(|e| e.store().join("trees").join(root_of(&e.home.join(format!("{A}-1.src.panpak")))))),
        ("farms/<root>", Box::new(|e| e.store().join("farms").join(root_of(&e.home.join(format!("{A}-1.src.panpak")))))),
        ("packages", Box::new(|e| e.store().join("packages"))),
        ("trees", Box::new(|e| e.store().join("trees"))),
        ("userdata", Box::new(|e| e.store().join("userdata"))),
        ("run/panpak/<id>", Box::new(|e| e.home.join("run/panpak").join(A))),
        ("run/panpak", Box::new(|e| e.home.join("run/panpak"))),
        ("removing", Box::new(|e| e.store().join("removing"))),
        (".cache/panpak", Box::new(|e| e.home.join(".cache/panpak"))),
    ];
    for (label, place) in &places {
        let e = env(&format!("link-{}", label.replace(['/', '<', '>'], "_")));
        let root = e.install(A, "1", "a");
        e.fake_tree(&root);
        e.fake_data(A);
        e.shared_tmp(A);
        let at = place(&e);
        let outside = plant(&e, &at);
        let o = e.remove(&["--purge-data", "--yes", A]);
        let _ = o; // refused or finished: either way nothing outside may change
        intact(&outside);
        // A second pass and a list do not follow it either.
        let _ = e.remove(&["--purge-data", "--yes", A]);
        let _ = cli(&e.home, &["list"]);
        intact(&outside);
        assert!(fs::symlink_metadata(&at).is_ok() || !at.exists(), "{label}");
    }
}

#[test]
fn links_inside_the_data_are_removed_and_not_followed() {
    let e = env("inner");
    e.install(A, "1", "a");
    e.fake_data(A);
    let outside = e.home.join("outside-inner");
    fs::create_dir_all(outside.join("deep")).unwrap();
    fs::write(outside.join("precious"), "keep").unwrap();
    fs::write(outside.join("deep/precious"), "keep").unwrap();
    symlink(&outside, e.userdata(A).join("data/home")).unwrap();
    symlink(outside.join("precious"), e.cache(A).join("file-link")).unwrap();
    symlink("/etc", e.userdata(A).join("config")).unwrap();
    let o = e.remove(&["--purge-data", "--yes", A]);
    assert!(o.status.success(), "{}", text(&o));
    intact(&outside);
    assert!(!e.userdata(A).exists() && !e.cache(A).exists());
    assert!(Path::new("/etc/passwd").exists());
}

#[test]
fn a_launcher_that_is_a_link_is_left_alone() {
    let e = env("launcher-link");
    e.install(A, "1", "a");
    let target = e.home.join("my-script");
    fs::write(&target, "#!/bin/sh\n# X-Panpak-App-Id: org.example.alpha\n").unwrap();
    fs::remove_file(e.launcher(A)).unwrap();
    symlink(&target, e.launcher(A)).unwrap();
    let o = e.remove(&[A]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(target.exists() && fs::symlink_metadata(e.launcher(A)).is_ok());
}

#[test]
fn a_removal_killed_after_the_move_is_finished_by_the_next_command() {
    for next in [&["list"][..], &["list", "--json"][..]] {
        let e = env(&format!("kill-{}", next.len()));
        let root = e.install(A, "1", "a");
        e.fake_tree(&root);
        let o = cmd(&e.home, &["remove", A]).env("PANPAK_TEST_KILL_AT", "moved").output().unwrap();
        assert!(!o.status.success(), "the process was killed");
        // Killed after the rename: the app is already out of apps/, the rest is waiting in removing/.
        assert!(!e.store().join("apps").join(A).exists());
        assert!(e.store().join("packages").join(format!("{A}-1.panpak")).exists());
        assert!(e.store().join("trees").join(&root).exists());
        assert_eq!(names(&e.store().join("removing")).len(), 1);
        let o = cli(&e.home, next);
        assert!(o.status.success(), "{}", text(&o));
        assert!(String::from_utf8_lossy(&o.stderr).contains("finished an interrupted removal"), "{}", text(&o));
        assert!(names(&e.store().join("packages")).is_empty() && names(&e.store().join("trees")).is_empty());
        assert!(names(&e.store().join("farms")).is_empty() && names(&e.store().join("removing")).is_empty());
    }
}

#[test]
fn a_removal_killed_before_the_move_leaves_the_app_installed() {
    let e = env("kill-early");
    let root = e.install(A, "1", "a");
    e.fake_tree(&root);
    let o = cmd(&e.home, &["remove", A]).env("PANPAK_TEST_KILL_AT", "journal").output().unwrap();
    assert!(!o.status.success());
    // The journal was not final and the app was not moved: the next command must not delete its packages.
    let o = cli(&e.home, &["list"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(e.store().join("apps").join(A).join("current").exists());
    assert!(e.store().join("packages").join(format!("{A}-1.panpak")).exists());
    assert!(e.store().join("trees").join(&root).exists());
    // And the app can be removed afterwards.
    assert!(e.remove(&[A]).status.success());
    assert!(names(&e.store().join("packages")).is_empty());
}

#[test]
fn a_reinstall_after_an_interrupted_removal_is_not_hit_by_it() {
    let e = env("kill-reinstall");
    e.install(A, "1", "a");
    let o = cmd(&e.home, &["remove", A]).env("PANPAK_TEST_KILL_AT", "moved").output().unwrap();
    assert!(!o.status.success());
    // `install` finishes the old removal first, then writes the new package.
    e.install(A, "1", "a");
    assert!(e.store().join("packages").join(format!("{A}-1.panpak")).exists());
    assert!(e.store().join("apps").join(A).join("current").exists());
    assert!(names(&e.store().join("removing")).is_empty());
}

#[test]
fn a_damaged_journal_deletes_nothing_it_names() {
    let e = env("bad-journal");
    e.install(A, "1", "a");
    let entry = e.store().join("removing").join("x.1");
    fs::create_dir_all(entry.join("app")).unwrap();
    fs::write(entry.join("journal"), format!("panpak-removal 1\napp {A}\npackage ../../secret.key\n")).unwrap();
    let o = cli(&e.home, &["list"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("could not finish"), "{}", text(&o));
    assert!(e.home.join("secret.key").exists() && e.store().join("packages").join(format!("{A}-1.panpak")).exists());
}

#[test]
fn list_with_none_one_and_several_apps() {
    let e = env("list");
    let t = text(&cli(&e.home, &["list"]));
    assert!(t.contains("no apps installed"), "{t}");
    assert_eq!(text(&cli(&e.home, &["list", "--json"])).trim(), "[]");
    e.install(A, "1", "a");
    let t = text(&cli(&e.home, &["list"]));
    assert!(t.contains("ID") && t.contains(A) && t.contains("App org.example.alpha") && t.contains(" ok"), "{t}");
    e.install(B, "2", "b");
    e.fake_data(B);
    let t = text(&cli(&e.home, &["list"]));
    assert!(t.contains(A) && t.contains(B), "{t}");
    let j = String::from_utf8(cli(&e.home, &["list", "--json"]).stdout).unwrap();
    // The output parses as JSON and has the fields.
    if let Ok(o) = Command::new("python3").args(["-I", "-c", "import json,sys; d=json.load(sys.stdin); assert len(d)==2 and d[0]['id']=='org.example.alpha' and d[1]['version']=='2' and d[1]['data_bytes']>0 and d[0]['data_bytes'] is None and d[0]['status']=='ok', d; print('json ok')"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().and_then(|mut c| {
        use std::io::Write;
        c.stdin.take().unwrap().write_all(j.as_bytes())?;
        c.wait_with_output()
    }) {
        assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    }
    assert!(j.starts_with("[\n") && j.contains("\"package_bytes\":"), "{j}");
    // Unknown arguments are refused with a code.
    let o = cli(&e.home, &["list", "--bogus"]);
    assert!(!o.status.success() && text(&o).contains("PANPAK-E5001"));
    let o = cli(&e.home, &["remove", "--bogus", A]);
    assert!(!o.status.success() && text(&o).contains("PANPAK-E5001"));
    let o = cli(&e.home, &["remove"]);
    assert!(!o.status.success() && text(&o).contains("PANPAK-E5002"));
}

#[test]
fn control_characters_in_a_name_do_not_reach_the_terminal() {
    let e = env("ctrl");
    let pkg = e.pack(A, "1", "a");
    // A name with an escape sequence cannot be packed; the list cleans what it prints anyway (unit test),
    // so here only check that the plain output has no escape byte.
    let o = cli(&e.home, &["install", "--yes", pkg.to_str().unwrap()]);
    assert!(o.status.success());
    assert!(!cli(&e.home, &["list"]).stdout.contains(&0x1b));
}

#[test]
fn a_data_folder_that_is_a_link_is_reported_and_left_for_the_user() {
    for which in ["userdata", "cache"] {
        let e = env(&format!("datalink-{which}"));
        e.install(A, "1", "a");
        e.fake_data(A);
        let at = if which == "userdata" { e.userdata(A) } else { e.cache(A) };
        let outside = plant(&e, &at);
        let o = e.remove(&["--purge-data", "--yes", A]);
        let t = text(&o);
        // The app itself is gone; the link is not followed and not removed, and the exit status says it is not complete.
        assert!(!o.status.success() && t.contains("PANPAK-E2003") && t.contains("not followed"), "{t}");
        assert!(!e.launcher(A).exists() && !e.store().join("apps").join(A).exists());
        assert!(fs::symlink_metadata(&at).unwrap().file_type().is_symlink());
        intact(&outside);
    }
}

// --- the menu action "Uninstall" and `remove --ask`

/// A folder with fake tools: each records its arguments in `<name>.args` and exits with the given status.
fn fake_tools(home: &Path, tools: &[(&str, i32)]) -> PathBuf {
    let dir = home.join("fake-tools");
    fs::create_dir_all(&dir).unwrap();
    for (name, status) in tools {
        let p = dir.join(name);
        fs::write(&p, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit {status}\n", dir.join(format!("{name}.args")).display())).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

/// The folder of fake tools is read only by a debug build of `panpak`. A release build looks in `/usr/bin`, where a
/// real dialog would open on the desktop of whoever runs the tests: those tests do not run then.
macro_rules! only_with_fake_tools {
    () => {
        if !cfg!(debug_assertions) {
            eprintln!("skipped: this build of panpak does not read PANPAK_TEST_TOOLS_DIR (run the tests without --release)");
            return;
        }
    };
}

fn ask(e: &Env, tools: &Path, id: &str) -> Output {
    cmd(&e.home, &["remove", "--ask", id]).env("PANPAK_TEST_TOOLS_DIR", tools).env("XDG_CURRENT_DESKTOP", "GNOME").output().unwrap()
}

#[test]
fn the_entry_has_an_uninstall_action_that_runs_the_stable_panpak() {
    let e = env("action");
    e.install(A, "1", "a");
    let d = fs::read_to_string(e.desktop(A)).unwrap();
    // The action runs the copy of panpak in the store, never the binary that happened to run `install`.
    let exe = e.store().join("bin/panpak");
    let exe = exe.to_str().unwrap();
    assert!(exe != env!("CARGO_BIN_EXE_panpak") && Path::new(exe).is_file());
    assert!(d.contains("\nActions=Uninstall;\n"), "{d}");
    assert!(d.contains("\n[Desktop Action Uninstall]\nName=Uninstall\n"), "{d}");
    assert!(d.contains("Name[it]=Disinstalla\n") && d.contains("Name[de]=") && d.contains("Name[fr]=") && d.contains("Name[es]="), "{d}");
    assert!(d.contains(&format!("Exec=\"{exe}\" remove --ask {A}\n")), "{d}");
    // The main group still comes first and carries the marker, so the entry is still recognised as ours.
    assert!(d.starts_with("[Desktop Entry]\n") && d.find("X-Panpak-App-Id=").unwrap() < d.find("[Desktop Action").unwrap(), "{d}");
    assert!(e.remove(&[A]).status.success() && !e.desktop(A).exists());
}

#[test]
fn ask_without_a_dialog_tool_removes_nothing_and_says_the_command() {
    only_with_fake_tools!();
    let e = env("ask-none");
    e.install(A, "1", "a");
    e.fake_data(A);
    let tools = fake_tools(&e.home, &[]);
    let o = ask(&e, &tools, A);
    let t = text(&o);
    assert!(!o.status.success() && t.contains("PANPAK-E5004") && t.contains("remove "), "{t}");
    assert!(e.launcher(A).exists() && e.desktop(A).exists() && e.userdata(A).join("data/file").exists());
    // With only a notification tool, the explanation goes there.
    let tools = fake_tools(&e.home, &[("notify-send", 0)]);
    let o = ask(&e, &tools, A);
    assert!(!o.status.success());
    let n = fs::read_to_string(tools.join("notify-send.args")).unwrap();
    assert!(n.contains("nothing was removed") && n.contains(&format!("remove {A}")), "{n}");
    assert!(e.launcher(A).exists());
}

#[test]
fn ask_with_the_answer_no_removes_nothing() {
    only_with_fake_tools!();
    let e = env("ask-no");
    e.install(A, "1", "a");
    e.fake_data(A);
    let tools = fake_tools(&e.home, &[("zenity", 1), ("notify-send", 0)]);
    let o = ask(&e, &tools, A);
    assert!(o.status.success(), "{}", text(&o));
    assert!(e.launcher(A).exists() && e.desktop(A).exists() && e.store().join("apps").join(A).join("current").exists());
    assert!(e.userdata(A).join("data/file").exists());
    // The dialog tells in plain words what is removed and what stays; no command, no path, and no stray "?" (a new line is a new line).
    let q = fs::read_to_string(tools.join("zenity.args")).unwrap();
    assert!(q.contains("--question") && q.contains(&format!("Remove App {A}?")), "{q}");
    assert!(q.contains("Your settings and data are kept (") && q.contains("If you install the app again, they are still there."), "{q}");
    assert!(!q.contains("--purge-data") && !q.contains(e.userdata(A).to_str().unwrap()) && !q.contains("terminal"), "{q}");
    assert_eq!(q.matches('?').count(), 1, "only the question mark of the question: {q}");
    assert!(!tools.join("notify-send.args").exists(), "no notification for a refusal by the user");
}

#[test]
fn ask_with_the_answer_yes_removes_the_app_keeps_the_data_and_notifies() {
    only_with_fake_tools!();
    let e = env("ask-yes");
    e.install(A, "1", "a");
    e.fake_data(A);
    let tools = fake_tools(&e.home, &[("zenity", 0), ("notify-send", 0)]);
    let o = ask(&e, &tools, A);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!e.launcher(A).exists() && !e.desktop(A).exists() && !e.store().join("apps").join(A).exists());
    assert!(e.userdata(A).join("data/file").exists() && e.cache(A).join("file").exists(), "the data stays");
    let n = fs::read_to_string(tools.join("notify-send.args")).unwrap();
    assert!(n.contains(&format!("App {A} was removed. Your settings and data are kept.")), "{n}");
    // KDE gets kdialog first.
    let e = env("ask-kde");
    e.install(A, "1", "a");
    let tools = fake_tools(&e.home, &[("zenity", 0), ("kdialog", 1)]);
    let o = cmd(&e.home, &["remove", "--ask", A]).env("PANPAK_TEST_TOOLS_DIR", &tools).env("XDG_CURRENT_DESKTOP", "KDE").output().unwrap();
    assert!(o.status.success() && e.launcher(A).exists(), "{}", text(&o));
    assert!(tools.join("kdialog.args").exists() && !tools.join("zenity.args").exists());
}

/// The words come from `po/it.po`, so this file has none of its own.
fn italian(id: &'static str) -> String {
    panpak_core::i18n::catalogs().iter().find(|c| c.name == "it").and_then(|c| c.entries.iter().find(|e| e.0 == id)).map(|e| e.1.to_string()).expect("po/it.po has the message")
}

#[test]
fn the_dialog_and_the_notification_speak_the_language_of_the_system() {
    only_with_fake_tools!();
    use panpak_core::messages as m;
    let e = env("ask-it");
    e.install(A, "1", "a");
    e.fake_data(A);
    let tools = fake_tools(&e.home, &[("zenity", 0), ("notify-send", 0)]);
    let o = cmd(&e.home, &["remove", "--ask", A]).env("PANPAK_TEST_TOOLS_DIR", &tools).env("XDG_CURRENT_DESKTOP", "GNOME").env("LC_ALL", "it_IT.UTF-8").output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let q = fs::read_to_string(tools.join("zenity.args")).unwrap();
    let ask = italian(m::REMOVE_ASK).replace("{name}", &format!("App {A}"));
    assert!(q.contains(&ask) && q.contains(&italian(m::REMOVE_WHAT)) && q.contains(&italian(m::REMOVE_TITLE)) && q.contains(&italian(m::REMOVE_OK_LABEL)), "{q}");
    assert!(!q.contains("Remove App") && !q.contains("are kept") && !q.contains("--purge-data"), "{q}");
    assert_eq!(q.matches('?').count(), 1, "{q}");
    let n = fs::read_to_string(tools.join("notify-send.args")).unwrap();
    assert!(n.contains(&italian(m::REMOVED_TITLE)) && n.contains(&italian(m::REMOVED_KEPT).replace("{name}", &format!("App {A}"))), "{n}");
}

#[test]
fn ask_with_a_running_copy_or_a_wrong_id_removes_nothing_and_says_why() {
    only_with_fake_tools!();
    let e = env("ask-busy");
    e.install(A, "1", "a");
    let lock_dir = e.home.join("run/panpak").join(A);
    fs::create_dir_all(&lock_dir).unwrap();
    let lock = File::create(lock_dir.join("lock")).unwrap();
    // SAFETY: flock on a descriptor of this test.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) }, 0);
    let tools = fake_tools(&e.home, &[("zenity", 0), ("notify-send", 0)]);
    let o = ask(&e, &tools, A);
    assert!(!o.status.success() && text(&o).contains("PANPAK-E2002"), "{}", text(&o));
    let n = fs::read_to_string(tools.join("notify-send.args")).unwrap();
    assert!(n.contains("is open. Close it and try again"), "{n}");
    assert!(e.launcher(A).exists());
    drop(lock);
    let o = ask(&e, &tools, "org.example.unknown");
    assert!(!o.status.success() && text(&o).contains("PANPAK-E2004"));
    let o = cli(&e.home, &["remove", "--ask", "--yes", A]);
    assert!(!o.status.success() && text(&o).contains("PANPAK-E5001"));
}

#[test]
fn remove_reports_version_and_space_and_a_second_run_has_nothing_to_do() {
    let e = env("report");
    e.install(A, "1.2", "a");
    e.fake_data(A);
    let o = e.remove(&[A]);
    assert!(o.status.success(), "{}", text(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    let first = out.lines().next().unwrap();
    assert!(first.starts_with(&format!("removed {A} 1.2: freed ")), "{out}");
    assert!(out.contains("kept (use `panpak remove --purge-data"), "{out}");
    assert!(out.contains("launcher ") && out.contains("desktop entry "), "{out}");
    // The same command again: nothing to do, said plainly, with the code and a failing exit; no "removed" line.
    let o = e.remove(&[A]);
    let (so, se) = (String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(!o.status.success() && se.contains("PANPAK-E2004") && se.contains(&format!("{A} is not installed: no app to remove")), "{so}{se}");
    // The data of the removed app is still there, so the message says how to delete it.
    assert!(se.contains("settings and data") && se.contains("--purge-data"), "{se}");
    assert!(!so.contains("removed"), "{so}");
    // Without the data there is just nothing.
    let o = e.remove(&["--purge-data", "--yes", A]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(String::from_utf8_lossy(&o.stdout).lines().any(|l| l.starts_with(&format!("removed {A}: freed "))), "{}", text(&o));
    let o = e.remove(&[A]);
    let se = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && se.contains("it was removed already, or never installed") && se.contains("panpak list"), "{se}");
}

#[test]
fn freed_space_leaves_out_a_tree_that_another_app_uses() {
    let e = env("freed");
    let root = e.install(A, "1", "same");
    e.install(B, "1", "same");
    e.fake_tree(&root);
    let tree_len = |o: &Output| String::from_utf8_lossy(&o.stdout).lines().next().unwrap().to_string();
    let o = e.remove(&[A]);
    let first = tree_len(&o);
    assert!(o.status.success() && first.starts_with(&format!("removed {A} 1: freed ")), "{}", text(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("stays (another package uses it)"), "{}", text(&o));
    // B was the last user: its tree goes with it, so more is freed.
    let o = e.remove(&[B]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!e.store().join("trees").join(&root).exists(), "the tree stays after the last app");
}

#[test]
fn list_of_an_empty_store_says_how_to_install() {
    let e = env("list-empty");
    let t = text(&cli(&e.home, &["list"]));
    assert!(t.contains("no apps installed. Install one with `panpak install <file.panpak>`."), "{t}");
    e.install(A, "1", "a");
    let t = String::from_utf8_lossy(&cli(&e.home, &["list"]).stdout).to_string();
    // The table, a blank line, then the explanation of the columns.
    let lines: Vec<&str> = t.lines().collect();
    assert!(lines[0].starts_with("ID") && lines[1].starts_with(A) && lines[2].is_empty() && lines[3].starts_with("APP is "), "{t}");
}
