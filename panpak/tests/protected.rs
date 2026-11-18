// SPDX-License-Identifier: MIT OR Apache-2.0
// Protected mode, the client side (PANPAK-FORMAT.md 3.1.10): what `list` and `doctor` say about an installed version below the floor its author set,
// `allow-internet` (the confirmation, the choice kept for one app, its cancelling), and the end of the mode when the fixed version arrives. The start
// itself (no network, no written programs) is tested in `tests/protected_mode.py` and in the unit tests of the stub.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const ID: &str = "org.example.prot";

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
    key: PathBuf,
    pubkey: String,
}

impl Drop for Lab {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = Command::new("chmod").args(["-R", "u+w"]).arg(&self.home).output();
            let _ = fs::remove_dir_all(&self.home);
        }
    }
}

fn lab(name: &str) -> Lab {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("protected-{name}-{}", std::process::id()));
    let _ = Command::new("chmod").args(["-R", "u+w"]).arg(&home).output();
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join("run")).unwrap();
    fs::set_permissions(home.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
    let key = home.join("secret.key");
    let o = cli(&home, &["keygen", "-o", key.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let pubkey = text(&o).lines().find_map(|l| l.strip_prefix("key:    ")).unwrap().trim().to_string();
    // An install makes the app ready with the runtime, and the checks of a later install read the tree that this made: the runtime is the real
    // stub next to `panpak` (a file that `cargo test` does not build by itself: `cargo build -p panpak-stub` first). The stub that `pack` embeds
    // is a script.
    let real = Path::new(env!("CARGO_BIN_EXE_panpak")).with_file_name("panpak-stub");
    assert!(real.is_file(), "{} is missing: build it first (cargo build -p panpak-stub, with the same --target)", real.display());
    let stub = home.join("stub");
    fs::write(&stub, "#!/bin/sh\necho 1\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    Lab { home, key, pubkey }
}

impl Lab {
    /// Pack `name` (the file is `<name>.panpak`): the tree holds `body`, the `[release]` text is the author's (may be empty), `previous` is the
    /// package that this one follows. Returns the package or the failed output.
    fn pack(&self, name: &str, body: &str, release: &str, previous: Option<&Path>) -> Result<PathBuf, Output> {
        let tree = self.home.join(format!("tree-{name}"));
        let _ = fs::remove_dir_all(&tree);
        fs::create_dir_all(tree.join("bin")).unwrap();
        fs::write(tree.join("bin/hello"), format!("#!/bin/sh\necho {body}\n")).unwrap();
        fs::set_permissions(tree.join("bin/hello"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            tree.join("panpak.toml"),
            format!("[app]\nid = \"{ID}\"\nname = \"Rel\"\nversion = \"{name}\"\nentry = \"bin/hello\"\n{release}[update]\nkey = \"{}\"\n", self.pubkey),
        )
        .unwrap();
        let out = self.home.join(format!("{name}.panpak"));
        let stub = self.home.join("stub");
        let (tree_s, stub_s, key_s, out_s) = (tree.to_str().unwrap(), stub.to_str().unwrap(), self.key.to_str().unwrap(), out.to_str().unwrap());
        let mut args = vec!["pack", tree_s, "--stub", stub_s, "--secret-file", key_s, "-o", out_s];
        if let Some(p) = previous {
            args.extend(["--previous", p.to_str().unwrap()]);
        }
        let o = cli(&self.home, &args);
        if o.status.success() { Ok(out) } else { Err(o) }
    }

    fn pack_ok(&self, name: &str, body: &str, release: &str, previous: Option<&Path>) -> PathBuf {
        self.pack(name, body, release, previous).unwrap_or_else(|o| panic!("pack {name}: {}", text(&o)))
    }

    fn install(&self, pkg: &Path, flags: &[&str]) -> Output {
        cli(&self.home, &[&["install", "--yes"], flags, &[pkg.to_str().unwrap()]].concat())
    }
}

fn code_of(o: &Output, code: &str) -> bool {
    !o.status.success() && text(o).contains(code)
}

fn ok(o: &Output) -> bool {
    o.status.success()
}

#[test]
fn a_floor_above_the_installed_version_shows_in_list_and_doctor_and_the_internet_is_turned_back_on_by_the_person_for_that_app() {
    let l = lab("cycle");
    let v1 = l.pack_ok("1", "a", "", None);
    assert!(ok(&l.install(&v1, &[])));
    let appdir = l.home.join(".local/share/panpak/apps").join(ID);
    // Not in protected mode: nothing to turn on, nothing said.
    let o = cli(&l.home, &["allow-internet", "--yes", ID]);
    assert!(!ok(&o) && text(&o).contains("not in protected mode"), "{}", text(&o));
    assert!(!text(&cli(&l.home, &["list"])).contains("protected mode"));
    // A package of another channel with floor 2 is refused, and its floor is seen.
    let beta = l.pack_ok("2", "b", "[release]\nchannel = \"beta\"\nfloor = 2\nrevision = 2\n", None);
    assert!(code_of(&l.install(&beta, &[]), "PANPAK-E4005"));
    assert_eq!(fs::read_to_string(appdir.join("floor")).unwrap().trim(), "2");
    let list = text(&cli(&l.home, &["list"]));
    assert!(list.contains("protected mode: no internet and no downloaded programs until updated"), "{list}");
    let json = text(&cli(&l.home, &["list", "--json"]));
    assert!(json.contains("\"protected\":{\"internet_back_on\":false}"), "{json}");
    let doctor = text(&cli(&l.home, &["doctor"]));
    assert!(doctor.contains("(org.example.prot) is in protected mode") && !doctor.contains("panpak update"), "{doctor}");
    // The state for a front end, with the texts, and the menu of the app (the action to turn the internet back on appears with the mode).
    let state = text(&cli(&l.home, &["protected", "--json", ID]));
    assert!(state.contains("\"protected\":true") && state.contains("\"floor\":2") && state.contains("\"actions\":[\"internet_on\"]") && state.contains("\"update\":null") && state.contains("\"kind\":\"terminal\""), "{state}");
    assert!(state.contains("\"title\":\"Rel is running in protected mode\""), "{state}");
    assert_eq!(text(&cli(&l.home, &["protected", "--json", "org.example.other"])).trim(), "{\"protected\":false}");
    let entry = l.home.join(format!(".local/share/applications/panpak.{ID}.desktop"));
    let menu = fs::read_to_string(&entry).unwrap();
    assert!(menu.contains("Actions=Uninstall;Internet;") && menu.contains(&format!("allow-internet --ask {ID}")) && menu.contains("Name=Turn the internet back on"), "{menu}");
    // Without a terminal and without --yes the question is not answered for the person.
    let o = cli(&l.home, &["allow-internet", ID]);
    assert!(!ok(&o) && !appdir.join("protected-net").exists(), "{}", text(&o));
    // With --yes: the risk is told first, the choice is kept, and list says so.
    let o = cli(&l.home, &["allow-internet", "--yes", ID]);
    assert!(ok(&o) && text(&o).contains("can be used against you") && text(&o).contains("from its next start"), "{}", text(&o));
    assert!(!text(&o).contains("panpak "), "no command in the text: {}", text(&o));
    assert!(fs::read_to_string(&entry).unwrap().contains("Name=Turn the internet off again"));
    assert!(appdir.join("protected-net").is_file());
    assert!(text(&cli(&l.home, &["list"])).contains("you turned the internet back on"));
    assert!(text(&cli(&l.home, &["list", "--json"])).contains("\"internet_back_on\":true"));
    // It can be cancelled, twice without harm.
    for _ in 0..2 {
        assert!(ok(&cli(&l.home, &["allow-internet", "--undo", ID])));
    }
    assert!(!appdir.join("protected-net").exists());
    // The fixed version (revision 2, floor 2, the channel of the installed one) ends the mode, tells it, and clears a choice made before.
    assert!(ok(&cli(&l.home, &["allow-internet", "--yes", ID])));
    let fixed = l.pack_ok("3", "c", "[release]\nrevision = 2\nfloor = 2\n", Some(&v1));
    let o = l.install(&fixed, &[]);
    assert!(ok(&o) && text(&o).contains("Protected mode is over"), "{}", text(&o));
    assert!(!appdir.join("protected-net").exists() && !appdir.join("protected").exists());
    assert!(!fs::read_to_string(&entry).unwrap().contains("allow-internet"), "the action goes with the mode");
    assert!(!text(&cli(&l.home, &["list"])).contains("protected mode"));
    assert!(!text(&cli(&l.home, &["doctor"])).contains("is in protected mode"));
    // The same status in another language, with the catalog of the program (the words are checked in `po/it.po` by the tests of the core).
    fs::write(appdir.join("floor"), "9\n").unwrap();
    let it = Command::new(env!("CARGO_BIN_EXE_panpak")).args(["list"]).current_dir(&l.home).env("HOME", &l.home).env("XDG_DATA_HOME", l.home.join(".local/share")).env("XDG_CACHE_HOME", l.home.join(".cache")).env("XDG_CONFIG_HOME", l.home.join(".config")).env("LC_ALL", "it_IT.UTF-8").output().unwrap();
    assert!(!text(&it).contains("protected mode") && text(&it).contains(ID), "the status is not in English: {}", text(&it));
}
