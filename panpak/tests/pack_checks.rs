// SPDX-License-Identifier: MIT OR Apache-2.0
// What `pack` works out from the files of a tree (the glibc it needs, the Qt plugins it lacks) and what `install` does with it.
#[path = "../../testsupport/testtmp.rs"]
mod testtmp;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_panpak")).env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/panpak-test-no-bus")
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("PANPAK_TEST_LINK_STORE", "1")
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env_remove("PANPAK_SECRET_FILE")
        // The texts that these tests read are the English ones, whatever the language of the person who runs them.
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
        self.pack_with(name, files, extra, true)
    }

    /// The same, with or without the links that `build` and `convert` make next to the programs (`links`).
    fn pack_with(&self, name: &str, files: &[(&str, Vec<u8>)], extra: &str, links: bool) -> (Output, PathBuf) {
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
        if links {
            panpak_core::libpath::make_links(&tree, &format!("org.example.{name}"), &mut |p, t| Ok(std::os::unix::fs::symlink(t, tree.join(p))?)).unwrap();
        }
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

fn host_glibc_of(manifest: &str) -> Option<String> {
    manifest.lines().find_map(|l| l.strip_prefix("glibc = ").map(|v| v.trim_matches('"').to_string()))
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
    assert_eq!(host_glibc_of(&l.manifest(&pkg)).as_deref(), Some(want.as_str()));
    // This system has at least that glibc (the program runs here), so the install is not refused for it.
    let o = cli(&l.home, &["install", "--yes", "--trust-first-key", pkg.to_str().unwrap()]);
    assert!(!text(&o).contains("PANPAK-E2005"), "{}", text(&o));

    // A script needs no glibc: no field.
    let (o, pkg) = l.pack("script", &[("bin/app", b"#!/bin/sh\necho hi\n".to_vec())], "");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(host_glibc_of(&l.manifest(&pkg)), None);

    // An author can raise the floor, and install refuses on this system, naming both versions.
    let (o, pkg) = l.pack("raised", &[("bin/app", b"#!/bin/sh\necho hi\n".to_vec())], "[host]\nglibc = \"99.1\"\n");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(host_glibc_of(&l.manifest(&pkg)).as_deref(), Some("99.1"));
    let o = cli(&l.home, &["install", "--yes", "--trust-first-key", pkg.to_str().unwrap()]);
    let t = text(&o);
    assert!(!o.status.success() && t.contains("PANPAK-E2005") && t.contains("needs glibc 99.1 or newer") && t.contains("found glibc"), "{t}");
    // Nothing was installed.
    assert!(!l.home.join(".local/share/panpak/apps/org.example.raised").exists());

    // A floor lower than what the files need is not kept: the file wins.
    let (o, pkg) = l.pack("lowered", &[("bin/app", elf)], "[host]\nglibc = \"2.1\"\n");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(host_glibc_of(&l.manifest(&pkg)).as_deref(), Some(want.as_str()));

    // A badly written floor is refused by pack.
    let (o, _) = l.pack("bad", &[("bin/app", b"#!/bin/sh\n".to_vec())], "[host]\nglibc = \"two\"\n");
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

#[test]
fn pack_refuses_a_link_that_points_outside_the_tree_and_says_which_and_how_to_fix_it() {
    let l = lab("outsidelink");
    let (o, _) = l.pack("outsidelink", &[("bin/app", b"\x7fELF".to_vec())], "");
    assert!(o.status.success(), "{}", text(&o));
    let tree = l.home.join("tree-outsidelink");
    let out = l.home.join("again.panpak");
    let pack = || cli(&l.home, &["pack", tree.to_str().unwrap(), "--stub", l.stub.to_str().unwrap(), "--secret-file", l.keyfile.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    // Links that stay inside, in the folder of the link, are fine.
    std::os::unix::fs::symlink("app", tree.join("bin/app2")).unwrap();
    std::os::unix::fs::symlink("../bin/app", tree.join("bin/app3")).unwrap();
    assert!(pack().status.success());
    for (name, target) in [("etc-link", "/etc/passwd"), ("up-link", "../../../elsewhere")] {
        std::os::unix::fs::symlink(target, tree.join("bin").join(name)).unwrap();
        let o = pack();
        let t = text(&o);
        assert!(!o.status.success(), "{target}: {t}");
        assert!(t.contains(&format!("bin/{name}")) && t.contains(target) && t.contains("outside the package") && t.contains("ln -sfn"), "{t}");
        fs::remove_file(tree.join("bin").join(name)).unwrap();
    }
}

#[test]
fn doctor_on_a_package_counts_the_libraries_it_carries_before_the_first_start() {
    let l = lab("doctorlibs");
    let app = ("bin/app", elf_needing("libcarried.so.1"));
    // The library is in the package: nothing is missing, although the tree is not on the disk yet (the app was never started).
    let (o, pkg) = l.pack("withlib", &[app.clone(), ("lib/libcarried.so.1", b"\x7fELF-not-read".to_vec())], "");
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&cli(&l.home, &["doctor", pkg.to_str().unwrap()]));
    assert!(t.contains("verified package") && !t.contains("unresolved libraries"), "{t}");
    // Through a link, as sonames usually are.
    let (o, pkg) = l.pack("withlink", &[app.clone(), ("lib/libcarried.so.1.2", b"\x7fELF-not-read".to_vec())], "");
    assert!(o.status.success());
    let tree = l.home.join("tree-withlink");
    std::os::unix::fs::symlink("libcarried.so.1.2", tree.join("lib/libcarried.so.1")).unwrap();
    let again = l.home.join("again.panpak");
    let o = cli(&l.home, &["pack", tree.to_str().unwrap(), "--stub", l.stub.to_str().unwrap(), "--secret-file", l.keyfile.to_str().unwrap(), "-o", again.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&cli(&l.home, &["doctor", again.to_str().unwrap()]));
    assert!(!t.contains("unresolved libraries"), "{t}");
    let _ = pkg;
    // The library is nowhere: the app cannot start, and doctor says which library.
    let (o, pkg) = l.pack("withoutlib", &[app], "");
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&cli(&l.home, &["doctor", pkg.to_str().unwrap()]));
    assert!(t.contains("unresolved libraries: libcarried.so.1 missing on the host and not included in the tree"), "{t}");
}

#[test]
fn doctor_finds_a_module_that_has_no_lib_prefix() {
    // FreeCAD: `usr/lib/Mesh.so` is asked for as `Mesh.so`; the inventory used to take only `lib*.so*`.
    let l = lab("doctormodule");
    let app = ("bin/app", elf_needing("Mesh.so"));
    let (o, pkg) = l.pack("withmodule", &[app.clone(), ("usr/lib/Mesh.so", elf_needing("libc.so.6"))], "");
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&cli(&l.home, &["doctor", pkg.to_str().unwrap()]));
    assert!(t.contains("verified package") && !t.contains("unresolved libraries"), "{t}");
    // The module is nowhere: said, with its name.
    let (o, pkg) = l.pack("withoutmodule", &[app], "");
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&cli(&l.home, &["doctor", pkg.to_str().unwrap()]));
    assert!(t.contains("unresolved libraries: Mesh.so missing on the host and not included in the tree"), "{t}");
}

#[test]
fn doctor_names_every_unresolved_library_in_one_line() {
    // Two programs of the package ask for a library each, and nothing provides either: one run says both (a packager used to need one run for each).
    let l = lab("doctorall");
    let (o, pkg) = l.pack("two", &[("bin/app", elf_needing("libfirst.so.1")), ("bin/tool", elf_needing("Second.so"))], "");
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&cli(&l.home, &["doctor", pkg.to_str().unwrap()]));
    let line = t.lines().find(|ln| ln.contains("unresolved libraries")).unwrap_or_else(|| panic!("{t}"));
    assert!(line.contains("libfirst.so.1") && line.contains("Second.so") && line.contains("missing on the host and not included in the tree"), "{line}");
}

#[test]
fn a_library_that_only_a_module_asks_for_is_not_an_error_and_inspect_says_it_is_not_available() {
    // The program is fine; a plugin asks for a library that nothing provides. Nobody declared anything: the signed report of the modules says that no program
    // needs it, so the package installs, doctor has no error, and inspect lists the library.
    let l = lab("moduleonly");
    let (o, pkg) = l.pack("plug", &[("bin/app", elf_needing("libc.so.6")), ("lib/plugin.so", elf_needing("libplugin-only.so.5"))], "");
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&cli(&l.home, &["doctor", pkg.to_str().unwrap()]));
    assert!(!t.contains("unresolved libraries"), "{t}");
    let t = text(&cli(&l.home, &["inspect", pkg.to_str().unwrap()]));
    assert!(t.contains("not available on this system") && t.contains("libplugin-only.so.5: asked by lib/plugin.so"), "{t}");
    // The manifest signs the hash of the report, and the report is in the package.
    let man = l.manifest(&pkg);
    assert!(man.contains("modules = \""), "{man}");
    let json = text(&cli(&l.home, &["inspect", "--json", pkg.to_str().unwrap()]));
    assert!(json.contains("\"modules_not_available\":[\"libplugin-only.so.5\"]"), "{json}");
    // A program that asks for it makes it required: an error again.
    let (o, pkg) = l.pack("prog", &[("bin/app", elf_needing("libplugin-only.so.5"))], "");
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&cli(&l.home, &["doctor", pkg.to_str().unwrap()]));
    assert!(t.contains("unresolved libraries: libplugin-only.so.5 missing"), "{t}");
}

#[test]
fn inspect_says_in_one_line_when_a_script_of_a_converted_app_sets_the_library_path() {
    let l = lab("inspectscript");
    let origin = "[origin]\nconverted_from = \"x.AppImage\"\nversion = \"1\"\nsha256 = \"0000000000000000000000000000000000000000000000000000000000000000\"\nsigned_by_upstream = false\n";
    let script = b"#!/bin/sh\nHERE=$(dirname \"$(readlink -f \"$0\")\")\nexport LD_LIBRARY_PATH=\"$HERE/lib\":$LD_LIBRARY_PATH\nexec \"$HERE/bin/app\" \"$@\"\n".to_vec();
    let quiet = b"#!/bin/sh\n# export LD_LIBRARY_PATH=$HERE/lib\nexec true\n".to_vec();
    let app = ("bin/app", b"#!/bin/sh\nexec true\n".to_vec());
    let inspect = |pkg: &Path| text(&cli(&l.home, &["inspect", pkg.to_str().unwrap()]));
    // Converted, with a script that sets it: one line, with the path of the script.
    let (o, pkg) = l.pack("withscript", &[app.clone(), ("usr/bin/launcher", script.clone())], origin);
    assert!(o.status.success(), "{}", text(&o));
    let t = inspect(&pkg);
    let lines: Vec<&str> = t.lines().filter(|ln| ln.contains("LD_LIBRARY_PATH")).collect();
    assert_eq!(lines.len(), 1, "{t}");
    assert!(lines[0].contains("usr/bin/launcher") && lines[0].contains("sets LD_LIBRARY_PATH"), "{}", lines[0]);
    // Converted, with a script that only mentions it in a comment: nothing.
    let (o, pkg) = l.pack("quietscript", &[app.clone(), ("usr/bin/launcher", quiet)], origin);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!inspect(&pkg).contains("LD_LIBRARY_PATH"));
    // Not converted (a package made here): the line is for converted apps only.
    let (o, pkg) = l.pack("madehere", &[app, ("usr/bin/launcher", script)], "");
    assert!(o.status.success(), "{}", text(&o));
    assert!(!inspect(&pkg).contains("LD_LIBRARY_PATH"));
}

/// A minimal little-endian ELF64 shared object whose only dynamic entry is DT_NEEDED `name` (enough for the DT_NEEDED reader).
fn elf_needing(name: &str) -> Vec<u8> {
    let mut strtab = vec![0u8];
    let off = strtab.len() as u64;
    strtab.extend_from_slice(name.as_bytes());
    strtab.push(0);
    // Layout: ELF header (64), a PT_LOAD, a PT_DYNAMIC and a PT_INTERP program header (3 * 56), then the dynamic section and the strings. The interpreter
    // makes a file that is not named `.so` a program, whose libraries are required.
    let ph_off = 64u64;
    let dyn_off = ph_off + 3 * 56;
    let dyn_size = 4 * 16u64; // NEEDED, STRTAB, STRSZ, NULL
    let str_off = dyn_off + dyn_size;
    let total = str_off + strtab.len() as u64;
    let mut b = Vec::new();
    b.extend_from_slice(b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0");
    b.extend_from_slice(&3u16.to_le_bytes()); // ET_DYN
    b.extend_from_slice(&62u16.to_le_bytes()); // x86-64
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes()); // entry
    b.extend_from_slice(&ph_off.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes()); // no section headers
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&64u16.to_le_bytes());
    b.extend_from_slice(&56u16.to_le_bytes());
    b.extend_from_slice(&3u16.to_le_bytes());
    b.extend_from_slice(&64u16.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    let ph = |b: &mut Vec<u8>, ty: u32, off: u64, size: u64| {
        b.extend_from_slice(&ty.to_le_bytes());
        b.extend_from_slice(&5u32.to_le_bytes());
        for v in [off, off, off, size, size, 8u64] {
            b.extend_from_slice(&v.to_le_bytes());
        }
    };
    ph(&mut b, 1, 0, total); // PT_LOAD
    ph(&mut b, 2, dyn_off, dyn_size); // PT_DYNAMIC
    ph(&mut b, 3, dyn_off, 1); // PT_INTERP
    for (tag, val) in [(1u64, off), (5, str_off), (10, strtab.len() as u64), (0, 0)] {
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&val.to_le_bytes());
    }
    b.extend_from_slice(&strtab);
    b
}

#[test]
fn pack_refuses_a_program_without_the_links_of_the_library_path() {
    let l = lab("links");
    let elf = host_elf();
    // A program in bin/ and one in libexec/: no links at all.
    let files = [("bin/app", elf.clone()), ("libexec/helper", elf.clone())];
    let (o, _) = l.pack_with("nolinks", &files, "", false);
    let t = text(&o);
    assert!(!o.status.success() && t.contains("PANPAK-E0009"), "{t}");
    let n = panpak_core::libpath::link_name("org.example.nolinks");
    // It names the three links, with the commands that make them.
    for want in [format!("ln -s . {n}"), format!("ln -s .. bin/{n}"), format!("ln -s .. libexec/{n}")] {
        assert!(t.contains(&want), "missing `{want}` in: {t}");
    }
    // With the links it is packed. Only the folder of the second program lacks its link: refused, and it is the only one named.
    let (o, _) = l.pack_with("onelink", &files, "", true);
    assert!(o.status.success(), "{}", text(&o));
    let tree = l.home.join("tree-onelink");
    let n1 = panpak_core::libpath::link_name("org.example.onelink");
    fs::remove_file(tree.join("libexec").join(&n1)).unwrap();
    fs::remove_file(tree.join("panpak.toml")).unwrap();
    fs::write(
        tree.join("panpak.toml"),
        format!("[app]\nid = \"org.example.onelink\"\nname = \"x\"\nversion = \"1\"\nentry = \"bin/app\"\n[update]\nkey = \"{}\"\n", l.key),
    )
    .unwrap();
    let o = cli(&l.home, &["pack", tree.to_str().unwrap(), "--stub", l.stub.to_str().unwrap(), "--secret-file", l.keyfile.to_str().unwrap(), "-o", l.home.join("x.panpak").to_str().unwrap()]);
    let t = text(&o);
    assert!(!o.status.success() && t.contains("PANPAK-E0009") && t.contains(&format!("ln -s .. libexec/{n1}")) && !t.contains(&format!("bin/{n1}")), "{t}");
    // A link with another target is as good as none.
    fs::remove_file(tree.join("bin").join(&n1)).unwrap();
    std::os::unix::fs::symlink("../lib", tree.join("bin").join(&n1)).unwrap();
    let o = cli(&l.home, &["pack", tree.to_str().unwrap(), "--stub", l.stub.to_str().unwrap(), "--secret-file", l.keyfile.to_str().unwrap(), "-o", l.home.join("x.panpak").to_str().unwrap()]);
    assert!(!o.status.success() && text(&o).contains(&format!("ln -s .. bin/{n1}")), "{}", text(&o));
    // A script needs none, and a library (the same bytes, a `.so` name) neither.
    let (o, _) = l.pack_with("scripts", &[("bin/app", b"#!/bin/sh\n".to_vec()), ("lib/libx.so.1", elf.clone())], "", false);
    assert!(o.status.success(), "{}", text(&o));
    // The name `.farm` in the root belongs to the runtime.
    let (o, _) = l.pack_with("farm", &[("bin/app", b"#!/bin/sh\n".to_vec()), (".farm/x", b"x".to_vec())], "", false);
    assert!(!o.status.success() && text(&o).contains(".farm"), "{}", text(&o));
}
