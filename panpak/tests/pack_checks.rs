// SPDX-License-Identifier: MIT OR Apache-2.0
// What `pack` works out from the files of a tree (the glibc it needs, the Qt plugins it lacks) and what `install` does with it.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_panpak"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env_remove("PANPAK_SECRET_FILE")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

struct Lab {
    home: PathBuf,
    key: String,
    keyfile: PathBuf,
    stub: PathBuf,
}

/// The folder holds a copy of `panpak` (install copies it): removed when the test ends, kept if it failed.
impl Drop for Lab {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = Command::new("chmod").args(["-R", "u+w"]).arg(&self.home).output();
            let _ = fs::remove_dir_all(&self.home);
        }
    }
}

fn lab(name: &str) -> Lab {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("pack-checks-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join("run")).unwrap();
    fs::set_permissions(home.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
    let keyfile = home.join("secret.key");
    let o = cli(&home, &["keygen", "-o", keyfile.to_str().unwrap()]);
    assert!(o.status.success());
    let key = text(&o).lines().find_map(|l| l.strip_prefix("key:").map(|k| k.trim().to_string())).unwrap();
    let stub = home.join("stub");
    fs::write(&stub, "#!/bin/sh\necho 1\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let o = cli(&home, &["install-runtime", stub.to_str().unwrap(), "--allow-unsigned"]);
    assert!(o.status.success(), "{}", text(&o));
    Lab { home, key, keyfile, stub }
}

impl Lab {
    /// A tree with the given files (path, bytes, mode) and a manifest with `extra` appended; packed. Returns the output and the package path.
    fn pack(&self, name: &str, files: &[(&str, Vec<u8>)], extra: &str) -> (Output, PathBuf) {
        let tree = self.home.join(format!("tree-{name}"));
        for (p, d) in files {
            let f = tree.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(&f, d).unwrap();
            fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(
            tree.join("panpak.toml"),
            format!("[app]\nid = \"org.example.{name}\"\nname = \"{name}\"\nversion = \"1\"\nentry = \"bin/app\"\n{extra}\n[update]\nkey = \"{}\"\n", self.key),
        )
        .unwrap();
        let pkg = self.home.join(format!("{name}.panpak"));
        let o = cli(&self.home, &["pack", tree.to_str().unwrap(), "--stub", self.stub.to_str().unwrap(), "--secret-file", self.keyfile.to_str().unwrap(), "-o", pkg.to_str().unwrap()]);
        (o, pkg)
    }

    fn manifest(&self, pkg: &Path) -> String {
        let out = self.home.join("m.toml");
        let o = cli(&self.home, &["dump-manifest", pkg.to_str().unwrap(), out.to_str().unwrap()]);
        assert!(o.status.success(), "{}", text(&o));
        fs::read_to_string(out).unwrap()
    }
}

fn glibc_min_of(manifest: &str) -> Option<String> {
    manifest.lines().find_map(|l| l.strip_prefix("glibc_min = ").map(|v| v.trim_matches('"').to_string()))
}

/// A small real glibc program of this system, as the bytes of a tree file.
fn host_elf() -> Vec<u8> {
    for p in ["/usr/bin/true", "/bin/true", "/usr/bin/env", "/bin/ls"] {
        if let Ok(b) = fs::read(p) {
            let tmp = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("pack-checks-probe-{}", std::process::id()));
            fs::write(&tmp, &b).unwrap();
            let uses = panpak_core::glibc_needed(&tmp).ok().flatten().is_some();
            let _ = fs::remove_file(&tmp);
            if uses {
                return b;
            }
        }
    }
    panic!("no glibc program found on this system");
}

#[test]
fn pack_writes_the_glibc_the_files_need_and_install_checks_it() {
    let l = lab("glibc");
    let elf = host_elf();
    let tmp = l.home.join("probe-elf");
    fs::write(&tmp, &elf).unwrap();
    let expect = panpak_core::glibc_needed(&tmp).unwrap().expect("a program of this system uses glibc symbol versions");
    let want = expect.iter().map(u32::to_string).collect::<Vec<_>>().join(".");
    let (o, pkg) = l.pack("withelf", &[("bin/app", elf.clone())], "");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(glibc_min_of(&l.manifest(&pkg)).as_deref(), Some(want.as_str()));
    // This system has at least that glibc (the program runs here), so the install is not refused for it.
    let o = cli(&l.home, &["install", "--yes", pkg.to_str().unwrap()]);
    assert!(!text(&o).contains("PANPAK-E2005"), "{}", text(&o));

    // A script needs no glibc: no field.
    let (o, pkg) = l.pack("script", &[("bin/app", b"#!/bin/sh\necho hi\n".to_vec())], "");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(glibc_min_of(&l.manifest(&pkg)), None);

    // An author can raise the floor, and install refuses on this system, naming both versions.
    let (o, pkg) = l.pack("raised", &[("bin/app", b"#!/bin/sh\necho hi\n".to_vec())], "[abi]\nglibc_min = \"99.1\"\n");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(glibc_min_of(&l.manifest(&pkg)).as_deref(), Some("99.1"));
    let o = cli(&l.home, &["install", "--yes", pkg.to_str().unwrap()]);
    let t = text(&o);
    assert!(!o.status.success() && t.contains("PANPAK-E2005") && t.contains("needs glibc 99.1 or newer") && t.contains("this system has glibc"), "{t}");
    // Nothing was installed.
    assert!(!l.home.join(".local/share/panpak/apps/org.example.raised").exists());

    // A floor lower than what the files need is not kept: the file wins.
    let (o, pkg) = l.pack("lowered", &[("bin/app", elf)], "[abi]\nglibc_min = \"2.1\"\n");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(glibc_min_of(&l.manifest(&pkg)).as_deref(), Some(want.as_str()));

    // A badly written floor is refused by pack.
    let (o, _) = l.pack("bad", &[("bin/app", b"#!/bin/sh\n".to_vec())], "[abi]\nglibc_min = \"two\"\n");
    assert!(!o.status.success());
}

#[test]
fn pack_says_what_a_qt_tree_lacks() {
    let l = lab("qt");
    let fake = |_n: &str| b"not an elf, only a name for the check".to_vec();
    let app = ("bin/app", b"\x7fELF-not-really".to_vec());
    // No Qt: silence.
    let (o, _) = l.pack("noqt", std::slice::from_ref(&app), "");
    assert!(o.status.success() && !text(&o).contains("Qt"), "{}", text(&o));
    // Qt without the portal theme: the note.
    let qt = [app.clone(), ("lib/libQt6Core.so.6", fake("core"))];
    let (o, _) = l.pack("qt1", &qt, "");
    let t = text(&o);
    assert!(o.status.success() && t.contains("WARNING") && t.contains("platformthemes/libqxdgdesktopportal.so"), "{t}");
    assert!(!t.contains("libqsvg"), "no Svg library in the tree: no note about SVG plugins\n{t}");
    // With the plugin: silence. With libQt6Svg and no SVG plugins: the SVG note.
    let mut with = qt.to_vec();
    with.push(("plugins/platformthemes/libqxdgdesktopportal.so", fake("theme")));
    let (o, _) = l.pack("qt2", &with, "");
    assert!(o.status.success() && !text(&o).contains("WARNING"), "{}", text(&o));
    with.push(("lib/libQt6Svg.so.6", fake("svg")));
    let (o, _) = l.pack("qt3", &with, "");
    let t = text(&o);
    assert!(t.contains("imageformats/libqsvg.so") && !t.contains("platformthemes"), "{t}");
    with.push(("plugins/imageformats/libqsvg.so", fake("fmt")));
    with.push(("plugins/iconengines/libqsvgicon.so", fake("eng")));
    let (o, _) = l.pack("qt4", &with, "");
    assert!(o.status.success() && !text(&o).contains("WARNING"), "{}", text(&o));
}

#[test]
fn pack_checks_the_env_table_and_the_entry() {
    let l = lab("env");
    let app = ("bin/app", b"\x7fELF-not-really".to_vec());
    let schemas = ("share/glib-2.0/schemas/gschemas.compiled", b"x".to_vec());
    // A good table: paths in the tree, one word.
    let good = "[env]\nGSETTINGS_SCHEMA_DIR = \"${PANPAK_ROOT}/share/glib-2.0/schemas\"\nXDG_DATA_DIRS = \"${PANPAK_ROOT}/share\"\nGSETTINGS_BACKEND = \"keyfile\"\n";
    let (o, pkg) = l.pack("envok", &[app.clone(), schemas.clone()], good);
    assert!(o.status.success(), "{}", text(&o));
    let m = l.manifest(&pkg);
    assert!(m.contains("[env]") && m.contains("GSETTINGS_BACKEND = \"keyfile\""), "{m}");
    // A path that is not in the tree.
    let (o, _) = l.pack("envmissing", std::slice::from_ref(&app), "[env]\nGSETTINGS_SCHEMA_DIR = \"${PANPAK_ROOT}/share/schemas\"\n");
    let t = text(&o);
    assert!(!o.status.success() && t.contains("PANPAK-E0007") && t.contains("env.GSETTINGS_SCHEMA_DIR") && t.contains("not in the tree"), "{t}");
    // Names that are not on the list, absolute paths, paths that leave the tree, a word that is not allowed.
    for bad in [
        "LD_PRELOAD = \"${PANPAK_ROOT}/lib/x.so\"",
        "PATH = \"${PANPAK_ROOT}/bin\"",
        "GSETTINGS_SCHEMA_DIR = \"/usr/share/glib-2.0/schemas\"",
        "GSETTINGS_SCHEMA_DIR = \"${PANPAK_ROOT}/../etc\"",
        "GSETTINGS_BACKEND = \"dconf\"",
    ] {
        let (o, _) = l.pack("envbad", &[app.clone(), schemas.clone()], &format!("[env]\n{bad}\n"));
        assert!(!o.status.success() && text(&o).contains("PANPAK-E0007"), "{bad}\n{}", text(&o));
    }
    // A shell script as the entry: a warning for a package made here.
    let (o, _) = l.pack("script", &[("bin/app", b"#!/bin/sh\nexec x\n".to_vec())], "");
    assert!(o.status.success() && text(&o).contains("is a shell script: use the [env] table"), "{}", text(&o));
    let (o, _) = l.pack("notscript", &[app], "");
    assert!(!text(&o).contains("shell script"), "{}", text(&o));
}
