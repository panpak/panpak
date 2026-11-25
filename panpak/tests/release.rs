// SPDX-License-Identifier: MIT OR Apache-2.0
// The release table and the history (PANPAK-FORMAT.md 3.1.10): what `pack --previous` makes, and what `install`, and `rollback` refuse when a
// package enters over an installed one: an older revision, two publications of one revision, a revision below the floor, another channel,
// a history that does not continue the installed version.
#[path = "../../testsupport/testtmp.rs"]
mod testtmp;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const ID: &str = "org.example.rel";

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_panpak")).env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/panpak-test-no-bus")
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
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("release-{name}-{}", std::process::id()));
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
        cli(&self.home, &[&["install", "--yes", "--trust-first-key"], flags, &[pkg.to_str().unwrap()]].concat())
    }

    fn manifest(&self, pkg: &Path) -> String {
        let m = self.home.join("m.toml");
        let o = cli(&self.home, &["dump-manifest", pkg.to_str().unwrap(), m.to_str().unwrap()]);
        assert!(o.status.success(), "{}", text(&o));
        fs::read_to_string(m).unwrap()
    }
}

fn code_of(o: &Output, code: &str) -> bool {
    !o.status.success() && text(o).contains(code)
}

#[test]
fn a_package_made_after_another_has_the_next_revision_and_a_history_that_the_next_one_extends() {
    let l = lab("chain");
    let v1 = l.pack_ok("1", "a", "", None);
    let m1 = l.manifest(&v1);
    assert!(m1.contains("revision = 1") && m1.contains("channel = \"stable\"") && !m1.contains("history"), "{m1}");
    let v2 = l.pack_ok("2", "b", "", Some(&v1));
    let m2 = l.manifest(&v2);
    assert!(m2.contains("revision = 2") && m2.contains("history = \""), "{m2}");
    let v3 = l.pack_ok("3", "c", "", Some(&v2));
    let m3 = l.manifest(&v3);
    assert!(m3.contains("revision = 3") && m3.contains("history = \"") && !m3.contains(&m2[m2.find("history = ").unwrap()..][..80]), "the history grew: {m3}");
    // The chain installs, one over the other.
    for p in [&v1, &v2, &v3] {
        let o = l.install(p, &[]);
        assert!(o.status.success(), "{}", text(&o));
    }
    // The same inputs give the same bytes.
    let again = l.pack_ok("3", "c", "", Some(&v2));
    assert_eq!(fs::read(&v3).unwrap(), fs::read(again).unwrap());
    // The critical list names the release keys.
    assert!(m3.contains("\"release.revision\"") && m3.contains("\"release.floor\""), "{m3}");
}

#[test]
fn pack_refuses_a_revision_that_does_not_follow_the_previous_package_and_a_history_that_the_author_wrote() {
    let l = lab("packrules");
    let v1 = l.pack_ok("1", "a", "[release]\nrevision = 4\n", None);
    assert!(l.manifest(&v1).contains("revision = 4"));
    // The next one may not be at or below it.
    for rel in ["[release]\nrevision = 4\n", "[release]\nrevision = 2\n"] {
        let o = l.pack("2", "b", rel, Some(&v1)).unwrap_err();
        assert!(text(&o).contains("is not above the revision 4"), "{}", text(&o));
    }
    assert!(l.pack("2", "b", "[release]\nrevision = 5\n", Some(&v1)).is_ok());
    // The history is made by `pack`, never written.
    let o = l.pack("3", "c", "[release]\nrevision = 6\nhistory = \"abc\"\n", None).unwrap_err();
    assert!(text(&o).contains("PANPAK-E0008") && text(&o).contains("release.history"), "{}", text(&o));
    // A bad number is refused with the name of the field.
    let o = l.pack("3", "c", "[release]\nrevision = 3\nfloor = 4\n", None).unwrap_err();
    assert!(text(&o).contains("floor and security cannot be above the revision"), "{}", text(&o));
    // The earlier package must be a package of this app.
    let other = l.home.join("junk.panpak");
    fs::write(&other, b"not a package").unwrap();
    assert!(l.pack("4", "d", "", Some(&other)).is_err());
    // The floor and the security number only grow from a package to the next.
    let f = l.pack_ok("f1", "x", "[release]\nrevision = 3\nfloor = 2\nsecurity = 3\n", None);
    let g = l.pack_ok("f2", "y", "", Some(&f));
    let m = l.manifest(&g);
    assert!(m.contains("revision = 4") && m.contains("floor = 2") && m.contains("security = 3"), "{m}");
}

#[test]
fn an_older_revision_is_a_downgrade_the_same_number_with_other_bytes_is_a_fork_and_the_same_package_is_fine() {
    let l = lab("order");
    let v1 = l.pack_ok("1", "a", "", None);
    let v2 = l.pack_ok("2", "b", "", Some(&v1));
    assert!(l.install(&v2, &[]).status.success());
    // The same package again: nothing to refuse.
    assert!(l.install(&v2, &[]).status.success());
    // An older one: refused, and the way out is named.
    let o = l.install(&v1, &[]);
    assert!(code_of(&o, "PANPAK-E4001") && text(&o).contains("--allow-downgrade"), "{}", text(&o));
    assert!(l.install(&v1, &["--allow-downgrade"]).status.success(), "{}", text(&l.install(&v1, &["--allow-downgrade"])));
    // Another publication of revision 2 (another bytes): a fork or a replay.
    assert!(l.install(&v2, &[]).status.success());
    let fork = l.pack_ok("2fork", "other", "[release]\nrevision = 2\n", None);
    let o = l.install(&fork, &[]);
    assert!(code_of(&o, "PANPAK-E4004") && text(&o).contains("panpak pack --previous"), "{}", text(&o));
    // The version text decides nothing: the fork says a higher version and is still revision 2.
    let o = l.install(&l.pack_ok("9.9.9", "other2", "[release]\nrevision = 2\n", None), &[]);
    assert!(code_of(&o, "PANPAK-E4004"), "{}", text(&o));
}

#[test]
fn nothing_below_the_floor_enters_by_install_and_allow_downgrade_does_not_change_it() {
    let l = lab("floor");
    let v1 = l.pack_ok("1", "a", "", None);
    let v2 = l.pack_ok("2", "b", "", Some(&v1));
    let v3 = l.pack_ok("3", "c", "[release]\nrevision = 3\nfloor = 3\n", Some(&v2));
    assert!(l.manifest(&v3).contains("floor = 3"));
    for p in [&v1, &v2] {
        assert!(l.install(p, &[]).status.success());
    }
    assert!(l.install(&v3, &[]).status.success());
    // An install of an older package is refused for the floor, and `--allow-downgrade` does not open it.
    let o = l.install(&v2, &["--allow-downgrade"]);
    assert!(code_of(&o, "PANPAK-E4003") && text(&o).contains("floor 3"), "{}", text(&o));
    // The floor is kept for the app: even after the installed version goes away from `current` by a rollback elsewhere, it is in the app folder.
    let kept = fs::read_to_string(l.home.join(".local/share/panpak/apps").join(ID).join("trust")).unwrap();
    assert!(kept.lines().any(|l| l == "floor=3"), "{kept}");
}

#[test]
fn a_channel_is_fixed_at_install_and_changes_only_when_the_person_says_so() {
    let l = lab("channel");
    let stable = l.pack_ok("1", "a", "", None);
    assert!(l.install(&stable, &[]).status.success());
    let beta = l.pack_ok("5", "b", "[release]\nrevision = 5\nchannel = \"beta\"\n", None);
    let o = l.install(&beta, &[]);
    assert!(code_of(&o, "PANPAK-E4005") && text(&o).contains("--switch-channel"), "{}", text(&o));
    let o = l.install(&beta, &["--switch-channel"]);
    assert!(o.status.success(), "{}", text(&o));
    // The beta line has its own history: a stable package that follows a beta one starts again (no history from the other line).
    let again = l.pack_ok("6", "c", "", Some(&beta));
    assert!(l.manifest(&again).contains("channel = \"stable\"") && !l.manifest(&again).contains("history"), "{}", l.manifest(&again));
}

#[test]
fn a_switch_of_channel_may_go_to_a_lower_revision_without_the_history_but_not_below_the_floor() {
    let l = lab("switch_down");
    // Stable runs at revision 7 (with a history); the beta line counts from its own 2.
    let s1 = l.pack_ok("1", "a", "[release]\nrevision = 1\n", None);
    let s7 = l.pack_ok("7", "b", "[release]\nrevision = 7\n", Some(&s1));
    for p in [&s1, &s7] {
        assert!(l.install(p, &[]).status.success());
    }
    let beta2 = l.pack_ok("b2", "c", "[release]\nrevision = 2\nchannel = \"beta\"\n", None);
    // Without the option: the channel is refused. With it: no `--allow-downgrade` and no history are asked, the program of the other line is in.
    assert!(code_of(&l.install(&beta2, &[]), "PANPAK-E4005"));
    let o = l.install(&beta2, &["--switch-channel"]);
    assert!(o.status.success(), "{}", text(&o));
    // Back to stable: the beta line is installed now, stable revision 7 is above it and enters as a switch again.
    let o = l.install(&s7, &["--switch-channel"]);
    assert!(o.status.success(), "{}", text(&o));
    // The floor is the only rule left: a beta below the floor that the author set is refused even with the option.
    let s9 = l.pack_ok("9", "d", "[release]\nrevision = 9\nfloor = 9\n", Some(&s7));
    assert!(l.install(&s9, &[]).status.success());
    let beta3 = l.pack_ok("b3", "e", "[release]\nrevision = 3\nchannel = \"beta\"\n", None);
    let o = l.install(&beta3, &["--switch-channel"]);
    assert!(code_of(&o, "PANPAK-E4003"), "{}", text(&o));
}

#[test]
fn a_package_that_does_not_continue_the_installed_version_is_refused_by_its_history() {
    let l = lab("history");
    let v1 = l.pack_ok("1", "a", "", None);
    let v2 = l.pack_ok("2", "b", "", Some(&v1));
    assert!(l.install(&v1, &[]).status.success());
    assert!(l.install(&v2, &[]).status.success());
    // v3 made after another v2 (the same revision, other bytes): the installed v2 is not in its history.
    let v2_other = l.pack_ok("2other", "other", "[release]\nrevision = 2\n", Some(&v1));
    let v3_forked = l.pack_ok("3forked", "c", "", Some(&v2_other));
    let o = l.install(&v3_forked, &[]);
    assert!(code_of(&o, "PANPAK-E4006") && text(&o).contains("not in the history"), "{}", text(&o));
    // v3 with no history at all, over an installed version that has one: the history may not be dropped.
    let v3_bare = l.pack_ok("3bare", "d", "[release]\nrevision = 3\n", None);
    let o = l.install(&v3_bare, &[]);
    assert!(code_of(&o, "PANPAK-E4006") && text(&o).contains("has no history"), "{}", text(&o));
    // v3 made after the installed v2 continues it.
    let v3 = l.pack_ok("3", "e", "", Some(&v2));
    let o = l.install(&v3, &[]);
    assert!(o.status.success(), "{}", text(&o));
    // Neither had a history: nothing is claimed, and an unrelated later revision enters.
    let l = lab("history-none");
    let a = l.pack_ok("1", "a", "", None);
    let b = l.pack_ok("2", "b", "[release]\nrevision = 2\n", None);
    assert!(l.install(&a, &[]).status.success());
    assert!(l.install(&b, &[]).status.success());
    // Nothing was changed by a refusal.
    let l = lab("history-kept");
    let v1 = l.pack_ok("1", "a", "", None);
    let v2 = l.pack_ok("2", "b", "", Some(&v1));
    let bare = l.pack_ok("3", "c", "[release]\nrevision = 3\n", None);
    assert!(l.install(&v1, &[]).status.success() && l.install(&v2, &[]).status.success());
    assert!(!l.install(&bare, &[]).status.success());
    let o = cli(&l.home, &["list"]);
    assert!(text(&o).contains('2') && !text(&o).contains("3"), "{}", text(&o));
}
