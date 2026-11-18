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
        .env("XDG_DATA_DIRS", home.join("sysdata")) // the data folders of this system must not hold an app with the id of a test
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env_remove("PANPAK_SECRET_FILE")
        .env("PANPAK_OWN_RUNTIME", "off") // the test programs have stand-in runtimes; the real one beside `panpak` must not replace them
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
fn an_older_stable_panpak_is_updated_and_a_newer_or_other_one_is_never_replaced() {
    let e = env("keep");
    fs::create_dir_all(e.store().join("bin")).unwrap();
    let mine = e.store().join("bin/panpak");
    let running = fs::read(env!("CARGO_BIN_EXE_panpak")).unwrap();
    // The stored one says its version and, when `date` is given, the date of its build (`--build-epoch`; an older build does not know the option).
    let say_at = |v: &str, date: Option<u64>| {
        let _ = fs::remove_file(&mine);
        let epoch = date.map(|d| format!("[ \"$1\" = --build-epoch ] && echo {d}\n")).unwrap_or_default();
        fs::write(&mine, format!("#!/bin/sh\n[ \"$1\" = --version ] && echo 'panpak {v}'\n{epoch}exit 0\n")).unwrap();
        fs::set_permissions(&mine, fs::Permissions::from_mode(0o755)).unwrap();
    };
    let say = |v: &str| say_at(v, None);
    // The date of the running panpak (none when this build did not know it: then a build is never told apart from another of the same version).
    let mine_date: Option<u64> = {
        let o = std::process::Command::new(env!("CARGO_BIN_EXE_panpak")).arg("--build-epoch").output().unwrap();
        String::from_utf8(o.stdout).unwrap().trim().parse().ok()
    };
    let leftovers = || {
        let mut v: Vec<String> = fs::read_dir(e.store().join("bin")).unwrap().flatten().map(|x| x.file_name().to_string_lossy().into_owned()).filter(|n| n.contains("tmp")).collect();
        v.sort();
        v
    };
    // An older one (an old store) is replaced by the running panpak, in one rename: no temporary file is left, the mode is kept.
    for old in ["0.0.1", "0.1.0-rc1"] {
        say(old);
        let o = e.install("1", "", &[]);
        assert!(o.status.success(), "{old}: {}", text(&o));
        let t = text(&o);
        assert!(t.contains("updated panpak in") && t.contains(&format!("from version {old} to {}", env!("CARGO_PKG_VERSION"))), "{old}: {t}");
        assert_eq!(fs::read(&mine).unwrap(), running, "{old}: the stable panpak was not replaced");
        assert_eq!(fs::metadata(&mine).unwrap().permissions().mode() & 0o777, 0o755);
        assert!(leftovers().is_empty(), "{:?}", leftovers());
    }
    // The same version, and a newer one: never replaced, never by an older one, and no warning. A stored one of the same version with the same
    // date or a later one is not replaced; a higher version is not whatever its date is.
    let same = env!("CARGO_PKG_VERSION");
    let mut keep: Vec<(&str, Option<u64>)> = vec![("99.0.0", None), ("99.0.0", Some(0)), ("0.1.1", None)];
    if let Some(d) = mine_date {
        keep.extend([(same, Some(d)), (same, Some(d + 1)), ("0.1.1", Some(0))]);
    } else {
        keep.push((same, None));
    }
    for (v, d) in keep {
        say_at(v, d);
        let before = fs::read(&mine).unwrap();
        let o = e.install("2", "", &[]);
        assert!(o.status.success(), "{v}: {}", text(&o));
        assert_eq!(fs::read(&mine).unwrap(), before, "{v} {d:?}: the stable panpak was replaced");
        assert!(!text(&o).contains("updated panpak") && !text(&o).contains("older than this one"), "{v} {d:?}: {}", text(&o));
    }
    // The same version, built earlier (or one that has no date at all): replaced by a build with a later date, and by it only.
    if let Some(d) = mine_date {
        for stored in [None, Some(0), Some(d.saturating_sub(1))].into_iter().filter(|s| s.is_none_or(|x| x < d)) {
            say_at(same, stored);
            let o = e.install("2b", "", &[]);
            assert!(o.status.success(), "{stored:?}: {}", text(&o));
            assert!(text(&o).contains("updated panpak in"), "{stored:?}: {}", text(&o));
            assert_eq!(fs::read(&mine).unwrap(), running, "{stored:?}: the stable panpak was not replaced");
            assert!(leftovers().is_empty(), "{:?}", leftovers());
        }
    }
    // One that does not say its version is not replaced, and the user is told.
    fs::write(&mine, "#!/bin/sh\nexit 0\n").unwrap();
    let before = fs::read(&mine).unwrap();
    let o = e.install("3", "", &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(fs::read(&mine).unwrap(), before);
    assert!(text(&o).contains("does not say its version"), "{}", text(&o));
    // A link of the user's own is not replaced either: the target and the link stay as they were.
    let target = e.home.join("my-panpak");
    fs::write(&target, "#!/bin/sh\n[ \"$1\" = --version ] && echo 'panpak 0.0.1'\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(&mine).unwrap();
    std::os::unix::fs::symlink(&target, &mine).unwrap();
    let before = fs::read(&target).unwrap();
    let o = e.install("4", "", &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(fs::read_link(&mine).unwrap(), target);
    assert_eq!(fs::read(&target).unwrap(), before);
    assert!(text(&o).contains("not a plain file"), "{}", text(&o));
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
fn another_app_with_the_same_id_gives_the_entry_a_name_of_its_own_and_it_follows_when_that_app_goes() {
    let e = env("twin");
    let (i16, i48) = (fixture("ok-16-rgba.png"), fixture("ok-48-rgba.png"));
    let toml = "[desktop]\nicons = [\"icons/a.png\", \"icons/b.png\"]\nwm_class = \"org.example.icon\"\n";
    let files = [("icons/a.png", i16.clone()), ("icons/b.png", i48.clone())];
    // Alone: the plain name, no note.
    let o = e.install("1", toml, &files);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!text(&o).contains("same id"), "{}", text(&o));
    let plain = e.entry();
    assert!(plain.contains("\nName=Icon App\n") && plain.contains(&format!("\nIcon=panpak.{ID}\n")) && !plain.contains("Twin"), "{plain}");
    assert_eq!(fs::read(e.icon(16)).unwrap(), i16, "the plain icon is the one of the package");
    // The same app installed another way appears (a Flatpak export, a distribution package): the next install says so and names the entry.
    let other = e.home.join("sysdata/applications");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join(format!("{ID}.desktop")), "[Desktop Entry]\nType=Application\nName=Icon App\nExec=true\n").unwrap();
    let o = e.install("2", toml, &files);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("another app with the same id") && text(&o).contains("\"Icon App (Panpak)\""), "{}", text(&o));
    let marked = e.entry();
    assert!(marked.contains("\nName=Icon App (Panpak)\n") && marked.contains(&format!("\nIcon=panpak.{ID}\n")), "{marked}");
    assert_eq!(marked.lines().nth(2), Some("X-Panpak-Twin=1"), "{marked}");
    // It goes: the entry is plain again.
    fs::remove_file(other.join(format!("{ID}.desktop"))).unwrap();
    let o = e.install("3", toml, &files);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!text(&o).contains("same id"), "{}", text(&o));
    assert_eq!(e.entry(), plain, "back to the plain entry");
    // An entry of the same id that is Panpak's own, from before the prefix, is not another app.
    let legacy = e.home.join(format!(".local/share/applications/{ID}.desktop"));
    fs::write(&legacy, format!("[Desktop Entry]\nType=Application\nName=Icon App\nExec={} %U\nX-Panpak-App-Id={ID}\n", e.home.join(".local/bin").join(ID).display())).unwrap();
    let o = e.install("4", toml, &files);
    assert!(o.status.success() && !text(&o).contains("same id"), "{}", text(&o));
    assert!(!e.entry().contains("(Panpak)"), "{}", e.entry());
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
    refused("[desktop]\nicons = [\"a.png\"]\n", &[("a.png", fixture("bad-nonsquare.png"))], "not square");
    refused("[desktop]\nicons = [\"a.png\"]\n", &[("a.png", fixture("bad-small.png"))], "side must be");
    refused("[desktop]\nicons = [\"a.png\"]\n", &[("a.png", b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec())], "SVG");
    refused("[desktop]\nicons = [\"a.png\"]\n", &[], "not a regular file of the tree");
    refused("[desktop]\nicons = [\"a.svg\"]\n", &[("a.svg", ok.clone())], "relative path to a .png");
    refused("[desktop]\nicons = [\"../a.png\"]\n", &[("a.png", ok.clone())], "relative path to a .png");
    refused("[desktop]\nicons = [\"/a.png\"]\n", &[("a.png", ok.clone())], "relative path to a .png");
    refused("[desktop]\nicons = [\"a.png\", \"a.png\"]\n", &[("a.png", ok.clone())], "listed twice");
    refused("[desktop]\nicons = [\"1.png\",\"2.png\",\"3.png\",\"4.png\",\"5.png\",\"6.png\",\"7.png\"]\n", &[], "more than 6");
    refused("[desktop]\nicons = [\"a.png\"]\n", &[("a.png", vec![0; 5 * 1024 * 1024])], "larger than");
}

#[test]
fn pack_cleans_an_icon_and_says_which_chunks_it_removed() {
    let e = env("packclean");
    let (text_png, icc_png) = (fixture("bad-text.png"), fixture("bad-icc.png"));
    let toml = "[desktop]\nicons = [\"a.png\", \"b.png\"]\n";
    let files = [("a.png", text_png.clone()), ("b.png", icc_png.clone())];
    let t = e.tree("1", toml, &files);
    let (o, pkg) = e.pack(&t, "1.panpak");
    assert!(o.status.success(), "{}", text(&o));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("icon a.png: removed the chunks tEXt") && err.contains("pixels are not touched"), "{err}");
    // Both are 32 pixels: the first stays and the note says so.
    assert!(err.contains("icon b.png: same size (32x32) as a.png, which is used; this one is not"), "{err}");
    let t2 = e.tree("9", "[desktop]\nicons = [\"b.png\"]\n", &[("b.png", icc_png.clone())]);
    let (o2, _) = e.pack(&t2, "icc.panpak");
    assert!(o2.status.success() && String::from_utf8_lossy(&o2.stderr).contains("icon b.png: removed the chunks iCCP"), "{}", text(&o2));
    // The file in the tree is not changed; the package holds the cleaned icon and install writes it.
    assert_eq!(fs::read(t.join("a.png")).unwrap(), text_png);
    let o = cli(&e.home, &["install", "--yes", pkg.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let a = fs::read(e.icon(32)).unwrap();
    assert_ne!(a, text_png);
    assert!(!a.windows(4).any(|w| w == b"tEXt" || w == b"iCCP"));
    // Packing again gives the same package: the cleaning is deterministic.
    let (o2, pkg2) = e.pack(&t, "2.panpak");
    assert!(o2.status.success());
    assert_eq!(fs::read(&pkg).unwrap(), fs::read(&pkg2).unwrap());
    // A tree whose icons are clean says nothing.
    let t = e.tree("1", "[desktop]\nicons = [\"a.png\"]\n", &[("a.png", fixture("ok-48-rgba.png"))]);
    let (o, _) = e.pack(&t, "3.panpak");
    assert!(o.status.success() && !String::from_utf8_lossy(&o.stderr).contains("removed the chunks"), "{}", text(&o));
}

/// Rewrite the manifest of a package and sign it again with the key of the test (the layout is in PANPAK-FORMAT.md §2).
fn resign(pkg: &Path, key_file: &Path, edit: impl Fn(String) -> String, out: &Path) {
    use ed25519_dalek::Signer;
    let d = fs::read(pkg).unwrap();
    let f = &d[d.len() - 80..];
    let (u32le, u64le) = (|b: &[u8]| u32::from_le_bytes(b.try_into().unwrap()), |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap()));
    let (ver, flags, ioff, ilen, moff) = (u32le(&f[8..12]), u32le(&f[12..16]), u64le(&f[16..24]), u64le(&f[24..32]), u64le(&f[32..40]) as usize);
    let mlen = u64le(&f[40..48]) as usize;
    let root = &f[48..80];
    let man = edit(String::from_utf8(d[moff..moff + mlen].to_vec()).unwrap()).into_bytes();
    let sk = panpak_core::parse_secret(fs::read_to_string(key_file).unwrap().trim()).unwrap();
    let signed = panpak_core::package_message(ver, flags, root.try_into().unwrap(), &man);
    let mut o = d[..moff].to_vec();
    o.extend_from_slice(&man);
    o.extend_from_slice(&panpak_core::encode_sig_block(&sk.sign(&signed).to_bytes()));
    o.extend_from_slice(&f[..8]);
    o.extend_from_slice(&ver.to_le_bytes());
    o.extend_from_slice(&flags.to_le_bytes());
    o.extend_from_slice(&ioff.to_le_bytes());
    o.extend_from_slice(&ilen.to_le_bytes());
    o.extend_from_slice(&(moff as u64).to_le_bytes());
    o.extend_from_slice(&(man.len() as u64).to_le_bytes());
    o.extend_from_slice(root);
    fs::write(out, o).unwrap();
}

#[test]
fn install_warns_about_fields_this_panpak_does_not_know_and_goes_on() {
    let e = env("unknown");
    let t = e.tree("1", "", &[]);
    let (o, pkg) = e.pack(&t, "1.panpak");
    assert!(o.status.success(), "{}", text(&o));
    // A package of a newer format: an unknown key in a known section and an unknown section.
    let newer = e.home.join("newer.panpak");
    resign(&pkg, &e.key, |m| format!("{m}\n[future]\nthing = 1\n").replace("[app]\n", "[app]\nsurprise = \"x\"\n"), &newer);
    let o = cli(&e.home, &["install", "--yes", newer.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("WARNING: this package uses fields that this version of panpak does not know (") && err.contains("app.surprise") && err.contains("future") && err.contains("panpak install-runtime"), "{err}");
    // A package that only has known fields says nothing of the kind.
    let o = cli(&e.home, &["install", "--yes", pkg.to_str().unwrap()]);
    assert!(o.status.success() && !text(&o).contains("fields that this version"), "{}", text(&o));
    // The refusal in `pack` stays, with its own code.
    let t = e.tree("2", "[desktop]\nfuture = 1\n", &[]);
    let (o, _) = e.pack(&t, "2.panpak");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("PANPAK-E0008") && err.contains("needs a newer panpak") && err.contains("panpak install-runtime"), "{err}");
}
