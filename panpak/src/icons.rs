// SPDX-License-Identifier: MIT OR Apache-2.0
//! The icon of an installed app is a file of its tree, reached by the stable link `<store>/apps/<id>/icon.png` that the `Icon=` line of the entry names (`panpak_core::desktop::point_icon`);
//! nothing is copied. Every icon passes `panpak_icon::check` when a version is installed or updated. Earlier versions of panpak copied the icons to
//! `~/.local/share/icons/hicolor/<S>x<S>/apps/panpak.<id>.png`: this module finds those copies and takes away the ones that panpak made.
//! Rules: docs/DESKTOP-INTEGRATION.md, section 1.

use anyhow::{Context, Result};
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs::{self, File},
    path::{Path, PathBuf},
};

/// A checked icon: the side of its image and the file as it is in the package.
pub struct Checked {
    pub side: u32,
    pub data: Vec<u8>,
}

pub fn hicolor() -> Result<PathBuf> {
    Ok(dirs::data_dir().context("no data dir")?.join("icons").join("hicolor"))
}

fn file_name(app_id: &str) -> String {
    format!("{}.png", panpak_core::desktop::icon_name(app_id))
}

/// Read and check the icons a manifest lists, from a verified package. A bad icon is an error: the package is refused.
pub fn read_from_package(f: &mut File, raw_index: &[u8], man: &panpak_core::Manifest) -> Result<Vec<Checked>> {
    let mut out: Vec<Checked> = Vec::new();
    for path in man.desktop.as_ref().and_then(|d| d.icons.as_deref()).unwrap_or_default() {
        let data = panpak_core::read_package_file(f, raw_index, path, panpak_icon::MAX_FILE).with_context(|| format!("icon {path}"))?;
        let side = panpak_icon::check(&data).map_err(|why| anyhow::anyhow!("icon {path}: {why}"))?.side;
        // Two files of one size: the first one stays.
        if !out.iter().any(|c| c.side == side) {
            out.push(Checked { side, data });
        }
    }
    Ok(out)
}

/// The same, from the tree of an installed version (an update has no package file: the icons are files of the tree it just made).
pub fn read_from_tree(tree: &Path, man: &panpak_core::Manifest) -> Result<Vec<Checked>> {
    use std::io::Read;
    let mut out: Vec<Checked> = Vec::new();
    for path in man.desktop.as_ref().and_then(|d| d.icons.as_deref()).unwrap_or_default() {
        let file = tree.join(panpak_core::safe_path(path)?);
        let meta = fs::symlink_metadata(&file).with_context(|| format!("icon {path}"))?;
        anyhow::ensure!(meta.file_type().is_file() && meta.len() <= panpak_icon::MAX_FILE as u64, "icon {path}: not a regular file of the size the format allows");
        let mut data = Vec::new();
        File::open(&file).with_context(|| format!("icon {path}"))?.take(panpak_icon::MAX_FILE as u64 + 1).read_to_end(&mut data)?;
        let side = panpak_icon::check(&data).map_err(|why| anyhow::anyhow!("icon {path}: {why}"))?.side;
        if !out.iter().any(|c| c.side == side) {
            out.push(Checked { side, data });
        }
    }
    Ok(out)
}

/// The BLAKE3 hashes of the icon files of every version of the app that the store still names (`current`, `previous`, `pending`): what an earlier
/// panpak could have copied to `hicolor`. A copy is ours only when its bytes are one of these (the name `panpak.<id>.png` alone is not enough: a file
/// that someone changed or put there stays).
fn known_hashes(store: &Path, app_id: &str) -> HashSet<[u8; 32]> {
    let appdir = store.join("apps").join(app_id);
    let mut out = HashSet::new();
    for link in ["current", "previous", "pending"] {
        let Ok(rec_path) = panpak_core::durable::resolve_link(&appdir.join(link)) else { continue };
        let Ok(rec) = panpak_core::record::open_verified(&rec_path) else { continue };
        let tree = store.join("trees").join(panpak_core::hex(&rec.footer.root));
        for c in read_from_tree(&tree, &rec.manifest).unwrap_or_default() {
            out.insert(*blake3::hash(&c.data).as_bytes());
        }
    }
    out
}

/// The copies of the icons of the app that an earlier panpak made in the user's `hicolor`: `(path, bytes, ours)`, where `ours` says that the file is a
/// regular file whose bytes are an icon of a version in `known`. Found by folder descriptors that do not follow links.
fn legacy_copies(app_id: &str, known: &HashSet<[u8; 32]>, delete: bool) -> Result<Vec<(PathBuf, u64, bool)>> {
    legacy_copies_in(&hicolor()?, app_id, known, delete)
}

fn legacy_copies_in(root: &Path, app_id: &str, known: &HashSet<[u8; 32]>, delete: bool) -> Result<Vec<(PathBuf, u64, bool)>> {
    use panpak_core::safefs::{Dir, Kind};
    use std::io::Read;
    let Ok(dir) = Dir::open_path(root) else { return Ok(Vec::new()) };
    let name = file_name(app_id);
    let mut found = Vec::new();
    for size in dir.list().unwrap_or_default() {
        let Some(s) = size.to_str() else { continue };
        let Some((w, h)) = s.split_once('x') else { continue };
        if !matches!((w.parse::<u32>(), h.parse::<u32>()), (Ok(w), Ok(h)) if w == h) {
            continue;
        }
        let Ok(Some(sizedir)) = dir.open_dir_opt(&size) else { continue };
        let Ok(Some(apps)) = sizedir.open_dir_opt(OsStr::new("apps")) else { continue };
        let Ok(Some(st)) = apps.lstat(OsStr::new(&name)) else { continue };
        if st.kind != Kind::File {
            continue;
        }
        let path = root.join(s).join("apps").join(&name);
        let mut data = Vec::new();
        let ours = st.size <= panpak_icon::MAX_FILE as u64
            && apps.open_file(OsStr::new(&name)).and_then(|f| f.take(panpak_icon::MAX_FILE as u64 + 1).read_to_end(&mut data)).is_ok()
            && known.contains(blake3::hash(&data).as_bytes());
        if ours && delete {
            apps.unlink(OsStr::new(&name)).with_context(|| format!("remove {}", path.display()))?;
        }
        found.push((path, st.size, ours));
    }
    Ok(found)
}

/// The copies of icons that an earlier panpak put in the user's `hicolor` and that are still there, with their sizes in bytes.
pub fn present(app_id: &str) -> Vec<(PathBuf, u64)> {
    legacy_copies(app_id, &HashSet::new(), false).unwrap_or_default().into_iter().map(|(p, n, _)| (p, n)).collect()
}

/// Take away from the user's `hicolor` the icons that an earlier panpak copied there for this app: only a regular file named `panpak.<id>.png` in a
/// `<S>x<S>/apps` folder whose bytes are an icon of a version the store still names; nothing else in that folder is touched, and no folder is removed.
/// Returns the paths taken away and the paths left because the file is not one that panpak made.
pub fn forget_legacy(store: &Path, app_id: &str) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let known = known_hashes(store, app_id);
    let (mut gone, mut left) = (Vec::new(), Vec::new());
    for (p, _, ours) in legacy_copies(app_id, &known, true)? {
        if ours {
            gone.push(p);
        } else {
            left.push(p);
        }
    }
    Ok((gone, left))
}

/// `forget_legacy` for `install` (before and after the switch to the new version), `update`, `repair` and `remove`: told, and never an error that stops the command.
pub fn forget_legacy_told(store: &Path, app_id: &str) {
    match forget_legacy(store, app_id) {
        Ok((gone, left)) => {
            if !gone.is_empty() {
                println!("icons: took {} icon file(s) that an earlier panpak copied to the user's hicolor folder (the menu entry now names the icon in the app's tree)", gone.len());
            }
            for p in left {
                eprintln!("panpak: {} is not one of the icons that panpak made for {app_id}: left as it is", p.display());
            }
        }
        Err(e) => eprintln!("panpak: the old icon copies of {app_id} were not taken away: {}", panpak_core::err_text(&e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("target").join(format!("icons-test-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn put(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    #[test]
    fn only_the_copies_that_panpak_made_go_and_nothing_else_in_the_folder() {
        let root = temp("legacy").join("hicolor");
        let ours = b"the icon that an earlier panpak copied".to_vec();
        let known: HashSet<[u8; 32]> = HashSet::from([*blake3::hash(&ours).as_bytes()]);
        let name = file_name("org.x");
        put(&root, &format!("48x48/apps/{name}"), &ours);
        put(&root, &format!("256x256/apps/{name}"), b"same name, bytes changed by someone");
        put(&root, &format!("64x64/apps/{}", file_name("org.other")), &ours);
        put(&root, "48x48/apps/another-program.png", &ours);
        put(&root, "index.theme", b"[Icon Theme]");
        std::os::unix::fs::symlink(root.join("48x48/apps/another-program.png"), root.join("32x32")).unwrap_or(());
        fs::create_dir_all(root.join("128x128/apps")).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", root.join(format!("128x128/apps/{name}"))).unwrap();
        // Looking changes nothing.
        let seen = legacy_copies_in(&root, "org.x", &known, false).unwrap();
        assert_eq!(seen.len(), 2, "the two regular files named for the app: {seen:?}");
        assert!(root.join(format!("48x48/apps/{name}")).exists());
        // Taking away: only the one whose bytes are an icon of a known version.
        let done = legacy_copies_in(&root, "org.x", &known, true).unwrap();
        assert_eq!(done.iter().filter(|(_, _, ours)| *ours).count(), 1);
        assert!(!root.join(format!("48x48/apps/{name}")).exists(), "the copy of panpak is gone");
        assert!(root.join(format!("256x256/apps/{name}")).exists(), "a file with the name but other bytes stays");
        assert!(fs::symlink_metadata(root.join(format!("128x128/apps/{name}"))).unwrap().file_type().is_symlink(), "a link is not touched");
        assert!(root.join(format!("64x64/apps/{}", file_name("org.other"))).exists(), "the icon of another app stays, whatever its bytes");
        assert!(root.join("48x48/apps/another-program.png").exists() && root.join("index.theme").exists(), "nothing else is touched");
        assert!(root.join("48x48/apps").is_dir(), "no folder is removed");
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }
}
