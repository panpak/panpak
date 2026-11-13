// SPDX-License-Identifier: MIT OR Apache-2.0
// What the commands print about permissions: the summary, the levels in words, what the app cannot do, the language, and what an update lists.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

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

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

impl Lab {
    fn new(name: &str) -> Lab {
        let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("perm-cli-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join("run")).unwrap();
        fs::set_permissions(home.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
        let l = Lab { home };
        let o = l.cli("C", &["keygen", "-o", l.home.join("k").to_str().unwrap()]);
        let pubkey = text(&o).lines().find_map(|x| x.strip_prefix("key:").map(|k| k.trim().to_string())).expect("keygen prints the key");
        fs::write(l.home.join("k.pub"), pubkey).unwrap();
        // A runtime to install against.
        let stub = l.home.join("rt-stub");
        fs::write(&stub, "#!/bin/sh\n[ \"$1\" = --abi-version ] && echo 1\nexit 0\n").unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(l.cli("C", &["install-runtime", stub.to_str().unwrap(), "--allow-unsigned"]).status.success());
        l
    }
    fn cli(&self, lang: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_panpak"))
            .args(args)
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_RUNTIME_DIR", self.home.join("run"))
            .env("PANPAK_OWN_RUNTIME", "off")
            .env_remove("PANPAK_SECRET_FILE")
            .env_remove("LANGUAGE")
            .env_remove("LC_ALL")
            .env_remove("NO_COLOR")
            .env("LANG", lang)
            .env("TERM", "xterm")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }
    fn package(&self, name: &str, extra: &str) -> PathBuf {
        let tree = self.home.join(format!("tree-{name}"));
        fs::create_dir_all(tree.join("bin")).unwrap();
        fs::write(tree.join("bin/app"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(tree.join("bin/app"), fs::Permissions::from_mode(0o755)).unwrap();
        let key = self.home.join("k");
        let pubkey = fs::read_to_string(self.home.join("k.pub")).unwrap();
        fs::write(
            tree.join("panpak.toml"),
            format!("[app]\nid = \"org.example.perm\"\nname = \"Perm\"\nversion = \"1\"\nentry = \"bin/app\"\n[update]\nkey = \"{pubkey}\"\n{extra}"),
        )
        .unwrap();
        let stub = self.home.join("pack-stub");
        fs::write(&stub, "#!/bin/sh\n[ \"$1\" = --abi-version ] && echo 1\nexit 0\n").unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let pkg = self.home.join(format!("{name}.panpak"));
        let o = self.cli("C", &["pack", tree.to_str().unwrap(), "--stub", stub.to_str().unwrap(), "--secret-file", key.to_str().unwrap(), "-o", pkg.to_str().unwrap()]);
        assert!(o.status.success(), "{}", text(&o));
        pkg
    }
}

/// The Italian text of a message, read from the catalog, so that the test holds no Italian of its own.
fn it(msgid: &str) -> String {
    let po = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("po").join("it.po")).unwrap();
    let key = format!("msgid \"{msgid}\"\n");
    let at = po.find(&key).unwrap_or_else(|| panic!("no {msgid:?} in po/it.po")) + key.len();
    let line = po[at..].lines().next().unwrap();
    let line = if line.starts_with("msgid_plural") { po[at..].lines().nth(1).unwrap() } else { line };
    line.split_once('"').unwrap().1.trim_end_matches('"').replace("\\\"", "\"")
}

const RICH: &str = "[permissions]\nx11 = true\nnetwork = true\nnetwork_allow_tcp = [443]\nnotifications = true\n[[permissions.home_paths]]\npath = \"Notes\"\naccess = \"ro\"\n";

#[test]
fn install_shows_the_summary_the_levels_in_words_and_what_it_cannot_do() {
    let l = Lab::new("install");
    let pkg = l.package("rich", RICH);
    let t = text(&l.cli("C", &["install", "--yes", pkg.to_str().unwrap()]));
    assert!(t.contains("Partial isolation: it can watch other apps (X11); it reaches your home folder"), "{t}");
    assert!(t.contains("[!!] High risk") && t.contains("[!] Attention") && t.contains("[ ] Normal"), "{t}");
    assert!(t.contains("What it cannot do:") && t.contains("It cannot record sound."), "{t}");
    assert!(!t.contains("It cannot use the internet"), "an app with the network must not say that it cannot use it: {t}");
    // No color and no Unicode symbols when the output is not a terminal and the locale is not UTF-8.
    assert!(!t.contains('\x1b') && !t.contains('\u{26a0}'), "{t}");
    let plain = l.package("plain", "");
    let t = text(&l.cli("C", &["inspect", plain.to_str().unwrap()]));
    assert!(t.contains("Full isolation") && t.contains("It asks for nothing beyond its own private folders."), "{t}");
}

#[test]
fn the_language_comes_from_the_environment_and_english_is_the_fallback() {
    let l = Lab::new("lang");
    let pkg = l.package("rich", RICH);
    let it_out = text(&l.cli("it_IT.UTF-8", &["inspect", pkg.to_str().unwrap()]));
    let (partial, high, cannot) = (self::it("Partial isolation:"), self::it("High risk"), self::it("What it cannot do:"));
    assert!(partial != "Partial isolation:" && it_out.contains(&partial) && it_out.contains(&format!("[!!] {high}")) && it_out.contains(&cannot), "{it_out}");
    assert!(it_out.contains('\u{26a0}'), "a UTF-8 locale gets the symbols: {it_out}");
    let xx = text(&l.cli("xx_YY.UTF-8", &["inspect", pkg.to_str().unwrap()]));
    assert!(xx.contains("Partial isolation:") && xx.contains("[!!] High risk"), "{xx}");
    let c = text(&l.cli("C", &["inspect", pkg.to_str().unwrap()]));
    assert!(c.contains("Partial isolation:"), "{c}");
}

#[test]
fn list_and_inspect_give_ids_and_levels_in_json() {
    let l = Lab::new("json");
    let pkg = l.package("rich", RICH);
    assert!(l.cli("C", &["install", "--yes", pkg.to_str().unwrap()]).status.success());
    let j = text(&l.cli("it_IT.UTF-8", &["inspect", "--json", "org.example.perm"]));
    assert!(j.contains("\"isolation\":\"partial\"") && j.contains("{\"id\":\"x11\",\"level\":\"high\",\"subjects\":[]}"), "{j}");
    assert!(j.contains("{\"id\":\"network-connect\",\"level\":\"attention\",\"subjects\":[\"443\"]}"), "{j}");
    let list = text(&l.cli("C", &["list", "--json"]));
    assert!(list.contains("\"permissions\":[{\"id\":\"x11\""), "{list}");
    let t = text(&l.cli("C", &["list"]));
    assert!(t.contains("PERMISSIONS") && t.contains("High risk, 4 permissions"), "{t}");
}

#[test]
fn an_update_lists_only_what_is_new_and_says_the_current_version_stays() {
    let l = Lab::new("update");
    let first = l.package("v1", "[permissions]\nnetwork = true\nnetwork_allow_tcp = [443]\nnotifications = true\n");
    assert!(l.cli("C", &["install", "--yes", first.to_str().unwrap()]).status.success());
    let second = l.package("v2", "[permissions]\nnetwork = true\nnetwork_allow_tcp = [443, 8080]\nnotifications = true\naudio = true\n");
    let o = l.cli("C", &["install", "--yes", second.to_str().unwrap()]);
    let t = text(&o);
    assert!(!o.status.success() && t.contains("PANPAK-E5004"), "{t}");
    assert!(t.contains("This package asks for more than the version you have:"), "{t}");
    assert!(t.contains("Play sound and use the microphone") && t.contains("ports 8080"), "{t}");
    assert!(!t.contains("ports 443") && !t.contains("Show notifications"), "only what is new is listed: {t}");
    assert!(t.contains("If you do not accept, you stay on the current version."), "{t}");
    let it_out = text(&l.cli("it_IT.UTF-8", &["install", "--yes", second.to_str().unwrap()]));
    assert!(it_out.contains(&self::it("If you do not accept, you stay on the current version.")), "{it_out}");
}

#[test]
fn the_table_is_printed_for_the_readme() {
    let l = Lab::new("table");
    let md = text(&l.cli("it_IT.UTF-8", &["permissions", "--markdown"]));
    assert!(md.starts_with("| Level |") && md.contains("See what other apps show"), "the README table is in English whatever the locale: {md}");
    let plain = text(&l.cli("C", &["permissions"]));
    assert!(plain.contains("desktop-bus") && plain.contains("High risk"), "{plain}");
}
