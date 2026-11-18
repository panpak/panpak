// SPDX-License-Identifier: MIT OR Apache-2.0
//! The library path of an app, valid only for the app's own programs. PANPAK-FORMAT.md §7.1.
//!
//! The runtime puts the farm and the library folders of the package in `LD_LIBRARY_PATH` of the app. Written as absolute
//! paths, the variable also reaches every system program that the app starts (a shell, `zenity`, `flatpak`), and those
//! load the package's libraries instead of their own. So the value is written relative to the program that is run:
//!
//! ```text
//! LD_LIBRARY_PATH=/$ORIGIN/.panpak-lib-<h>/.farm:/$ORIGIN/.panpak-lib-<h>/lib:...
//! ```
//!
//! glibc replaces `$ORIGIN` with the folder of the program (`//<folder>/.panpak-lib-<h>/...`). Next to every executable of
//! the package there is a link `.panpak-lib-<h>` to the root of the tree, so the path leads into the tree. For a program
//! of the host, the folder is `/usr/bin` or alike, where the link does not exist, and the loader skips the entry. The
//! slash in front keeps the value harmless on musl, which does not expand `$ORIGIN` and takes a relative entry as a path
//! under the current folder: with the slash the entry is `/$ORIGIN/...` in the root of the file system.
//!
//! `<h>` is derived from the app id, not from the root hash: the links are part of the content that the root hash covers.
use crate::{coded, hex, ErrorCode};
use anyhow::{Context, Result};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, File},
    io::Read,
    os::unix::{ffi::OsStringExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};

/// Start of the name of the link that leads from a program's folder to the root of the tree.
pub const LINK_PREFIX: &str = ".panpak-lib-";
/// The link in the root of the tree that leads to the farm of the app. Made by the runtime when it makes the tree.
pub const FARM_LINK: &str = ".farm";

/// Most program headers read from one file: a real executable has a dozen.
const MAX_PHDRS: usize = 128;
const PT_INTERP: u32 = 3;

/// The name of the link for an app: `.panpak-lib-` and 8 hex digits of the hash of the id.
pub fn link_name(app_id: &str) -> String {
    format!("{LINK_PREFIX}{}", &hex(blake3::hash(app_id.as_bytes()).as_bytes())[..8])
}

/// Where the link in `dir` (a folder of the tree, relative to its root) points: `.` for the root, `..`, `../..` below it.
pub fn link_target(dir: &Path) -> String {
    let depth = dir.components().filter(|c| matches!(c, std::path::Component::Normal(_))).count();
    if depth == 0 {
        ".".to_string()
    } else {
        vec![".."; depth].join("/")
    }
}

/// Where the farm link of a tree points, from the root of the tree: the farm is `<store>/farms/<root>`, the tree
/// `<store>/trees/<root>`.
pub fn farm_target(root_hex: &str) -> String {
    format!("../../farms/{root_hex}")
}

/// Is this file a program of the package: an ELF executable with an interpreter (PT_INTERP), not named like a library
/// (`.so` in the name), with an execute bit? Only the headers are read, bounded; a file that is not an ELF, or whose
/// headers do not fit, is not a program.
pub fn is_program(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
    if name.contains(".so") {
        return false;
    }
    let Ok(mut f) = File::open(path) else { return false };
    let Ok(meta) = f.metadata() else { return false };
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return false;
    }
    has_interp(&mut f).unwrap_or(false)
}

fn has_interp(f: &mut File) -> Option<bool> {
    let mut h = [0u8; 64];
    f.read_exact(&mut h).ok()?;
    if &h[0..4] != b"\x7fELF" || h[5] != 1 {
        return Some(false); // not an ELF, or not little-endian
    }
    let (phoff, phsize, phnum) = match h[4] {
        2 => (u64::from_le_bytes(h[0x20..0x28].try_into().ok()?), u16::from_le_bytes(h[0x36..0x38].try_into().ok()?), u16::from_le_bytes(h[0x38..0x3a].try_into().ok()?)),
        1 => (u32::from_le_bytes(h[0x1c..0x20].try_into().ok()?) as u64, u16::from_le_bytes(h[0x2a..0x2c].try_into().ok()?), u16::from_le_bytes(h[0x2c..0x2e].try_into().ok()?)),
        _ => return Some(false),
    };
    let (phsize, phnum) = (phsize as usize, phnum as usize);
    let min = if h[4] == 2 { 56 } else { 32 };
    if phnum == 0 || phnum > MAX_PHDRS || phsize < min || phsize > 256 {
        return Some(false);
    }
    use std::os::unix::fs::FileExt;
    let mut ph = vec![0u8; phsize * phnum];
    f.read_exact_at(&mut ph, phoff).ok()?;
    Some((0..phnum).any(|i| u32::from_le_bytes(ph[i * phsize..i * phsize + 4].try_into().unwrap()) == PT_INTERP))
}

/// The folders (relative to `root`, the root itself as the empty path) that hold a program. Links are not followed.
pub fn program_dirs(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut out = BTreeSet::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let dir = root.join(&rel);
        for e in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let e = e?;
            let ty = e.file_type()?;
            if ty.is_dir() {
                stack.push(rel.join(e.file_name()));
            } else if ty.is_file() && is_program(&e.path()) {
                out.insert(rel.clone());
            }
        }
    }
    Ok(out)
}

/// The links a package needs: one in each folder that holds a program, and one in the root (the mark by which the runtime
/// knows that the package is made this way). Empty when there is no program. Each is `(path of the link, target)`.
pub fn required_links(app_id: &str, dirs: &BTreeSet<PathBuf>) -> Vec<(PathBuf, String)> {
    if dirs.is_empty() {
        return Vec::new();
    }
    let name = link_name(app_id);
    let mut all: BTreeSet<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    all.insert(Path::new(""));
    all.into_iter().map(|d| (d.join(&name), link_target(d))).collect()
}

/// The links of `required_links` that `have` does not report with the right target (`have` gives the target of the link at
/// a path, or `None`).
pub fn missing_links(app_id: &str, dirs: &BTreeSet<PathBuf>, have: &dyn Fn(&Path) -> Option<String>) -> Vec<(PathBuf, String)> {
    required_links(app_id, dirs).into_iter().filter(|(p, t)| have(p).as_deref() != Some(t.as_str())).collect()
}

/// The text of the error of `pack` for links that are missing: names them and gives the commands.
pub fn missing_error(missing: &[(PathBuf, String)]) -> anyhow::Error {
    let mut text = String::from("the package has programs, and each folder with a program needs a link that makes the library path of the app valid for that program only (PANPAK-FORMAT.md section 7.1). Missing:");
    for (p, t) in missing.iter().take(8) {
        text.push_str(&format!("\n  ln -s {t} {}", p.display()));
    }
    if missing.len() > 8 {
        text.push_str(&format!("\n  ... and {} more", missing.len() - 8));
    }
    text.push_str("\n`panpak build` and `panpak convert` make these links; for a folder you pack by hand, run the commands (from the folder).");
    coded(ErrorCode::E0009, text)
}

/// A package whose program has no link in the root of the tree: it was not made by a current `panpak pack` (those have no absolute
/// library list to fall back on any more).
pub fn no_links_error() -> anyhow::Error {
    coded(ErrorCode::E0009, "this package has a program and none of the links that give it its library path (PANPAK-FORMAT.md section 7.1): it was made with a test version of panpak; make it again with the current panpak")
}

/// Make the links of `required_links` for the tree at `root` through `create(path, target)`, which the caller supplies so
/// that a tree under a confined writer is written the way it writes everything else. A link that is already right is left;
/// anything else at that path is an error. Returns how many were made.
pub fn make_links(root: &Path, app_id: &str, create: &mut dyn FnMut(&Path, &str) -> Result<()>) -> Result<usize> {
    let mut n = 0;
    for (path, target) in required_links(app_id, &program_dirs(root)?) {
        match fs::symlink_metadata(root.join(&path)) {
            Ok(m) if m.file_type().is_symlink() && fs::read_link(root.join(&path))?.to_str() == Some(target.as_str()) => continue,
            Ok(_) => return Err(coded(ErrorCode::E0009, format!("{} exists and is not the link `{target}` that the library path needs; remove it", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("stat {}", path.display())),
        }
        create(&path, &target)?;
        n += 1;
    }
    Ok(n)
}

/// The value of `LD_LIBRARY_PATH` of an app of this kind: `ld_path` is the list the runtime resolved (the farm first, then
/// the folders of the tree), as absolute paths; each becomes an entry under `/$ORIGIN/<link>`.
pub fn env_value(app_id: &str, tree: &Path, farm: &Path, ld_path: &[PathBuf]) -> Result<OsString> {
    let base = format!("/$ORIGIN/{}", link_name(app_id));
    let mut out: Vec<u8> = Vec::new();
    for (i, p) in ld_path.iter().enumerate() {
        if i > 0 {
            out.push(b':');
        }
        out.extend_from_slice(base.as_bytes());
        let rel: PathBuf = if p == farm {
            PathBuf::from(FARM_LINK)
        } else {
            p.strip_prefix(tree).with_context(|| format!("{} is neither the farm nor inside the tree", p.display()))?.to_path_buf()
        };
        if !rel.as_os_str().is_empty() {
            out.push(b'/');
            out.extend_from_slice(rel.as_os_str().as_encoded_bytes());
        }
    }
    Ok(OsString::from_vec(out))
}

/// Does this script set `LD_LIBRARY_PATH` itself? A line that is not a comment and holds an assignment (`LD_LIBRARY_PATH=...`, alone, after `export`, after
/// `env`, after `;` or `&&`) counts; a mention (`echo $LD_LIBRARY_PATH`), `unset`, a comment and another name that ends in it (`MY_LD_LIBRARY_PATH=`) do not.
pub fn sets_library_path(text: &str) -> bool {
    const VAR: &str = "LD_LIBRARY_PATH=";
    text.lines().any(|line| {
        let line = line.trim_start();
        if line.starts_with('#') {
            return false;
        }
        line.match_indices(VAR).any(|(i, _)| line[..i].chars().next_back().is_none_or(|c| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | '{' | '"' | '\'' | '`')))
    })
}

/// The scripts of a package that set `LD_LIBRARY_PATH` themselves (a converted AppImage often has one: its own launcher). Candidates are the executable files of
/// at most 64 KiB, not named like a library, that start with `#!`; `read` gives the bytes of one of them (the caller knows where they are: the chunks of a package
/// file, or the tree of an installed app) or `None` when it cannot. The paths come back in order.
pub fn scripts_setting_the_path(raw_index: &[u8], mut read: impl FnMut(&crate::Entry, &[crate::Chunk]) -> Option<Vec<u8>>) -> Result<Vec<String>> {
    const MAX: u64 = 64 << 10;
    let (chunks, entries) = crate::parse_index(raw_index)?;
    let mut out = Vec::new();
    for e in &entries {
        let crate::Kind::File { chunks: ids } = &e.kind else { continue };
        let name = e.path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let size: u64 = ids.iter().map(|&id| chunks.get(id as usize).map_or(u64::MAX, |c| c.raw as u64)).fold(0, u64::saturating_add);
        if e.mode & 0o111 == 0 || name.contains(".so") || !(4..=MAX).contains(&size) {
            continue;
        }
        let Some(bytes) = read(e, &chunks) else { continue };
        if bytes.starts_with(b"#!") && std::str::from_utf8(&bytes).is_ok_and(sets_library_path) {
            out.push(e.path.display().to_string());
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-libpath-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// The smallest ELF64 that this module calls a program: a header and one program header of type PT_INTERP. Built here so that
    /// the test does not depend on the system (a musl test binary has no interpreter) and the file is the same everywhere.
    fn fake_program() -> Vec<u8> {
        let mut b = vec![0u8; 64 + 56];
        b[0..4].copy_from_slice(b"\x7fELF");
        b[4] = 2; // 64-bit
        b[5] = 1; // little-endian
        b[6] = 1;
        b[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
        b[18..20].copy_from_slice(&62u16.to_le_bytes()); // x86-64
        b[0x20..0x28].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        b[0x34..0x36].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
        b[0x36..0x38].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        b[0x38..0x3a].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        b[64..68].copy_from_slice(&PT_INTERP.to_le_bytes());
        b
    }

    /// The program above, as an executable file.
    fn write_program(path: &Path) {
        fs::write(path, fake_program()).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn the_name_is_fixed_by_the_id() {
        assert_eq!(link_name("org.example.app"), link_name("org.example.app"));
        assert_ne!(link_name("org.example.app"), link_name("org.example.other"));
        let n = link_name("org.example.app");
        assert!(n.starts_with(LINK_PREFIX) && n.len() == LINK_PREFIX.len() + 8, "{n}");
        assert!(n[LINK_PREFIX.len()..].bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn the_target_goes_up_to_the_root() {
        assert_eq!(link_target(Path::new("")), ".");
        assert_eq!(link_target(Path::new("bin")), "..");
        assert_eq!(link_target(Path::new("usr/libexec/x")), "../../..");
        assert_eq!(farm_target("ab"), "../../farms/ab");
    }

    #[test]
    fn a_program_is_an_elf_with_an_interpreter_that_is_not_a_library() {
        let d = temp("is-program");
        let exe = d.join("tool");
        write_program(&exe);
        assert!(is_program(&exe));
        // The same bytes with a library name, without the execute bit, a script, and data are not programs.
        write_program(&d.join("libx.so.1"));
        assert!(!is_program(&d.join("libx.so.1")));
        let plain = d.join("plain");
        write_program(&plain);
        fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_program(&plain));
        fs::write(d.join("script"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(d.join("script"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!is_program(&d.join("script")));
        // A truncated ELF header is not a program and does not panic.
        let bytes = fake_program();
        for cut in [4usize, 20, 63, 64, 100] {
            let f = d.join(format!("cut{cut}"));
            fs::write(&f, &bytes[..cut]).unwrap();
            fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(!is_program(&f), "cut at {cut}");
        }
        assert!(!is_program(&d.join("missing")));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn links_are_required_in_every_folder_with_a_program_and_in_the_root() {
        let d = temp("required");
        fs::create_dir_all(d.join("bin")).unwrap();
        fs::create_dir_all(d.join("libexec/sub")).unwrap();
        fs::create_dir_all(d.join("lib")).unwrap();
        write_program(&d.join("bin/app"));
        write_program(&d.join("libexec/sub/helper"));
        write_program(&d.join("lib/libx.so.1"));
        let dirs = program_dirs(&d).unwrap();
        assert_eq!(dirs.iter().map(|p| p.to_str().unwrap()).collect::<Vec<_>>(), ["bin", "libexec/sub"]);
        let n = link_name("org.example.app");
        let req = required_links("org.example.app", &dirs);
        let shown: Vec<(String, String)> = req.iter().map(|(p, t)| (p.to_str().unwrap().to_string(), t.clone())).collect();
        assert_eq!(
            shown,
            [(n.clone(), ".".to_string()), (format!("bin/{n}"), "..".to_string()), (format!("libexec/sub/{n}"), "../..".to_string())]
        );
        // No program: nothing is required, and the runtime keeps the absolute path for such a package.
        assert!(required_links("org.example.app", &BTreeSet::new()).is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn make_links_makes_them_once_and_refuses_something_else_in_their_place() {
        let d = temp("make");
        fs::create_dir_all(d.join("bin")).unwrap();
        write_program(&d.join("bin/app"));
        let made = std::cell::Cell::new(0);
        let mut create = |p: &Path, t: &str| -> Result<()> {
            std::os::unix::fs::symlink(t, d.join(p))?;
            made.set(made.get() + 1);
            Ok(())
        };
        assert_eq!(make_links(&d, "org.example.app", &mut create).unwrap(), 2);
        assert_eq!(make_links(&d, "org.example.app", &mut create).unwrap(), 0, "a second run changes nothing");
        assert_eq!(made.get(), 2);
        let n = link_name("org.example.app");
        assert_eq!(fs::read_link(d.join("bin").join(&n)).unwrap().to_str(), Some(".."));
        assert_eq!(fs::read_link(d.join(&n)).unwrap().to_str(), Some("."));
        // A file in the place of a link, and a link with another target, are refused.
        fs::remove_file(d.join("bin").join(&n)).unwrap();
        fs::write(d.join("bin").join(&n), "x").unwrap();
        let err = make_links(&d, "org.example.app", &mut create).unwrap_err();
        assert_eq!(crate::code_of(&err, ErrorCode::E0000), ErrorCode::E0009, "{err:#}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_links_names_what_is_absent_or_wrong() {
        let dirs: BTreeSet<PathBuf> = [PathBuf::from("bin")].into();
        let n = link_name("org.example.app");
        let have = |p: &Path| -> Option<String> {
            if p == Path::new(&n) {
                Some(".".to_string())
            } else if p == Path::new("bin").join(&n) {
                Some("../x".to_string())
            } else {
                None
            }
        };
        let miss = missing_links("org.example.app", &dirs, &have);
        assert_eq!(miss.len(), 1);
        assert_eq!(miss[0].0, Path::new("bin").join(&n));
        let text = format!("{}", missing_error(&miss));
        assert!(text.contains("ln -s .. bin/.panpak-lib-"), "{text}");
        assert_eq!(crate::code_of(&missing_error(&miss), ErrorCode::E0000), ErrorCode::E0009);
    }

    #[test]
    fn the_value_is_relative_to_the_program_and_keeps_the_order() {
        let tree = Path::new("/home/u/.local/share/panpak/trees/abc");
        let farm = Path::new("/home/u/.local/share/panpak/farms/abc");
        let ld = vec![farm.to_path_buf(), tree.join("lib"), tree.join("lib/gdk-pixbuf/loaders"), tree.to_path_buf()];
        let v = env_value("org.example.app", tree, farm, &ld).unwrap();
        let n = link_name("org.example.app");
        assert_eq!(
            v.to_str().unwrap(),
            format!("/$ORIGIN/{n}/.farm:/$ORIGIN/{n}/lib:/$ORIGIN/{n}/lib/gdk-pixbuf/loaders:/$ORIGIN/{n}")
        );
        // Nothing absolute of the store is left in the value, so a program of the host that inherits it looks in a folder
        // that does not exist.
        assert!(!v.to_str().unwrap().contains("/home/"));
        // A folder that is neither the farm nor in the tree is a bug of the caller: refused.
        assert!(env_value("org.example.app", tree, farm, &[PathBuf::from("/usr/lib")]).is_err());
    }

    #[test]
    fn a_script_that_sets_the_library_path_itself_is_told_from_one_that_only_mentions_it() {
        // The script of Shotcut 26.9.27 (usr/bin/shotcut).
        let shotcut = "#!/bin/sh\nCURRENT_DIR=$(readlink -f \"$0\")\nINSTALL_DIR=$(dirname \"$CURRENT_DIR\")\nexport LD_LIBRARY_PATH=\"$INSTALL_DIR/lib\":$LD_LIBRARY_PATH\nexport MLT_REPOSITORY=\"$INSTALL_DIR/lib/mlt-7\"\n";
        assert!(sets_library_path(shotcut));
        for yes in ["LD_LIBRARY_PATH=/opt/x prog\n", "  export LD_LIBRARY_PATH=$HERE/lib\n", "exec env LD_LIBRARY_PATH=/x/lib app\n", "a; LD_LIBRARY_PATH=/y app\n"] {
            assert!(sets_library_path(yes), "{yes:?}");
        }
        for no in [
            "# export LD_LIBRARY_PATH=${HERE}/usr/lib\n",                // the AppRun of FreeCAD: commented out
            "echo $LD_LIBRARY_PATH\n",
            "unset LD_LIBRARY_PATH\n",
            "MY_LD_LIBRARY_PATH=/x\n",
            "export LD_LIBRARY_PATH_PRIVATE=/x\n",
            "",
        ] {
            assert!(!sets_library_path(no), "{no:?}");
        }
    }
}
