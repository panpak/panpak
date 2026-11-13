// SPDX-License-Identifier: MIT OR Apache-2.0
// The runtime that comes with `panpak` (the file `panpak-stub` in the same folder): `install` puts it in the store when there is none, replaces it when it is
// older, and never replaces one that was installed by hand with the same ABI.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Lab {
    home: PathBuf,
    bin: PathBuf,
    key: PathBuf,
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

fn stub_script(path: &Path, abi: u32, note: &str) {
    fs::write(path, format!("#!/bin/sh\n# {note}\n[ \"$1\" = --abi-version ] && echo {abi}\nexit 0\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

impl Lab {
    fn new(name: &str) -> Lab {
        let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("own-runtime-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join("run")).unwrap();
        fs::set_permissions(home.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
        // The program in a folder of its own, with the runtime beside it.
        let bin = home.join("prog");
        fs::create_dir_all(&bin).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_panpak"), bin.join("panpak")).unwrap();
        fs::set_permissions(bin.join("panpak"), fs::Permissions::from_mode(0o755)).unwrap();
        let key = home.join("secret.key");
        let l = Lab { home, bin, key };
        assert!(l.cli(&["keygen", "-o", l.key.to_str().unwrap()]).status.success());
        l
    }
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(self.bin.join("panpak"))
            .args(args)
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_RUNTIME_DIR", self.home.join("run"))
            .env_remove("PANPAK_SECRET_FILE")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }
    /// A package made with a stand-in runtime (the one beside the program is not needed for `pack`).
    fn package(&self) -> PathBuf {
        let tree = self.home.join("tree");
        fs::create_dir_all(tree.join("bin")).unwrap();
        fs::write(tree.join("bin/app"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(tree.join("bin/app"), fs::Permissions::from_mode(0o755)).unwrap();
        let o = self.cli(&["keygen", "-o", self.home.join("k2").to_str().unwrap()]);
        let pubkey = text(&o).lines().find_map(|l| l.strip_prefix("key:").map(|k| k.trim().to_string())).unwrap();
        fs::write(tree.join("panpak.toml"), format!("[app]\nid = \"org.example.own\"\nname = \"Own\"\nversion = \"1\"\nentry = \"bin/app\"\n[update]\nkey = \"{pubkey}\"\n")).unwrap();
        let stub = self.home.join("pack-stub");
        stub_script(&stub, 1, "for pack");
        let pkg = self.home.join("own.panpak");
        let o = self.cli(&["pack", tree.to_str().unwrap(), "--stub", stub.to_str().unwrap(), "--secret-file", self.home.join("k2").to_str().unwrap(), "-o", pkg.to_str().unwrap()]);
        assert!(o.status.success(), "{}", text(&o));
        pkg
    }
    fn install(&self, pkg: &Path) -> String {
        text(&self.cli(&["install", "--yes", pkg.to_str().unwrap()]))
    }
    fn stored_note(&self, abi: u32) -> String {
        fs::read_to_string(self.home.join(format!(".local/share/panpak/runtimes/{abi}/panpak-stub"))).unwrap_or_default()
    }
}

#[test]
fn install_puts_the_runtime_that_comes_with_the_program_and_updates_it() {
    let l = Lab::new("auto");
    let pkg = l.package();
    // No panpak-stub beside the program and none installed: the old message, nothing invented.
    let t = l.install(&pkg);
    assert!(!t.contains("runtime 1 installed from"), "{t}");
    // With the runtime beside the program: installed, with one line.
    stub_script(&l.bin.join("panpak-stub"), 1, "first");
    let t = l.install(&pkg);
    assert!(t.contains("panpak: runtime 1 installed from") && t.contains("it comes with this program"), "{t}");
    assert!(l.stored_note(1).contains("# first"));
    // Nothing to do the next time: no line.
    let t = l.install(&pkg);
    assert!(!t.contains("panpak: runtime"), "{t}");
    // The program is rebuilt (the same ABI, other bytes): the runtime put here by this mechanism follows it.
    stub_script(&l.bin.join("panpak-stub"), 1, "second");
    let t = l.install(&pkg);
    assert!(t.contains("panpak: runtime 1 updated from"), "{t}");
    assert!(l.stored_note(1).contains("# second"));
    // A newer ABI is installed beside the old one.
    stub_script(&l.bin.join("panpak-stub"), 2, "third");
    let t = l.install(&pkg);
    assert!(t.contains("panpak: runtime 2 updated from"), "{t}");
    assert!(l.stored_note(2).contains("# third"));
}

#[test]
fn a_runtime_installed_by_hand_is_not_replaced_by_one_of_the_same_abi() {
    let l = Lab::new("hand");
    let pkg = l.package();
    let mine = l.home.join("mine-stub");
    stub_script(&mine, 1, "by hand");
    let o = l.cli(&["install-runtime", mine.to_str().unwrap(), "--allow-unsigned"]);
    assert!(o.status.success(), "{}", text(&o));
    stub_script(&l.bin.join("panpak-stub"), 1, "beside the program");
    let t = l.install(&pkg);
    assert!(!t.contains("panpak: runtime"), "{t}");
    assert!(l.stored_note(1).contains("# by hand"));
}
