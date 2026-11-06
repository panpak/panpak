// SPDX-License-Identifier: MIT OR Apache-2.0
// Step 7b: stable paths in the launcher and the entry, the stub link, icons. Everything runs in a scratch HOME.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn work(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("desktop-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("run")).unwrap();
    fs::set_permissions(d.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
    d
}

fn cmd_with(bin: &Path, home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(bin);
    c.args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env_remove("PANPAK_SECRET_FILE")
        .stdin(std::process::Stdio::null());
    c
}

fn cli(home: &Path, args: &[&str]) -> Output {
    cmd_with(Path::new(env!("CARGO_BIN_EXE_panpak")), home, args).output().unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn fixture(name: &str) -> Vec<u8> {
    fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../panpak-icon/tests/data").join(name)).unwrap()
}

const ID: &str = "org.example.icon";

struct Env {
    home: PathBuf,
    key: PathBuf,
    pubkey: String,
}

fn script_stub(home: &Path, name: &str, abi: u32) -> PathBuf {
    let stub = home.join(name);
    fs::write(&stub, format!("#!/bin/sh\necho {abi}\n")).unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    stub
}

fn env(name: &str) -> Env {
    let home = work(name);
    let key = home.join("secret.key");
    let o = cli(&home, &["keygen", "-o", key.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let pubkey = text(&o).lines().find_map(|l| l.strip_prefix("key:    ")).unwrap().trim().to_string();
    let stub = script_stub(&home, "stub", 1);
    let o = cli(&home, &["install-runtime", stub.to_str().unwrap(), "--allow-unsigned"]);
    assert!(o.status.success(), "{}", text(&o));
    Env { home, key, pubkey }
}

impl Env {
    fn store(&self) -> PathBuf {
        self.home.join(".local/share/panpak")
    }
    fn tree(&self, version: &str, extra_toml: &str, files: &[(&str, Vec<u8>)]) -> PathBuf {
        let tree = self.home.join(format!("tree-{version}"));
        let _ = fs::remove_dir_all(&tree);
        fs::create_dir_all(tree.join("bin")).unwrap();
        fs::write(tree.join("bin/hello"), "#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(tree.join("bin/hello"), fs::Permissions::from_mode(0o755)).unwrap();
        for (p, data) in files {
            fs::create_dir_all(tree.join(p).parent().unwrap()).unwrap();
            fs::write(tree.join(p), data).unwrap();
        }
        fs::write(
            tree.join("panpak.toml"),
            format!("[app]\nid = \"{ID}\"\nname = \"Icon App\"\nversion = \"{version}\"\nentry = \"bin/hello\"\n{extra_toml}[update]\nkey = \"{}\"\n", self.pubkey),
        )
        .unwrap();
        tree
    }
    fn pack(&self, tree: &Path, out: &str) -> (Output, PathBuf) {
        let pkg = self.home.join(out);
        let o = cli(&self.home, &["pack", tree.to_str().unwrap(), "--stub", self.home.join("stub").to_str().unwrap(), "--secret-file", self.key.to_str().unwrap(), "-o", pkg.to_str().unwrap()]);
        (o, pkg)
    }
    fn install(&self, version: &str, extra_toml: &str, files: &[(&str, Vec<u8>)]) -> Output {
        let t = self.tree(version, extra_toml, files);
        let (o, pkg) = self.pack(&t, &format!("{version}.panpak"));
        assert!(o.status.success(), "{}", text(&o));
        cli(&self.home, &["install", "--yes", pkg.to_str().unwrap()])
    }
    fn icon(&self, side: u32) -> PathBuf {
        self.home.join(format!(".local/share/icons/hicolor/{side}x{side}/apps/panpak.{ID}.png"))
    }
    fn entry(&self) -> String {
        fs::read_to_string(self.home.join(format!(".local/share/applications/panpak.{ID}.desktop"))).unwrap()
    }
    fn launcher(&self) -> String {
        fs::read_to_string(self.home.join(".local/bin").join(ID)).unwrap()
    }
}

#[test]
fn launcher_and_entry_name_only_the_store_whatever_ran_install() {
    let e = env("stable");
    // `install` runs from a copy of the binary in a folder that will go away.
    let tmp = e.home.join("scratch-build");
    fs::create_dir_all(&tmp).unwrap();
    let copy = tmp.join("panpak");
    fs::copy(env!("CARGO_BIN_EXE_panpak"), &copy).unwrap();
    let t = e.tree("1", "", &[]);
    let (o, pkg) = e.pack(&t, "1.panpak");
    assert!(o.status.success(), "{}", text(&o));
    let o = cmd_with(&copy, &e.home, &["install", "--yes", pkg.to_str().unwrap()]).output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let (launcher, entry) = (e.launcher(), e.entry());
    for t in [&launcher, &entry] {
        assert!(!t.contains("scratch-build"), "a path of the build folder is in:\n{t}");
    }
    let store = e.store();
    assert!(launcher.contains(&format!("exec '{}' run '{}'", store.join("bin/panpak-stub").display(), store.join(format!("apps/{ID}/current")).display())), "{launcher}");
    assert!(!launcher.contains("runtimes/"), "{launcher}");
    assert!(entry.contains(&format!("Exec=\"{}\" remove --ask {ID}\n", store.join("bin/panpak").display())), "{entry}");
    // The stable files: a copy of the CLI that runs, and a relative link to the runtime.
    let stable = store.join("bin/panpak");
    assert_eq!(fs::metadata(&stable).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(fs::read(&stable).unwrap(), fs::read(&copy).unwrap());
    assert_eq!(fs::read_link(store.join("bin/panpak-stub")).unwrap(), Path::new("../runtimes/1/panpak-stub"));
    assert!(text(&o).contains("copied panpak to"), "{}", text(&o));
}

#[test]
fn an_installed_panpak_is_never_replaced_and_an_older_one_is_reported() {
    let e = env("keep");
    fs::create_dir_all(e.store().join("bin")).unwrap();
    let mine = e.store().join("bin/panpak");
    let say = |v: &str| {
        fs::write(&mine, format!("#!/bin/sh\n[ \"$1\" = --version ] && echo 'panpak {v}'\n")).unwrap();
        fs::set_permissions(&mine, fs::Permissions::from_mode(0o755)).unwrap();
    };
    say("0.0.1");
    let before = fs::read(&mine).unwrap();
    let o = e.install("1", "", &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(fs::read(&mine).unwrap(), before, "the stable panpak was replaced");
    let t = text(&o);
    assert!(t.contains("is version 0.0.1, older than this one") && t.contains("install-runtime"), "{t}");
    // The same version: no warning. A newer one: none either.
    for v in [env!("CARGO_PKG_VERSION"), "99.0.0"] {
        say(v);
        let o = e.install("2", "", &[]);
        assert!(o.status.success() && !text(&o).contains("older than this one"), "{v}: {}", text(&o));
    }
}

#[test]
fn a_new_runtime_reaches_the_launcher_through_the_link() {
    let e = env("link");
    assert!(e.install("1", "", &[]).status.success());
    let launcher = e.launcher();
    let run = || String::from_utf8_lossy(&Command::new(e.home.join(".local/bin").join(ID)).output().unwrap().stdout).trim().to_string();
    assert_eq!(run(), "1");
    let o = cli(&e.home, &["install-runtime", script_stub(&e.home, "stub2", 2).to_str().unwrap(), "--allow-unsigned"]);
    assert!(o.status.success(), "{}", text(&o));
    // The launcher is the same file; the link moved.
    assert_eq!(e.launcher(), launcher);
    assert_eq!(fs::read_link(e.store().join("bin/panpak-stub")).unwrap(), Path::new("../runtimes/2/panpak-stub"));
    assert_eq!(run(), "2");
    // The old runtime stays for launchers written before the link existed.
    assert!(e.store().join("runtimes/1/panpak-stub").is_file());
    // No temporary link is left.
    let names: Vec<_> = fs::read_dir(e.store().join("bin")).unwrap().flatten().map(|d| d.file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().all(|n| !n.contains("tmp")), "{names:?}");
}

#[test]
fn icons_are_installed_replaced_and_removed() {
    let e = env("icons");
    let (i16, i48) = (fixture("ok-16-rgba.png"), fixture("ok-48-rgba.png"));
    let toml = "[desktop]\nicons = [\"icons/a.png\", \"icons/b.png\"]\n";
    let o = e.install("1", toml, &[("icons/a.png", i16.clone()), ("icons/b.png", i48.clone())]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!text(&o).contains("NOTE: the menu entry changed"), "{}", text(&o));
    assert_eq!(fs::read(e.icon(16)).unwrap(), i16);
    assert_eq!(fs::read(e.icon(48)).unwrap(), i48);
    assert_eq!(fs::metadata(e.icon(48)).unwrap().permissions().mode() & 0o777, 0o644);
    assert!(e.entry().contains(&format!("\nIcon=panpak.{ID}\n")), "{}", e.entry());
    // A version with one icon: the size it no longer has goes away.
    let o = e.install("2", "[desktop]\nicons = [\"icons/b.png\"]\n", &[("icons/b.png", i48.clone())]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!text(&o).contains("NOTE: the menu entry changed"), "same entry: {}", text(&o));
    assert!(!e.icon(16).exists() && e.icon(48).is_file());
    // A version without any: the entry goes back to the generic icon and no file stays.
    let o = e.install("3", "", &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("NOTE: the menu entry changed") && text(&o).contains("log in again"), "{}", text(&o));
    assert!(!e.icon(48).exists(), "an icon of an older version stays");
    assert!(e.entry().contains("\nIcon=application-x-executable\n"), "{}", e.entry());
    // With icons again, then remove: the files go and the report says so.
    assert!(e.install("4", toml, &[("icons/a.png", i16), ("icons/b.png", i48)]).status.success());
    let o = cli(&e.home, &["remove", ID]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!e.icon(16).exists() && !e.icon(48).exists());
    assert!(text(&o).contains("icons: 2 files"), "{}", text(&o));
    // Files of other apps in the same folders are not touched.
    let other = e.home.join(".local/share/icons/hicolor/48x48/apps/panpak.org.example.other.png");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    fs::write(&other, "x").unwrap();
    assert!(e.install("5", "", &[]).status.success());
    assert!(cli(&e.home, &["remove", ID]).status.success());
    assert!(other.is_file());
}

#[test]
fn an_app_without_an_icon_gets_the_generic_one() {
    let e = env("noicon");
    assert!(e.install("1", "", &[]).status.success());
    assert!(e.entry().contains("\nIcon=application-x-executable\n"));
    assert!(!e.home.join(".local/share/icons").exists());
}

#[test]
fn a_link_in_place_of_the_icon_stops_the_install_before_the_store_is_touched() {
    let e = env("iconlink");
    let victim = e.home.join("victim");
    fs::write(&victim, "keep").unwrap();
    let dest = e.icon(16);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&victim, &dest).unwrap();
    let o = e.install("1", "[desktop]\nicons = [\"i.png\"]\n", &[("i.png", fixture("ok-16-rgba.png"))]);
    assert!(!o.status.success() && text(&o).contains("PANPAK-E2001") && text(&o).contains("symbolic link"), "{}", text(&o));
    assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
    assert!(!e.store().join("apps").join(ID).exists(), "the store was touched");
}

#[test]
fn pack_refuses_icons_that_would_not_be_installed() {
    let e = env("packbad");
    let refused = |toml: &str, files: &[(&str, Vec<u8>)], why: &str| {
        let t = e.tree("1", toml, files);
        let (o, _) = e.pack(&t, "bad.panpak");
        assert!(!o.status.success() && text(&o).contains(why), "{toml}: {}", text(&o));
    };
    let ok = fixture("ok-48-rgba.png");
    refused("[desktop]\nicons = [\"a.png\"]\n", &[("a.png", fixture("bad-text.png"))], "not accepted");
    refused("[desktop]\nicons = [\"a.png\"]\n", &[("a.png", fixture("bad-icc.png"))], "not accepted");
    refused("[desktop]\nicons = [\"a.png\"]\n", &[("a.png", fixture("bad-nonsquare.png"))], "not square");
    refused("[desktop]\nicons = [\"a.png\"]\n", &[("a.png", fixture("bad-small.png"))], "side must be");
    refused("[desktop]\nicons = [\"a.png\"]\n", &[("a.png", b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec())], "SVG");
    refused("[desktop]\nicons = [\"a.png\"]\n", &[], "not a regular file of the tree");
    refused("[desktop]\nicons = [\"a.svg\"]\n", &[("a.svg", ok.clone())], "relative path to a .png");
    refused("[desktop]\nicons = [\"../a.png\"]\n", &[("a.png", ok.clone())], "relative path to a .png");
    refused("[desktop]\nicons = [\"/a.png\"]\n", &[("a.png", ok.clone())], "relative path to a .png");
    refused("[desktop]\nicons = [\"a.png\", \"a.png\"]\n", &[("a.png", ok.clone())], "listed twice");
    refused("[desktop]\nicons = [\"1.png\",\"2.png\",\"3.png\",\"4.png\",\"5.png\",\"6.png\",\"7.png\"]\n", &[], "more than 6");
    refused("[desktop]\nicons = [\"a.png\"]\n", &[("a.png", vec![0; 300 * 1024])], "larger than");
}
