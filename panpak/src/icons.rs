// SPDX-License-Identifier: MIT OR Apache-2.0
//! The icon files of an installed app: `~/.local/share/icons/hicolor/<S>x<S>/apps/panpak.<id>.png`.
//! Rules: docs/DESKTOP-INTEGRATION.md, section 1. Every file passes `panpak_icon::check` before it is written.

use anyhow::{Context, Result};
use panpak_core::{bail_code, ErrorCode};
use std::{
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

pub fn path_of(app_id: &str, side: u32) -> Result<PathBuf> {
    Ok(hicolor()?.join(format!("{side}x{side}")).join("apps").join(file_name(app_id)))
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

/// A destination that is a link or not a regular file is not replaced.
fn check_destination(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => bail_code!(ErrorCode::E2001, "refusing to replace icon {}: it is a symbolic link", path.display()),
        Ok(m) if !m.file_type().is_file() => bail_code!(ErrorCode::E2001, "refusing to replace icon {}: not a regular file", path.display()),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("inspect {}", path.display())),
    }
}

/// Check the destinations before anything is written.
pub fn check_destinations(app_id: &str, icons: &[Checked]) -> Result<()> {
    for i in icons {
        check_destination(&path_of(app_id, i.side)?)?;
    }
    Ok(())
}

/// Write the icons (each in one rename) and remove the sizes of the app that are not among them.
/// Returns the paths written.
pub fn install(app_id: &str, icons: &[Checked]) -> Result<Vec<PathBuf>> {
    check_destinations(app_id, icons)?;
    let mut written = Vec::new();
    for i in icons {
        let path = path_of(app_id, i.side)?;
        fs::create_dir_all(path.parent().unwrap())?;
        panpak_core::desktop::write_file_lazy(&path, &i.data, 0o644).with_context(|| format!("write icon {}", path.display()))?;
        written.push(path);
    }
    let keep: Vec<u32> = icons.iter().map(|i| i.side).collect();
    remove(app_id, &keep)?;
    Ok(written)
}

/// Delete the icon files of the app, except the sizes in `keep`. Only a regular file of the exact name is
/// deleted, found by folder descriptors that do not follow links. Returns `(path, bytes)` of what went.
pub fn remove(app_id: &str, keep: &[u32]) -> Result<Vec<(PathBuf, u64)>> {
    walk(app_id, keep, true)
}

/// The icon files the app has now, with their sizes in bytes.
pub fn present(app_id: &str) -> Vec<(PathBuf, u64)> {
    walk(app_id, &[], false).unwrap_or_default()
}

fn walk(app_id: &str, keep: &[u32], delete: bool) -> Result<Vec<(PathBuf, u64)>> {
    use panpak_core::safefs::{Dir, Kind};
    let root = hicolor()?;
    let Ok(dir) = Dir::open_path(&root) else { return Ok(Vec::new()) };
    let name = file_name(app_id);
    let mut gone = Vec::new();
    for size in dir.list().unwrap_or_default() {
        let Some(s) = size.to_str() else { continue };
        let Some((w, h)) = s.split_once('x') else { continue };
        let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) else { continue };
        if w != h || keep.contains(&w) {
            continue;
        }
        let Ok(Some(sizedir)) = dir.open_dir_opt(&size) else { continue };
        let Ok(Some(apps)) = sizedir.open_dir_opt(OsStr::new("apps")) else { continue };
        if let Ok(Some(st)) = apps.lstat(OsStr::new(&name)) {
            if st.kind == Kind::File {
                if delete {
                    apps.unlink(OsStr::new(&name)).with_context(|| format!("remove {}", root.join(s).join("apps").join(&name).display()))?;
                }
                gone.push((root.join(s).join("apps").join(&name), st.size));
            }
        }
    }
    Ok(gone)
}
