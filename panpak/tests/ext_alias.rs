// SPDX-License-Identifier: MIT OR Apache-2.0
// File extensions: `.panpak` is the official one, `.pan` an accepted alias. A package
// is recognised by its magic and signature only, never by its extension.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn work(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("ext-alias-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

struct Env {
    dir: PathBuf,
    key: PathBuf,
    tree: PathBuf,
    /// Stand-in runtime: a script that reports an ABI, enough for pack and install.
    stub: PathBuf,
}

fn cli(env: &Env, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_panpak"))
        .args(args)
        .current_dir(&env.dir)
        .env("HOME", &env.dir)
        .env("XDG_DATA_HOME", env.dir.join("data"))
        .env_remove("PANPAK_SECRET_FILE")
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn setup(name: &str) -> Env {
    let dir = work(name);
    let env = Env { key: dir.join("secret.key"), tree: dir.join("tree"), stub: dir.join("stub"), dir };
    let o = cli(&env, &["keygen", "-o", env.key.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let pubkey = text(&o).lines().find_map(|l| l.strip_prefix("key:    ")).unwrap().trim().to_string();
    fs::create_dir_all(env.tree.join("bin")).unwrap();
    fs::write(env.tree.join("bin/hello"), "#!/bin/sh\necho hi\n").unwrap();
    fs::set_permissions(env.tree.join("bin/hello"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        env.tree.join("panpak.toml"),
        format!(
            "[app]\nid = \"org.example.ext\"\nname = \"Ext\"\nversion = \"1\"\nentry = \"bin/hello\"\n[update]\nkey = \"{pubkey}\"\n"
        ),
    )
    .unwrap();
    fs::write(&env.stub, "#!/bin/sh\necho 1\n").unwrap();
    fs::set_permissions(&env.stub, fs::Permissions::from_mode(0o755)).unwrap();
    let o = cli(&env, &["install-runtime", env.stub.to_str().unwrap(), "--allow-unsigned"]);
    assert!(o.status.success(), "{}", text(&o));
    env
}

/// `doctor` also reports host checks that depend on the machine, so look at the
/// package line only.
fn assert_verified(o: &Output) {
    assert!(text(o).contains("OK   verified package: org.example.ext"), "{}", text(o));
}

fn pack(env: &Env, out: Option<&str>) -> Output {
    let mut a = vec!["pack", env.tree.to_str().unwrap(), "--stub", env.stub.to_str().unwrap(), "--secret-file", env.key.to_str().unwrap()];
    if let Some(o) = out {
        a.extend(["-o", o]);
    }
    cli(env, &a)
}

#[test]
fn pack_default_name_ends_with_panpak() {
    let env = setup("default");
    let o = pack(&env, None);
    assert!(o.status.success(), "{}", text(&o));
    assert!(env.dir.join("org.example.ext.panpak").is_file());
    assert!(!env.dir.join("org.example.ext.pan").exists());
}

#[test]
fn pack_keeps_the_pan_name_it_was_given() {
    let env = setup("pan");
    let o = pack(&env, Some("x.pan"));
    assert!(o.status.success(), "{}", text(&o));
    assert!(env.dir.join("x.pan").is_file());
    assert!(!env.dir.join("x.pan.panpak").exists() && !env.dir.join("x.panpak").exists());
    assert_verified(&cli(&env, &["doctor", "x.pan"]));
}

#[test]
fn install_and_doctor_accept_panpak_pan_and_no_extension() {
    for ext in [".panpak", ".pan", ""] {
        let env = setup(&format!("any{}", ext.replace('.', "-")));
        let name = format!("pkg{ext}");
        let o = pack(&env, Some(&name));
        assert!(o.status.success(), "{ext}: {}", text(&o));
        assert_verified(&cli(&env, &["doctor", &name]));
        let i = cli(&env, &["install", "--yes", &name]);
        assert!(i.status.success(), "install {name}: {}", text(&i));
    }
}

#[test]
fn pan_file_with_wrong_magic_is_rejected_with_e0001() {
    let env = setup("badmagic");
    assert!(pack(&env, Some("good.pan")).status.success());
    let mut bytes = fs::read(env.dir.join("good.pan")).unwrap();
    let n = bytes.len();
    bytes[n - 80] = b'X'; // first byte of the footer magic
    for name in ["bad.pan", "bad.panpak"] {
        fs::write(env.dir.join(name), &bytes).unwrap();
        let d = cli(&env, &["doctor", name]);
        assert!(!d.status.success() && text(&d).contains("INVALID package: wrong magic"), "doctor {name}: {}", text(&d));
        let i = cli(&env, &["install", "--yes", name]);
        assert!(!i.status.success(), "install accepted a wrong magic: {name}");
        assert!(text(&i).contains("PANPAK-E0001"), "install {name}: {}", text(&i));
    }
}

#[test]
fn correct_magic_with_other_extension_is_accepted() {
    let env = setup("bin");
    assert!(pack(&env, Some("good.panpak")).status.success());
    fs::rename(env.dir.join("good.panpak"), env.dir.join("whatever.bin")).unwrap();
    assert_verified(&cli(&env, &["doctor", "whatever.bin"]));
    let i = cli(&env, &["install", "--yes", "whatever.bin"]);
    assert!(i.status.success(), "{}", text(&i));
}
