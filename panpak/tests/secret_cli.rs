// SPDX-License-Identifier: MIT OR Apache-2.0
// Secret-key handling of the CLI: file permissions, symlinks, keygen, no --secret.
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
};

const KEY: &str = "2929292929292929292929292929292929292929292929292929292929292929";

fn work(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secret-cli-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn key_file(dir: &Path, mode: u32) -> PathBuf {
    let p = dir.join("key");
    fs::write(&p, format!("{KEY}\n")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
    p
}

fn cli(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_panpak"));
    c.args(args).env_remove("PANPAK_SECRET_FILE");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn all_output(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

// `release` fails after reading the key (no binaries in a temp cwd), which is
// enough to tell "key accepted" from "key rejected" by the message.
fn release(key: &Path, env: &[(&str, &str)], dir: &Path) -> Output {
    let mut a = vec!["release", "--version", "1", "--out-dir"];
    let out = dir.join("out");
    a.push(out.to_str().unwrap());
    if env.is_empty() {
        a.push("--secret-file");
        a.push(key.to_str().unwrap());
    }
    let mut c = Command::new(env!("CARGO_BIN_EXE_panpak"));
    c.args(&a).current_dir(dir).env_remove("PANPAK_SECRET_FILE");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

#[test]
fn mode_0644_rejected_with_fix_hint() {
    let d = work("0644");
    let k = key_file(&d, 0o644);
    let o = release(&k, &[], &d);
    assert!(!o.status.success());
    let text = all_output(&o);
    assert!(text.contains("PANPAK-E5003"), "{text}");
    assert!(!text.contains(KEY));
}

#[test]
fn mode_0600_accepted() {
    let d = work("0600");
    let k = key_file(&d, 0o600);
    let text = all_output(&release(&k, &[], &d));
    // Past the key check: fails only because there are no built binaries.
    assert!(!text.contains("PANPAK-E5003") && !text.contains("PANPAK-E5002"), "{text}");
    assert!(!text.contains(KEY));
}

#[test]
fn env_var_path_accepted_and_checked() {
    let d = work("env");
    let k = key_file(&d, 0o600);
    let text = all_output(&release(&k, &[("PANPAK_SECRET_FILE", k.to_str().unwrap())], &d));
    assert!(!text.contains("PANPAK-E5003") && !text.contains("PANPAK-E5002"), "{text}");
    fs::set_permissions(&k, fs::Permissions::from_mode(0o640)).unwrap();
    let text = all_output(&release(&k, &[("PANPAK_SECRET_FILE", k.to_str().unwrap())], &d));
    assert!(text.contains("PANPAK-E5003"), "{text}");
}

#[test]
fn symlink_and_directory_rejected() {
    let d = work("link");
    let k = key_file(&d, 0o600);
    let l = d.join("link");
    symlink(&k, &l).unwrap();
    let text = all_output(&release(&l, &[], &d));
    assert!(text.contains("PANPAK-E5003"), "{text}");
    let text = all_output(&release(&d, &[], &d));
    assert!(text.contains("PANPAK-E5003"), "{text}");
}

#[test]
fn keygen_writes_0600_file_and_never_overwrites() {
    let d = work("keygen");
    let f = d.join("new.key");
    let o = cli(&["keygen", "-o", f.to_str().unwrap()], &[]);
    assert!(o.status.success(), "{}", all_output(&o));
    let secret = fs::read_to_string(&f).unwrap();
    assert_eq!(secret.trim().len(), 64);
    assert_eq!(fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
    let text = all_output(&o);
    assert!(!text.contains(secret.trim()), "secret printed");
    assert!(text.contains("ed25519:") && text.contains("fingerprint"));
    // Second run must fail and leave the file untouched.
    let o2 = cli(&["keygen", "-o", f.to_str().unwrap()], &[]);
    assert!(!o2.status.success());
    assert_eq!(fs::read_to_string(&f).unwrap(), secret);
    // keygen without -o does not print a key anywhere.
    let o3 = cli(&["keygen"], &[]);
    assert!(!o3.status.success());
    let t3 = all_output(&o3);
    assert!(t3.starts_with("PANPAK-E5002: ") && !t3.contains("secret:"), "{t3}");
}

#[test]
fn no_command_accepts_secret_or_rotate_from_hex() {
    let d = work("nosecret");
    let k = key_file(&d, 0o600);
    for args in [
        vec!["pack", "tree", "--stub", "s", "--secret", KEY],
        vec!["convert", "x.AppImage", "--id", "a.b", "--stub", "s", "--secret", KEY],
        vec!["release", "--version", "1", "--secret", KEY],
        vec!["pack", "tree", "--stub", "s", "--rotate-from", KEY, "--secret-file", k.to_str().unwrap()],
    ] {
        let o = cli(&args, &[]);
        assert!(!o.status.success(), "{args:?}");
        let text = all_output(&o);
        assert!(text.starts_with("PANPAK-E5001: "), "{args:?}: {text}");
        assert!(!text.contains(KEY), "key echoed for {args:?}");
    }
}
