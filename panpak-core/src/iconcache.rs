// SPDX-License-Identifier: MIT OR Apache-2.0
//! `icon-theme.cache` for an icon theme folder, byte for byte as `gtk-update-icon-cache -f -t -i <folder>` (GTK 3) writes it.
//!
//! A port of `gtk/updateiconcache.c` (GTK 3.24) in index-only mode: no image is read or decoded, only names. The order of
//! the icons in a hash bucket comes from the iteration order of two GLib hash tables (the names of one folder, then all the
//! names), so this module keeps the same tables as GLib 2.x (`glib/ghash.c`): the same hash, sizes, probing and resizes.
//! A `.icon` file next to an image is the one file whose content goes into the cache (embedded text rectangle, attach
//! points, display names); it is read as GLib's `GKeyFile` reads it. A `.icon` file that GTK reads with undefined behavior
//! (no `[Icon Data]` group, an attach point without a comma, a display name that cannot be read) is refused with an error.
//! An empty result means no cache: the tool writes no file for a theme without icons. A result that GTK's own validator
//! refuses (for example a name with a space) is an error, as the tool writes no cache then.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::{BTreeMap, HashMap};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const HAS_SUFFIX_XPM: u16 = 1 << 0;
const HAS_SUFFIX_SVG: u16 = 1 << 1;
const HAS_SUFFIX_PNG: u16 = 1 << 2;
const HAS_ICON_FILE: u16 = 1 << 3;

const MAJOR_VERSION: u16 = 1;
const MINOR_VERSION: u16 = 0;
const HASH_OFFSET: i64 = 12;

/// Folders deeper than this are refused: a link to a parent folder would make the walk endless.
const MAX_DEPTH: usize = 64;
/// Links followed in a row for a `.icon` file (GTK follows them without a limit).
const MAX_LINKS: usize = 40;

/// The cache of the icon theme folder `theme_dir`. Folders and files are found as the tool finds them: links are followed,
/// only regular files count, files in the top folder are ignored.
pub fn build(theme_dir: &Path) -> Result<Vec<u8>> {
    std::fs::read_dir(theme_dir).with_context(|| format!("cannot read the icon theme folder {}", theme_dir.display()))?;
    let mut files = Vec::new();
    walk(theme_dir, &mut Vec::new(), 0, &mut files)?;
    let base = base_path(theme_dir);
    let mut loader = |rel: &[u8]| -> Result<IconLoad> {
        let mut plain = base.clone();
        plain.push(b'/');
        plain.extend_from_slice(rel);
        let identity = match follow_links(&plain)? {
            Some(mut target) => {
                canonicalize_filename(&mut target);
                target
            }
            None => plain.clone(),
        };
        let data = std::fs::read(std::ffi::OsStr::from_bytes(&plain)).ok();
        Ok(IconLoad { identity, data })
    };
    build_core(&files, &mut loader)
}

/// The cache of a theme given by the paths of its regular files, relative to the theme folder, with `/` between names
/// (for example `48x48/apps/foo.png`). Reads nothing. A `.icon` file next to an image needs its content: use
/// [`build_from_files_with`] for a theme that has one.
pub fn build_from_files(files: &[String]) -> Result<Vec<u8>> {
    if needs_icon_content(files) {
        bail!("a .icon file next to an image needs its content");
    }
    build_from_files_with(files, &mut |_| None)
}

/// As [`build_from_files`]; `read_icon_file` gives the content of a `.icon` file from its relative path, or `None` when it
/// cannot be read (the tool then keeps no data for it).
pub fn build_from_files_with(files: &[String], read_icon_file: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> Result<Vec<u8>> {
    let mut rels = Vec::with_capacity(files.len());
    for f in files {
        check_rel(f)?;
        rels.push(f.as_bytes().to_vec());
    }
    let mut loader = |rel: &[u8]| -> Result<IconLoad> {
        let s = std::str::from_utf8(rel).map_err(|_| anyhow!("a path is not UTF-8"))?;
        Ok(IconLoad { identity: rel.to_vec(), data: read_icon_file(s) })
    };
    build_core(&rels, &mut loader)
}

/// True when a `.icon` file shares its folder and base name with a `.png`, `.svg` or `.xpm` file.
fn needs_icon_content(files: &[String]) -> bool {
    let mut images = std::collections::HashSet::new();
    let mut icons = Vec::new();
    for f in files {
        let Some((dir, name)) = f.rsplit_once('/') else { continue };
        let Some((stem, _)) = name.rsplit_once('.') else { continue };
        if name.ends_with(".png") || name.ends_with(".svg") || name.ends_with(".xpm") {
            images.insert((dir, stem));
        } else if name.ends_with(".icon") {
            icons.push((dir, stem));
        }
    }
    icons.iter().any(|k| images.contains(k))
}

fn check_rel(f: &str) -> Result<()> {
    if f.is_empty() || f.split('/').any(|c| c.is_empty() || c == "." || c == "..") || f.as_bytes().contains(&0) {
        bail!("invalid path in an icon theme: {f:?}");
    }
    Ok(())
}

/// The theme folder as the tool builds paths from it (`g_build_path`): trailing separators dropped.
fn base_path(dir: &Path) -> Vec<u8> {
    let mut b = dir.as_os_str().as_bytes().to_vec();
    while b.len() > 1 && b.last() == Some(&b'/') {
        b.pop();
    }
    b
}

fn walk(base: &Path, rel: &mut Vec<u8>, depth: usize, out: &mut Vec<Vec<u8>>) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("icon theme folders nested deeper than {MAX_DEPTH} (a link loop?) at {}", String::from_utf8_lossy(rel));
    }
    let dir = if rel.is_empty() { base.to_path_buf() } else { base.join(std::ffi::OsStr::from_bytes(rel)) };
    let Ok(rd) = std::fs::read_dir(&dir) else { return Ok(()) };
    let mut names: Vec<Vec<u8>> = rd.filter_map(|e| e.ok()).map(|e| e.file_name().as_bytes().to_vec()).collect();
    names.sort();
    for name in names {
        let path = dir.join(std::ffi::OsStr::from_bytes(&name));
        let meta = std::fs::metadata(&path).ok();
        let old = rel.len();
        if !rel.is_empty() {
            rel.push(b'/');
        }
        rel.extend_from_slice(&name);
        if meta.as_ref().is_some_and(|m| m.is_dir()) {
            walk(base, rel, depth + 1, out)?;
        } else if old != 0 && meta.as_ref().is_some_and(|m| m.is_file()) {
            out.push(rel.clone());
        }
        rel.truncate(old);
    }
    Ok(())
}

/// `follow_links` of the tool: the target of a chain of links on the last name, or `None` when the path is not a link.
fn follow_links(path: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut p = path.to_vec();
    let mut n = 0;
    loop {
        let os = std::ffi::OsStr::from_bytes(&p);
        let is_link = std::fs::symlink_metadata(os).map(|m| m.file_type().is_symlink()).unwrap_or(false);
        if !is_link {
            break;
        }
        let Ok(target) = std::fs::read_link(os) else { break };
        n += 1;
        if n > MAX_LINKS {
            bail!("too many links for {}", String::from_utf8_lossy(path));
        }
        let t = target.as_os_str().as_bytes();
        if t.first() == Some(&b'/') {
            p = t.to_vec();
        } else {
            let mut d = dirname(&p);
            if d.last() != Some(&b'/') {
                d.push(b'/');
            }
            let t = trim_leading_slashes(t);
            d.extend_from_slice(t);
            p = d;
        }
    }
    Ok(if p == path { None } else { Some(p) })
}

fn trim_leading_slashes(t: &[u8]) -> &[u8] {
    let n = t.iter().take_while(|&&c| c == b'/').count();
    &t[n..]
}

/// `g_path_get_dirname`.
fn dirname(p: &[u8]) -> Vec<u8> {
    let Some(mut base) = p.iter().rposition(|&c| c == b'/') else { return b".".to_vec() };
    while base > 0 && p[base] == b'/' {
        base -= 1;
    }
    if base == 0 && p[0] == b'/' {
        return b"/".to_vec();
    }
    p[..=base].to_vec()
}

/// `canonicalize_filename` of the tool: `.` and `..` resolved and repeated separators collapsed, without the file system.
fn canonicalize_filename(f: &mut Vec<u8>) {
    let src = f.clone();
    let mut q: Vec<u8> = Vec::with_capacity(src.len());
    let mut last_was_slash = false;
    let mut p = 0;
    let at = |i: usize| src.get(i).copied().unwrap_or(0);
    while p < src.len() {
        if src[p] == b'/' {
            if !last_was_slash {
                q.push(b'/');
            }
            last_was_slash = true;
        } else if last_was_slash && src[p] == b'.' {
            if at(p + 1) == b'/' || at(p + 1) == 0 {
                if at(p + 1) == 0 {
                    break;
                }
                p += 1;
            } else if at(p + 1) == b'.' && (at(p + 2) == b'/' || at(p + 2) == 0) {
                if q.len() > 1 {
                    q.pop();
                    while q.len() > 1 && q[q.len() - 1] != b'/' {
                        q.pop();
                    }
                }
                if at(p + 2) == 0 {
                    break;
                }
                p += 2;
            } else {
                q.push(src[p]);
                last_was_slash = false;
            }
        } else {
            q.push(src[p]);
            last_was_slash = false;
        }
        p += 1;
    }
    if q.len() > 1 && q[q.len() - 1] == b'/' {
        q.pop();
    }
    *f = q;
}

struct IconLoad {
    /// Two `.icon` files with the same identity share one entry, as links to one file do in the tool.
    identity: Vec<u8>,
    data: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------------------------------------------------
// GLib's GHashTable, as far as the order of its entries goes (glib/ghash.c, 2.x since 2.40).

const HASH_TABLE_MIN_SHIFT: u32 = 3;
const PRIME_MOD: [u32; 32] = [
    1, 2, 3, 7, 13, 31, 61, 127, 251, 509, 1021, 2039, 4093, 8191, 16381, 32749, 65521, 131071, 262139, 524287, 1048573, 2097143, 4194301, 8388593,
    16777213, 33554393, 67108859, 134217689, 268435399, 536870909, 1073741789, 2147483647,
];

/// `g_str_hash`: djb2 over signed chars.
fn g_str_hash(s: &[u8]) -> u32 {
    s.iter().fold(5381u32, |h, &c| h.wrapping_mul(33).wrapping_add(c as i8 as i32 as u32))
}

/// A table with string keys that are only added, never removed (no tombstones). Each slot holds an index into the
/// caller's list of keys.
struct GHash {
    size: u32,
    modulo: u32,
    mask: u32,
    nnodes: u32,
    noccupied: u32,
    hashes: Vec<u32>,
    slots: Vec<usize>,
}

impl GHash {
    fn new() -> GHash {
        let mut t = GHash { size: 0, modulo: 0, mask: 0, nnodes: 0, noccupied: 0, hashes: Vec::new(), slots: Vec::new() };
        t.set_shift(HASH_TABLE_MIN_SHIFT);
        t.hashes = vec![0; t.size as usize];
        t.slots = vec![0; t.size as usize];
        t
    }

    fn set_shift(&mut self, shift: u32) {
        self.size = 1 << shift;
        self.modulo = PRIME_MOD[shift as usize];
        self.mask = self.size - 1;
    }

    fn hash_to_index(&self, hash: u32) -> u32 {
        hash.wrapping_mul(11) % self.modulo
    }

    /// Index of the entry for `key`, or of the free slot where it goes; the hash; whether it was found.
    fn lookup_node(&self, key: &[u8], keys: &[Vec<u8>]) -> (u32, u32, bool) {
        let mut hash = g_str_hash(key);
        if hash < 2 {
            hash = 2;
        }
        let mut index = self.hash_to_index(hash);
        let mut step = 0;
        loop {
            let h = self.hashes[index as usize];
            if h == 0 {
                return (index, hash, false);
            }
            if h == hash && keys[self.slots[index as usize]] == key {
                return (index, hash, true);
            }
            step += 1;
            index = (index + step) & self.mask;
        }
    }

    fn lookup(&self, key: &[u8], keys: &[Vec<u8>]) -> Option<usize> {
        let (i, _, found) = self.lookup_node(key, keys);
        found.then(|| self.slots[i as usize])
    }

    /// Adds `id` (whose key is `keys[id]`) when its key is not in the table yet.
    fn insert_new(&mut self, id: usize, keys: &[Vec<u8>]) {
        let (index, hash, found) = self.lookup_node(&keys[id], keys);
        if found {
            return;
        }
        self.hashes[index as usize] = hash;
        self.slots[index as usize] = id;
        self.nnodes += 1;
        self.noccupied += 1;
        self.maybe_resize();
    }

    fn maybe_resize(&mut self) {
        let size = self.size;
        let noccupied = self.noccupied;
        if (size > 1 << HASH_TABLE_MIN_SHIFT && (size - 1) / 4 >= self.nnodes) || size <= noccupied + noccupied / 16 {
            self.resize();
        }
    }

    fn resize(&mut self) {
        let old_size = self.size;
        let n = self.nnodes + self.nnodes / 3;
        let shift = (32 - n.leading_zeros()).max(HASH_TABLE_MIN_SHIFT);
        self.set_shift(shift);
        let bitmap_len = if self.size > old_size {
            self.hashes.resize(self.size as usize, 0);
            self.slots.resize(self.size as usize, 0);
            self.size
        } else {
            old_size
        };
        let mut placed = vec![false; bitmap_len as usize];
        for i in 0..old_size as usize {
            let mut node_hash = self.hashes[i];
            if node_hash < 2 {
                self.hashes[i] = 0;
                continue;
            }
            if placed[i] {
                continue;
            }
            self.hashes[i] = 0;
            let mut key = self.slots[i];
            loop {
                let mut step = 0;
                let mut hv = self.hash_to_index(node_hash);
                while placed[hv as usize] {
                    step += 1;
                    hv = (hv + step) & self.mask;
                }
                placed[hv as usize] = true;
                let replaced = self.hashes[hv as usize];
                self.hashes[hv as usize] = node_hash;
                if replaced < 2 {
                    self.slots[hv as usize] = key;
                    break;
                }
                node_hash = replaced;
                key = std::mem::replace(&mut self.slots[hv as usize], key);
            }
        }
        if self.size < old_size {
            self.hashes.truncate(self.size as usize);
            self.slots.truncate(self.size as usize);
        }
        self.noccupied = self.nnodes;
    }

    /// Entries in the order `g_hash_table_foreach_remove` visits them.
    fn order(&self) -> Vec<usize> {
        (0..self.size as usize).filter(|&i| self.hashes[i] >= 2).map(|i| self.slots[i]).collect()
    }
}

/// `g_spaced_primes_closest`.
fn spaced_primes_closest(n: u32) -> u32 {
    const PRIMES: [u32; 34] = [
        11, 19, 37, 73, 109, 163, 251, 367, 557, 823, 1237, 1861, 2777, 4177, 6247, 9371, 14057, 21089, 31627, 47431, 71143, 106721, 160073,
        240101, 360163, 540217, 810343, 1215497, 1823231, 2734867, 4102283, 6153409, 9230113, 13845163,
    ];
    PRIMES.iter().copied().find(|&p| p > n).unwrap_or(PRIMES[PRIMES.len() - 1])
}

/// `icon_name_hash` of the tool: `h * 31 + c` over signed chars, starting from the first char.
fn icon_name_hash(s: &[u8]) -> u32 {
    let Some((&first, rest)) = s.split_first() else { return 0 };
    let mut h = first as i8 as i32 as u32;
    if h != 0 {
        for &c in rest {
            h = (h << 5).wrapping_sub(h).wrapping_add(c as i8 as i32 as u32);
        }
    }
    h
}

// ---------------------------------------------------------------------------------------------------------------------
// The scan.

#[derive(Default)]
struct Tree {
    children: BTreeMap<Vec<u8>, Option<Tree>>,
}

struct Image {
    flags: u16,
    dir_index: u32,
    /// Relative path of the `.icon` file of this image, loaded when the cache is written.
    icon_path: Option<Vec<u8>>,
    icon: Option<usize>,
    icon_data_size: i64,
}

struct IconData {
    has_embedded_rect: bool,
    rect: [i32; 4],
    attach_points: Vec<i32>,
    /// Language and name, in pairs.
    display_names: Vec<Vec<u8>>,
    offset: i64,
    size: i64,
}

struct State {
    directories: Vec<Vec<u8>>,
    images: Vec<Image>,
    /// Keys of `files`, and the images of each (the last one added first).
    names: Vec<Vec<u8>>,
    lists: Vec<Vec<usize>>,
    files: GHash,
}

fn build_core(files: &[Vec<u8>], loader: &mut dyn FnMut(&[u8]) -> Result<IconLoad>) -> Result<Vec<u8>> {
    let mut root = Tree::default();
    for f in files {
        let mut node = &mut root;
        let parts: Vec<&[u8]> = f.split(|&c| c == b'/').collect();
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            let entry = node.children.entry(part.to_vec()).or_insert_with(|| if last { None } else { Some(Tree::default()) });
            match (entry, last) {
                (None, true) => break,
                (Some(t), false) => node = t,
                _ => bail!("{} is both a file and a folder in the icon theme", String::from_utf8_lossy(f)),
            }
        }
    }
    let mut st = State { directories: Vec::new(), images: Vec::new(), names: Vec::new(), lists: Vec::new(), files: GHash::new() };
    scan_directory(&root, None, &mut st);
    if st.files.nnodes == 0 {
        return Ok(Vec::new());
    }

    // Load the `.icon` files that are kept, once per identity.
    let mut icons: Vec<IconData> = Vec::new();
    let mut by_identity: HashMap<Vec<u8>, Option<usize>> = HashMap::new();
    for img in 0..st.images.len() {
        let Some(path) = st.images[img].icon_path.clone() else { continue };
        if st.images[img].flags == HAS_ICON_FILE {
            continue;
        }
        let load = loader(&path)?;
        let idx = match by_identity.get(&load.identity) {
            Some(i) => *i,
            None => {
                let parsed = match &load.data {
                    Some(d) => load_icon_data(d).with_context(|| format!("unsupported .icon file {}", String::from_utf8_lossy(&path)))?,
                    None => None,
                };
                let i = parsed.map(|d| {
                    icons.push(d);
                    icons.len() - 1
                });
                by_identity.insert(load.identity, i);
                i
            }
        };
        st.images[img].icon = idx;
    }

    let out = write_file(&mut st, &mut icons)?;
    validate(&out).map_err(|what| anyhow!("the icon cache would be invalid ({what}); gtk-update-icon-cache writes none"))?;
    Ok(out)
}

fn scan_directory(node: &Tree, subdir: Option<&[u8]>, st: &mut State) {
    let mut dir_keys: Vec<Vec<u8>> = Vec::new();
    let mut dir_images: Vec<usize> = Vec::new();
    let mut dir_hash = GHash::new();
    let mut dir_added = false;
    let mut dir_index = 0xffff;
    for (name, child) in &node.children {
        if let Some(sub) = child {
            let subsub = match subdir {
                Some(s) => [s, b"/", name].concat(),
                None => name.clone(),
            };
            scan_directory(sub, Some(&subsub), st);
            continue;
        }
        let Some(subdir) = subdir else { continue };
        let flags = if name.ends_with(b".png") {
            HAS_SUFFIX_PNG
        } else if name.ends_with(b".svg") {
            HAS_SUFFIX_SVG
        } else if name.ends_with(b".xpm") {
            HAS_SUFFIX_XPM
        } else if name.ends_with(b".icon") {
            HAS_ICON_FILE
        } else {
            continue;
        };
        let dot = name.iter().rposition(|&c| c == b'.').unwrap_or(name.len());
        let basename = &name[..dot];
        let image = match dir_hash.lookup(basename, &dir_keys) {
            Some(k) => dir_images[k],
            None => {
                if !dir_added {
                    dir_added = true;
                    dir_index = st.directories.len() as u32;
                    st.directories.push(subdir.to_vec());
                }
                st.images.push(Image { flags: 0, dir_index, icon_path: None, icon: None, icon_data_size: 0 });
                dir_keys.push(basename.to_vec());
                dir_images.push(st.images.len() - 1);
                dir_hash.insert_new(dir_keys.len() - 1, &dir_keys);
                st.images.len() - 1
            }
        };
        st.images[image].flags |= flags;
        if flags == HAS_ICON_FILE {
            st.images[image].icon_path = Some([subdir, b"/", name].concat());
        }
    }
    // Move the folder into the big table, in the order of its own table.
    for k in dir_hash.order() {
        let image = dir_images[k];
        if st.images[image].flags == HAS_ICON_FILE {
            continue;
        }
        let key = &dir_keys[k];
        match st.files.lookup(key, &st.names) {
            Some(id) => st.lists[id].insert(0, image),
            None => {
                st.names.push(key.clone());
                st.lists.push(vec![image]);
                st.files.insert_new(st.names.len() - 1, &st.names);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The `.icon` file, read as GKeyFile reads it (glib/gkeyfile.c).

struct KeyFile {
    groups: Vec<(Vec<u8>, Group)>,
}

#[derive(Default)]
struct Group {
    pairs: Vec<(Vec<u8>, Vec<u8>)>,
    map: HashMap<Vec<u8>, usize>,
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn is_continuation(c: u8) -> bool {
    c & 0xc0 == 0x80
}

/// `g_key_file_load_from_data`; `Ok(None)` for a file that GLib does not load.
fn parse_key_file(data: &[u8]) -> Result<Option<KeyFile>> {
    if data.contains(&0) {
        bail!("a NUL byte");
    }
    let mut kf = KeyFile { groups: Vec::new() };
    let mut current: Option<usize> = None;
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut rest = data;
    while let Some(nl) = rest.iter().position(|&c| c == b'\n') {
        let mut line = &rest[..nl];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        lines.push(line);
        rest = &rest[nl + 1..];
    }
    lines.push(rest);
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if !parse_line(&mut kf, &mut current, line)? {
            return Ok(None);
        }
    }
    Ok(Some(kf))
}

/// One line; false when GLib refuses the file for it.
fn parse_line(kf: &mut KeyFile, current: &mut Option<usize>, line: &[u8]) -> Result<bool> {
    let start = line.iter().take_while(|&&c| is_space(c)).count();
    let l = &line[start..];
    if l.is_empty() || l[0] == b'#' {
        return Ok(true);
    }
    if line_is_group(l) {
        let end = l.iter().rposition(|&c| c == b']').unwrap_or(0);
        let name = &l[1..end];
        if name.is_empty() || name.iter().any(|&c| c == b']' || c == b'[' || c < 32 || c == 127) {
            return Ok(false);
        }
        match kf.groups.iter().position(|(n, _)| n == name) {
            Some(i) => *current = Some(i),
            None => {
                kf.groups.push((name.to_vec(), Group::default()));
                *current = Some(kf.groups.len() - 1);
            }
        }
        return Ok(true);
    }
    let Some(eq) = l.iter().position(|&c| c == b'=') else { return Ok(false) };
    if eq == 0 {
        return Ok(false);
    }
    let Some(cur) = *current else { return Ok(false) };
    let mut key_end = eq;
    while key_end > 0 && is_space(l[key_end - 1]) {
        key_end -= 1;
    }
    let Some(valid) = is_key_name(l, key_end) else { bail!("a key with a locale outside ASCII") };
    if !valid {
        return Ok(false);
    }
    let key = l[..key_end].to_vec();
    let mut vs = eq + 1;
    while vs < l.len() && is_space(l[vs]) {
        vs += 1;
    }
    let value = l[vs..].to_vec();
    // The start group is the first one.
    if cur == 0 && key == b"Encoding" && !(value.len() == 5 && value.eq_ignore_ascii_case(b"UTF-8")) {
        return Ok(false);
    }
    let g = &mut kf.groups[cur].1;
    g.pairs.push((key.clone(), value));
    g.map.insert(key, g.pairs.len() - 1);
    Ok(true)
}

fn line_is_group(l: &[u8]) -> bool {
    if l.first() != Some(&b'[') {
        return false;
    }
    let Some(close) = l.iter().position(|&c| c == b']') else { return false };
    let next = |mut i: usize| {
        i += 1;
        while i < l.len() && is_continuation(l[i]) {
            i += 1;
        }
        i
    };
    let mut p = next(close);
    while p < l.len() && (l[p] == b' ' || l[p] == b'\t') {
        p = next(p);
    }
    p >= l.len()
}

/// `g_key_file_is_key_name` on `line[..len]` (the byte at `len` is a space or `=`). `None` for a locale with characters
/// outside ASCII, where GLib asks Unicode tables.
fn is_key_name(line: &[u8], len: usize) -> Option<bool> {
    let at = |i: usize| line.get(i).copied().unwrap_or(0);
    let next = |mut i: usize| {
        i += 1;
        while i < len && is_continuation(line[i]) {
            i += 1;
        }
        i.min(len)
    };
    let mut q = 0;
    while q < len && !matches!(line[q], 0 | b'=' | b'[' | b']') {
        q = next(q);
    }
    if q == 0 {
        return Some(false);
    }
    if line[0] == b' ' || line[q - 1] == b' ' {
        return Some(false);
    }
    if at(q) == b'[' {
        q += 1;
        while q < len && line[q] != 0 {
            let c = line[q];
            if !c.is_ascii() {
                return None;
            }
            if !(c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'@')) {
                break;
            }
            q = next(q);
        }
        if at(q) != b']' {
            return Some(false);
        }
        q += 1;
    }
    Some(q >= len)
}

impl KeyFile {
    fn group(&self, name: &[u8]) -> Option<&Group> {
        self.groups.iter().find(|(n, _)| n == name).map(|(_, g)| g)
    }
}

impl Group {
    fn value(&self, key: &[u8]) -> Option<&[u8]> {
        self.map.get(key).map(|&i| self.pairs[i].1.as_slice())
    }

    /// `g_key_file_get_string`.
    fn string(&self, key: &[u8]) -> Option<Vec<u8>> {
        let v = self.value(key)?;
        std::str::from_utf8(v).ok()?;
        parse_value_as_string(v, None).map(|(s, _)| s)
    }

    /// `g_key_file_get_integer_list` with `,` between values.
    fn integer_list(&self, key: &[u8]) -> Option<Vec<i32>> {
        let v = self.value(key)?;
        std::str::from_utf8(v).ok()?;
        let (_, pieces) = parse_value_as_string(v, Some(b','))?;
        pieces.iter().map(|p| parse_integer(p)).collect()
    }

    /// `g_key_file_get_locale_string`.
    fn locale_string(&self, key: &[u8], locale: &[u8]) -> Option<Vec<u8>> {
        for variant in locale_variants(locale) {
            if variant == b"C" {
                break;
            }
            let candidate = [key, b"[", &variant, b"]"].concat();
            if let Some(s) = self.string(&candidate) {
                return Some(s);
            }
        }
        self.string(key)
    }
}

/// `g_key_file_parse_value_as_string`; with a separator, also the pieces.
fn parse_value_as_string(value: &[u8], sep: Option<u8>) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
    let mut out = Vec::with_capacity(value.len());
    let mut pieces = Vec::new();
    let mut q0 = 0;
    let mut p = 0;
    while p < value.len() {
        let c = value[p];
        if c == b'\\' {
            p += 1;
            let e = *value.get(p)?;
            match e {
                b's' => out.push(b' '),
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'r' => out.push(b'\r'),
                b'\\' => out.push(b'\\'),
                _ if Some(e) == sep => out.push(e),
                _ => return None,
            }
        } else {
            out.push(c);
            if Some(c) == sep {
                pieces.push(out[q0..out.len() - 1].to_vec());
                q0 = out.len();
            }
        }
        p += 1;
    }
    if sep.is_some() && q0 < out.len() {
        pieces.push(out[q0..].to_vec());
    }
    Some((out, pieces))
}

/// `strtol (s, &end, 10)`: the value (saturated) and the index of its end (0 when there is no number).
fn strtol(s: &[u8]) -> (i64, usize) {
    let mut i = s.iter().take_while(|&&c| is_space(c)).count();
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let digits_start = i;
    let limit = i128::from(i64::MAX) + 1;
    let mut v: i128 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        v = (v * 10 + i128::from(s[i] - b'0')).min(limit);
        i += 1;
    }
    if i == digits_start {
        return (0, 0);
    }
    let v = if neg { -v } else { v };
    (v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64, i)
}

/// `g_key_file_parse_value_as_integer`.
fn parse_integer(s: &[u8]) -> Option<i32> {
    let (v, end) = strtol(s);
    if s.is_empty() || (end < s.len() && !is_space(s[end])) {
        return None;
    }
    i32::try_from(v).ok()
}

/// `atoi`.
fn atoi(s: &[u8]) -> i32 {
    strtol(s).0 as i32
}

/// `g_get_locale_variants`.
fn locale_variants(locale: &[u8]) -> Vec<Vec<u8>> {
    let find = |from: usize, c: u8| locale[from..].iter().position(|&x| x == c).map(|i| i + from);
    let uscore = find(0, b'_');
    let dot = find(uscore.unwrap_or(0), b'.');
    let at = find(dot.or(uscore).unwrap_or(0), b'@');
    let mut mask = 0u32;
    let (modifier, at_pos) = match at {
        Some(a) => {
            mask |= 4;
            (&locale[a..], a)
        }
        None => (&locale[0..0], locale.len()),
    };
    let (codeset, dot_pos) = match dot {
        Some(d) if d < at_pos => {
            mask |= 1;
            (&locale[d..at_pos], d)
        }
        _ => (&locale[0..0], at_pos),
    };
    let (territory, us_pos) = match uscore {
        Some(u) if u < dot_pos => {
            mask |= 2;
            (&locale[u..dot_pos], u)
        }
        _ => (&locale[0..0], dot_pos),
    };
    let language = &locale[..us_pos];
    let mut out = Vec::new();
    for j in 0..=mask {
        let i = mask - j;
        if i & !mask == 0 {
            let mut v = language.to_vec();
            if i & 2 != 0 {
                v.extend_from_slice(territory);
            }
            if i & 1 != 0 {
                v.extend_from_slice(codeset);
            }
            if i & 4 != 0 {
                v.extend_from_slice(modifier);
            }
            out.push(v);
        }
    }
    out
}

/// `load_icon_data` of the tool. `Ok(None)` when GLib does not load the file; an error where the tool has undefined
/// behavior.
fn load_icon_data(data: &[u8]) -> Result<Option<IconData>> {
    let Some(kf) = parse_key_file(data)? else { return Ok(None) };
    let Some(g) = kf.group(b"Icon Data") else { bail!("no [Icon Data] group") };
    let mut d = IconData { has_embedded_rect: false, rect: [0; 4], attach_points: Vec::new(), display_names: Vec::new(), offset: 0, size: -1 };
    if let Some(v) = g.integer_list(b"EmbeddedTextRectangle") {
        if v.len() == 4 {
            d.has_embedded_rect = true;
            d.rect = [v[0], v[1], v[2], v[3]];
        }
    }
    if let Some(s) = g.string(b"AttachPoints") {
        if !s.is_empty() {
            for piece in s.split(|&c| c == b'|') {
                let Some(comma) = piece.iter().position(|&c| c == b',') else { bail!("an attach point without a comma") };
                d.attach_points.push(atoi(&piece[..comma]));
                d.attach_points.push(atoi(&piece[comma + 1..]));
            }
        }
    }
    for (key, _) in &g.pairs {
        if !key.starts_with(b"DisplayName") {
            continue;
        }
        let open = key.iter().position(|&c| c == b'[');
        let close = open.and_then(|o| key[o..].iter().position(|&c| c == b']').map(|c| c + o));
        let (lang, name) = match (open, close) {
            (Some(o), Some(c)) => {
                let lang = key[o + 1..c].to_vec();
                let name = g.locale_string(b"DisplayName", &lang);
                (lang, name)
            }
            _ => (b"C".to_vec(), g.string(b"DisplayName")),
        };
        let Some(name) = name else { bail!("a display name that cannot be read") };
        if name.contains(&0) {
            bail!("a display name with a NUL character");
        }
        d.display_names.push(lang);
        d.display_names.push(name);
    }
    Ok(Some(d))
}

// ---------------------------------------------------------------------------------------------------------------------
// The writer.

fn align4(n: usize) -> i64 {
    ((n + 1 + 3) & !3) as i64
}

struct Writer {
    out: Vec<u8>,
    pool: HashMap<Vec<u8>, i64>,
}

impl Writer {
    fn card16(&mut self, n: u16) {
        self.out.extend_from_slice(&n.to_be_bytes());
    }
    fn card32(&mut self, n: u32) {
        self.out.extend_from_slice(&n.to_be_bytes());
    }
    fn offset32(&mut self, n: i64) -> Result<()> {
        let v = u32::try_from(n).map_err(|_| anyhow!("icon cache too large"))?;
        self.card32(v);
        Ok(())
    }
    fn string(&mut self, s: &[u8]) {
        let l = align4(s.len()) as usize;
        self.out.extend_from_slice(s);
        self.out.resize(self.out.len() + l - s.len(), 0);
    }
    fn find(&self, s: &[u8]) -> i64 {
        self.pool.get(s).copied().unwrap_or(0)
    }
    fn add(&mut self, s: &[u8], v: i64) {
        self.pool.insert(s.to_vec(), v);
    }
    fn pos(&self) -> i64 {
        self.out.len() as i64
    }
}

fn meta_data_size(w: &mut Writer, img: &mut Image, icons: &mut [IconData]) -> i64 {
    if img.icon_data_size == 0 {
        if let Some(i) = img.icon {
            let data = &mut icons[i];
            if data.size < 0 {
                let mut size = 0;
                if data.has_embedded_rect || !data.attach_points.is_empty() || !data.display_names.is_empty() {
                    size += 12;
                }
                if data.has_embedded_rect {
                    size += 8;
                }
                let n_attach = (data.attach_points.len() / 2) as i64;
                if n_attach > 0 {
                    size += 4 + n_attach * 4;
                }
                let n_names = (data.display_names.len() / 2) as i64;
                if n_names > 0 {
                    size += 4 + 8 * n_names;
                    for s in &data.display_names {
                        if w.find(s) == 0 {
                            size += align4(s.len());
                            w.add(s, -1);
                        }
                    }
                }
                img.icon_data_size = size;
                data.size = 0;
            }
        }
    }
    img.icon_data_size
}

fn image_data_size(w: &mut Writer, img: &mut Image, icons: &mut [IconData]) -> i64 {
    let len = meta_data_size(w, img, icons);
    if len > 0 || img.icon.is_some() {
        len + 8
    } else {
        len
    }
}

fn write_icon_data(w: &mut Writer, d: &IconData, offset: i64) -> Result<()> {
    let mut ofs = offset + 12;
    if d.has_embedded_rect {
        w.offset32(ofs)?;
        ofs += 8;
    } else {
        w.card32(0);
    }
    let n_attach = (d.attach_points.len() / 2) as i64;
    if n_attach > 0 {
        w.offset32(ofs)?;
        ofs += 4 + 4 * n_attach;
    } else {
        w.card32(0);
    }
    let n_names = (d.display_names.len() / 2) as i64;
    if n_names > 0 {
        w.offset32(ofs)?;
    } else {
        w.card32(0);
    }
    if d.has_embedded_rect {
        for v in d.rect {
            w.card16(v as u16);
        }
    }
    if n_attach > 0 {
        w.card32(n_attach as u32);
        for &v in &d.attach_points {
            w.card16(v as u16);
        }
    }
    if n_names > 0 {
        w.card32(n_names as u32);
        ofs += 4 + 8 * n_names;
        let mut tmp = ofs;
        for s in &d.display_names {
            let mut tmp2 = w.find(s);
            if tmp2 == 0 || tmp2 == -1 {
                tmp2 = tmp;
                tmp += align4(s.len());
                w.add(s, -tmp2);
            } else if tmp2 < 0 {
                tmp2 = -tmp2;
            }
            w.offset32(tmp2)?;
        }
        for s in &d.display_names {
            let tmp2 = w.find(s);
            if tmp2 < 0 {
                w.add(s, -tmp2);
                w.string(s);
            }
        }
    }
    Ok(())
}

fn write_file(st: &mut State, icons: &mut [IconData]) -> Result<Vec<u8>> {
    let size = spaced_primes_closest(st.files.nnodes / 3) as usize;
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); size];
    for id in st.files.order() {
        buckets[icon_name_hash(&st.names[id]) as usize % size].push(id);
    }
    for b in &mut buckets {
        b.reverse();
    }

    let mut w = Writer { out: Vec::new(), pool: HashMap::new() };
    w.card16(MAJOR_VERSION);
    w.card16(MINOR_VERSION);
    w.offset32(HASH_OFFSET)?;
    w.card32(0);

    // The hash table.
    w.card32(size as u32);
    let mut node_offset = HASH_OFFSET + 4 + size as i64 * 4;
    for _ in 0..size {
        w.card32(0);
    }
    let mut first_offsets = vec![None; size];
    for (b, chain) in buckets.iter().enumerate() {
        for (k, &id) in chain.iter().enumerate() {
            let offset = node_offset;
            debug_assert_eq!(offset, w.pos());
            if k == 0 {
                first_offsets[b] = Some(offset);
            }
            let name = st.names[id].clone();
            // get_single_node_size
            let mut node_size = 12;
            if w.find(&name) == 0 {
                node_size += align4(name.len());
                w.add(&name, -1);
            }
            let list = st.lists[id].clone();
            node_size += 4 + list.len() as i64 * 8;
            let mut image_data_size_total = 0;
            for &img in &list {
                image_data_size_total += image_data_size(&mut w, &mut st.images[img], icons);
            }
            let mut image_data_offset = offset + node_size;
            let next_offset = offset + node_size + image_data_size_total;
            if k + 1 < chain.len() {
                w.offset32(next_offset)?;
            } else {
                w.card32(0xffff_ffff);
            }
            let mut name_size = 0;
            let mut name_offset = w.find(&name);
            if name_offset <= 0 {
                name_offset = offset + 12;
                name_size = align4(name.len());
                w.add(&name, name_offset);
            }
            w.offset32(name_offset)?;
            w.offset32(offset + 12 + name_size)?;
            if name_size > 0 {
                w.string(&name);
            }
            w.card32(list.len() as u32);
            let mut data_offset = image_data_offset;
            for &img in &list {
                let image_size = image_data_size(&mut w, &mut st.images[img], icons);
                w.card16(st.images[img].dir_index as u16);
                w.card16(st.images[img].flags);
                if image_size > 0 {
                    w.offset32(data_offset)?;
                    data_offset += image_size;
                } else {
                    w.card32(0);
                }
            }
            for &img in &list {
                let meta = meta_data_size(&mut w, &mut st.images[img], icons);
                if image_data_size(&mut w, &mut st.images[img], icons) == 0 {
                    continue;
                }
                // No pixel data in index-only mode.
                w.card32(0);
                let icon = st.images[img].icon;
                if meta > 0 {
                    let i = icon.expect("meta data without icon data");
                    icons[i].offset = image_data_offset + 8;
                    w.offset32(icons[i].offset)?;
                } else {
                    let o = icon.map(|i| icons[i].offset).unwrap_or(0);
                    w.offset32(o)?;
                }
                if meta > 0 {
                    let i = icon.expect("meta data without icon data");
                    let o = icons[i].offset;
                    write_icon_data(&mut w, &icons[i], o)?;
                }
                image_data_offset += meta + 8;
            }
            node_offset = next_offset;
        }
    }
    let dir_list_offset = node_offset;
    debug_assert_eq!(dir_list_offset, w.pos());

    // The folder index.
    let n_dirs = st.directories.len() as i64;
    w.card32(n_dirs as u32);
    let mut tmp = dir_list_offset + 4 + n_dirs * 4;
    let dirs = std::mem::take(&mut st.directories);
    for d in &dirs {
        let mut tmp2 = w.find(d);
        if tmp2 == 0 || tmp2 == -1 {
            tmp2 = tmp;
            tmp += align4(d.len());
            w.add(d, -tmp2);
        } else if tmp2 < 0 {
            tmp2 = -tmp2;
        }
        w.offset32(tmp2)?;
    }
    for d in &dirs {
        let tmp2 = w.find(d);
        if tmp2 < 0 {
            w.add(d, -tmp2);
            w.string(d);
        }
    }

    // Bucket offsets and the header.
    for (b, o) in first_offsets.iter().enumerate() {
        let v = match o {
            Some(o) => u32::try_from(*o).map_err(|_| anyhow!("icon cache too large"))?,
            None => 0xffff_ffff,
        };
        let at = (HASH_OFFSET + 4) as usize + b * 4;
        w.out[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }
    let v = u32::try_from(dir_list_offset).map_err(|_| anyhow!("icon cache too large"))?;
    w.out[8..12].copy_from_slice(&v.to_be_bytes());
    Ok(w.out)
}

// ---------------------------------------------------------------------------------------------------------------------
// GTK's validator (gtk/gtkiconcachevalidator.c) with offsets and strings checked; the tool writes no cache it refuses.

struct Validator<'a> {
    cache: &'a [u8],
    n_directories: u32,
}

type Check = std::result::Result<(), &'static str>;

fn check(name: &'static str, ok: bool) -> Check {
    if ok {
        Ok(())
    } else {
        Err(name)
    }
}

impl Validator<'_> {
    fn u16(&self, off: u32) -> Option<u16> {
        let o = off as usize;
        (o < self.cache.len()).then(|| {
            let b = |i: usize| self.cache.get(o + i).copied().unwrap_or(0);
            u16::from_be_bytes([b(0), b(1)])
        })
    }
    fn u32(&self, off: u32) -> Option<u32> {
        let o = off as usize;
        (o < self.cache.len()).then(|| {
            let b = |i: usize| self.cache.get(o + i).copied().unwrap_or(0);
            u32::from_be_bytes([b(0), b(1), b(2), b(3)])
        })
    }
    fn string(&self, off: u32, utf8: bool) -> Check {
        check("string offset", (off as usize) < self.cache.len())?;
        let mut i = 0;
        while i < 1024 {
            let at = off as usize + i;
            check("string offset", at < self.cache.len())?;
            let c = self.cache[at];
            if c == 0 {
                break;
            }
            if !utf8 {
                check("string content", c.is_ascii_graphic())?;
            }
            i += 1;
        }
        check("string length", i < 1024)?;
        if utf8 {
            let s = &self.cache[off as usize..off as usize + i];
            check("string utf8 data", std::str::from_utf8(s).is_ok())?;
        }
        Ok(())
    }
    fn get32(&self, name: &'static str, off: u32) -> std::result::Result<u32, &'static str> {
        self.u32(off).ok_or(name)
    }
    fn meta(&self, off: u32) -> Check {
        let rect = self.get32("offset, embedded rect", off)?;
        let attach = self.get32("offset, attach point list", off.wrapping_add(4))?;
        let names = self.get32("offset, display name list", off.wrapping_add(8))?;
        let size = self.cache.len() as u64;
        if rect != 0 {
            check("embedded rect", u64::from(rect) + 4 < size)?;
        }
        if attach != 0 {
            let n = self.get32("offset, attach point list", attach)?;
            check("attach points", u64::from(attach.wrapping_add(4).wrapping_add(n.wrapping_mul(4))) < size)?;
        }
        if names != 0 {
            let n = self.get32("offset, display name list", names)?;
            for i in 0..n {
                let o = self.u32(names.wrapping_add(4).wrapping_add(8 * i)).unwrap_or(0);
                self.string(o, false)?;
                let o = self.u32(names.wrapping_add(4).wrapping_add(8 * i + 4)).unwrap_or(0);
                self.string(o, true)?;
            }
        }
        Ok(())
    }
    fn image_list(&self, off: u32) -> Check {
        let n = self.get32("offset, image list", off)?;
        for i in 0..n {
            let o = off.wrapping_add(4).wrapping_add(8 * i);
            let index = self.u16(o).ok_or("offset, image index")?;
            let flags = self.u16(o.wrapping_add(2)).ok_or("offset, image flags")?;
            let data = self.get32("offset, image data offset", o.wrapping_add(4))?;
            check("image index", u32::from(index) < self.n_directories)?;
            check("image flags", flags < 16)?;
            if data != 0 {
                let pixel = self.get32("offset, pixel data", data)?;
                let meta = self.get32("offset, meta data", data.wrapping_add(4))?;
                // No pixel data is written in index-only mode.
                check("pixel data", pixel == 0)?;
                if meta != 0 {
                    self.meta(meta)?;
                }
            }
        }
        Ok(())
    }
}

fn validate(cache: &[u8]) -> Check {
    let mut v = Validator { cache, n_directories: 0 };
    check("major version", v.u16(0) == Some(MAJOR_VERSION))?;
    check("minor version", v.u16(2) == Some(MINOR_VERSION))?;
    let hash_offset = v.get32("header, hash offset", 4)?;
    let dir_offset = v.get32("header, directory list offset", 8)?;
    v.n_directories = v.get32("offset, directory list", dir_offset)?;
    for i in 0..v.n_directories {
        let o = v.get32("offset, directory", dir_offset.wrapping_add(4).wrapping_add(4 * i))?;
        v.string(o, false)?;
    }
    let n_buckets = v.get32("offset, hash size", hash_offset)?;
    for i in 0..n_buckets {
        let mut icon = v.get32("offset, hash chain", hash_offset.wrapping_add(4).wrapping_add(4 * i))?;
        let mut seen = 0usize;
        while icon != 0xffff_ffff {
            seen += 1;
            check("icon chain", seen <= cache.len())?;
            let chain = v.get32("offset, icon chain", icon)?;
            let name = v.get32("offset, icon name", icon.wrapping_add(4))?;
            let list = v.get32("offset, icon image list", icon.wrapping_add(8))?;
            v.string(name, false)?;
            v.image_list(list)?;
            icon = chain;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    const TOOL: &str = "/usr/bin/gtk-update-icon-cache";

    struct Root(PathBuf);

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn root() -> Root {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("target").join(format!("iconcache-test-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Root(p)
    }

    fn put(theme: &Path, rel: &str, content: &[u8]) {
        let p = theme.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn link(theme: &Path, rel: &str, target: &str) {
        let p = theme.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, p).unwrap();
    }

    /// The version of GTK 3 that this port follows byte for byte.
    const REFERENCE: &str = "3.24.52";

    /// Is the tool of this machine of the version that the port follows? (The version of the package that has it; unknown counts as no.)
    fn reference_tool() -> bool {
        let q = |cmd: &str, args: &[&str]| Command::new(cmd).args(args).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let v = q("pkg-config", &["--modversion", "gtk+-3.0"]).or_else(|| q("dpkg-query", &["-W", "-f=${Version}", "libgtk-3-bin"])).unwrap_or_default();
        v.starts_with(REFERENCE)
    }

    fn has_icon_file(theme: &Path) -> bool {
        let mut rels = Vec::new();
        walk(theme, &mut Vec::new(), 0, &mut rels).is_ok() && rels.iter().any(|r| r.ends_with(b".icon"))
    }

    /// Whether the real tool succeeded, and the cache it wrote (`None` when it wrote none).
    fn run_tool(theme: &Path) -> (bool, Option<Vec<u8>>) {
        let out = Command::new(TOOL).args(["-f", "-t", "-i", "-q"]).arg(theme).output().unwrap();
        (out.status.success(), std::fs::read(theme.join("icon-theme.cache")).ok())
    }

    fn first_difference(a: &[u8], b: &[u8]) -> String {
        let i = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
        let hex = |s: &[u8]| s[i.min(s.len())..(i + 32).min(s.len())].iter().map(|c| format!("{c:02x}")).collect::<Vec<_>>().join(" ");
        format!("lengths {} and {}, first difference at 0x{i:x}\n  ours: {}\n  tool: {}", a.len(), b.len(), hex(a), hex(b))
    }

    /// Compares `build` with the tool on the theme folder; with `from_files`, also `build_from_files_with` and
    /// `build_from_files` with `build`. Returns the size of the cache.
    fn compare(theme: &Path, label: &str, from_files: bool) -> usize {
        let ours = build(theme);
        if from_files {
            let mut rels = Vec::new();
            walk(theme, &mut Vec::new(), 0, &mut rels).unwrap();
            let rels: Vec<String> = rels.into_iter().map(|r| String::from_utf8(r).unwrap()).collect();
            let mut read = |p: &str| std::fs::read(theme.join(p)).ok();
            let listed = build_from_files_with(&rels, &mut read);
            match (&ours, &listed) {
                (Ok(a), Ok(b)) => assert!(a == b, "{label}: build and build_from_files_with differ: {}", first_difference(a, b)),
                (Err(_), Err(_)) => {}
                _ => panic!("{label}: build {:?}, build_from_files_with {:?}", ours.as_ref().map(Vec::len), listed.as_ref().map(Vec::len)),
            }
            if needs_icon_content(&rels) {
                assert!(build_from_files(&rels).is_err(), "{label}: build_from_files must ask for the .icon content");
            } else {
                assert_eq!(build_from_files(&rels).ok(), listed.ok(), "{label}: build_from_files");
            }
        }
        // What the tool writes for a `.icon` file changed between versions of GTK 3 (the sizes of the strings differ in 3.24.41, which the CI has): those themes are compared only with the
        // version this port follows. The other themes are compared with any version.
        if has_icon_file(theme) && !reference_tool() {
            eprintln!("{label}: not compared with the tool: it is not GTK {REFERENCE} (`.icon` files are written differently by other versions)");
            return ours.map(|a| a.len()).unwrap_or(0);
        }
        let (ok, tool) = run_tool(theme);
        match (ours, ok, tool) {
            (Ok(a), true, Some(b)) => {
                assert!(a == b, "{label}: {}", first_difference(&a, &b));
                a.len()
            }
            (Ok(a), true, None) => {
                assert!(a.is_empty(), "{label}: the tool wrote no cache, ours has {} bytes", a.len());
                0
            }
            (Err(e), false, _) => {
                eprintln!("{label}: refused, as the tool refuses it: {e:#}");
                0
            }
            (ours, ok, tool) => panic!("{label}: ours {:?}, tool success {ok}, tool cache {:?}", ours.map(|v| v.len()), tool.map(|v| v.len())),
        }
    }

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
    }

    const ICON_FILES: &[&[u8]] = &[
        b"[Icon Data]\nDisplayName=Hello\nDisplayName[de]=Hallo\nEmbeddedTextRectangle=1,2,3,4\nAttachPoints=1,2|3,4\n",
        b"[Icon Data]\n",
        b"# comment\n\n[Icon Data]  \nDisplayName = apps\nDisplayName[fr]=16x16/apps\nDisplayName[it]=Hello\n",
        b"[Icon Data]\nDisplayName[de_DE.UTF-8@euro]=Euro\nDisplayName[sr@latin]=Latin\nDisplayName[C]=Cee\nDisplayName[]=Empty\nDisplayName=Base\n",
        b"[Icon Data]\nDisplayName=One\nDisplayName=Two\nDisplayNameX=Other\n",
        b"[Icon Data]\nDisplayName=a\\sb\\nc\\\\d\nDisplayName[fr]=bad\\q\nDisplayName[es]=caf\xc3\xa9\n",
        b"[Icon Data]\nEmbeddedTextRectangle=1,2,3\nAttachPoints=5,6\n",
        b"[Icon Data]\nEmbeddedTextRectangle= 1, 2,3,4 \nAttachPoints=-1,70000|  7x,8\n",
        b"[Icon Data]\nEmbeddedTextRectangle=-5,70000,3,4,\n",
        b"[Icon Data]\nEmbeddedTextRectangle=10,x,3,4\nAttachPoints=1,2|3,4|5,6\n",
        b"[Icon Data]\nEmbeddedTextRectangle=\nDisplayName=Hallo\n",
        b"[Icon Data]\r\nDisplayName=Crlf\r\nEmbeddedTextRectangle=1,1,2,2\r\n",
        b"[Other]\nDisplayName=Elsewhere\n[Icon Data]\nDisplayName=Second\n[Other]\nX=1\n[Icon Data]\nDisplayName[nl]=Again\n",
        b"[Icon Data]\nEncoding=UTF-8\nDisplayName=Enc\n",
        b"[Icon Data]\nEncoding=latin1\nDisplayName=Refused\n",
        b"garbage line\n[Icon Data]\nDisplayName=Refused\n",
        b"DisplayName=before a group\n[Icon Data]\n",
        b"[Icon Data]\nDisplay Name[de=broken key\n",
        b"[Icon Data]\nDisplayName=Hello\nDisplayName[de]=Hallo",
        b"[Icon Data]\nEmbeddedTextRectangle=1\\,2,3,4,5\nDisplayName=Hello\n",
    ];

    #[test]
    fn iconcache_matches_gtk_update_icon_cache() {
        if !Path::new(TOOL).exists() {
            eprintln!("skipped: {TOOL} is not installed");
            return;
        }
        let r = root();
        let mut n = 0;
        let mut theme = |name: &str| {
            n += 1;
            let p = r.0.join(format!("{n:02}-{name}"));
            std::fs::create_dir_all(&p).unwrap();
            p
        };

        // An empty theme, and one with files only in the top folder or that are no icons.
        let t = theme("empty");
        assert_eq!(compare(&t, "empty", true), 0);
        let t = theme("top-only");
        put(&t, "index.theme", b"[Icon Theme]\nName=x\n");
        put(&t, "top.png", b"x");
        put(&t, "top.svg", b"x");
        put(&t, "48x48/apps/readme.txt", b"x");
        put(&t, "48x48/apps/lonely.icon", b"[Icon Data]\nDisplayName=x\n");
        assert_eq!(compare(&t, "top-only", true), 0);

        // Tiny themes. The files are not images: the tool reads none in index-only mode.
        let t = theme("tiny");
        for f in [
            "16x16/apps/a.png", "16x16/apps/a.svg", "16x16/apps/b.svg", "16x16/apps/c.xpm", "16x16/apps/c.txt", "48x48/apps/a.png", "48x48/apps/d.png",
            "48x48/apps/sub/z.png", "48x48/apps/sub/deeper/a.xpm", "scalable/apps/a.svg", "scalable/apps/e.svg", "top.png", "a/0.png", "a/b/x.png",
            "a/c.png", "b/b/b.png", "b/a.png", "x/.png", "x/a.b.png", "x/a.b.svg", "x/a.png", "x/A.png", "x/foo.PNG", "x/foo.png.txt", "x/-+_.svg",
        ] {
            put(&t, f, b"not an image");
        }
        assert!(compare(&t, "tiny", true) > 0);
        let t = theme("one");
        put(&t, "48x48/apps/only.png", b"");
        assert!(compare(&t, "one", true) > 0);

        // Real PNG content makes no difference.
        let t = theme("png-content");
        let png: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89\0\0\0\rIDATx\x9cc\xf8\x0f\x04\0\x09\xfb\x03\xfd\xe3U\xf2\x9c\0\0\0\0IEND\xaeB`\x82";
        put(&t, "16x16/apps/p.png", png);
        put(&t, "16x16/apps/q.png", png);
        assert!(compare(&t, "png-content", true) > 0);

        // `.icon` files of every kind, next to images and alone.
        let t = theme("icon-files");
        for (i, content) in ICON_FILES.iter().enumerate() {
            put(&t, &format!("16x16/apps/i{i}.png"), b"x");
            put(&t, &format!("16x16/apps/i{i}.icon"), content);
            put(&t, &format!("48x48/apps/i{i}.svg"), b"x");
            put(&t, &format!("48x48/apps/i{i}.icon"), content);
            put(&t, &format!("48x48/apps/alone{i}.icon"), content);
        }
        put(&t, "16x16/apps/apps.png", b"x");
        put(&t, "22x22/apps/Hello.png", b"x");
        assert!(compare(&t, "icon-files", true) > 0);

        // Each `.icon` file in a theme of its own.
        for (i, content) in ICON_FILES.iter().enumerate() {
            let t = theme(&format!("icon-{i}"));
            put(&t, "32x32/apps/x.png", b"x");
            put(&t, "32x32/apps/x.icon", content);
            put(&t, "32x32/apps/Hello.svg", b"x");
            assert!(compare(&t, &format!("icon-{i}"), true) > 0);
        }

        // Links: to an image, to a folder, broken, and several `.icon` names for one file (one entry in the cache).
        let t = theme("links");
        put(&t, "48x48/apps/a.png", b"x");
        put(&t, "48x48/apps/a.icon", ICON_FILES[0]);
        put(&t, "48x48/apps/b.png", b"x");
        link(&t, "48x48/apps/b.icon", "a.icon");
        put(&t, "48x48/apps/c.png", b"x");
        link(&t, "48x48/apps/c.icon", "../apps/./b.icon");
        link(&t, "48x48/apps/l.png", "a.png");
        link(&t, "48x48/apps/broken.png", "missing.png");
        link(&t, "64x64", "48x48");
        put(&t, "32x32/apps/d.svg", b"x");
        link(&t, "32x32/apps/d.icon", "../../48x48/apps/a.icon");
        assert!(compare(&t, "links", false) > 0);

        // Names that GTK's validator refuses: the tool writes no cache.
        let t = theme("bad-name");
        put(&t, "48x48/apps/has space.png", b"x");
        assert!(build(&t).is_err());
        compare(&t, "bad-name", true);
        let t = theme("bad-locale");
        put(&t, "48x48/apps/a.png", b"x");
        put(&t, "48x48/apps/a.icon", b"[Icon Data]\nDisplayName[x.y@]=z\nDisplayName[\xc3\xa9]=z\n");
        compare(&t, "bad-locale", true);

        // A generated theme: 3000 names in 20 folders, many in several folders and with several extensions.
        let t = theme("generated");
        let mut rng = Rng(42);
        let words = ["go", "edit", "view", "media", "app", "folder", "x", "list", "net", "mail", "zoom", "a", "b", "doc", "user", "sys"];
        let mut names: Vec<String> = Vec::new();
        let mut known = std::collections::HashSet::new();
        while names.len() < 3000 {
            let mut s = String::new();
            for k in 0..1 + rng.below(4) {
                if k > 0 {
                    s.push(['-', '_', '.'][rng.below(3) as usize]);
                }
                s.push_str(words[rng.below(words.len() as u64) as usize]);
            }
            if rng.below(3) == 0 {
                s.push_str(&rng.below(100).to_string());
            }
            if known.insert(s.clone()) {
                names.push(s);
            }
        }
        let sizes = ["16x16", "22x22", "24x24", "32x32", "48x48", "scalable"];
        let contexts = ["apps", "actions", "places", "status"];
        let mut folders: Vec<String> = Vec::new();
        while folders.len() < 20 {
            let f = format!("{}/{}", sizes[rng.below(6) as usize], contexts[rng.below(4) as usize]);
            let f = if rng.below(5) == 0 { format!("{f}/extra") } else { f };
            if !folders.contains(&f) {
                folders.push(f);
            }
        }
        for f in &folders {
            for name in &names {
                match rng.below(10) {
                    0..=2 => put(&t, &format!("{f}/{name}.png"), b"x"),
                    3 => put(&t, &format!("{f}/{name}.svg"), b"x"),
                    4 => {
                        put(&t, &format!("{f}/{name}.png"), b"x");
                        put(&t, &format!("{f}/{name}.svg"), b"x");
                    }
                    5 if rng.below(8) == 0 => {
                        put(&t, &format!("{f}/{name}.xpm"), b"x");
                        put(&t, &format!("{f}/{name}.icon"), ICON_FILES[rng.below(4) as usize]);
                    }
                    6 if rng.below(8) == 0 => put(&t, &format!("{f}/{name}.icon"), ICON_FILES[0]),
                    _ => {}
                }
            }
        }
        let bytes = compare(&t, "generated", true);
        assert!(bytes > 100_000, "generated: only {bytes} bytes");

        // Copies of the themes of this machine.
        for sys in ["/usr/share/icons/hicolor", "/usr/share/icons/Adwaita"] {
            if !Path::new(sys).is_dir() {
                eprintln!("{sys}: not present, not compared");
                continue;
            }
            let t = theme(Path::new(sys).file_name().unwrap().to_str().unwrap());
            let st = Command::new("cp").arg("-r").arg(format!("{sys}/.")).arg(&t).status().unwrap();
            assert!(st.success(), "cp {sys}");
            let _ = std::fs::remove_file(t.join("icon-theme.cache"));
            let bytes = compare(&t, sys, true);
            eprintln!("{sys}: {bytes} bytes, equal");
        }
    }

    #[test]
    fn iconcache_glib_helpers() {
        let keys: Vec<Vec<u8>> = (0..5000).map(|i| format!("k{i}").into_bytes()).collect();
        let mut t = GHash::new();
        for i in 0..keys.len() {
            t.insert_new(i, &keys);
        }
        assert_eq!(t.nnodes, 5000);
        assert_eq!(t.size, 8192);
        let mut seen = t.order();
        seen.sort_unstable();
        assert_eq!(seen, (0..5000).collect::<Vec<_>>());
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(t.lookup(k, &keys), Some(i));
        }
        // The order of GLib 2.88 (`GLib.get_locale_variants` from Python).
        let want = ["en_GB.UTF-8@euro", "en_GB@euro", "en.UTF-8@euro", "en@euro", "en_GB.UTF-8", "en_GB", "en.UTF-8", "en"];
        assert_eq!(locale_variants(b"en_GB.UTF-8@euro"), want.map(|s| s.as_bytes().to_vec()));
        assert_eq!(icon_name_hash(b""), 0);
        assert_eq!(g_str_hash(b""), 5381);
        let mut p = b"/a//b/./c/../d/".to_vec();
        canonicalize_filename(&mut p);
        assert_eq!(p, b"/a/b/d");
        assert!(build_from_files(&["a/../b.png".to_string()]).is_err());
        assert!(build_from_files(&["48x48/a.png".to_string(), "48x48/a.icon".to_string()]).is_err());
        assert!(build_from_files(&[]).unwrap().is_empty());
    }
}

