// SPDX-License-Identifier: MIT OR Apache-2.0
// Panpak stub: verify (spec §5) -> materialize -> Landlock sandbox -> exec.
// Build: cargo build --release --target x86_64-unknown-linux-musl
use anyhow::{bail, ensure, Context, Result};
use panpak_core::{bail_code, coded, ensure_code, ErrorCode};
use panpak_core::{hex, ChunkDecoder, parse_index, verify, Chunk, Entry, Kind};
#[cfg(test)]
use panpak_core::read_footer;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{fs::{OpenOptionsExt, PermissionsExt}, io::AsRawFd, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(target_arch = "x86_64")]
mod busrouter;
mod chooser;
mod dbus_layer;
mod opener;
mod probe;
mod receiver;
mod sharedtmp;
#[cfg(test)]
mod seccomp_test;
#[cfg(test)]
mod testutil;
mod view;
mod wxhint;

/// A file that is past this size is written by several threads at once, each a part of it (the others are one piece of work).
const MAX_ASSEMBLE: u64 = 8 << 20;

/// Create `base/rel` one level at a time, refusing symlinks or non-dirs
/// in prefixes: `create_dir_all` would follow them (tmp escape).
fn mkdir_secure(base: &Path, rel: &Path) -> Result<()> {
    let mut cur = base.to_path_buf();
    for c in rel.components() {
        cur.push(c);
        match fs::symlink_metadata(&cur) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&cur)?;
            }
            Err(e) => return Err(e.into()),
            Ok(m) => {
                ensure!(!m.file_type().is_symlink(), "unexpected symlink in path");
                ensure!(m.is_dir(), "non-directory prefix");
            }
        }
    }
    Ok(())
}

/// Run `f` on every item, on `threads` threads, stopping at the first error.
fn par_each<T: Sync>(items: &[T], threads: usize, f: &(dyn Fn(&T) -> Result<()> + Sync)) -> Result<()> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failed: std::sync::Mutex<Option<anyhow::Error>> = std::sync::Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..threads.max(1).min(items.len().max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(item) = items.get(i) else { break };
                if failed.lock().map(|e| e.is_some()).unwrap_or(true) {
                    break;
                }
                if let Err(e) = f(item) {
                    if let Ok(mut slot) = failed.lock() {
                        slot.get_or_insert(e);
                    }
                    break;
                }
            });
        }
    });
    match failed.into_inner().ok().flatten() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Where the bytes of a chunk come from when a tree is made: the package itself (`install`, a start of a package file), or what an update
/// downloaded together with the files of the tree it replaces (`prepare --chunks`). Shared by the threads that make the files: it holds no
/// state that changes (a package is read with `pread`), and every thread brings its own decoder and buffer.
trait ChunkSource: Sync {
    /// The raw bytes of chunk `c`, checked against its hash.
    fn raw<'a>(&self, c: &Chunk, dec: &mut ChunkDecoder, scratch: &'a mut Vec<u8>) -> Result<&'a [u8]>;
}

/// The chunks of an open package file.
struct PackageChunks<'f> {
    f: &'f File,
}

impl ChunkSource for PackageChunks<'_> {
    fn raw<'a>(&self, c: &Chunk, dec: &mut ChunkDecoder, scratch: &'a mut Vec<u8>) -> Result<&'a [u8]> {
        use std::os::unix::fs::FileExt;
        let mut comp = vec![0u8; c.comp as usize];
        self.f.read_exact_at(&mut comp, c.off)?;
        // Cap = declared raw (validated by the parser: <= 1 MiB).
        dec.decode(&comp, c.raw as u64, scratch)?;
        panpak_core::verify_chunk(c, scratch)?;
        Ok(scratch)
    }
}

/// The chunks of an update: the ones that were downloaded (a file `<hash in hex>` of the folder, the compressed chunk as it is in the
/// package) and the ones that the files of an installed tree already hold (found by the index of that version). A chunk is checked against
/// its hash wherever it comes from; one that is in neither is an error that says which.
struct StagedChunks {
    dir: Option<PathBuf>,
    /// hash -> (file of the old tree, offset in it, raw length)
    old: std::collections::HashMap<[u8; 32], (PathBuf, u64, u32)>,
}

impl StagedChunks {
    /// `old_tree` and the index of its version (`old_chunks`, `old_entries`): where each of its chunks is in its files.
    fn new(dir: Option<PathBuf>, old: Option<(&Path, &[Chunk], &[Entry])>) -> Self {
        let mut map = std::collections::HashMap::new();
        if let Some((tree, chunks, entries)) = old {
            for e in entries {
                let Kind::File { chunks: ids } = &e.kind else { continue };
                let mut off = 0u64;
                for &id in ids {
                    let c = &chunks[id as usize];
                    map.entry(c.hash).or_insert_with(|| (tree.join(&e.path), off, c.raw));
                    off += c.raw as u64;
                }
            }
        }
        Self { dir, old: map }
    }
}

impl ChunkSource for StagedChunks {
    fn raw<'a>(&self, c: &Chunk, dec: &mut ChunkDecoder, scratch: &'a mut Vec<u8>) -> Result<&'a [u8]> {
        use std::os::unix::fs::FileExt;
        if let Some((path, off, len)) = self.old.get(&c.hash) {
            if *len == c.raw {
                // A damaged file of the old tree is not an error here: the chunk is looked for in the download next.
                let mut bytes = vec![0u8; *len as usize];
                if File::open(path).and_then(|f| f.read_exact_at(&mut bytes, *off)).is_ok() && panpak_core::verify_chunk(c, &bytes).is_ok() {
                    scratch.clear();
                    scratch.extend_from_slice(&bytes);
                    return Ok(scratch);
                }
            }
        }
        let dir = self.dir.as_ref().with_context(|| format!("chunk {} is not in the files of the installed version and no download was given", hex(&c.hash)))?;
        let file = dir.join(hex(&c.hash));
        let comp = fs::read(&file).with_context(|| format!("chunk {} is neither in the installed files nor in {}", hex(&c.hash), dir.display()))?;
        ensure!(comp.len() == c.comp as usize, "chunk {}: the downloaded file has {} bytes, the index says {}", hex(&c.hash), comp.len(), c.comp);
        dec.decode(&comp, c.raw as u64, scratch)?;
        panpak_core::verify_chunk(c, scratch)?;
        Ok(scratch)
    }
}

/// Write the shared blob (atomic) and link it with a hardlink;
/// fallback to copy if the fs does not support hardlinks.
fn link_or_copy(blob: &Path, dst: &Path, buf: Option<&[u8]>, mode: u32) -> Result<()> {
    if fs::hard_link(blob, dst).is_ok() {
        return Ok(());
    }
    let data = match buf {
        Some(b) => b.to_vec(),
        None => fs::read(blob)?,
    };
    fs::write(dst, &data)?;
    fs::set_permissions(dst, fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// Strip write bits from files and dirs (symlinks excluded: they have no
/// permissions of their own and set_permissions would follow them). Defense in depth beyond Landlock.
fn strip_write_bits(root: &Path) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let m = fs::symlink_metadata(&p)?;
        if m.file_type().is_symlink() {
            continue;
        }
        if m.is_dir() {
            for e in fs::read_dir(&p)? {
                stack.push(e?.path());
            }
        }
        let mode = m.permissions().mode();
        if mode & 0o222 != 0 {
            fs::set_permissions(&p, fs::Permissions::from_mode(mode & !0o222))?;
        }
    }
    Ok(())
}

/// Make an already materialized tree non-writable (idempotent).
/// The marker is a *sibling* file of the tree: nothing can be created inside,
/// the tree is already a-w.
fn ensure_tree_readonly(tree: &Path) -> Result<()> {
    let marker = tree.with_extension("readonly");
    if marker.exists() {
        return Ok(());
    }
    strip_write_bits(tree)?;
    File::create(&marker)?;
    Ok(())
}

/// The modification time of every folder and link of a tree in the store, and the earliest of a file: 2000-01-01T00:00:00Z. A package does not carry times, and the
/// time at which the store happened to create each entry is of no use: it made a cache file older than the folder that holds it (a folder gets a new time whenever an
/// entry is created in it, and `icon-theme.cache` comes before the folders that sort after it), and Qt ignores an icon cache that is older than its folder.
/// A cache file that describes a folder (`icon-theme.cache`) takes the latest time of that folder and of the folders below it (`tree_times`), so it is never older than what it describes.
pub const TREE_MTIME: i64 = 946_684_800;

/// How far above `TREE_MTIME` the time of a file can be: 2^25 seconds, a little more than a year.
const FILE_TIME_SPAN: u64 = 1 << 25;

/// The modification time of a file, from its content: `TREE_MTIME` and a number of seconds made of the first bytes of the name of its blob (the hash of its chunks and
/// its mode). The same content has the same time in every tree and every version, and another content has another time (but for a chance of one in 33 million), so a
/// program that keeps what it learned about a file by the time of the file (GIMP: the plug-ins that it has queried) asks again for the files that changed and for no other.
/// Files that are shared between trees (hard links of the blobs) have one time, since it is the time of the inode.
pub fn content_mtime(blob: &str) -> i64 {
    let n = blob.get(..8).and_then(|h| u32::from_str_radix(h, 16).ok()).unwrap_or(0) as u64;
    TREE_MTIME + (n % FILE_TIME_SPAN) as i64
}

/// The time of every file and folder of a tree, made from what the package says and from nothing else (a store, a machine and a version give the same times):
/// a file has the time of its content (`content_mtime` of its blob); a folder has the time of its list, a number of seconds made like that of a file from the hash of
/// the names and the contents of its entries (a name, what it is, and the name of the blob, the target of the link or nothing for a folder), so a folder whose list changes has another
/// time, and a folder whose list is the same has the same one in every version. A link has `TREE_MTIME`. A cache that says it describes a folder (a file named
/// `icon-theme.cache`) is given the latest time of its folder and of the folders below, because GTK and Qt drop it when it is older than any of them.
fn tree_times(entries: &[Entry], chunks: &[Chunk]) -> std::collections::HashMap<PathBuf, i64> {
    use std::collections::{BTreeMap, HashMap};
    let mut list: BTreeMap<PathBuf, BTreeMap<Vec<u8>, Vec<u8>>> = BTreeMap::new();
    let mut times: HashMap<PathBuf, i64> = HashMap::new();
    for e in entries {
        let what: Vec<u8> = match &e.kind {
            Kind::File { chunks: ids } => {
                let name = blob_name(chunks, ids, e.mode);
                times.insert(e.path.clone(), content_mtime(&name));
                [b"f:".as_slice(), name.as_bytes()].concat()
            }
            Kind::Link(t) => [b"l:".as_slice(), t.as_bytes()].concat(),
            Kind::Dir => b"d:".to_vec(),
        };
        let mut child = e.path.clone();
        while let Some(parent) = child.parent() {
            use std::os::unix::ffi::OsStrExt;
            let name = child.file_name().map(|n| n.as_bytes().to_vec()).unwrap_or_default();
            let w = if child == e.path { what.clone() } else { b"d:".to_vec() };
            list.entry(parent.to_path_buf()).or_default().insert(name, w);
            child = parent.to_path_buf();
            if child.as_os_str().is_empty() {
                break;
            }
        }
    }
    for (dir, items) in &list {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let mut h = blake3::Hasher::new();
        h.update(b"panpak-folder-v1");
        for (name, what) in items {
            h.update(&(name.len() as u32).to_le_bytes());
            h.update(name);
            h.update(&(what.len() as u32).to_le_bytes());
            h.update(what);
        }
        times.insert(dir.clone(), content_mtime(&hex(h.finalize().as_bytes())));
    }
    // Folders without entries are the ones that the package lists as folders: the time of an empty list.
    for e in entries {
        if matches!(e.kind, Kind::Dir) {
            times.entry(e.path.clone()).or_insert_with(|| content_mtime(&hex(blake3::Hasher::new().update(b"panpak-folder-v1").finalize().as_bytes())));
        }
    }
    let caches: Vec<PathBuf> = entries.iter().filter(|e| matches!(e.kind, Kind::File { .. }) && e.path.file_name().is_some_and(|n| n == "icon-theme.cache")).map(|e| e.path.clone()).collect();
    let folders: Vec<&PathBuf> = list.keys().filter(|p| !p.as_os_str().is_empty()).chain(entries.iter().filter(|e| matches!(e.kind, Kind::Dir)).map(|e| &e.path)).collect();
    for c in caches {
        let dir = c.parent().map(Path::to_path_buf).unwrap_or_default();
        let latest = folders.iter().filter(|p| p.starts_with(&dir)).filter_map(|p| times.get(*p)).copied().max().unwrap_or(TREE_MTIME);
        times.insert(c, latest);
    }
    times
}

/// Give every entry of a tree the one time (`TREE_MTIME`) and take the write bits from the files and folders (a link has neither). The entries are
/// the ones that were made (the folders, the files and the links), so nothing is walked, and the work is shared between threads: it is a few thousand system calls on different inodes.
fn finalize_tree(root: &Path, paths: &[PathBuf], times: &std::collections::HashMap<PathBuf, i64>) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    fn one(p: &Path, sec: i64) -> Result<()> {
        let c = std::ffi::CString::new(p.as_os_str().as_bytes()).context("path with a NUL")?;
        let t = libc::timespec { tv_sec: sec, tv_nsec: 0 };
        // SAFETY: utimensat(2) with a NUL-terminated path and two valid timespecs; the link itself is changed, never what it points to.
        unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), [t, t].as_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
        let m = fs::symlink_metadata(p)?;
        if !m.file_type().is_symlink() {
            let mode = m.permissions().mode();
            if mode & 0o222 != 0 {
                fs::set_permissions(p, fs::Permissions::from_mode(mode & !0o222))?;
            }
        }
        Ok(())
    }
    let threads = work_threads().min(paths.len().max(1));
    let per = paths.len().div_ceil(threads.max(1)).max(1);
    let failed: std::sync::Mutex<Option<anyhow::Error>> = std::sync::Mutex::new(None);
    std::thread::scope(|s| {
        for part in paths.chunks(per) {
            let failed = &failed;
            s.spawn(move || {
                for rel in part {
                    if let Err(err) = one(&root.join(rel), times.get(rel).copied().unwrap_or(TREE_MTIME)) {
                        if let Ok(mut f) = failed.lock() {
                            f.get_or_insert(err);
                        }
                        return;
                    }
                }
            });
        }
    });
    if let Some(e) = failed.into_inner().ok().flatten() {
        return Err(e);
    }
    // The mark and the root last: they are not entries of the index (and a folder's own time does not change when its entries do not).
    one(&root.join(".complete"), TREE_MTIME)?;
    one(root, TREE_MTIME)
}

/// One file of a tree to make: where, from which chunks of the index, with which mode.
struct FileJob<'a> {
    dst: PathBuf,
    ids: &'a [u32],
    mode: u32,
}

struct Made {
    decode: std::time::Duration,
    write: std::time::Duration,
    threads: usize,
    /// How many blobs were written now (the others of the tree were in the store).
    blobs_written: usize,
}

/// How many threads make the files of a tree and sync them: the cores, at most 8 (the work is decoding, which uses a core each, and system
/// calls on different files).
fn sync_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(2, 8) * 2
}

fn work_threads() -> usize {
    // `PANPAK_THREADS` is for measuring (and for a machine where the default is wrong).
    if let Some(n) = std::env::var("PANPAK_THREADS").ok().and_then(|v| v.parse::<usize>().ok()).filter(|n| (1..=64).contains(n)) {
        return n;
    }
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 8)
}

/// The name of the blob of a file: the hash of its chunk hashes and its mode. The index gives it, so whether the blob is in the store is
/// known without reading the file: two files with the same chunks have the same bytes (the hash of each chunk is checked when it is decoded).
fn blob_name(chunks: &[Chunk], ids: &[u32], mode: u32) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"panpak-blob-v2");
    h.update(&mode.to_le_bytes());
    for &id in ids {
        h.update(&chunks[id as usize].hash);
    }
    format!("{}-{mode:o}", hex(h.finalize().as_bytes()))
}

/// Does the blob hold the chunks that the index lists for the file: every chunk, read from the blob, has the hash of the index?
fn blob_matches(blob: &Path, chunks: &[Chunk], ids: &[u32]) -> bool {
    use std::io::Read;
    let Ok(mut f) = File::open(blob) else { return false };
    let mut buf: Vec<u8> = Vec::new();
    for &id in ids {
        let c = &chunks[id as usize];
        // One read of the whole chunk (`read_to_end` on a `take` asks for a few bytes at a time and costs five calls for each chunk).
        buf.resize(c.raw as usize, 0);
        if f.read_exact(&mut buf).is_err() || panpak_core::verify_chunk(c, &buf).is_err() {
            return false;
        }
    }
    // Nothing after the last chunk.
    let mut extra = [0u8; 1];
    matches!(f.read(&mut extra), Ok(0))
}

/// The work folders that starts which were killed left in `blobs/` (`.work-<pid>-<time>`): the ones whose process is gone are removed. A folder whose
/// process still runs (an install at the same time) is left alone, and so is one whose number a later process has taken: that only costs space until
/// that process ends.
fn remove_stale_work_dirs(blobs: &Path) {
    let Ok(rd) = fs::read_dir(blobs) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(".work-") else { continue };
        let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<i32>().ok()) else { continue };
        if pid == std::process::id() as i32 {
            continue;
        }
        // SAFETY: kill(2) with signal 0 only asks whether the process exists.
        let gone = unsafe { libc::kill(pid, 0) } == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if gone {
            let _ = remove_partial_tree(&e.path());
        }
    }
}

/// One piece of work for the threads that make the files.
enum Work {
    /// A blob that is not in `blobs/` (or is there but is not what the index says): decoded and written into the work folder.
    Blob(usize),
    /// Chunks `from..to` of a large file, written at `off` (the file was made and sized beforehand): its chunks are decoded and written by
    /// several threads at once.
    Part { job: usize, from: usize, to: usize, off: u64 },
}

/// The chunks of a large file that one piece of work takes.
const PART_CHUNKS: usize = 16;

/// The free space, in bytes, of the filesystem that holds `p` (what a user that is not root can use).
fn free_bytes(p: &Path) -> Option<u64> {
    use std::ffi::CString;
    let c = CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: statvfs(2) fills the struct that is passed, and nothing else.
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        (st.f_bavail as u64).checked_mul(st.f_frsize as u64)
    }
}

/// Space that is left for the folders, the links and the records that a tree needs besides the files that are new.
const SPACE_RESERVE: u64 = 16 << 20;

/// Is there room in the store for the files that are new? They are known from the index (every file that has no blob in the store yet, once),
/// so a start that cannot finish stops before it writes anything. A filesystem that does not say how much is free is trusted.
fn ensure_room(blobs: &Path, need: u64) -> Result<()> {
    if need == 0 {
        return Ok(());
    }
    let Some(free) = free_bytes(blobs) else { return Ok(()) };
    if free < need.saturating_add(SPACE_RESERVE) {
        let mib = |b: u64| b.div_ceil(1 << 20);
        let msg = format!(
            "not enough free space in the store: the new files of this version need {} MiB and {} MiB are free (with a reserve of {} MiB); nothing was written. Free some space (`panpak gc` takes out what no installed version uses) and try again",
            mib(need),
            free >> 20,
            SPACE_RESERVE >> 20
        );
        return Err(anyhow::Error::new(std::io::Error::new(std::io::ErrorKind::StorageFull, msg)));
    }
    Ok(())
}

/// Make the files of a tree. Every file, large or small, goes the same way: it is a blob of the store, named by the hashes of its chunks, and the
/// file of the tree is a link to it. A blob that is in the store is checked against the index (every chunk, read from the blob, has the hash that
/// the index gives) before it is linked, because the file of the tree is that very file. A blob that is not there is written into a work folder
/// (a large one by several threads, each a part of it), made durable, and only then named.
fn make_files(src: &dyn ChunkSource, chunks: &[Chunk], jobs: &[FileJob], blobs: &Path) -> Result<Made> {
    use std::os::unix::fs::FileExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let total_of = |job: &FileJob| -> u64 { job.ids.iter().map(|&id| chunks[id as usize].raw as u64).sum() };
    // The names, and which blobs have to be made (once each): their size is the space that this start needs.
    let names: Vec<String> = jobs.iter().map(|job| blob_name(chunks, job.ids, job.mode)).collect();
    let mut first_of: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut unique: Vec<(usize, bool)> = Vec::new();
    let mut need = 0u64;
    for (i, job) in jobs.iter().enumerate() {
        if !first_of.insert(names[i].as_str()) {
            continue;
        }
        let missing = fs::symlink_metadata(blobs.join(&names[i])).is_err();
        if missing {
            need += total_of(job);
        }
        unique.push((i, missing));
    }
    ensure_room(blobs, need)?;
    remove_stale_work_dirs(blobs);
    let work_dir = blobs.join(format!(".work-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
    fs::create_dir(&work_dir).with_context(|| format!("make {}", work_dir.display()))?;
    // The work: the large files by parts (they are the long pieces, first), then the other blobs, one piece each.
    let mut work: Vec<Work> = Vec::new();
    let mut large: Vec<usize> = Vec::new();
    for &(i, missing) in &unique {
        let total = total_of(&jobs[i]);
        if missing && total > MAX_ASSEMBLE {
            let f = File::create(work_dir.join(&names[i]))?;
            f.set_len(total)?;
            let mut off = 0u64;
            for (n, group) in jobs[i].ids.chunks(PART_CHUNKS).enumerate() {
                work.push(Work::Part { job: i, from: n * PART_CHUNKS, to: n * PART_CHUNKS + group.len(), off });
                off += group.iter().map(|&id| chunks[id as usize].raw as u64).sum::<u64>();
            }
            large.push(i);
        }
    }
    for &(i, missing) in &unique {
        if !(missing && total_of(&jobs[i]) > MAX_ASSEMBLE) {
            work.push(Work::Blob(i));
        }
    }

    let next = AtomicUsize::new(0);
    // The files of the tree that were linked already, while the other threads were still decoding (a blob that is in `blobs/` is linked at once).
    let linked: Vec<std::sync::atomic::AtomicBool> = (0..jobs.len()).map(|_| std::sync::atomic::AtomicBool::new(false)).collect();
    let failed: std::sync::Mutex<Option<anyhow::Error>> = std::sync::Mutex::new(None);
    let threads = work_threads().min(work.len().max(1));
    let results: std::sync::Mutex<Vec<(Vec<String>, std::time::Duration, std::time::Duration)>> = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut written: Vec<String> = Vec::new();
                let (mut t_decode, mut t_write) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
                let run = (|| -> Result<()> {
                    let mut dec = ChunkDecoder::new()?;
                    let mut scratch = Vec::new();
                    loop {
                        let w = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = work.get(w) else { return Ok(()) };
                        if failed.lock().map(|f| f.is_some()).unwrap_or(true) {
                            return Ok(());
                        }
                        let i = match item {
                            Work::Part { job, from, to, off } => {
                                let ji = *job;
                                let job = &jobs[ji];
                                let out = OpenOptions::new().write(true).open(work_dir.join(&names[ji]))?;
                                let mut at = *off;
                                for &id in &job.ids[*from..*to] {
                                    let t = std::time::Instant::now();
                                    let raw = src.raw(&chunks[id as usize], &mut dec, &mut scratch)?;
                                    t_decode += t.elapsed();
                                    let t = std::time::Instant::now();
                                    out.write_all_at(raw, at)?;
                                    at += raw.len() as u64;
                                    t_write += t.elapsed();
                                }
                                panpak_core::durable::start_writeback(&out);
                                continue;
                            }
                            Work::Blob(i) => *i,
                        };
                        let job = &jobs[i];
                        let name = names[i].as_str();
                        let blob = blobs.join(name);
                        let t = std::time::Instant::now();
                        if fs::symlink_metadata(&blob).is_ok() {
                            // In `blobs/`, so on disk. It is not taken on trust: the file of the tree is this very file, so a blob that is not what
                            // the signed index says would be inherited by the tree. It is taken out of the store then (the other trees that hold it
                            // keep their copy of the inode, which `repair` of those apps finds) and made again below.
                            if blob_matches(&blob, chunks, job.ids) {
                                ensure_blob_readonly(&blob)?;
                                link_or_copy(&blob, &job.dst, None, job.mode)?;
                                linked[i].store(true, Ordering::Relaxed);
                                t_write += t.elapsed();
                                continue;
                            }
                            fs::remove_file(&blob)?;
                        }
                        // Written one chunk after the other: the memory is a chunk, whatever the size of the file.
                        let path = work_dir.join(name);
                        let mut out = File::create(&path)?;
                        for &id in job.ids {
                            let t = std::time::Instant::now();
                            let raw = src.raw(&chunks[id as usize], &mut dec, &mut scratch)?;
                            t_decode += t.elapsed();
                            let t = std::time::Instant::now();
                            out.write_all(raw)?;
                            t_write += t.elapsed();
                        }
                        let t = std::time::Instant::now();
                        panpak_core::durable::start_writeback(&out);
                        drop(out);
                        // Shared blob: never writable (defense in depth).
                        fs::set_permissions(&path, fs::Permissions::from_mode(job.mode & !0o222))?;
                        written.push(name.to_string());
                        t_write += t.elapsed();
                    }
                })();
                if let Err(e) = run {
                    if let Ok(mut f) = failed.lock() {
                        f.get_or_insert(e);
                    }
                }
                if let Ok(mut r) = results.lock() {
                    r.push((written, t_decode, t_write));
                }
            });
        }
    });
    if let Some(e) = failed.into_inner().ok().flatten() {
        return Err(e);
    }
    let mut made = Made { decode: std::time::Duration::ZERO, write: std::time::Duration::ZERO, threads, blobs_written: 0 };
    let mut written: Vec<String> = Vec::new();
    for (w, d, t) in results.into_inner().unwrap_or_default() {
        written.extend(w);
        made.decode += d;
        made.write += t;
    }
    // The large blobs were written by parts: now they are whole, and get their mode like the others.
    for &i in &large {
        fs::set_permissions(work_dir.join(&names[i]), fs::Permissions::from_mode(jobs[i].mode & !0o222))?;
        written.push(names[i].clone());
    }
    made.blobs_written = written.len();
    // Publish the blobs: on disk first, then named.
    let t = std::time::Instant::now();
    panpak_core::failpoint!("prepare.blobs_written");
    if !written.is_empty() {
        let in_work: Vec<PathBuf> = written.iter().map(|n| work_dir.join(n)).collect();
        panpak_core::durable::fsync_files(&in_work, sync_threads())?;
        panpak_core::failpoint!("prepare.blobs_synced");
        par_each(&written, work_threads(), &|n: &String| {
            let dst = blobs.join(n);
            if fs::symlink_metadata(&dst).is_ok() {
                // Another start named it meanwhile: that one is on disk (it was synced before it was named), this copy goes.
                let _ = fs::remove_file(work_dir.join(n));
                Ok(())
            } else {
                fs::rename(work_dir.join(n), &dst).with_context(|| format!("name the blob {n}"))
            }
        })?;
        panpak_core::failpoint!("prepare.blobs_named");
        panpak_core::durable::fsync_dir(blobs)?;
    }
    fs::remove_dir(&work_dir).with_context(|| format!("remove {}", work_dir.display()))?;
    // The rest of the files of the tree, linked to their blobs (every one of them is in `blobs/` now).
    par_each(&(0..jobs.len()).collect::<Vec<_>>(), work_threads(), &|&i: &usize| {
        if linked[i].load(Ordering::Relaxed) {
            return Ok(());
        }
        let job = &jobs[i];
        let blob = blobs.join(&names[i]);
        ensure_blob_readonly(&blob)?;
        link_or_copy(&blob, &job.dst, None, job.mode)
    })?;
    made.write += t.elapsed();
    panpak_core::failpoint!("prepare.files_linked");
    Ok(made)
}

/// Remove what a start that was killed left in `<root>.partial`. Its folders may already be read-only (the last steps before the publish take the
/// write bits away), and a read-only folder cannot lose its entries: they are made writable first. Links are never followed.
fn remove_partial_tree(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(m) = fs::symlink_metadata(&p) else { continue };
        if m.is_dir() {
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755))?;
            for e in fs::read_dir(&p)? {
                stack.push(e?.path());
            }
        }
    }
    fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))
}

/// Materialize the tree of a package file into the store (see `materialize_from`).
fn materialize(
    f: &mut File,
    store: &Path,
    root: &[u8; 32],
    chunks: &[Chunk],
    entries: &[Entry],
) -> Result<PathBuf> {
    materialize_from(&PackageChunks { f: &*f }, store, root, chunks, entries)
}

/// Materialize the tree into the store; chunks are verified before use.
/// Identical files (by content+mode) share the same inode from store/blobs.
///
/// Durable in this order (PANPAK-FORMAT.md 6.2): the data of every file is written back, then the folders of the tree, and only then
/// is the tree published with a rename and the `trees` folder synced: nothing that names the tree can be on disk before the tree.
fn materialize_from(
    src: &dyn ChunkSource,
    store: &Path,
    root: &[u8; 32],
    chunks: &[Chunk],
    entries: &[Entry],
) -> Result<PathBuf> {
    use std::collections::HashMap;
    let tree = store.join("trees").join(hex(root));
    if tree.join(".complete").exists() {
        ensure_tree_readonly(&tree)?;
        return Ok(tree);
    } // Fast startup.
    // Making a tree is work that a collection must not see half done (a start of a package that is not installed does it too): the store lock, shared, for as long as it takes.
    let _store_lock = panpak_core::applock::store_shared(store, panpak_core::applock::WAIT)?;
    // Where the time goes (shown with PANPAK_DEBUG): decoding the chunks, writing files and links, making the tree durable.
    let t_start = std::time::Instant::now();
    // One builder per tree: starts that overlap on a store without the tree wait here, then find it done.
    // The lock goes with the file descriptor, so a builder that dies never leaves the others stuck.
    fs::create_dir_all(store.join("trees"))?;
    let lock = OpenOptions::new().create(true).truncate(false).write(true).open(tree.with_extension("lock")).context("open the tree lock")?;
    // SAFETY: flock(2) on a descriptor this function owns.
    ensure!(unsafe { libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) } == 0, "cannot lock the tree");
    if tree.join(".complete").exists() {
        ensure_tree_readonly(&tree)?;
        return Ok(tree);
    }
    // Under the lock nobody else writes here: a folder from a start that was killed is removed, and the
    // new one is built apart and published with one rename.
    let tmp = tree.with_extension("partial");
    remove_partial_tree(&tmp)?;
    fs::create_dir_all(&tmp)?;
    let blobs = store.join("blobs");
    fs::create_dir_all(&blobs)?;

    // Pass 1: the folders (one thread) and the list of the files to make. No symlink exists yet in tmp.
    let t_pass1 = std::time::Instant::now();
    let mut jobs: Vec<FileJob> = Vec::new();
    // The folders: every one the index names and every parent of a file, made level by level (a level needs the one above), the folders of a level
    // on several threads. The tree is a folder that this start has just made, under its lock, and holds no link yet (links come after the files),
    // so a plain `mkdir` is safe; `mkdir_secure`, which checks every component of the path, was a third of the time of making 940 folders.
    let mut dir_modes: HashMap<&Path, u32> = HashMap::new();
    let mut made_dirs: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    let add_with_parents = |p: &Path, set: &mut std::collections::BTreeSet<PathBuf>| {
        let mut cur = Some(p);
        while let Some(c) = cur {
            if c.as_os_str().is_empty() || !set.insert(c.to_path_buf()) {
                break;
            }
            cur = c.parent();
        }
    };
    let mut seen: std::collections::HashSet<&Path> = std::collections::HashSet::new();
    for e in entries {
        ensure!(seen.insert(e.path.as_path()), "path already exists");
        match &e.kind {
            Kind::Dir => {
                dir_modes.insert(&e.path, e.mode);
                add_with_parents(&e.path, &mut made_dirs);
            }
            Kind::Link(_) => {
                if let Some(parent) = e.path.parent() {
                    add_with_parents(parent, &mut made_dirs);
                }
            }
            Kind::File { chunks: ids } => {
                let dst = tmp.join(&e.path);
                if let Some(parent) = e.path.parent() {
                    add_with_parents(parent, &mut made_dirs);
                }
                jobs.push(FileJob { dst, ids, mode: e.mode });
            }
        }
    }
    let mut levels: Vec<Vec<&PathBuf>> = Vec::new();
    for d in &made_dirs {
        let depth = d.components().count();
        if levels.len() < depth {
            levels.resize_with(depth, Vec::new);
        }
        levels[depth - 1].push(d);
    }
    for level in &levels {
        par_each(level, work_threads(), &|rel: &&PathBuf| {
            let path = tmp.join(rel.as_path());
            fs::create_dir(&path).with_context(|| format!("make the folder {}", path.display()))?;
            if let Some(mode) = dir_modes.get(rel.as_path()) {
                fs::set_permissions(&path, fs::Permissions::from_mode(*mode))?;
            }
            Ok(())
        })?;
    }
    for job in &jobs {
        ensure!(fs::symlink_metadata(&job.dst).is_err(), "path already exists");
    }
    panpak_core::failpoint!("prepare.folders_made");
    // Pass 1b: the files, on every core. A file whose blob is already in the store is only linked: the blob is named by the hashes of its
    // chunks, which the index gives, so nothing of it is decoded or written. The others are decoded and written into a work folder, made durable,
    // and only then named in `blobs/` (see `make_files`).
    let t_folders = t_pass1.elapsed();
    let t_files_pass = std::time::Instant::now();
    let made = make_files(src, chunks, &jobs, &blobs)?;
    let t_files_wall = t_files_pass.elapsed();
    let t_after = std::time::Instant::now();
    let (t_decode, t_write) = (made.decode, made.write);

    // Pass 2: symlinks, checking the target RESOLVED against the
    // link's directory (spec §5.5): a relative target with leading ".."
    // is legal if it stays inside the root (e.g. usr/translations/x ->
    // ../share/... == usr/share/...). A purely lexical check on the
    // lone target would give false positives; chains stay verified
    // later (pass 3).
    use panpak_core::link_escapes as escaped;
    let mut links: HashMap<PathBuf, String> = HashMap::new();
    for e in entries {
        if let Kind::Link(t) = &e.kind {
            ensure!(!escaped(&e.path, t), "symlink escapes root: {}", e.path.display());
            let dst = tmp.join(&e.path);
            if let Some(parent) = e.path.parent() {
                if !parent.as_os_str().is_empty() {
                    mkdir_secure(&tmp, parent)?;
                }
            }
            ensure!(fs::symlink_metadata(&dst).is_err(), "path already exists");
            std::os::unix::fs::symlink(t, &dst)?;
            links.insert(e.path.clone(), t.clone());
        }
    }

    // Pass 3: resolve symlink chains per prefix (max 40 iterations)
    // and refuse composed escapes (e.g. a -> b/passwd with b -> ..), which the
    // single-target check cannot see. Pure lexical normalization.
    fn resolve(links: &HashMap<PathBuf, String>, start: &Path) -> Result<()> {
        use std::path::Component::*;
        let mut cur = start.to_path_buf();
        for _ in 0..40 {
            // Longest prefix of cur that is a symlink (cur itself included).
            let mut node: Option<PathBuf> = None;
            let mut anc = cur.as_path();
            loop {
                if links.contains_key(anc) {
                    node = Some(anc.to_path_buf());
                    break;
                }
                match anc.parent() {
                    Some(p) if !p.as_os_str().is_empty() => anc = p,
                    _ => break,
                }
            }
            let node = match node {
                Some(n) => n,
                None => return Ok(()), // Only real files/dirs: inside tmp.
            };
            let t = links[&node].clone();
            ensure!(!PathBuf::from(&t).is_absolute(), "absolute symlink in chain");
            let rest = cur.strip_prefix(&node).unwrap().to_path_buf();
            let base = node.parent().unwrap_or(Path::new("")).to_path_buf();
            let mut norm = base;
            for c in PathBuf::from(&t).components().chain(rest.components()) {
                match c {
                    Normal(x) => norm.push(x),
                    ParentDir => ensure!(norm.pop(), "symlink chain escapes root"),
                    CurDir => {}
                    _ => bail!("invalid component in chain"),
                }
            }
            cur = norm;
        }
        bail!("symlink chain too long or cyclic");
    }
    for path in links.keys() {
        resolve(&links, path)?;
    }

    // The link to the farm of this tree: the library path of the app leads through it (`panpak_core::libpath`). It is not part of
    // the package, so it does not change the root hash; it goes in before the tree is made read-only.
    let farm_link = tmp.join(panpak_core::libpath::FARM_LINK);
    ensure!(fs::symlink_metadata(&farm_link).is_err(), "the package has a path that the runtime keeps for the farm: {}", panpak_core::libpath::FARM_LINK);
    std::os::unix::fs::symlink(panpak_core::libpath::farm_target(&hex(root)), &farm_link)?;
    let t_links_only = t_after.elapsed();
    let t_links = std::time::Instant::now();
    File::create(tmp.join(".complete"))?;
    // One time for everything, and nothing writable, before the tree is published (defense in depth).
    let mut all: Vec<PathBuf> = made_dirs.iter().cloned().collect();
    all.extend(entries.iter().filter(|e| !matches!(e.kind, Kind::Dir)).map(|e| e.path.clone()));
    // The time of each file comes from its content and the time of each folder from its list (see `tree_times`); links have `TREE_MTIME`.
    let times = tree_times(entries, chunks);
    finalize_tree(&tmp, &all, &times)?;
    panpak_core::failpoint!("prepare.tree_complete");
    let t_meta = t_links.elapsed();
    let t_sync = std::time::Instant::now();
    // Everything is made; now it is made durable, files first (each was asked to start its writeback as it was written, so the
    // `fsync`s here mostly wait for writes that are already under way), then the blobs folder, then every folder of the tree and the mark.
    let t_files = t_sync.elapsed();
    panpak_core::durable::fsync_file(&tmp.join(".complete"))?;
    panpak_core::durable::fsync_dir(&blobs)?;
    let t_blobs = t_sync.elapsed();
    fsync_tree_dirs(&tmp)?;
    let t_dirs = t_sync.elapsed();
    panpak_core::failpoint!("prepare.tree_synced");
    fs::rename(&tmp, &tree)?;
    panpak_core::failpoint!("prepare.tree_renamed");
    panpak_core::durable::fsync_dir(&store.join("trees"))?;
    if panpak_core::debug_enabled() {
        eprintln!(
            "panpak: debug: folders {:.0} ms, files {:.0} ms (wall), links {:.0} ms; tree made in {:.0} ms ({} threads, {} blobs written for {} files): decode {:.0} ms, write {:.0} ms (cpu time of all threads), links and times {:.0} ms, fsync {:.0} ms (files {:.0}, blobs folder {:.0}, tree folders {:.0})",
            t_folders.as_secs_f64() * 1000.0,
            t_files_wall.as_secs_f64() * 1000.0,
            t_links_only.as_secs_f64() * 1000.0,
            t_start.elapsed().as_secs_f64() * 1000.0,
            made.threads,
            made.blobs_written,
            jobs.len(),
            t_decode.as_secs_f64() * 1000.0,
            t_write.as_secs_f64() * 1000.0,
            t_meta.as_secs_f64() * 1000.0,
            t_sync.elapsed().as_secs_f64() * 1000.0,
            t_files.as_secs_f64() * 1000.0,
            (t_blobs - t_files).as_secs_f64() * 1000.0,
            (t_dirs - t_blobs).as_secs_f64() * 1000.0
        );
    }
    File::create(tree.with_extension("readonly"))?;
    drop(lock);
    Ok(tree)
}

/// `fsync` of every folder of a tree (links, files and folders made in them are entries of their folder). A folder is synced on its own
/// (a rule of the file system, not of ours), and a sync that waits for the disk costs the same however little it has to write: 938 folders one
/// after the other took 5 ms each. Several at a time wait for the same write of the disk together, so a few threads do them.
fn fsync_tree_dirs(root: &Path) -> Result<()> {
    let mut dirs = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d)? {
            let e = e?;
            if e.file_type()?.is_dir() {
                stack.push(e.path());
            }
        }
        dirs.push(d);
    }
    panpak_core::durable::fsync_dirs(&dirs, sync_threads())
}

/// Blob from a previous version (still writable): strip the write bits.
fn ensure_blob_readonly(blob: &Path) -> Result<()> {
    let mode = fs::symlink_metadata(blob)?.permissions().mode();
    if mode & 0o222 != 0 {
        fs::set_permissions(blob, fs::Permissions::from_mode(mode & !0o222))?;
    }
    Ok(())
}

/// ABI resolution (spec §7, SONAME-granular v1).
/// Symbol versions are not compared: at equal name the host is used,
/// and if incompatible the loader fails explicitly
/// (fail-closed), never a silently wrong lib.
#[cfg(test)]
use panpak_core::abi;
mod resolution;

/// Landlock core shared with tests: read-only+execute tree,
/// app data and /tmp read+write, rest of fs read-only.
/// No silent fallback (the caller checks NotEnforced).
///
/// Detect the highest supported ABI (best-effort V9..V1) and try
/// per-single-node /dev rules (null, zero, full, urandom, random, tty).
/// If the kernel refuses per-file rules, fall back to whole /dev in RW
/// (privileged nodes stay DAC-protected): never a silent denial,
/// always and only between these two explicit choices.
/// Pure network decision (testable): what to do when the kernel offers/denies V4.
#[derive(Debug, PartialEq, Eq)]
enum NetDecision {
    /// Filter (handle net + allowlist/deny).
    Enforce,
    /// Old kernel + explicit opt-out: run unfiltered, with warning.
    UnfilteredWarn,
    /// Old kernel without opt-out, or unhonorable network=true: refuse.
    Refuse(&'static str),
}

fn decide_net(has_v4: bool, wants_net: bool, allow_unfiltered: bool) -> NetDecision {
    match (has_v4, wants_net, allow_unfiltered) {
        (true, _, _) => NetDecision::Enforce,
        (false, true, _) => NetDecision::Refuse("network=true requires Landlock ABI >= V4"),
        (false, false, true) => NetDecision::UnfilteredWarn,
        (false, false, false) => NetDecision::Refuse(
            "network cannot be filtered on this kernel (Landlock < V4): rerun with --network-unfiltered or upgrade the kernel",
        ),
    }
}

/// The view of an app that asks for no network has a network namespace of its own with only the loopback interface (see
/// `ViewPlan::no_network`). Nothing in it reaches the host or another app, so the Landlock filter of TCP ports is not
/// applied there: the app may use the loopback as it likes (a local server on a port it picks, as OpenShot does).
fn loopback_only_view(in_view: bool, any_network: bool) -> bool {
    in_view && !any_network
}

/// Grant for `panpak open`: exact file (always RW if write/rename)
/// plus, only with rename, create/rename/remove rights in the
/// parent folder WITHOUT reading/listing neighbors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct OpenGrant {
    write: bool,
    rename: bool,
}

/// Paths the sandbox opens besides the system ones, the tree and /tmp.
#[derive(Debug, Default)]
struct Access {
    /// Read and write: the two app roots and the `rw` home paths.
    rw: Vec<PathBuf>,
    /// Read only: the user's theme, font and icon files and the `ro` home paths.
    ro: Vec<PathBuf>,
    /// Path sockets every app may reach without listing them (Wayland, journal, resolver, audio with
    /// `audio = true`), only the ones that exist. Landlock V9 gets a `ResolveUnix` rule for each.
    sockets: Vec<PathBuf>,
    /// The environment of the user that the view shows, read-only, at the real paths (see `panpak_core::EnvLayers`). Only
    /// with the private home of the view; elsewhere the same files are in `ro`.
    env: panpak_core::EnvLayers,
    /// The sandbox runs inside the isolated view, which hides the sockets the manifest does not list.
    in_view: bool,
    /// The home folder, listable (`ReadDir`). Only set when the isolated view is active: there the
    /// home folder is an empty tmpfs that holds the granted folders only. Never set without the view.
    home_list: Option<PathBuf>,
    /// Places below a writable folder that stay executable (read and execute): the opener names of the runtime folder, which are the stub mounted read-only.
    exec: Vec<PathBuf>,
}

/// Refuse a link in `rel` below `home`: every part that exists must be a real
/// directory (the last one may be a file). Missing parts end the check.
fn reject_links(home: &Path, rel: &str) -> Result<()> {
    let mut cur = home.to_path_buf();
    for part in rel.split('/') {
        cur.push(part);
        match fs::symlink_metadata(&cur) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("check {}", cur.display())),
            Ok(m) => ensure_code!(
                !m.file_type().is_symlink(),
                ErrorCode::E3000,
                "home path {rel}: {} is a symbolic link, refused (a link could point outside the granted folder)",
                cur.display()
            ),
        }
    }
    Ok(())
}

/// The home folder of the user as the isolated view shows it: canonical, a folder, not `/`. With it the view mounts the
/// private home of the app there; without it the view has no home to give.
fn view_home() -> Option<PathBuf> {
    dirs::home_dir().and_then(|h| fs::canonicalize(h).ok()).filter(|h| h.is_dir() && h != Path::new("/"))
}

/// Resolve what the sandbox opens: the app roots, the readable user settings and the
/// `home_paths` of the manifest. An `rw` home path is created if missing and must be a
/// folder. Runs before Landlock is applied. `layered`: the view gives the app a private home with the environment of
/// the user as read-only mounts, so that environment is not a read rule.
fn access_for(man: &panpak_core::Manifest, dirs: &panpak_core::AppDirs, layered: bool) -> Result<Access> {
    let mut access = Access::default();
    access.rw.extend(dirs.roots().iter().map(|p| p.to_path_buf()));
    if layered {
        access.env = panpak_core::env_layers();
    } else {
        access.ro.extend(panpak_core::readonly_user_paths());
    }
    access.ro.extend(panpak_core::system_cache_paths());
    let paths = man.permissions.as_ref().and_then(|p| p.home_paths.as_deref()).unwrap_or_default();
    if paths.is_empty() {
        return Ok(access);
    }
    let home = dirs::home_dir().context("no home dir")?;
    for h in paths {
        reject_links(&home, &h.path)?;
        let full = home.join(&h.path);
        if h.access == "rw" {
            match fs::symlink_metadata(&full) {
                Ok(m) => ensure_code!(m.is_dir(), ErrorCode::E3000, "home path {}: rw needs a folder, found a file", h.path),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir_all(&full).with_context(|| format!("create {}", full.display()))?;
                }
                Err(e) => return Err(e).with_context(|| format!("check {}", full.display())),
            }
            access.rw.push(full);
        } else if full.exists() {
            access.ro.push(full);
        }
    }
    Ok(access)
}

/// The user folders the app may use: the ones pinned at install that `user-dirs.dirs` still names the
/// same way. A folder that changed, or that fails the rules now, is not granted (with a warning).
/// Granted folders go into `access`: `rw` ones are writable, `ro` ones readable. Returns (name, path
/// relative to home) of each.
fn xdg_grants(man: &panpak_core::Manifest, store: &Path, installed_mode: bool, access: &mut Access) -> Result<Vec<(String, String)>> {
    let wanted = man.permissions.as_ref().and_then(|p| p.xdg_dirs.as_deref()).unwrap_or_default();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    if !installed_mode {
        eprintln!("panpak: user folders are only granted to an installed app; none given");
        return Ok(Vec::new());
    }
    let pin = match fs::read_to_string(store.join("apps").join(&man.app.id).join("xdg-dirs")) {
        Ok(t) => match panpak_core::parse_pin(&t) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("panpak: WARNING: user folder pin unusable ({}); none given, reinstall to grant them", panpak_core::err_text(&e));
                return Ok(Vec::new());
            }
        },
        Err(_) => {
            eprintln!("panpak: WARNING: no user folders were fixed at install; none given, reinstall to grant them");
            return Ok(Vec::new());
        }
    };
    let home = dirs::home_dir().context("no home dir")?;
    let text = match dirs::config_dir().map(|c| c.join("user-dirs.dirs")).map(fs::read_to_string) {
        Some(Ok(t)) => Some(t),
        // No file, or unreadable: the default folder names apply; a folder that then differs from the one
        // pinned at install is refused with a message below.
        _ => None,
    };
    let mut out = Vec::new();
    for d in wanted {
        let Some(pinned) = pin.get(&d.name) else { continue };
        let now = panpak_core::resolve_xdg_dir(&d.name, &home, text.as_deref());
        if now.as_deref().ok() != Some(pinned.as_str()) {
            eprintln!("panpak: WARNING: the {} folder changed since install (was ~/{pinned}); not granted, reinstall to accept the new one", d.name);
            continue;
        }
        if let Err(e) = panpak_core::no_links(&home, pinned) {
            eprintln!("panpak: WARNING: the {} folder is not granted: {}", d.name, panpak_core::err_text(&e));
            continue;
        }
        let full = home.join(pinned);
        match fs::symlink_metadata(&full) {
            Ok(m) if m.is_dir() => {}
            _ => {
                eprintln!("panpak: WARNING: ~/{pinned} ({}) is not a folder; not granted", d.name);
                continue;
            }
        }
        if d.access == "rw" {
            access.rw.push(full);
        } else {
            access.ro.push(full);
        }
        out.push((d.name.clone(), pinned.clone()));
    }
    Ok(out)
}

/// Write `$XDG_CONFIG_HOME/user-dirs.dirs` in the private folder: granted folders with their path,
/// every other one at `$HOME/`. A file without the marker is the user's and stays.
fn write_user_dirs(dirs: &panpak_core::AppDirs, granted: &[(String, String)]) -> Result<()> {
    let want = panpak_core::generated_user_dirs(granted)?;
    panpak_core::safefs::write_generated(&dirs.config, &["user-dirs.dirs"], panpak_core::USER_DIRS_MARKER, &want, 0o644)
        .with_context(|| format!("write {}", dirs.config.join("user-dirs.dirs").display()))?;
    Ok(())
}

/// Write `$XDG_CONFIG_HOME/fontconfig/fonts.conf` in the private folder, so fontconfig finds the
/// user's own configuration and font folders. A file without the marker is the user's and stays.
fn write_fonts_conf(dirs: &panpak_core::AppDirs) -> Result<()> {
    let (Some(config), Some(data), Some(home)) = (dirs::config_dir(), dirs::data_dir(), dirs::home_dir()) else {
        return Ok(());
    };
    write_fonts_conf_with(dirs, &config, &data, &home)
}

fn write_fonts_conf_with(dirs: &panpak_core::AppDirs, config: &Path, data: &Path, home: &Path) -> Result<()> {
    let want = panpak_core::fonts_conf(config, data, home);
    panpak_core::safefs::write_generated(&dirs.config, &["fontconfig", "fonts.conf"], panpak_core::FONTS_CONF_MARKER, &want, 0o644)
        .with_context(|| format!("write {}", dirs.config.join("fontconfig/fonts.conf").display()))?;
    Ok(())
}

fn apply_landlock(
    tree: &Path,
    access: &Access,
    man: &panpak_core::Manifest,
    allow_unfiltered: bool,
    open: Option<(&Path, OpenGrant)>,
) -> Result<landlock::RestrictionStatus> {
    let abi = panpak_core::detect_abi().context("Landlock unsupported by kernel")?;
    // /dev single nodes first; if the kernel refuses them (build or
    // restrict), explicit fallback to whole /dev. Never a silent denial.
    let rs = match build_ruleset(tree, access, man, abi, true, allow_unfiltered, open) {
        Ok(rs) => rs,
        Err(e) => {
            eprintln!("panpak: cannot build per-node /dev rules on this kernel ({}), using whole /dev", panpak_core::err_text(&e));
            build_ruleset(tree, access, man, abi, false, allow_unfiltered, open)?
        }
    };
    let res = rs.restrict_self();
    let st = match res {
        Ok(st) => st,
        Err(e) => {
            eprintln!("panpak: kernel rejected per-node /dev rules ({e}), using whole /dev (DAC still applies)");
            build_ruleset(tree, access, man, abi, false, allow_unfiltered, open)?
                .restrict_self()
                .map_err(|_| anyhow::anyhow!("cannot enforce sandbox on this kernel: cannot run safely, upgrade the kernel (>= 5.13)"))?
        }
    };
    ensure_code!(
        st.ruleset == landlock::RulesetStatus::FullyEnforced,
        ErrorCode::E3000,
        "partial Landlock ({:?}): required rights exceed ABI, refused",
        st.ruleset
    );
    Ok(st)
}

/// Build the ruleset. With `fine_dev` try per-single-node rules
/// on /dev (null, zero, full, urandom, random, tty) + dri/shm dirs;
/// otherwise whole /dev in RW (privileged nodes DAC-protected).
/// The descriptor for a Landlock rule on `path` (`O_PATH`, see `panpak_core::open_o_path`). A path that is
/// not there gives `None` without a message: there is nothing to allow and access stays denied. Any other
/// failure (permission, wrong type) is reported, because the rule is left out.
fn rule_fd(path: &Path) -> Option<std::os::fd::OwnedFd> {
    match panpak_core::open_o_path(path) {
        Ok(fd) => Some(fd),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!("panpak: WARNING: Landlock rule for {} left out ({e}); access stays denied", path.display());
            None
        }
    }
}

/// Network (V4+), scope (V6+), Unix sockets (V9): only if the ABI offers them,
/// otherwise error only if the manifest really requires them.
fn build_ruleset(
    tree: &Path,
    access: &Access,
    man: &panpak_core::Manifest,
    abi: landlock::ABI,
    fine_dev: bool,
    allow_unfiltered: bool,
    open: Option<(&Path, OpenGrant)>,
) -> Result<landlock::RulesetCreated> {
    use landlock::{
        Access, AccessFs, AccessNet, NetPort, PathBeneath, Ruleset, RulesetAttr,
        RulesetCreatedAttr, Scope,
    };
    let new_abi = abi >= landlock::ABI::V4;
    let new_scope = abi >= landlock::ABI::V6;
    let new_unix = abi >= landlock::ABI::V9;
    let x11 = man.permissions.as_ref().and_then(|p| p.x11).unwrap_or(false);
    // Nothing the app can write may be executed (W^X): the rights of every writable place lack Execute, unless the package asks
    // for `exec_written`. The tree, `/usr` and the other read-only places keep theirs. `mmap` with PROT_EXEC is not a right of
    // Landlock and stays possible (PANPAK-FORMAT.md 3.1.8).
    let exec_written = man.permissions.as_ref().and_then(|p| p.exec_written).unwrap_or(false);
    let no_exec = |a: landlock::BitFlags<AccessFs>| if exec_written { a } else { a & !AccessFs::Execute };
    // TCP network: Enforce / UnfilteredWarn / Refuse (never silent fallback).
    // unfilterable network=true is always an error, even with the flag.
    let net_wanted = man.permissions.as_ref().and_then(|p| p.network).unwrap_or(false);
    let allow_tcp = man.permissions.as_ref().and_then(|p| p.network_allow_tcp.clone()).unwrap_or_default();
    let bind_tcp = man.permissions.as_ref().and_then(|p| p.network_bind_tcp.clone()).unwrap_or_default();
    let any_net = net_wanted || !allow_tcp.is_empty() || !bind_tcp.is_empty();
    let handle_net = if loopback_only_view(access.in_view, any_net) {
        false
    } else {
        match decide_net(new_abi, any_net, allow_unfiltered) {
        NetDecision::Enforce => true,
        NetDecision::UnfilteredWarn => {
            eprintln!("WARNING panpak: network CANNOT be filtered on this kernel ({abi:?} < V4); explicit execution with --network-unfiltered");
            false
        }
        NetDecision::Refuse(msg) => bail_code!(ErrorCode::E3000, "{msg} (detected ABI: {abi:?})"),
        }
    };
    let socks = man.permissions.as_ref().and_then(|p| p.sockets_allow.clone()).unwrap_or_default();
    if !socks.is_empty() {
        // Without V9 the isolated view is what lets the listed sockets (and only them) through.
        ensure_code!(
            new_unix || access.in_view,
            ErrorCode::E3000,
            "sockets_allow requires Landlock ABI >= V9 or the isolated view (bubblewrap), this kernel has {abi:?}"
        );
    }
    let mut b = Ruleset::default().handle_access(AccessFs::from_all(abi))?;
    if handle_net {
        b = b.handle_access(AccessNet::from_all(abi))?;
    }
    if new_scope {
        let scopes = if x11 {
            Scope::from_all(abi) & !Scope::AbstractUnixSocket
        } else {
            Scope::from_all(abi)
        };
        b = b.scope(scopes)?;
    }
    let mut rs = b.create()?;
    if x11 {
        eprintln!("panpak: WARNING X11: the app can observe other apps' input and screen; abstract sockets are not isolated");
        if let Some(fd) = rule_fd(Path::new("/tmp/.X11-unix")) {
            let mut access = AccessFs::from_read(abi);
            if new_unix {
                access |= AccessFs::ResolveUnix;
            }
            rs = rs.add_rule(PathBeneath::new(fd, access))?;
        }
    }
    for p in ["/usr", "/lib", "/lib64", "/etc", "/proc", "/sys", "/run"] {
        if let Some(fd) = rule_fd(Path::new(p)) {
            rs = rs.add_rule(PathBeneath::new(fd, AccessFs::from_read(abi)))?;
        }
    }
    // Rights on /dev: every right but `Refer`, and the device ioctl right (Landlock V5) only where it is named.
    // Without the ioctl right on a device node, an ioctl on a file opened after the sandbox is refused with EACCES;
    // a descriptor that the app inherited (its terminal) is not affected.
    let dev_ioctl = if abi >= landlock::ABI::V5 { AccessFs::IoctlDev.into() } else { landlock::BitFlags::<AccessFs>::empty() };
    let dev_rw = no_exec(AccessFs::from_all(abi) & !AccessFs::Refer & !AccessFs::IoctlDev);
    if fine_dev {
        // Minimum legitimate rights for files (no Make*/Remove* from directories).
        // Missing or unopenable nodes (e.g. /dev/tty with no controlling
        // terminal, /dev/dri with no GPU) are skipped: the self-test on
        // /dev/null in sandbox() acts as the safety net.
        for (node, write) in [
            ("/dev/null", true),
            ("/dev/zero", true),
            ("/dev/full", true),
            ("/dev/urandom", false),
            ("/dev/random", false),
            ("/dev/tty", true),
        ] {
            if let Some(fd) = rule_fd(Path::new(node)) {
                let mut access = if write {
                    AccessFs::ReadFile | AccessFs::WriteFile
                } else {
                    AccessFs::ReadFile.into()
                };
                // The terminal of the session keeps its ioctls (termios, window size): programs that open
                // /dev/tty need them. TIOCSTI and TIOCLINUX, which inject input, are refused by seccomp.
                if node == "/dev/tty" {
                    access |= dev_ioctl;
                }
                rs = rs.add_rule(PathBeneath::new(fd, access))?;
            }
        }
        // A pty: the view has a devpts of its own (see `view_args`), so these are the app's own terminals and nobody else's.
        // The master side (`ptmx`) and the slave nodes need read, write and the terminal ioctls (window size, session, job control);
        // TIOCSTI and TIOCLINUX stay refused by seccomp.
        for node in ["/dev/ptmx", "/dev/pts"] {
            if let Some(fd) = rule_fd(Path::new(node)) {
                rs = rs.add_rule(PathBeneath::new(fd, AccessFs::ReadFile | AccessFs::WriteFile | dev_ioctl))?;
            }
        }
        // The GPU needs its ioctls (DRM); /dev/shm holds plain files, to which the ioctl right does not apply.
        if let Some(fd) = rule_fd(Path::new("/dev/dri")) {
            rs = rs.add_rule(PathBeneath::new(fd, dev_rw | dev_ioctl))?;
        }
        if let Some(fd) = rule_fd(Path::new("/dev/shm")) {
            rs = rs.add_rule(PathBeneath::new(fd, dev_rw))?;
        }
    } else if let Some(fd) = rule_fd(Path::new("/dev")) {
        // Whole /dev (the kernel refused the single nodes): no device ioctls, except the GPU and the ptys below.
        rs = rs.add_rule(PathBeneath::new(fd, dev_rw))?;
        for node in ["/dev/ptmx", "/dev/pts"] {
            if let Some(fd) = rule_fd(Path::new(node)) {
                rs = rs.add_rule(PathBeneath::new(fd, dev_rw | dev_ioctl))?;
            }
        }
        if let Some(fd) = rule_fd(Path::new("/dev/dri")) {
            rs = rs.add_rule(PathBeneath::new(fd, dev_rw | dev_ioctl))?;
        }
    }
    // Tree: read + execute, NEVER write (blobs are shared
    // across trees and apps: a write would alter them everywhere).
    let rx = AccessFs::from_read(abi) | AccessFs::Execute;
    rs = rs.add_rule(PathBeneath::new(panpak_core::open_o_path(tree).with_context(|| format!("open {}", tree.display()))?, rx))?;
    // Writable dirs (app roots, rw home paths, tmp, dev): every right except Refer (link and rename between
    // directories, ABI V2). Refer on a writable dir would let the app hard-link a tree file into it (the tree has no
    // Refer, but Landlock only asks for the destination to hold the rights of the source), and a write through that
    // link would change the shared blob. Without Refer, a rename inside one directory still works (atomic save).
    // In the isolated view the tree and the system are read-only mounts of their own, and the kernel refuses a link
    // or a move across mounts (EXDEV, tests/app_dirs.py), so there Refer is granted: a program that creates a file in
    // a private folder and renames it into place, as Qt does for its local sockets, then works.
    let rw = no_exec(if access.in_view { AccessFs::from_all(abi) } else { AccessFs::from_all(abi) & !AccessFs::Refer });
    for dir in &access.rw {
        rs = rs.add_rule(PathBeneath::new(panpak_core::open_o_path(dir).with_context(|| format!("open {}", dir.display()))?, rw))?;
    }
    // What stays executable inside a writable folder: it is a read-only mount of the stub, not something the app wrote.
    for p in &access.exec {
        if let Some(fd) = rule_fd(p) {
            rs = rs.add_rule(PathBeneath::new(fd, AccessFs::from_read(abi)))?;
        }
    }
    // The home folder can be listed inside the view, so that the granted user folders show up in it.
    if let (true, Some(home)) = (access.in_view, &access.home_list) {
        rs = rs.add_rule(PathBeneath::new(panpak_core::open_o_path(home).with_context(|| format!("open {}", home.display()))?, AccessFs::ReadDir))?;
    }
    // Read only: theme, fonts and icons of the user, ro home paths. A missing or
    // unreadable entry is skipped. Files get file rights only (the kernel refuses
    // directory rights on a file); nothing here can be executed.
    for path in &access.ro {
        if let Some(fd) = rule_fd(Path::new(path)) {
            let rights = if path.is_dir() { AccessFs::ReadFile | AccessFs::ReadDir } else { AccessFs::ReadFile.into() };
            rs = rs.add_rule(PathBeneath::new(fd, rights))?;
        }
    }
    // Shared /tmp (RW): nearly all apps require it; denying it by
    // default would break even trivial scripts. Future: [permissions]tmp.
    if let Some(fd) = rule_fd(Path::new("/tmp")) {
        rs = rs.add_rule(PathBeneath::new(fd, rw))?;
    }
    // Explicitly opened file (`panpak open` / double click): ONLY that path,
    // read by default, write only with explicit declaration.
    // The caller passes the already canonicalized path.
    if let Some((path, grant)) = open {
        let fd = panpak_core::open_o_path(path)
            .with_context(|| format!("open: cannot open file: {}", path.display()))?;
        let access = if grant.write || grant.rename {
            if grant.rename {
                eprintln!("panpak: WARNING: WRITE access to {} + write, create, rename and remove anywhere below {} (neighbors cannot be read or listed, but can be overwritten/deleted)", path.display(), path.parent().map(|p| p.display().to_string()).unwrap_or_default());
            } else {
                eprintln!("panpak: the app can read and save {} (only this file)", path.display());
            }
            // Truncate is part of writing (open O_TRUNC / fs::write).
            AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::Truncate
        } else {
            eprintln!("panpak: the app can read {} (only this file).", path.display());
            AccessFs::ReadFile.into()
        };
        rs = rs.add_rule(PathBeneath::new(fd, access))?;
        // Atomic save (temp+rename, e.g. gedit): the parent folder
        // grants ONLY execute (traversal), creation, removal:
        // NO read/list. Without it, temps would not be writable and
        // rename would fail (fail-closed); with it, neighbors stay unreadable
        // but overwritable/deletable (documented exposure).
        if grant.rename {
            if let Some(parent) = path.parent() {
                let fd = panpak_core::open_o_path(parent).with_context(|| {
                    format!("open: cannot open directory: {}", parent.display())
                })?;
                rs = rs.add_rule(PathBeneath::new(
                    fd,
                    no_exec(AccessFs::Execute | AccessFs::MakeReg | AccessFs::RemoveFile | AccessFs::WriteFile | AccessFs::Truncate),
                ))?;
            }
        }
    }
    // Network: port allowlist (deny-all if network=true without ports, or if
    // network=false and the ABI allows it).
    if handle_net {
        for port in &allow_tcp {
            rs = rs.add_rule(NetPort::new(*port, AccessNet::ConnectTcp))?;
        }
        for port in &bind_tcp {
            rs = rs.add_rule(NetPort::new(*port, AccessNet::BindTcp))?;
        }
    }
    // Path Unix sockets (V9+): the sockets every app may reach, then the allowlist from the
    // manifest; under appdata/tmp they already pass via ResolveUnix.
    if new_unix {
        for path in &access.sockets {
            if let Some(fd) = rule_fd(path) {
                let rights = AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::ResolveUnix;
                rs = rs.add_rule(PathBeneath::new(fd, rights))?;
            }
        }
    }
    for s in socks.iter().filter(|_| new_unix) {
        let path = panpak_core::expand_socket_path(s)?;
        match panpak_core::open_o_path(&path) {
            Ok(fd) => {
                let access = AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::ResolveUnix;
                rs = rs.add_rule(PathBeneath::new(fd, access))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("panpak: socket {s} absent, rule omitted (access remains denied)")
            }
            Err(e) => eprintln!("panpak: WARNING: socket {s}: rule omitted ({e}); access remains denied"),
        }
    }
    Ok(rs)
}

/// Landlock from the manifest permissions (spec §3.1, policy in PANPAK-FORMAT.md §9):
/// - app tree: read + execute, never write;
/// - the private app folders (`panpak_core::AppDirs`), the `rw` home paths and /tmp: read+write;
/// - theme, font and icon files of the user and the `ro` home paths: read only;
/// - /usr /lib /etc /proc /sys /run: read-only (host runtime);
/// - /dev: only null/zero/full/urandom/random/tty nodes + dri/shm dirs
///   (explicit fallback to whole /dev if the kernel refuses per-file rules);
/// - network: denied by default (V4+), allowlist from the manifest;
///   on kernels without V4, explicit --network-unfiltered is required (warning at every start).
fn sandbox(tree: &Path, man: &panpak_core::Manifest, access: &Access, allow_unfiltered: bool, open: Option<(&Path, OpenGrant)>) -> Result<()> {
    let st = apply_landlock(tree, access, man, allow_unfiltered, open)?;
    // No silent fallback: if not applied, explicit consent is required.
    ensure_code!(
        st.ruleset != landlock::RulesetStatus::NotEnforced,
        ErrorCode::E3001,
        "Landlock unavailable: use `panpak doctor`"
    );
    // Fail-closed self-test: /dev/null openable, tree writes denied.
    // If the rules had no real effect, fail hard here.
    ensure!(
        fs::OpenOptions::new().read(true).write(true).open("/dev/null").is_ok(),
        "sandbox self-test: cannot open /dev/null"
    );
    // An unnamed temporary file in the tree: if the rules let it be made, nothing stays on disk.
    ensure!(
        fs::OpenOptions::new().write(true).custom_flags(libc::O_TMPFILE).open(tree).is_err(),
        "sandbox self-test: tree writes allowed"
    );
    Ok(())
}

/// How long the start waits for `gsettings` in total.
const GSETTINGS_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

/// Where `gsettings` is looked for: system paths only, never `PATH`. The stub runs it outside
/// the sandbox, so a program placed in a folder the user's `PATH` lists must not be picked.
const GSETTINGS_PATHS: &[&str] = &["/usr/bin/gsettings", "/bin/gsettings"];

fn find_gsettings() -> Option<PathBuf> {
    GSETTINGS_PATHS.iter().map(PathBuf::from).find(|p| p.is_file())
}

/// Variables that point an app at the user's agents; removed from every app's environment.
const AGENT_VARS: &[&str] = &["SSH_AUTH_SOCK", "GPG_AGENT_INFO", "SSH_AGENT_PID"];

/// Value that D-Bus libraries (libdbus, GDBus, sd-bus) read as "no bus": an unknown transport.
const NO_BUS_ADDRESS: &str = "disabled:";

/// Hide the local sockets the environment points to. `allows_session` and `allows_system` are true when
/// the manifest lists that bus; its address variable is then left alone. This is hygiene, not a barrier: below
/// Landlock V9 the app can still open `$XDG_RUNTIME_DIR/bus` or an agent socket by path.
fn hide_local_sockets(cmd: &mut Command, allows_session: bool, allows_system: bool) {
    for v in AGENT_VARS {
        cmd.env_remove(v);
    }
    if !allows_session {
        cmd.env("DBUS_SESSION_BUS_ADDRESS", NO_BUS_ADDRESS);
    }
    if !allows_system {
        cmd.env("DBUS_SYSTEM_BUS_ADDRESS", NO_BUS_ADDRESS);
    }
}

/// The two `gsettings` queries (`color-scheme` and `gtk-theme` of `org.gnome.desktop.interface`), running as
/// children of this process. They are started early and collected late, so that their 10 ms or so overlap the
/// work of the start. Dropping the value stops what is still running.
struct InterfaceQuery {
    kids: [Option<std::process::Child>; 2],
    deadline: std::time::Instant,
}

impl InterfaceQuery {
    /// Start both queries. The two share `timeout`, counted from now.
    fn start(gsettings: &Path, timeout: std::time::Duration) -> InterfaceQuery {
        use std::process::Stdio;
        let spawn = |key: &str| {
            Command::new(gsettings)
                .args(["get", "org.gnome.desktop.interface", key])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                // Only the dark-theme hint depends on it: a program that cannot start means "no preference".
                .ok()
        };
        InterfaceQuery { kids: [spawn("color-scheme"), spawn("gtk-theme")], deadline: std::time::Instant::now() + timeout }
    }

    /// Wait for the answers. Returns `(color-scheme, gtk-theme)` without the quotes, or None on any failure.
    fn finish(mut self) -> Option<(String, String)> {
        use std::io::Read as _;
        let mut out: [Option<String>; 2] = [None, None];
        while out.iter().any(Option::is_none) {
            for (i, kid) in self.kids.iter_mut().enumerate() {
                let Some(c) = kid else {
                    // Could not start: nothing to wait for.
                    out[i] = Some(String::new());
                    continue;
                };
                if out[i].is_some() {
                    continue;
                }
                if let Ok(Some(status)) = c.try_wait() {
                    let mut text = String::new();
                    if status.success() {
                        if let Some(mut so) = c.stdout.take() {
                            let _ = so.read_to_string(&mut text);
                        }
                    }
                    out[i] = Some(text);
                }
            }
            if std::time::Instant::now() >= self.deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let unquote = |s: &str| s.trim().trim_matches('\'').to_string();
        let [Some(scheme), Some(theme)] = out else { return None };
        Some((unquote(&scheme), unquote(&theme)))
    }
}

impl Drop for InterfaceQuery {
    /// Whatever is still running (after the deadline, or when the start ends in an error) is stopped and reaped.
    fn drop(&mut self) {
        for kid in self.kids.iter_mut().flatten() {
            if kid.try_wait().ok().flatten().is_none() {
                let _ = kid.kill();
                let _ = kid.wait();
            }
        }
    }
}

/// `GTK_THEME` for a dark desktop. GTK3 reads the dark preference from dconf, which the app
/// cannot reach with its private config folder. If the user prefers dark, the stub hands the
/// theme over as `<gtk-theme>:dark` (`Adwaita:dark` when no theme is set). The value is read once
/// at start: the app does not follow later changes. None when GTK_THEME is already set,
/// `gsettings` is missing, slow or fails, or the scheme is not `prefer-dark`.
fn gtk_dark_theme(query: Option<InterfaceQuery>) -> Option<String> {
    let (scheme, theme) = query?.finish()?;
    if scheme != "prefer-dark" {
        return None;
    }
    // A theme name is a folder name: refuse anything odd rather than pass it on.
    let ok = theme.bytes().all(|b| b.is_ascii_alphanumeric() || b" ._+-".contains(&b));
    let theme = if theme.is_empty() { "Adwaita" } else { theme.as_str() };
    ok.then(|| format!("{theme}:dark"))
}

/// True if the file starts with the ELF magic. Decides whether to use the included loader.
fn is_elf(path: &Path) -> Result<bool> {
    use std::io::Read;
    let mut magic = [0u8; 4];
    let mut f = File::open(path)?;
    let n = f.read(&mut magic)?;
    Ok(n == 4 && magic == *b"\x7fELF")
}

/// Places in the home where a file that is written, created or replaced runs a program later (a shell starts, a session logs in, a command is found on the
/// path) or opens a way in. A folder that holds one of them, or lies inside one, does not get the grant of `--write-rename`: a Landlock rule on a folder covers
/// everything below it.
const STARTUP_PLACES: &[&str] = &[
    ".bashrc", ".bash_profile", ".bash_login", ".bash_logout", ".profile", ".zshrc", ".zshenv", ".zprofile", ".zlogin", ".xprofile", ".xinitrc", ".xsession", ".pam_environment",
    ".config/autostart", ".config/systemd", ".config/environment.d", ".config/fish", ".config/plasma-workspace", ".config/shell", ".config/nushell", ".config/xdg-desktop-portal",
    ".local/bin", ".local/share/applications", ".local/share/systemd", ".ssh", ".gnupg", ".config/git", ".gitconfig",
];

/// Why the folder of `file` cannot be opened for writing, if it cannot: it is the home or a folder that holds the home, it holds one of `STARTUP_PLACES`,
/// or it lies inside one. `home` is the canonical home folder.
fn rename_scope_refusal(file: &Path, home: Option<&Path>) -> Option<String> {
    let parent = file.parent()?;
    let hint = "use the default (only the file), or move the file into a folder of its own";
    let home = home?;
    if home.starts_with(parent) {
        return Some(format!("--write-rename would let the app create and replace files anywhere below {}, which holds your home folder; {hint}", parent.display()));
    }
    for place in STARTUP_PLACES {
        let path = home.join(place);
        if path.starts_with(parent) || parent.starts_with(&path) {
            return Some(format!("--write-rename would let the app create and replace files anywhere below {}, which holds or lies in ~/{place} (a program started from there runs later, outside the sandbox); {hint}", parent.display()));
        }
    }
    None
}

/// What to say when `--panpak-open-rename` was given and the document portal serves the file instead: the flag is for a start without the
/// portal (the file is then a host path and Landlock opens that one path for reading and writing, and with `--panpak-open-rename` its folder). With the portal the app gets the file as a document, and
/// the portal lets it save by replacing the file (a temporary file and a rename in the document folder), so the folder of the file is never opened.
fn open_flags_note(open: Option<&(PathBuf, OpenGrant)>) -> Option<String> {
    let (path, grant) = open?;
    if grant.rename {
        Some(format!("--write-rename is not used: the document portal gives {} to the app and lets it save by replacing the file; the folder is not opened", path.display()))
    } else {
        None
    }
}

/// Split the stub-consumed flags from the app arguments.
/// `--network-unfiltered`, `--panpak-open <path>`, `--panpak-open-readonly` and
/// `--panpak-open-rename` never reach the application. The file of `--panpak-open` is opened for reading and writing (and never its folder), unless
/// `--panpak-open-readonly` says otherwise: whoever opens a file with an app means to work on it. The path is
/// canonicalized here, so the Landlock rule always covers the real target.
type StubFlags = (bool, Option<(PathBuf, OpenGrant)>, Vec<String>);
fn split_stub_flags(args: Vec<String>) -> Result<StubFlags> {
    let mut allow_unfiltered = false;
    let mut open_path: Option<String> = None;
    let mut open_readonly = false;
    let mut open_rename = false;
    let mut app_args = Vec::with_capacity(args.len());
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--network-unfiltered" {
            allow_unfiltered = true;
        } else if a == "--panpak-open" {
            open_path = Some(it.next().ok_or_else(|| coded(ErrorCode::E5002, "--panpak-open requires a path"))?);
        } else if a == "--panpak-open-readonly" {
            open_readonly = true;
        } else if a == "--panpak-open-write" {
            bail_code!(ErrorCode::E5001, "--panpak-open-write is no longer needed: a file given with --panpak-open can now be saved. Use --panpak-open-readonly to open it for reading only.");
        } else if a == "--panpak-open-rename" {
            open_rename = true;
        } else {
            app_args.push(a);
        }
    }
    let open_file = match open_path {
        Some(p) => {
            let abs = std::fs::canonicalize(&p).context("check the path").with_context(|| format!("open: cannot open file: {p}"))?;
            if open_rename {
                let home = dirs::home_dir().and_then(|h| std::fs::canonicalize(h).ok());
                if let Some(why) = rename_scope_refusal(&abs, home.as_deref()) {
                    bail_code!(ErrorCode::E5001, "refused: {why}");
                }
            }
            Some((abs, OpenGrant { write: !open_readonly, rename: open_rename }))
        }
        None => None,
    };
    if (open_readonly || open_rename) && open_file.is_none() {
        bail_code!(ErrorCode::E5001, "--panpak-open-readonly/--panpak-open-rename without --panpak-open");
    }
    if open_readonly && open_rename {
        bail_code!(ErrorCode::E5001, "--panpak-open-readonly and --panpak-open-rename cannot be used together.");
    }
    Ok((allow_unfiltered, open_file, app_args))
}

/// The folder of the document portal for apps is there: files given at the start are shared through it.
fn documents_served(runtime_dir: Option<&Path>) -> bool {
    runtime_dir.is_some_and(|rt| rt.join("doc/by-app").is_dir())
}

/// Files and folders among the arguments (a double click, "Open with", the command line) are shared with the app through the document portal, and the
/// document paths take their place (docs/FILE-CHOOSER.md, section 3.9). An argument that cannot be shared is dropped, with a line on standard error and a
/// notification; the app starts with the others.
fn share_start_files(args: Vec<String>, app_id: &str, runtime_dir: Option<&Path>, writable_roots: &[PathBuf], want_write: bool) -> Vec<String> {
    let (Some(rt), Ok(cwd)) = (runtime_dir, std::env::current_dir()) else { return args };
    if !rt.join("doc/by-app").is_dir() || !panpak_core::startfiles::any_file(&args, &cwd) {
        return args;
    }
    let shared = panpak_core::startfiles::share(&args, &cwd, &format!("panpak.{app_id}"), &rt.to_string_lossy(), writable_roots, want_write, || {
        panpak_core::dbus::Connection::session(std::time::Duration::from_secs(10))
    });
    for d in &shared.dropped {
        eprintln!("panpak: {d}");
    }
    if !shared.dropped.is_empty() {
        panpak_core::chooserhost::notify("Panpak: a file was not opened", &shared.dropped.join("\n"));
    }
    shared.args
}

/// When the stub started, for the timing lines of `PANPAK_DEBUG`.
/// The lazy update of the menu entry and the launcher (docs/DESKTOP-INTEGRATION.md, section 3): when the entry is older
/// than the template of this runtime, it is rewritten. Usually this reads 128 bytes and goes on. A failure is only
/// reported with `PANPAK_DEBUG`; it never stops the start.
fn refresh_menu_entry(store: &Path, man: &panpak_core::Manifest) {
    use panpak_core::desktop::{refresh_entry, Refresh};
    let Some(dir) = dirs::data_dir().map(|d| d.join("applications")) else { return };
    let schemes = man.desktop.as_ref().and_then(|d| d.schemes.clone()).unwrap_or_default();
    let mimes = man.desktop.as_ref().and_then(|d| d.mime_types.clone()).unwrap_or_default();
    let class = man.desktop.as_ref().and_then(|d| d.wm_class.as_deref());
    let Ok(launcher) = panpak_core::desktop::launcher_path(&man.app.id) else { return };
    let twin = panpak_core::desktop::twin_of(&man.app.id, class).is_some();
    match refresh_entry(store, &dir, &launcher, &man.app.id, &man.app.name, &schemes, &mimes, class, twin) {
        Ok(Refresh::Rewritten) if panpak_core::debug_enabled() => eprintln!("panpak: debug: the menu entry of {} was older and is rewritten", man.app.id),
        Ok(_) => {}
        Err(e) if panpak_core::debug_enabled() => eprintln!("panpak: debug: menu entry not refreshed: {e:#}"),
        Err(_) => {}
    }
}

static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// With `PANPAK_DEBUG` set: print how long the start has taken up to this point. A tool to see where the
/// milliseconds of the fixed cost go (`tests/startup_budget.py`).
fn stage(what: &str) {
    if panpak_core::debug_enabled() {
        if let Some(t0) = STARTED.get() {
            eprintln!("panpak: debug: +{:6.2} ms  {what}", t0.elapsed().as_secs_f64() * 1000.0);
        }
    }
}

fn main() {
    let _ = STARTED.set(std::time::Instant::now());
    if let Err(e) = real_main() {
        if panpak_core::is_storage_full(&e) {
            eprintln!("out of space: free disk space and retry");
        }
        eprintln!("{}", panpak_core::render(&e, ErrorCode::E2000));
        std::process::exit(1);
    }
}

/// The tree of a record: it must be there and complete (`PANPAK-E2007` otherwise: a record holds no chunks to make it again).
fn existing_tree(store: &Path, root: &[u8; 32]) -> Result<PathBuf> {
    let tree = store.join("trees").join(hex(root));
    ensure_code!(
        tree.join(".complete").exists(),
        ErrorCode::E2007,
        "the files of the app are missing from the store ({}); run `panpak repair <app id>` to fetch them again",
        tree.display()
    );
    ensure_tree_readonly(&tree)?;
    Ok(tree)
}

/// The embedded licenses of a version, read from its tree (the files under `.panpak/licenses`, as the index lists them).
fn read_tree_licenses(tree: &Path, raw_index: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    const MAX_TOTAL: usize = 16 << 20;
    let (_chunks, entries) = parse_index(raw_index)?;
    let mut out = Vec::new();
    let mut total = 0usize;
    for e in &entries {
        if !matches!(e.kind, Kind::File { .. }) {
            continue;
        }
        let Ok(rest) = e.path.strip_prefix(panpak_core::LICENSES_DIR) else { continue };
        let data = fs::read(tree.join(&e.path)).with_context(|| format!("read {}", e.path.display()))?;
        total += data.len();
        ensure!(total <= MAX_TOTAL, "embedded licenses too large");
        out.push((rest.to_string_lossy().into_owned(), data));
    }
    Ok(out)
}

/// The id of an installed app from the path of its package (`<store>/apps/<id>/current`), when the path has that shape.
fn installed_app_id(pkg: &Path) -> Option<(String, PathBuf)> {
    let dir = pkg.parent()?;
    let id = dir.file_name()?.to_str()?;
    let apps = dir.parent()?;
    if apps.file_name()? != "apps" || panpak_core::validate_app_id(id).is_err() {
        return None;
    }
    Some((id.to_string(), apps.to_path_buf()))
}

/// An update that was staged (`panpak update --stage`) is applied at the start of the app, before anything of the version is read: `panpak apply --at-start`
/// moves `current` when no copy of the app runs and the version asks for nothing new; in any other case it leaves it for later. Costs one `lstat` when nothing
/// is staged, and nothing when what is staged waits for the user (the state file says so). A failure only means that this start runs the installed version.
fn apply_staged_update(pkg: &Path) {
    let Some((id, apps)) = installed_app_id(pkg) else { return };
    let appdir = apps.join(&id);
    if fs::symlink_metadata(panpak_core::pending::link_path(&appdir)).is_err() {
        return;
    }
    if panpak_core::pending::read_state(&appdir).is_some_and(|s| s.needs_accept && !s.accepted) {
        return;
    }
    let Some(store) = apps.parent() else { return };
    let panpak = store.join("bin/panpak");
    if !panpak.exists() {
        return;
    }
    match Command::new(&panpak).args(["apply", "--at-start", &id]).stdin(std::process::Stdio::null()).status() {
        Ok(st) if st.success() => {}
        Ok(st) => eprintln!("panpak: the staged update of {id} was not applied ({st}); this start runs the installed version"),
        Err(e) => eprintln!("panpak: cannot run {}: {e}", panpak.display()),
    }
}

/// The `panpak` of the store and the id of the app, when the work at the end of the last copy is to be done: an installed app, a `panpak` next to the runtime.
fn exit_job(pkg: &Path) -> Option<(PathBuf, String)> {
    if std::env::var_os("PANPAK_NO_BACKGROUND").is_some() {
        return None;
    }
    installed_panpak(pkg)
}

/// The same, whatever `PANPAK_NO_BACKGROUND` says: the start of an installed app is counted (`note_start_end`), which needs this process to stay until the app ends.
fn installed_panpak(pkg: &Path) -> Option<(PathBuf, String)> {
    let (id, apps) = installed_app_id(pkg)?;
    let panpak = apps.parent()?.join("bin/panpak");
    panpak.exists().then_some((panpak, id))
}

/// Take the exclusive lock of the copies without waiting: true when no other copy holds it. (The lock of this copy is the same open file, so it is converted.)
fn flock_try_exclusive(lock: &File) -> bool {
    // SAFETY: flock on a descriptor that `lock` owns.
    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// A start of an installed app ended: count how it went (PANPAK-FORMAT.md section 6.10) and, when the version failed to start twice in a row and an earlier
/// version is known to work, ask the person in a window (`panpak rollback --ask`, detached). Nothing is rolled back here, and nothing leaves the computer.
/// Any trouble is ignored: this must never change how the app ends.
fn note_start_end(panpak: &Path, id: &str, root: &str, info: view::EndInfo) {
    use panpak_core::starts;
    let Some(store) = panpak.parent().and_then(|b| b.parent()) else { return };
    let appdir = store.join("apps").join(id);
    let a_start = !std::env::args_os().skip(1).any(|a| matches!(a.to_str(), Some("--version" | "--help" | "-h" | "-V")));
    let outcome = starts::classify_after(info.code, info.signal, info.forwarded, info.secs, a_start, starts::quick_secs());
    let Ok(ev) = starts::record_end(&appdir, root, outcome) else { return };
    if ev != starts::Event::Ask {
        return;
    }
    let current = panpak_core::durable::resolve_link(&appdir.join("current")).ok();
    match (starts::last_good(&appdir), current) {
        (Some(good), Some(cur)) if good != cur => {}
        _ => return,
    }
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(panpak);
    cmd.args(["rollback", "--ask", id]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    // SAFETY: setsid is async-signal-safe; the window outlives this process.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = cmd.spawn();
}

/// The last copy of the app has ended: let go of the lock of the copies (the work needs it free) and start `panpak background --exit <id>`, detached, and go.
/// The work is what the app left undone: the update that was staged is applied, files found damaged are repaired (this one may use the network), and the daily
/// check runs if it is due. It runs at the lowest priority and never delays the end of this process.
fn start_exit_work(lock: &File, panpak: &Path, id: &str) {
    use std::os::unix::process::CommandExt;
    // SAFETY: flock on a descriptor that `lock` owns.
    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    let mut cmd = Command::new(panpak);
    cmd.args(["background", "--exit", id]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    // SAFETY: setsid is async-signal-safe; the job leaves the session of the app and outlives this process.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = cmd.spawn();
}

/// Protected mode, before anything of the app is read: the app is marked as in protected mode (`apps/<id>/protected`, one file looked for at every start), so
/// `panpak protected-start` runs and waits: it shows the state the way that fits how the app was started and, for a program of the terminal with an
/// interactive terminal, asks its questions (an update that is ready, the internet back on). What it decides is read below, after it ends. A failure of it
/// never stops the start.
fn protected_pre(pkg: &Path) {
    let Some((id, apps)) = installed_app_id(pkg) else { return };
    if !panpak_core::release::is_marked_protected(&apps.join(&id)) {
        return;
    }
    let Some(panpak) = apps.parent().map(|s| s.join("bin/panpak")).filter(|p| p.exists()) else { return };
    let _ = Command::new(panpak).args(["protected-start", &id]).status();
}

/// Protected mode: the installed version is below a floor that its author set (and that this machine saw in an update). It is not blocked: its manifest is
/// read in memory with no network (not a port) and no program that the app wrote, unless the person turned the internet back on (then the network the app
/// asked for stays, the programs do not), and it may not open addresses (they are a way out for data) unless the person turned the internet back on.
/// Nothing signed is touched. Returns `Some(internet_back_on)` when the app is in the mode. One small file is read at every start; the rest happens only here.
fn protected_start(man: &mut panpak_core::Manifest, pkg: &Path) -> Option<bool> {
    let (id, apps) = installed_app_id(pkg)?;
    let appdir = apps.join(&id);
    let rev = man.release.as_ref().map(|r| r.revision)?;
    panpak_core::release::protected_floor(&appdir, rev)?;
    let back_on = panpak_core::release::internet_is_back_on(&appdir);
    if let Some(p) = man.permissions.as_mut() {
        panpak_core::release::protect(p, back_on);
    }
    Some(back_on)
}

/// What the state file of an installed app says at a start. Nothing here reaches the network or waits: files that the job found damaged are named (the app
/// starts if it can; the repair is done when the last copy ends, or by `panpak repair`), and when the daily job is due it is started detached and the start goes on.
fn background_work(pkg: &Path) {
    let Some((id, apps)) = installed_app_id(pkg) else { return };
    let appdir = apps.join(&id);
    let state = panpak_core::bgstate::read(&appdir);
    if state.damaged > 0 {
        eprintln!("panpak: {id}: {} file(s) do not match the signed index (for example {}); the app may not work: run `panpak repair {id}`", state.damaged, state.damaged_first);
    }
    let Some(store) = apps.parent() else { return };
    let panpak = store.join("bin/panpak");
    if !panpak.exists() {
        return;
    }
    // `PANPAK_NO_BACKGROUND` (set by the tests, and by whoever packages panpak for a machine without a network) keeps the daily job from starting.
    if state.next_check > panpak_core::bgstate::now() || std::env::var_os("PANPAK_NO_BACKGROUND").is_some() {
        return;
    }
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(&panpak);
    cmd.args(["background", "--after-start", &id]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    // SAFETY: setsid is async-signal-safe; the job leaves the process group of the app and survives the exec of the app.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = cmd.spawn();
}

/// Take the shared lock of the copies of an installed app, then check that the app is still installed.
/// `None` when the path is not that of an installed app or there is no runtime folder.
fn start_lock(pkg: &Path) -> Result<Option<File>> {
    let Some((id, apps)) = installed_app_id(pkg) else { return Ok(None) };
    let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute()) else { return Ok(None) };
    let lock = sharedtmp::early_lock(&rt, &id)?;
    // `panpak remove` may have taken the app away while this start waited for the lock.
    match fs::symlink_metadata(apps.join(&id)) {
        Ok(m) if m.is_dir() => {}
        _ => bail_code!(ErrorCode::E2004, "{id} was removed (or is being removed): nothing to start"),
    }
    Ok(Some(lock))
}

/// `panpak-stub prepare <package or record> [--chunks <folder>] [--from-record <record> | --from-tree <folder>]`: everything the first start does before the sandbox, so that
/// the app is ready when `install` or `update` ends: the package (or the record) is verified, the tree is made if it is not there, and
/// the resolution (the farm and the cache of it) is made with the same code and the same stub identity that a start uses, so the first start is a
/// hit. Nothing of the app is run. For a package the chunks come from the file; for a record they come from the folder `--chunks`
/// (what an update downloaded, a file per chunk named by its hash) and from the files of the tree of `--from-record` (the version being replaced,
/// or the same one when `repair` makes the tree again, from `--from-tree`: the damaged tree, moved aside, whose intact files are used).
fn cmd_prepare(args: &[String]) -> Result<()> {
    let pkg = PathBuf::from(args.first().ok_or_else(|| coded(ErrorCode::E5002, "usage: panpak-stub prepare <package or record> [--chunks <folder>] [--from-record <record>]"))?);
    let (mut chunks_dir, mut from_record, mut from_tree) = (None, None, None);
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--chunks" => chunks_dir = Some(PathBuf::from(it.next().ok_or_else(|| coded(ErrorCode::E5002, "--chunks needs a folder"))?)),
            "--from-record" => from_record = Some(PathBuf::from(it.next().ok_or_else(|| coded(ErrorCode::E5002, "--from-record needs a record"))?)),
            "--from-tree" => from_tree = Some(PathBuf::from(it.next().ok_or_else(|| coded(ErrorCode::E5002, "--from-tree needs a folder"))?)),
            other => bail_code!(ErrorCode::E5001, "unknown option {other}"),
        }
    }
    let store = dirs::data_dir().context("no data dir")?.join("panpak");
    let mut f = File::open(&pkg)?;
    let (ft, is_record) = panpak_core::record::read_any_footer(&mut f)?;
    let (man, raw_index) = verify(&mut f, &ft)?;
    panpak_core::check_runtime(&man, panpak_core::RUNTIME_ABI)?;
    let (chunks, entries) = parse_index(&raw_index)?;
    let tree = if is_record {
        panpak_core::record::ensure_in_store(&pkg, &store)?;
        let root = hex(&ft.root);
        if store.join("trees").join(&root).join(".complete").exists() {
            existing_tree(&store, &ft.root)?
        } else {
            // The files of the version that is replaced, found by its index.
            let old = match (&from_record, &from_tree) {
                (None, Some(t)) => {
                    // The same version again: the files that are still right are used, the index is this record's.
                    let trees = fs::canonicalize(store.join("trees"))?;
                    ensure!(fs::canonicalize(t)?.parent() == Some(trees.as_path()), "--from-tree must be a folder of the store's trees");
                    Some((t.clone(), chunks.clone(), entries.clone()))
                }
                (Some(r), _) => {
                    panpak_core::record::ensure_in_store(r, &store)?;
                    let mut of = File::open(r)?;
                    let (oft, o_rec) = panpak_core::record::read_any_footer(&mut of)?;
                    ensure!(o_rec, "--from-record needs a record");
                    let (_oman, oraw) = verify(&mut of, &oft)?;
                    let (ochunks, oentries) = parse_index(&oraw)?;
                    Some((store.join("trees").join(hex(&oft.root)), ochunks, oentries))
                }
                (None, None) => None,
            };
            let src = StagedChunks::new(chunks_dir, old.as_ref().map(|(t, c, e)| (t.as_path(), c.as_slice(), e.as_slice())));
            materialize_from(&src, &store, &ft.root, &chunks, &entries)?
        }
    } else {
        let stub_len = chunks.iter().map(|c| c.off).min().unwrap_or(ft.index_off);
        panpak_core::verify_stub_range(&mut f, stub_len, &man)?;
        materialize(&mut f, &store, &ft.root, &chunks, &entries)?
    };
    stage("tree ready");
    // The resolution, for the tree just made: the farm and its cache entry.
    let plan = resolution::resolve(&store, &pkg, &hex(&ft.root), &tree, &man)?;
    drop(plan);
    panpak_core::failpoint!("prepare.resolution_done");
    stage("resolution done");
    Ok(())
}

fn real_main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    // In the isolated view this stub is also `xdg-open` (see opener.rs): hand the address to the outer stub.
    if args.first().is_some_and(|a| opener::is_opener_name(a)) {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute());
        std::process::exit(opener::client(&args[1..], runtime.as_deref()));
    }
    // --abi-version: for `panpak install-runtime` (works on a bare stub too).
    if args.get(1).map(|s| s.as_str()) == Some("--abi-version") {
        println!("{}", panpak_core::RUNTIME_ABI);
        return Ok(());
    }
    // The receiver of the open channel (see receiver.rs): started by the outer stub with its two sockets.
    if args.get(1).map(|s| s.as_str()) == Some(receiver::FLAG) {
        return receiver::main_receiver();
    }
    // The router of the private bus of an app (see busrouter.rs): started by the stub of the first copy with the control listener.
    if args.get(1).map(|s| s.as_str()) == Some(busrouter::FLAG) {
        return busrouter::main_router(&args[2..]);
    }
    // Inside the isolated view: the outer stub already verified the package (see view.rs).
    if args.get(1).map(|s| s.as_str()) == Some("--panpak-inner") {
        let fd: i32 = args
            .get(2)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| coded(ErrorCode::E5002, "usage: panpak-stub --panpak-inner <fd>"))?;
        return run_inner(fd);
    }
    // `panpak-stub prepare ...`: what `panpak install` and `panpak update` run so that the app is ready when they end (see `cmd_prepare`).
    if args.get(1).map(|s| s.as_str()) == Some("prepare") {
        return cmd_prepare(&args[2..]);
    }
    // Two modes: self-contained package (current_exe) or installed runtime
    // (`stub run <package> [args...]`, used by `panpak install` launchers).
    let installed_mode = args.get(1).map(|s| s.as_str()) == Some("run");
    let (pkg, app_args): (PathBuf, Vec<String>) = if installed_mode {
        let p = args.get(2).ok_or_else(|| coded(ErrorCode::E5002, "usage: panpak-stub run <package> [args]"))?.into();
        (p, args.into_iter().skip(3).collect())
    } else {
        (std::env::current_exe()?, args.into_iter().skip(1).collect())
    };
    // Before anything of the app is read from the store: the shared lock of the copies. `panpak remove` holds the
    // exclusive one while it takes the app away, so a start in that moment waits and then finds the app gone.
    if installed_mode {
        protected_pre(&pkg);
        apply_staged_update(&pkg);
    }
    if installed_mode {
        background_work(&pkg);
    }
    let early = if installed_mode { start_lock(&pkg)? } else { None };
    // `--panpak-licenses` (a stub flag, like --panpak-open): print the
    // embedded license texts of the verified package and exit.
    if app_args.iter().any(|a| a == "--panpak-licenses") {
        let mut f = File::open(&pkg)?;
        let (ft, is_record) = panpak_core::record::read_any_footer(&mut f)?;
        let (_man, raw_index) = verify(&mut f, &ft)?;
        let files = if is_record {
            // A record holds no chunks: the licenses are files of the tree.
            let store = dirs::data_dir().context("no data dir")?.join("panpak");
            panpak_core::record::ensure_in_store(&pkg, &store)?;
            read_tree_licenses(&store.join("trees").join(hex(&ft.root)), &raw_index)?
        } else {
            panpak_core::read_package_licenses(&mut f, &raw_index)?
        };
        ensure!(!files.is_empty(), "package has no embedded licenses");
        for (name, data) in files {
            println!("===== {name} =====");
            println!("{}", String::from_utf8_lossy(&data));
        }
        return Ok(());
    }
    // Stub flags (never the app's), removed before exec:
    // --network-unfiltered, --panpak-open <path> [--panpak-open-readonly|--panpak-open-rename].
    let (allow_unfiltered, open_file, app_args) = split_stub_flags(app_args)?;
    // The desktop's dark preference is asked of `gsettings` now and read at the end of the preparation: the
    // two programs run while this one verifies the package and builds the view. Before Landlock: `gsettings` and
    // dconf are outside the sandbox.
    let theme_query = if std::env::var_os("GTK_THEME").is_none() { find_gsettings().map(|g| InterfaceQuery::start(&g, GSETTINGS_TIMEOUT)) } else { None };
    stage("started");
    let mut f = File::open(&pkg)?;
    // A package, or the record of an installed version (no stub, no chunks: the tree is the only copy of the files).
    let (ft, is_record) = panpak_core::record::read_any_footer(&mut f)?;
    ensure_code!(!is_record || installed_mode, ErrorCode::E0010, "{}", panpak_core::record::NOT_A_PACKAGE);
    let (mut man, raw_index) = verify(&mut f, &ft)?;
    panpak_core::check_runtime(&man, panpak_core::RUNTIME_ABI)?;
    // In protected mode: no network, no written programs, no addresses (see `protected_start`).
    let protected = if installed_mode { protected_start(&mut man, &pkg) } else { None };
    {
        use panpak_core::{i18n::{fill, tr}, messages as m};
        let name: String = man.app.name.chars().filter(|c| !c.is_control()).collect();
        let notice = (protected == Some(false)).then(|| opener::Notice {
            title: fill(&tr(m::PROTECTED_TITLE), &[("name", name.clone())]),
            text: fill(&tr(m::LINKS_BLOCKED), &[("name", name)]),
            graphical: panpak_core::release::is_graphical(&man),
        });
        opener::forbid_addresses(protected == Some(false), notice);
    }
    // The filtered session bus takes 15 to 25 ms to come up (a systemd scope, the proxy): its checks are made now and
    // the rest runs on a thread while the package is verified, the view is made and the app is prepared.
    let view_usable = panpak_core::view_status().usable();
    // Filled in below, when the folders the app can write are known; the file dialog reads it (links in those folders are not trusted).
    let writable_roots: std::sync::Arc<std::sync::OnceLock<Vec<PathBuf>>> = Default::default();
    let dbus_early = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && view_usable)
        .map(|rt| dbus_layer::start(&dbus_layer::Request { man: &man, runtime_dir: &rt, writable_roots: writable_roots.clone() }));

    // The D-Bus layer is the one that mounts the document folder of the app, so the files given at the start are shared only when it is there.
    let layer_ok = matches!(dbus_early, Some(Ok(_)));

    let store = dirs::data_dir().context("no data dir")?.join("panpak");
    if is_record {
        panpak_core::record::ensure_in_store(&pkg, &store)?;
        panpak_core::record::check_identity(&pkg, &man.app.id, &man.app.version, &hex(&ft.root))?;
    }
    // TOFU (spec §3.2): if the app is installed (pinned key), the signature
    // must match the pin; on direct run show the fingerprint.
    let key = man.trust.as_ref().and_then(|t| t.root.first()).context("manifest without [trust].root")?;
    let fp = panpak_core::fingerprint(&panpak_core::parse_pubkey(key)?);
    let app_dir = store.join("apps").join(&man.app.id);
    if installed_mode {
        let pinned = panpak_core::trust::read(&app_dir).context("app not installed (no trust record): use `panpak install`")?;
        // One key, or two for the moment in which an update replaces the key (`panpak apply`): either is the pin.
        ensure_code!(
            pinned.roots.iter().any(|l| l.trim() == key.trim()),
            ErrorCode::E1004,
            "key differs from the pinned key: unauthorized update, reinstall"
        );
        // `[host]`: the display and the Landlock ABI can change from one session to the next, so they are looked at again (nothing else is: it was looked at when
        // the app was installed). An app installed with --force-host is not refused for them.
        start_host_check(&man, pinned.forced_host)?;
    } else {
        start_host_check(&man, false)?;
        eprintln!("panpak: {} {} — key fingerprint: {fp}", man.app.id, man.app.version);
    }
    stage("package checked");
    if installed_mode {
        refresh_menu_entry(&store, &man);
        stage("menu entry checked");
    }
    let (chunks, entries) = parse_index(&raw_index)?;
    // The stub being run (ourselves in direct-mode, never the one in the file in
    // run-mode) must match the signed one: no other stub runs.
    stage("index parsed");
    let tree = if is_record {
        // The stub that runs is the installed runtime, and the stub of the package was checked when it was installed: a record has none.
        existing_tree(&store, &ft.root)?
    } else {
        let stub_len = chunks.iter().map(|c| c.off).min().unwrap_or(ft.index_off);
        panpak_core::verify_stub_range(&mut f, stub_len, &man)?;
        stage("stub range verified");
        materialize(&mut f, &store, &ft.root, &chunks, &entries)?
    };
    stage("tree ready");

    // On cache hit, no bundle inventory or transitive parsing when warm.
    // Signature/pin/stub/index verified above; Landlock applied below.
    let plan = resolution::resolve(&store, &pkg, &hex(&ft.root), &tree, &man)?;
    let ld_path =
        std::env::join_paths(plan.ld_path.iter()).context("non-UTF-8 LD_LIBRARY_PATH")?;
    // The value of `LD_LIBRARY_PATH` of the app: relative to the program that is run, so that a program of the host that
    // the app starts does not use it (PANPAK-FORMAT.md section 7.1). There is no absolute list: a package with a program and
    // without the links is refused, and one with no program has no value to give.
    let ld_env = library_path_env_or_refuse(&store, &tree, &ft.root, &man, &plan)?;

    stage("libraries resolved");
    // Private folders (0700), created before the sandbox closes the rest of the disk.
    let host_dirs = panpak_core::app_dirs(&man.app.id)?;
    host_dirs.create()?;
    // With the view the private home is mounted at the real home and the app sees its folders there; the environment of
    // the user is mounted into it. Without the view (or without a home folder to mount at) the old rules apply: the
    // environment is read from the real folders, found through the search paths and a generated `fonts.conf`.
    // The socket probe (a measurement) runs the app without the view, so it gets the folders as the host has them.
    let probing = std::env::var_os("PANPAK_SOCKET_PROBE").is_some_and(|v| !v.is_empty());
    let view_home = if view_usable && !probing { view_home() } else { None };
    let layered = view_home.is_some();
    if !layered {
        if let Err(e) = write_fonts_conf(&host_dirs) {
            eprintln!("panpak: the fonts.conf of the app was not written: {}", panpak_core::err_text(&e));
        }
    }
    // The folders as the app sees them.
    let app_dirs = match &view_home {
        Some(h) => host_dirs.seen_at(h),
        None => host_dirs.clone(),
    };
    let mut access = access_for(&man, &host_dirs, layered)?;
    let granted = xdg_grants(&man, &store, installed_mode, &mut access)?;
    let _ = writable_roots.set(access.rw.clone());
    if !granted.is_empty() || man.permissions.as_ref().and_then(|p| p.xdg_dirs.as_ref()).is_some_and(|d| !d.is_empty()) {
        if let Err(e) = write_user_dirs(&host_dirs, &granted) {
            eprintln!("panpak: the user-dirs.dirs of the app was not written: {}", panpak_core::err_text(&e));
        }
    }
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute());
    access.sockets = panpak_core::implied_sockets(
        man.permissions.as_ref(),
        runtime_dir.as_deref(),
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
    )
    .into_iter()
    .filter(|p| p.exists())
    .collect();
    let (config_dirs, data_dirs) =
        panpak_core::xdg_search_dirs(std::env::var("XDG_CONFIG_DIRS").ok(), std::env::var("XDG_DATA_DIRS").ok(), !layered)?;
    // Socket probe (measurement only, off unless asked): the log is opened before Landlock closes the disk.
    let probe_log = match std::env::var_os("PANPAK_SOCKET_PROBE").filter(|v| !v.is_empty()) {
        #[cfg(target_arch = "x86_64")]
        Some(path) => Some(probe::open_log(Path::new(&path))?),
        #[cfg(not(target_arch = "x86_64"))]
        Some(_) => bail!("PANPAK_SOCKET_PROBE is only available on x86-64"),
        None => None,
    };
    // The answer of `gsettings` is needed only when the environment of the app is complete. With the view that is after the Wayland
    // context, the D-Bus layer and the shared folders, which take about as long as the query: it is collected there (see
    // `enter_view`), so the wait overlaps their work. Without the view it is collected here.
    let (dark_theme, theme_query) = if view_usable { (None, theme_query) } else { (gtk_dark_theme(theme_query), None) };
    if !view_usable {
        stage("theme answered");
    }
    let launch = man.launch.as_ref();
    let launch_args = launch
        .and_then(|l| l.args.as_deref())
        .unwrap_or_default()
        .iter()
        .map(|a| app_dirs.expand(a))
        .collect::<Result<Vec<_>>>()?;
    let launch_env = launch
        .and_then(|l| l.env.as_ref())
        .into_iter()
        .flatten()
        .map(|(k, v)| Ok((k.clone(), app_dirs.expand(v)?)))
        .collect::<Result<Vec<_>>>()?;

    // The `[env]` table of the manifest: variables that point the app at its own tree (PANPAK-FORMAT.md §3.1.6). Checked again here.
    let pkg_env = match &man.env {
        Some(env) => panpak_core::pkgenv::expand(env, &tree)?,
        None => Vec::new(),
    };
    let data_dirs = match pkg_env.iter().find(|(k, _, before)| *before && k == "XDG_DATA_DIRS") {
        Some((_, v, _)) => format!("{v}:{data_dirs}"),
        None => data_dirs,
    };
    let mut cmd = match &plan.loader {
        // Included glibc: all of user-space uses it, loader included.
        // BUT only if the entry is an ELF: a script passed to ld.so would die
        // with "invalid ELF header" (found with the GTK test: entry was a wrapper).
        Some(loader) if is_elf(&tree.join(&man.app.entry))? => {
            let mut c = Command::new(loader);
            c.arg("--library-path").arg(&ld_path);
            c.arg(tree.join(&man.app.entry));
            c
        }
        _ => Command::new(tree.join(&man.app.entry)),
    };
    // XDG_RUNTIME_DIR stays as it is. In the view HOME is the real path and the app writes in its private home there;
    // without the view the private home is the home of the app.
    if !view_usable {
        cmd.env("HOME", &host_dirs.root);
    }
    cmd.env("XDG_CONFIG_HOME", &app_dirs.config)
        .env("XDG_DATA_HOME", &app_dirs.data)
        .env("XDG_STATE_HOME", &app_dirs.state)
        .env("XDG_CACHE_HOME", &app_dirs.cache)
        .env("TMPDIR", &app_dirs.tmp)
        .env("SQLITE_TMPDIR", &app_dirs.tmp)
        .env("XDG_CONFIG_DIRS", config_dirs)
        .env("XDG_DATA_DIRS", data_dirs)
        .envs(pkg_env.iter().filter(|(k, _, before)| !*before && k != "XDG_DATA_DIRS").map(|(k, v, _)| (k, v)))
        .envs(launch_env);
    hide_local_sockets(
        &mut cmd,
        panpak_core::allows_session_bus(man.permissions.as_ref()),
        panpak_core::allows_system_bus(man.permissions.as_ref()),
    );
    if let Some(theme) = dark_theme {
        cmd.env("GTK_THEME", theme);
    }
    let app_args = if layer_ok && installed_mode { share_start_files(app_args, &man.app.id, runtime_dir.as_deref(), access.rw.as_slice(), open_file.as_ref().is_none_or(|(_, g)| g.write)) } else { app_args };
    // With the document portal served, the file the launcher names with `--panpak-open` is shared as a document with the other arguments (just above), so the older rule
    // of Landlock on the path of the host is not added: it would be a second, read-only grant to the same file, and a line on standard error that says so.
    let served = layer_ok && installed_mode && documents_served(runtime_dir.as_deref());
    if let Some(note) = served.then(|| open_flags_note(open_file.as_ref())).flatten() {
        eprintln!("panpak: {note}");
    }
    let open_file = if served { None } else { open_file };
    cmd.args(&launch_args)
        .args(&app_args)
        .env("PANPAK_APP_ID", &man.app.id)
        .env_remove("PANPAK_SOCKET_PROBE");
    match &ld_env {
        Some(v) => cmd.env("LD_LIBRARY_PATH", v),
        None => cmd.env_remove("LD_LIBRARY_PATH"),
    };
    let open_arg = open_file.as_ref().map(|(p, w)| (p.as_path(), *w));
    #[cfg(target_arch = "x86_64")]
    if let Some(log) = probe_log {
        // The probe measures the sandbox as it is without the view. The app runs as a child and
        // this process stays as the supervisor.
        sandbox(&tree, &man, &access, allow_unfiltered, open_arg)?;
        std::process::exit(probe::run(cmd, log)?);
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = probe_log;
    // Outer stub -> bubblewrap -> inner stub (Landlock) -> app. Without a usable bubblewrap the
    // stub applies Landlock itself and starts the app, as it always did.
    if view_usable {
        // Only with the view: its home folder is an empty tmpfs, so listing it shows nothing else.
        // Only a convenience (the home folder shows the granted folders when listed): without it the listing is empty.
        access.home_list = view_home.clone();
        let mut store_paths = plan.ld_path.clone();
        store_paths.extend(plan.loader.clone());
        store_paths.push(PathBuf::from(cmd.get_program()));
        let ctx = ViewStart { tree: &tree, man: &man, access: &access, allow_unfiltered, open: open_file.as_ref(), runtime_dir: runtime_dir.as_deref(), early_lock: early.as_ref(), exit_job: exit_job(&pkg), watch: installed_panpak(&pkg), store_paths, dbus: std::cell::Cell::new(dbus_early), theme: std::cell::Cell::new(theme_query) };
        stage("entering the view");
        enter_view(&ctx, &mut cmd)?;
    }
    sandbox(&tree, &man, &access, allow_unfiltered, open_arg)?;
    // Without the view this process becomes the app: the lock must go on to hold while the app runs.
    if let Some(f) = &early {
        // SAFETY: fcntl on a descriptor owned by `f`; the descriptor stays open across the exec.
        unsafe {
            let fl = libc::fcntl(f.as_raw_fd(), libc::F_GETFD);
            if fl >= 0 {
                libc::fcntl(f.as_raw_fd(), libc::F_SETFD, fl & !libc::FD_CLOEXEC);
            }
        }
    }
    // Without the view there is no network namespace to take the network away with, and Landlock only covers TCP: an app in protected mode (internet not
    // turned back on) gets no IP socket at all, so no datagram, no name lookup and no connection leaves it (its loopback goes too).
    exec_filtered(&mut cmd, &man, protected != Some(false))
}

/// The last step before the app: the seccomp filter, then the exec. Nothing may come between the two, and every
/// path that starts an app goes through here, so the filter (and `no_new_privs`) is in place for the app and
/// for everything it runs. Does not return on success.
fn exec_filtered(cmd: &mut Command, man: &panpak_core::Manifest, inet: bool) -> Result<()> {
    panpak_core::seccomp::install_options(man.permissions.as_ref().is_some_and(|p| p.devices.as_ref().is_some_and(|d| !d.is_empty())), inet)?;
    stage("seccomp installed, executing the app");
    let err = cmd.exec(); // Replaces the process.
    bail!("exec failed: {err}")
}

/// What `enter_view` needs from `real_main`.
struct ViewStart<'a> {
    tree: &'a Path,
    man: &'a panpak_core::Manifest,
    access: &'a Access,
    allow_unfiltered: bool,
    open: Option<&'a (PathBuf, OpenGrant)>,
    runtime_dir: Option<&'a Path>,
    /// The shared lock taken at the start of an installed app (see `start_lock`).
    early_lock: Option<&'a File>,
    /// The work to do when the last copy of the app ends: the `panpak` of the store and the app id (None for a package that is not installed, or when `PANPAK_NO_BACKGROUND` is set).
    exit_job: Option<(PathBuf, String)>,
    /// The `panpak` of the store and the id of an installed app: its starts are counted.
    watch: Option<(PathBuf, String)>,
    /// What the inner stub and the app read from the store: library folders and the loader.
    store_paths: Vec<PathBuf>,
    /// The D-Bus layer, being built on another thread since the package was verified (see `dbus_layer`).
    dbus: std::cell::Cell<Option<Result<dbus_layer::Layer>>>,
    /// The `gsettings` query for the dark-theme hint, collected as late as the job allows.
    theme: std::cell::Cell<Option<InterfaceQuery>>,
}

/// Start the app in the isolated view: build the job for the inner stub and replace this
/// process with `bwrap`. Returns only on error.
/// The relative `LD_LIBRARY_PATH` of an app whose package has the links of `panpak_core::libpath` (the one in the root of the
/// tree is the mark), or `None` for a package without them (one with no program: `pack` makes the links for every folder with a program). Before it is used the farm link of the tree is checked: with a
/// farm that cannot be reached the loader would skip the entry and the app would start with other libraries than the ones
/// chosen, with no message.
fn library_path_env(store: &Path, tree: &Path, root: &[u8; 32], man: &panpak_core::Manifest, plan: &panpak_core::abi::Plan) -> Result<Option<std::ffi::OsString>> {
    use panpak_core::libpath;
    let mark = tree.join(libpath::link_name(&man.app.id));
    if !fs::symlink_metadata(&mark).is_ok_and(|m| m.file_type().is_symlink()) {
        return Ok(None);
    }
    let farm = store.join("farms").join(hex(root));
    check_farm(tree, &farm, &hex(root))?;
    Ok(Some(libpath::env_value(&man.app.id, tree, &farm, &plan.ld_path)?))
}

/// `library_path_env`, and the refusal of a program without the links: with no mark and an entry that is an ELF program there is no
/// library path to give (the absolute list of earlier versions is gone), so the package is refused and told to be made again.
fn library_path_env_or_refuse(store: &Path, tree: &Path, root: &[u8; 32], man: &panpak_core::Manifest, plan: &panpak_core::abi::Plan) -> Result<Option<std::ffi::OsString>> {
    let v = library_path_env(store, tree, root, man, plan)?;
    if v.is_none() && is_elf(&tree.join(&man.app.entry))? {
        return Err(panpak_core::libpath::no_links_error());
    }
    Ok(v)
}

/// The farm of a tree is `<store>/farms/<root>`, reached through `<tree>/.farm`. Anything else (the link gone or changed, the
/// farm folder gone or a link) stops the start with a message that says what to do.
fn check_farm(tree: &Path, farm: &Path, root_hex: &str) -> Result<()> {
    let link = tree.join(panpak_core::libpath::FARM_LINK);
    let want = panpak_core::libpath::farm_target(root_hex);
    let have = fs::read_link(&link).ok().map(|t| t.to_string_lossy().into_owned());
    let farm_ok = fs::symlink_metadata(farm).is_ok_and(|m| m.is_dir());
    ensure_code!(
        have.as_deref() == Some(want.as_str()) && farm_ok,
        ErrorCode::E2006,
        "the library folder of the app cannot be reached: {} should point to {want} ({}), and {} should be a folder ({}); the app would start with other libraries than the ones chosen. Remove the folder {} (it is made again at the next start) and start the app again",
        link.display(),
        match &have { Some(t) => format!("it points to {t}"), None => "it is missing".to_string() },
        farm.display(),
        if farm_ok { "it is" } else { "it is not" },
        tree.display()
    );
    Ok(())
}

fn enter_view(ctx: &ViewStart, cmd: &mut Command) -> Result<()> {
    let perms = ctx.man.permissions.as_ref();
    let x11 = perms.is_some_and(|p| p.x11 == Some(true));
    // `/tmp` of the view is a small tmpfs (memory, with a cap). TMPDIR is the folder for large temporary files, on disk, in the cache of the
    // app (`~/.cache/tmp` in the view): `real_main` set it from the folders as the view shows them.
    // Sockets: the ones the manifest lists and the ones every app gets. Only existing ones.
    let mut sockets = Vec::new();
    for s in perms.and_then(|p| p.sockets_allow.as_ref()).into_iter().flatten() {
        let path = panpak_core::expand_socket_path(s)?;
        if path.exists() {
            sockets.push(path);
        }
    }
    // The Wayland socket of the app: a socket of its own that the compositor knows as sandboxed, when it can.
    let wayland = wayland_context(ctx);
    stage("Wayland context");
    sockets.extend(ctx.access.sockets.iter().filter(|s| wayland.as_ref().is_none_or(|w| **s != w.display)).cloned());
    let mut ro_run_dirs = Vec::new();
    for d in [Some(PathBuf::from("/run/systemd/resolve")), panpak_core::resolv_conf_run_dir()].into_iter().flatten() {
        if d.is_dir() {
            ro_run_dirs.push(d);
        }
    }
    let runtime_text = ctx.runtime_dir.and_then(Path::to_str).filter(|t| t.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))).map(str::to_string);
    let display = std::env::var("DISPLAY").ok();
    let x11_sockets: Vec<PathBuf> = if x11 { view::x11_socket(display.as_deref()).into_iter().collect() } else { Vec::new() };
    // The home folder is an empty tmpfs in the view. Put back only what the app and the inner stub
    // need: the tree, the library folders and the loader of the store, the stub, the private
    // folders and `rw` home paths, the settings Landlock allows to read, the files of `panpak open`
    // and, with X11, the Xauthority file. Paths outside the home folder are mounted too, unless they are in the system folders.
    let home = view_home();
    // The private home of the app (its folders on the host), mounted at the real home. Without a home folder to mount
    // at, or without folders of the app, the view is the old one.
    let app_dirs = match panpak_core::app_dirs(&ctx.man.app.id) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("panpak: WARNING: app folders unavailable ({}); no shared /tmp, no stale lock search", panpak_core::err_text(&e));
            None
        }
    };
    let private = home.as_ref().zip(app_dirs.as_ref()).map(|(h, d)| (h.clone(), d.clone()));
    let in_home = |p: &Path| home.as_ref().is_some_and(|h| p.starts_with(h));
    let real = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let stub = std::env::current_exe().context("path of the stub")?;
    let mut ro_wanted: Vec<PathBuf> = vec![ctx.tree.to_path_buf(), stub.clone()];
    ro_wanted.extend(ctx.store_paths.iter().cloned());
    ro_wanted.extend(ctx.access.ro.iter().cloned());
    let mut xauthority = None;
    if x11 {
        xauthority = std::env::var_os("XAUTHORITY")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".Xauthority")))
            .filter(|p| p.is_file());
        ro_wanted.extend(xauthority.clone());
    }
    // The two roots of the app are the home and its cache in the view (`PrivateHome`), not binds at their own paths. A
    // folder that holds the home is never mounted: it would show the real home over the private one.
    let is_root = |p: &Path| private.as_ref().is_some_and(|(_, d)| d.roots().contains(&p));
    let covers_home = |p: &Path| private.as_ref().is_some_and(|(h, _)| h.starts_with(p));
    let mut binds: Vec<view::Bind> = ctx.access.rw.iter().filter(|p| !is_root(p)).map(|p| view::Bind { path: real(p), rw: true }).filter(|b| !covers_home(&b.path)).collect();
    if let Some((path, grant)) = ctx.open {
        // The file is writable in the view only if the grant lets the app write it.
        if grant.rename {
            binds.extend(view::existing_parent(path).map(|p| view::Bind { path: real(&p), rw: true }).filter(|b| !covers_home(&b.path)));
        } else if grant.write {
            binds.push(view::Bind { path: real(path), rw: true });
        } else {
            ro_wanted.push(path.clone());
        }
    }
    for p in ro_wanted {
        let p = real(&p);
        // Read-only paths in the system folders of the view are visible as they are; any other is mounted.
        if p.exists() && (in_home(&p) || ctx.runtime_dir.is_some_and(|rt| p.starts_with(rt)) || !view::in_system_view(&p)) {
            binds.push(view::Bind { path: p, rw: false });
        }
    }
    let binds = view::merge_binds(binds);
    let (mut mounts, opener_server) = open_channel(ctx, cmd, &stub, runtime_text.as_deref());
    // The filtered session bus, when this system can give it. Before the sockets and the job are
    // fixed: it adds mounts, variables and one path Landlock must allow.
    let dbus = ctx.dbus.take().and_then(|r| match r {
        Ok(l) => Some(l),
        Err(e) => {
            if e.downcast_ref::<dbus_layer::Quiet>().is_none() {
                eprintln!("panpak: WARNING: no D-Bus for the app (portals, notifications): {}", panpak_core::err_text(&e));
            }
            None
        }
    });
    stage("D-Bus layer");
    let mut symlinks = Vec::new();
    if let Some(w) = &wayland {
        mounts.push(view::Mount { src: w.listener.path.clone(), dest: w.display.clone(), rw: true });
    }
    if let Some(l) = &dbus {
        mounts.extend(l.mounts.iter().cloned());
        symlinks.extend(l.symlinks.iter().cloned());
        for (k, v) in &l.env {
            cmd.env(k, v);
        }
    }
    // /tmp and /dev/shm are shared by the copies of this app that run together, and by no other app.
    let shared = ctx.runtime_dir.zip(app_dirs.as_ref()).and_then(|(rt, dirs)| match sharedtmp::prepare(&rt.join("panpak").join(&ctx.man.app.id), &dirs.tmp, &sharedtmp::system_shm(&ctx.man.app.id).unwrap_or_else(|| rt.join("panpak").join(&ctx.man.app.id).join("shm")), &panpak_core::stalelocks::in_use_marker(dirs), ctx.early_lock.as_ref().and_then(|f| f.try_clone().ok())) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("panpak: shared /tmp off, using a private one: {}", panpak_core::err_text(&e));
            None
        }
    });
    // No other copy is alive and the last run did not end normally (it was killed, or the machine went
    // down): lock files it left hold process ids of a view that no longer exists, and the new view starts
    // counting again. After a normal end there is nothing to look for.
    if let (Some(_), Some(dirs)) = (shared.as_ref().filter(|s| s.stale), app_dirs.as_ref()) {
        let roots = panpak_core::stalelocks::roots_of(dirs);
        for gone in panpak_core::stalelocks::remove(&roots.iter().map(PathBuf::as_path).collect::<Vec<_>>()) {
            eprintln!("panpak: removed a stale lock file: {}", gone.display());
        }
    }
    // The first copy that starts on another tree than the one the cache was made on (an update, a repair, a rollback) empties the cache: a file of the new tree may have an
    // older time than the cache that was made from the old one, and some caches (ksycoca, the icon caches) only look for files that are newer (PANPAK-FORMAT.md section 6.9).
    if let (Some(_), Some(dirs)) = (shared.as_ref().filter(|s| s.alone), app_dirs.as_ref()) {
        if let Some(tree) = ctx.tree.file_name().and_then(|n| n.to_str()) {
            if let Err(e) = panpak_core::cachewipe::at_start(&dirs.cache, tree) {
                eprintln!("panpak: the cache of the app was not emptied: {}", panpak_core::err_text(&e));
            }
        }
    }
    // The private folders searched for stale locks when the last copy ends.
    let end_roots: Vec<PathBuf> = app_dirs.as_ref().map(|d| panpak_core::stalelocks::roots_of(d).to_vec()).unwrap_or_default();
    // The permission exec_written, and the start: an app that ends with an error after it wrote a program file gets a line in the log.
    let exec_written = ctx.man.permissions.as_ref().and_then(|p| p.exec_written).unwrap_or(false);
    let (app_name, started) = (ctx.man.app.name.clone(), std::time::SystemTime::now());
    stage("shared folders");
    let plan = view::ViewPlan {
        root: view::root_layout(Path::new("/")),
        tmp_size: view::TMP_SIZE,
        watch_tmp: Some(ctx.man.app.name.clone()),
        var_tmp: app_dirs.as_ref().map(|d| d.tmp.clone()),
        shared_shm: shared.as_ref().map(|s| s.shm.clone()),
        mounts,
        symlinks,
        runtime_dir: ctx.runtime_dir.map(Path::to_path_buf),
        sockets: view::unique(sockets),
        ro_run_dirs: view::unique(ro_run_dirs),
        x11: x11_sockets,
        home,
        private_home: private.as_ref().map(|(_, d)| view::PrivateHome { data: d.root.clone(), cache: d.cache.clone() }),
        layers: layers_of(&ctx.access.env),
        binds,
        x11_access: x11,
        no_network: !panpak_core::network_requested(ctx.man.permissions.as_ref()),
        stub,
        fd: -1,
    };
    // Landlock inside the view also lets the app write in its runtime folder (a tmpfs of its own). The two roots of the
    // app are the home (with the cache in it) there.
    let mut job_rw: Vec<PathBuf> = ctx.access.rw.iter().filter(|p| !is_root(p)).cloned().collect();
    job_rw.extend(private.as_ref().map(|(h, _)| h.clone()));
    // TMPDIR: the folder on disk is mounted at /var/tmp (not in `access`: a grant there would bind the host's /var/tmp), and a rule is by path.
    job_rw.extend(private.as_ref().map(|_| PathBuf::from(panpak_core::VIEW_TMP)));
    job_rw.extend(ctx.runtime_dir.map(Path::to_path_buf));
    // With X11 access the app reads the Xauthority file, which is put back into the view above.
    let mut job_ro = ctx.access.ro.clone();
    job_ro.extend(xauthority);
    if let Some(q) = ctx.theme.take() {
        if let Some(theme) = gtk_dark_theme(Some(q)) {
            cmd.env("GTK_THEME", theme);
        }
        stage("theme answered");
    }
    let parts = view::Job::command_of(cmd)?;
    let job = view::Job {
        version: view::JOB_VERSION,
        tree: ctx.tree.to_path_buf(),
        manifest: ctx.man.clone(),
        rw: job_rw,
        ro: job_ro,
        sockets: ctx.access.sockets.iter().cloned().chain(dbus.iter().flat_map(|l| l.landlock_sockets.iter().cloned())).collect(),
        allow_unfiltered: ctx.allow_unfiltered,
        home_list: ctx.access.home_list.clone(),
        exec: ctx.runtime_dir.map(|rt| vec![rt.join(opener::BIN_DIR)]).unwrap_or_default(),
        open: ctx.open.map(|(p, g)| view::JobOpen { path: p.clone(), write: g.write, rename: g.rename }),
        program: parts.program,
        args: parts.args,
        env_set: parts.env_set,
        env_remove: parts.env_remove,
        dbus_wait: dbus.as_ref().map(|l| l.wait.clone()),
    };
    stage("view planned");
    // Bubblewrap, the app and the proxy MUST start in the scope of the app, so this is where the start waits for systemd (if the
    // desktop did not already start the app in one). If the scope fails there is no bus: the inner stub sees the file that
    // the layer leaves and says so to the app.
    let mut dbus = dbus;
    if let Some(l) = dbus.as_mut() {
        l.wait_scope();
        stage("scope ready");
    }
    let fd = view::job_memfd(&job)?;
    let plan = view::ViewPlan { fd, ..plan };
    let exit = ctx.exit_job.clone();
    let watch = ctx.watch.clone();
    let root_name = ctx.tree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let exit_lock = ctx.early_lock.and_then(|f| f.try_clone().ok());
    // The lock of the shared folders must outlive bwrap, so the outer stub stays whenever there is one, and whenever there is work for the end.
    match (opener_server, shared, dbus, wayland) {
        (None, None, None, None) if exit.is_none() && watch.is_none() => match view::exec_bwrap(&plan)? {},
        (server, shared, dbus, wayland) => match view::run_bwrap(&plan, (server, shared, dbus, wayland), move |(_, shared, _, _), clean, info| {
            if let Some((panpak, id)) = watch.as_ref() {
                note_start_end(panpak, id, &root_name, info);
            }
            if !clean && !exec_written {
                let roots: Vec<&Path> = end_roots.iter().map(PathBuf::as_path).collect();
                if let Some(m) = wxhint::message(&app_name, &wxhint::written_programs(&roots, started)) {
                    eprintln!("panpak: {m}");
                }
            }
            // The last copy to end looks for the lock files it left (see `Shared::ended`). The cost is at exit, not at start.
            let mut last = None;
            if let Some(s) = shared {
                let t0 = std::time::Instant::now();
                let roots: Vec<&Path> = end_roots.iter().map(PathBuf::as_path).collect();
                let (gone, l) = s.ended(clean, &roots);
                for g in &gone {
                    eprintln!("panpak: removed a stale lock file at exit: {}", g.display());
                }
                if panpak_core::debug_enabled() {
                    eprintln!("panpak: debug: exit: {} the last copy, stale lock search {:.2} ms, {} removed", if l { "is" } else { "is not" }, t0.elapsed().as_secs_f64() * 1000.0, gone.len());
                }
                last = Some(l);
            }
            // The lock of the copies tells whether any other copy is alive: it is ours alone when the exclusive lock can be taken.
            if let (Some((panpak, id)), Some(lock)) = (exit.as_ref(), exit_lock.as_ref()) {
                let alone = last.unwrap_or_else(|| flock_try_exclusive(lock));
                if alone {
                    start_exit_work(lock, panpak, id);
                }
            }
        })? {},
    }
}

/// The mounts of the layers of the user's environment in the view. The folder of the user is found by its real path (a link in the way is resolved); the place in the view is the path the user knows. Single
/// files are read-only mounts, and the files that are private to the user are empty in the view.
fn layers_of(env: &panpak_core::EnvLayers) -> view::Layers {
    let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    view::Layers {
        dirs: env.dirs.iter().map(|p| view::Mount::ro(canon(p), p.clone())).collect(),
        files: env.files.iter().map(|p| view::Mount::ro(canon(p), p.clone())).collect(),
        hidden: env.hidden.clone(),
    }
}

/// The security context of the app's Wayland socket (see `panpak_core::wayland`).
struct WaylandContext {
    /// Where the host's socket is, and where the app finds its own in the view.
    display: PathBuf,
    /// Kept to the end: dropping it removes the socket and tells the compositor to stop listening.
    listener: panpak_core::wayland::Listener,
}

/// Register a security context with the compositor and return the socket the app is to use instead of the
/// compositor's own. `None` leaves the normal socket in place: no compositor, no support (GNOME's Mutter has
/// none: `panpak doctor` says so), or a failure, which is reported (the app then has the full socket).
fn wayland_context(ctx: &ViewStart) -> Option<WaylandContext> {
    use panpak_core::wayland::{register, Listener, Meta, Outcome};
    use std::os::fd::AsFd;
    let rt = ctx.runtime_dir?;
    let host = panpak_core::implied_sockets(None, Some(rt), std::env::var("WAYLAND_DISPLAY").ok().as_deref()).into_iter().next()?;
    // The first implied socket is the compositor's only when WAYLAND_DISPLAY is set.
    std::env::var("WAYLAND_DISPLAY").ok().filter(|w| !w.is_empty())?;
    if !fs::symlink_metadata(&host).is_ok_and(|m| std::os::unix::fs::FileTypeExt::is_socket(&m.file_type())) {
        return None;
    }
    let id = &ctx.man.app.id;
    let dir = rt.join("panpak").join(id);
    let made = (|| -> Result<(Listener, Outcome)> {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).with_context(|| format!("create {}", dir.display()))?;
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let instance = format!("{}-{}", std::process::id(), nanos);
        let listener = Listener::create(&dir.join(format!("wayland-{instance}")))?;
        let app_id = format!("panpak.{id}");
        let out = register(&host, listener.socket.as_fd(), listener.close_read.as_fd(), &Meta { app_id: &app_id, instance_id: &instance })?;
        Ok((listener, out))
    })();
    match made {
        Ok((listener, panpak_core::wayland::Outcome::Registered)) => Some(WaylandContext { display: host, listener }),
        Ok((_, panpak_core::wayland::Outcome::Unsupported)) => {
            if panpak_core::debug_enabled() {
                eprintln!("panpak: the compositor has no security context protocol; the app gets the normal Wayland socket");
            }
            None
        }
        Err(e) => {
            eprintln!("panpak: WARNING: no Wayland security context ({}); the app gets the normal socket", panpak_core::err_text(&e));
            None
        }
    }
}

/// Set up the way to open addresses from the view: the socket and the server (outer side), the stub
/// as `xdg-open` and its relatives (links first in `PATH`, and over the system's own `xdg-open`, so
/// that a full path is covered too), `BROWSER`, and a private `mimeapps.list` for GIO. Needs the
/// real `xdg-open` in the `PATH` of this process and a plain runtime folder; otherwise nothing is set
/// up and the view has no `xdg-open` that works. Failures only turn the channel off.
/// The part of `[host]` that the start looks at (PANPAK-FORMAT.md §3.1.11): what the session gives (the display server, the portals, the bus) and the Landlock ABI.
/// Nothing is read when the package names none of them.
fn start_host_check(man: &panpak_core::Manifest, forced: bool) -> Result<()> {
    if man.host.as_ref().is_some_and(|h| h.display.is_some() || h.landlock.is_some() || h.portals.is_some() || h.bus.is_some()) {
        let facts = panpak_core::host::Facts::detect_for_start(man.host.as_ref());
        panpak_core::host::check_start(man, &facts, forced)?;
        for note in panpak_core::host::start_notes(man, &facts) {
            eprintln!("panpak: {note}");
        }
    }
    Ok(())
}

fn open_channel(ctx: &ViewStart, cmd: &mut Command, stub: &Path, runtime: Option<&str>) -> (Vec<view::Mount>, Option<opener::Server>) {
    let none = (Vec::new(), None);
    let (Some(rt), Some(path)) = (runtime, std::env::var_os("PATH")) else { return none };
    let Some(real) = opener::find_in_path("xdg-open", &path) else {
        eprintln!("panpak: no xdg-open found in PATH; the app cannot open addresses");
        return none;
    };
    let dir = Path::new(rt).join("panpak").join(&ctx.man.app.id);
    let server = match opener::Server::start(&dir, opener::SystemXdgOpen::new(real)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("panpak: opener channel off: {}", panpak_core::err_text(&e));
            return none;
        }
    };
    let rtp = Path::new(rt);
    let mut mounts = vec![view::Mount::ro(server.socket().to_path_buf(), rtp.join(opener::SOCKET))];
    for n in opener::NAMES {
        mounts.push(view::Mount::ro(stub.to_path_buf(), rtp.join(opener::BIN_DIR).join(n)));
    }
    // Over every `xdg-open` the system has in its PATH, outside the home folder.
    let home = dirs::home_dir();
    for d in std::env::split_paths(&path).filter(|d| d.is_absolute() && home.as_ref().is_none_or(|h| !d.starts_with(h))) {
        let p = d.join("xdg-open");
        if fs::symlink_metadata(&p).is_ok() {
            mounts.push(view::Mount::ro(stub.to_path_buf(), p));
        }
    }
    let bin = format!("{rt}/{}", opener::BIN_DIR);
    let old = cmd.get_envs().find(|(k, _)| *k == "PATH").and_then(|(_, v)| v.map(|v| v.to_os_string())).unwrap_or(path);
    let mut new = std::ffi::OsString::from(format!("{bin}:"));
    new.push(old);
    cmd.env("PATH", new).env("BROWSER", format!("{bin}/xdg-open"));
    if let Err(e) = write_opener_config(&ctx.man.app.id, &format!("{bin}/xdg-open")) {
        eprintln!("panpak: GIO opener setup failed: {}", panpak_core::err_text(&e));
    }
    (mounts, Some(server))
}

const OPENER_MARKER: &str = "X-Panpak-Generated";

/// The private `mimeapps.list` and `.desktop` file that make GIO (`g_app_info_launch_default_for_uri`,
/// GTK, `gio open`) run the opener of the view for web and mail addresses instead of a browser of the
/// system. A file without the marker is left alone.
fn write_opener_config(app_id: &str, exec: &str) -> Result<()> {
    write_opener_config_in(&panpak_core::app_dirs(app_id)?, exec)
}

fn write_opener_config_in(dirs: &panpak_core::AppDirs, exec: &str) -> Result<()> {
    let ids = ["x-scheme-handler/http", "x-scheme-handler/https", "x-scheme-handler/mailto", "text/html"];
    let mime: String = ids.iter().map(|m| format!("{m};")).collect();
    let list: String = ids.iter().map(|m| format!("{m}=panpak-opener.desktop\n")).collect();
    let files: [(&Path, &[&str], String); 2] = [
        (&dirs.config, &["mimeapps.list"], format!("# {OPENER_MARKER}: written at every start.\n[Default Applications]\n{list}")),
        (
            &dirs.data,
            &["applications", "panpak-opener.desktop"],
            format!("# {OPENER_MARKER}\n[Desktop Entry]\nType=Application\nName=Link opener\nExec={exec} %u\nMimeType={mime}\nNoDisplay=true\nTerminal=false\n"),
        ),
    ];
    for (base, rel, want) in files {
        panpak_core::safefs::write_generated(base, rel, OPENER_MARKER, &want, 0o644).with_context(|| format!("write {}", base.join(rel.join("/")).display()))?;
    }
    Ok(())
}

/// The inner stub: Landlock, then the app. The job comes from the outer stub, which verified the package.
fn run_inner(fd: i32) -> Result<()> {
    let _ = STARTED.set(std::time::Instant::now());
    let mut job = view::read_job(fd)?;
    stage("inner: job read");
    // Before Landlock, which needs the socket of the proxy to be there to allow it.
    if let Some(w) = job.dbus_wait.clone() {
        w.wait(&mut job.env_set, &mut job.env_remove);
        stage("inner: bus layer ready");
    }
    let access = Access { rw: job.rw.clone(), ro: job.ro.clone(), sockets: job.sockets.clone(), in_view: true, home_list: job.home_list.clone(), exec: job.exec.clone(), ..Access::default() };
    let open = job.open.as_ref().map(|o| (o.path.as_path(), OpenGrant { write: o.write, rename: o.rename }));
    sandbox(&job.tree, &job.manifest, &access, job.allow_unfiltered, open)?;
    stage("inner: Landlock applied");
    exec_filtered(&mut job.to_command(), &job.manifest, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    use panpak_core::{encode_footer, encode_index, Footer, FOOTER_LEN, MAGIC};

    fn test_keypair() -> (ed25519_dalek::SigningKey, String) {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let vk = sk.verifying_key();
        (sk, format!("ed25519:{}", hex(vk.as_bytes())))
    }

    /// TOML of test panpaks, with [runtime].stub_hash matching the fake stub.
    fn fake_toml(key: &str, manifest_extra: &str) -> Vec<u8> {
        let stub_hash = hex(blake3::hash(b"FAKE-STUB").as_bytes());
        let rec = "5a".repeat(32);
        // The `[format]` table is made as `pack` makes it: the list of critical fields comes from what the manifest uses.
        let plain = format!(
            "[app]\nid = \"org.example.hello\"\nname = \"Hello\"\nversion = \"0.1.0\"\nentry = \"bin/hello\"\n[release]\nrevision = 1\n{manifest_extra}[runtime]\nstub_hash = \"{stub_hash}\"\n[trust]\nroot = [\"{key}\"]\nthreshold = 1\nrecovery = \"blake3:{rec}\"\n"
        );
        let critical = panpak_core::critical_paths(&toml::from_str(&plain).unwrap()).iter().map(|c| format!("{c:?}")).collect::<Vec<_>>().join(", ");
        format!(
            "[app]\nid = \"org.example.hello\"\nname = \"Hello\"\nversion = \"0.1.0\"\nentry = \"bin/hello\"\n[release]\nrevision = 1\n{manifest_extra}[format]\nmin_panpak = 1\ncritical = [{critical}]\ncompression = \"zstd\"\nchunking = \"fastcdc:16384:65536:262144\"\npacked_by = \"test\"\n[runtime]\nstub_hash = \"{stub_hash}\"\n[trust]\nroot = [\"{key}\"]\nthreshold = 1\nrecovery = \"blake3:{rec}\"\n"
        )
        .into_bytes()
    }

    /// Input for `sign_and_assemble` (struct instead of 8 positional params).
    struct AssembleIn<'a> {
        dir: &'a Path,
        name: &'a str,
        sk: &'a ed25519_dalek::SigningKey,
        key: &'a str,
        chunks: &'a [Chunk],
        chunk_blobs: &'a [Vec<u8>],
        entries: &'a [Entry],
        manifest_extra: &'a str,
    }

    fn sign_and_assemble(p: AssembleIn) -> PathBuf {
        let raw_index = encode_index(p.chunks, p.entries);
        let root = *blake3::hash(&raw_index).as_bytes();
        let toml = fake_toml(p.key, p.manifest_extra);
        let sig = p.sk.sign(&panpak_core::package_message(1, 0, &root, &toml)).to_bytes();

        let comp_index = zstd::encode_all(&raw_index[..], 0).unwrap();
        let stub = b"FAKE-STUB";
        let index_off = stub.len() as u64 + p.chunk_blobs.iter().map(|b| b.len()).sum::<usize>() as u64;
        let man_off = index_off + comp_index.len() as u64;
        let mut out = Vec::new();
        out.extend_from_slice(stub);
        for b in p.chunk_blobs {
            out.extend_from_slice(b);
        }
        out.extend_from_slice(&comp_index);
        out.extend_from_slice(&toml);
        out.extend_from_slice(&panpak_core::encode_sig_block(&sig));
        let ft = Footer {
            version: 1,
            flags: 0,
            index_off,
            index_len: comp_index.len() as u64,
            man_off,
            man_len: toml.len() as u64,
            root,
        };
        out.extend_from_slice(&encode_footer(&ft));
        assert_eq!(&out[out.len() - 80..out.len() - 72], MAGIC);
        assert_eq!(out.len() as u64 - 80, man_off + toml.len() as u64 + panpak_core::MIN_SIG_BLOCK);

        let path = p.dir.join(p.name);
        fs::write(&path, &out).unwrap();
        path
    }

    /// Minimal valid .panpak file: empty index, no chunks.
    fn fake_panpak(dir: &Path, sk: &ed25519_dalek::SigningKey, key: &str) -> (PathBuf, Vec<u8>, [u8; 32]) {
        let raw_index = encode_index(&[], &[]);
        let root = *blake3::hash(&raw_index).as_bytes();
        let path = sign_and_assemble(AssembleIn { dir, name: "fake.panpak", sk, key, chunks: &[], chunk_blobs: &[], entries: &[], manifest_extra: "" });
        let toml = fake_toml(key, "");
        (path, toml, root)
    }

    /// .panpak file with one real single-chunk file: for materialize tests.
    fn fake_panpak_with_file(dir: &Path, data: &[u8]) -> (PathBuf, [u8; 32], Vec<Chunk>, Vec<Entry>) {
        let (sk, key) = test_keypair();
        let comp = zstd::encode_all(data, 3).unwrap();
        let chunks = vec![Chunk {
            hash: *blake3::hash(data).as_bytes(),
            off: b"FAKE-STUB".len() as u64,
            comp: comp.len() as u32,
            raw: data.len() as u32,
        }];
        let entries = vec![Entry {
            path: PathBuf::from("bin/hello"),
            mode: 0o755,
            kind: Kind::File { chunks: vec![0] }, info: Default::default(),
        }];
        let raw_index = encode_index(&chunks, &entries);
        let root = *blake3::hash(&raw_index).as_bytes();
        let path = sign_and_assemble(AssembleIn { dir, name: "one.panpak", sk: &sk, key: &key, chunks: &chunks, chunk_blobs: &[comp], entries: &entries, manifest_extra: "" });
        (path, root, chunks, entries)
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("panpak-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Fixture directory outside the paths covered by broad rules
    /// (`/tmp`, appdata, tree): needed to prove the `open` rule covers
    /// only the exact file. `/var/tmp` stays default-deny.
    fn outside_tmpdir(tag: &str) -> PathBuf {
        let dir = PathBuf::from("/var/tmp").join(format!("panpak-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Fixture: signed .panpak (manifest_extra for [permissions]...), tree
    /// materialized, appdata. Returns (dir, panpak, tree, appdata).
    fn fixture_tree(tag: &str, manifest_extra: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let dir = tmpdir(tag);
        let (panpak, tree, appdata) = fixture_tree_in(&dir, manifest_extra);
        (dir, panpak, tree, appdata)
    }

    /// Variant on a pre-existing dir (does not delete it: needed to bind
    /// sockets inside store/ before signing the manifest that lists them.
    fn fixture_tree_in(dir: &Path, manifest_extra: &str) -> (PathBuf, PathBuf, PathBuf) {
        let (sk, key) = test_keypair();
        let data = b"do-not-modify";
        let comp = zstd::encode_all(&data[..], 3).unwrap();
        let chunks = vec![Chunk {
            hash: *blake3::hash(&data[..]).as_bytes(),
            off: b"FAKE-STUB".len() as u64,
            comp: comp.len() as u32,
            raw: data.len() as u32,
        }];
        let entries = vec![Entry {
            path: PathBuf::from("bin/hello"),
            mode: 0o755,
            kind: Kind::File { chunks: vec![0] }, info: Default::default(),
        }];
        let panpak = sign_and_assemble(AssembleIn { dir, name: "t.panpak", sk: &sk, key: &key, chunks: &chunks, chunk_blobs: &[comp], entries: &entries, manifest_extra });
        let store = dir.join("store");
        let mut f = File::open(&panpak).unwrap();
        let ft = read_footer(&mut f).unwrap();
        let tree = materialize(&mut f, &store, &ft.root, &chunks, &entries).unwrap();
        let appdata = dir.join("appdata");
        fs::create_dir_all(&appdata).unwrap();
        (panpak, tree, appdata)
    }

    /// Run `body` in a forked child (the parent stays free);
    /// return the lines written to `results` (inside a writable dir).
    fn fork_lines<F>(results: &Path, body: F) -> String
    where
        F: FnOnce(&mut Vec<String>) -> Result<(), &'static str>,
    {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let code: Result<(), &'static str> = (|| {
                let mut res = Vec::new();
                body(&mut res)?;
                fs::write(results, res.concat()).map_err(|_| "results")?;
                Ok(())
            })();
            unsafe { libc::_exit(i32::from(code.is_err()) * 42) };
        }
        let mut status = 0;
        loop {
            let w = unsafe { libc::waitpid(pid, &mut status, 0) };
            if w == pid {
                break;
            }
            assert!(w >= 0, "waitpid failed");
        }
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "child failed (status={status})"
        );
        fs::read_to_string(results).unwrap()
    }

    /// Panpak manifest + sandbox applied in the child. Returns (man).
    fn child_sandboxed(
        res: &mut Vec<String>,
        panpak: &Path,
        tree: &Path,
        appdata: &Path,
        open: Option<(&Path, OpenGrant)>,
    ) -> Result<panpak_core::Manifest, &'static str> {
        let mut f = File::open(panpak).map_err(|_| "open panpak")?;
        let ft = read_footer(&mut f).map_err(|_| "footer")?;
        let (man, _) = verify(&mut f, &ft).map_err(|_| "verify")?;
        let access = Access { rw: vec![appdata.to_path_buf()], ..Access::default() };
        let st = apply_landlock(tree, &access, &man, false, open).map_err(|_| "apply")?;
        if st.ruleset == landlock::RulesetStatus::NotEnforced {
            return Err("notenforced");
        }
        let _ = res;
        Ok(man)
    }

    fn denied(res: &mut Vec<String>, name: &str, failed: bool) {
        res.push(format!("{name}={}\n", if failed { "deny" } else { "ALLOW" }));
    }

    /// Skip explicitly (with reason) if the kernel ABI is too old.
    fn require_abi(min: landlock::ABI, what: &str) -> bool {
        match panpak_core::detect_abi() {
            Ok(a) if a >= min => true,
            other => {
                let why = format!("{what}: detected ABI {other:?}, requires {min:?}");
                // A test that needs more than the kernel of the CI runner has cannot run there: that is named, not hidden.
                if min > crate::testutil::RUNNER_ABI { crate::testutil::skip_known(&why) } else { crate::testutil::skip(&why) }
            }
        }
    }

    #[test]
    fn sandbox_network_denied_by_default_and_allowlist() {
        if !crate::testutil::alone() {
            return;
        }
        use std::net::TcpListener;
        if !require_abi(landlock::ABI::V4, "TCP network") {
            return;
        }
        // Parent listener on loopback: the child tries to connect.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (dir, panpak, tree, appdata) = fixture_tree("net-deny", "");
        unsafe { std::env::set_var("PANPAK_TPORT", port.to_string()) };
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, None)?;
            let p: u16 = std::env::var("PANPAK_TPORT").unwrap().parse().unwrap();
            denied(res, "tcp_default", std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], p)),
                std::time::Duration::from_secs(2),
            ).is_err());
            Ok(())
        });
        assert!(res.contains("tcp_default=deny"), "TCP not filtered:\n{res}");
        // listener stays alive for the allow part (same port in allowlist).

        // With an allowlist on the port: the connect passes, the others do not.
        let extra = format!("[permissions]\nnetwork = true\nnetwork_allow_tcp = [{port}]\n");
        let (dir2, panpak2, tree2, appdata2) = fixture_tree("net-allow", &extra);
        unsafe { std::env::set_var("PANPAK_TPORT2", port.to_string()) };
        let listener2 = TcpListener::bind("127.0.0.1:0").unwrap();
        let other = listener2.local_addr().unwrap().port();
        unsafe { std::env::set_var("PANPAK_TPORT_OTHER", other.to_string()) };
        let res2 = fork_lines(&appdata2.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak2, &tree2, &appdata2, None)?;
            let p: u16 = std::env::var("PANPAK_TPORT2").unwrap().parse().unwrap();
            let o: u16 = std::env::var("PANPAK_TPORT_OTHER").unwrap().parse().unwrap();
            let conn_listed = std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], p)),
                std::time::Duration::from_secs(2),
            );
            let ok_listed = conn_listed.is_ok();
            res.push(format!("tcp_allow={}\n", if ok_listed { "ok" } else { "FAIL" }));
            if !ok_listed {
                res.push(format!("tcp_allow_err={conn_listed:?}\n"));
            }
            denied(res, "tcp_other", std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], o)),
                std::time::Duration::from_secs(2),
            ).is_err());
            Ok(())
        });
        assert!(res2.contains("tcp_allow=ok"), "allowlist does not grant access:\n{res2}");
        assert!(res2.contains("tcp_other=deny"), "unlisted port open:\n{res2}");
        drop(listener);
        drop(listener2);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&dir2);
    }

    /// Nothing the app can write is executed, unless the package says `exec_written`: a program copied into the folders of the
    /// app, into `/tmp` or into the shared `/dev/shm` is refused (EACCES); the same program in the tree and in `/usr` runs.
    #[test]
    fn a_program_the_app_wrote_is_not_executed_unless_exec_written_is_asked_for() {
        if !crate::testutil::alone() {
            return;
        }
        let run = |extra: &str, tag: &str| -> String {
            let (dir, panpak, tree, appdata) = fixture_tree(tag, extra);
            let res = fork_lines(&appdata.join("r.txt"), |res| {
                let _man = child_sandboxed(res, &panpak, &tree, &appdata, None)?;
                let try_run = |res: &mut Vec<String>, name: &str, folder: &Path| {
                    let target = folder.join(format!("wx-{}", std::process::id()));
                    let copied = fs::copy("/usr/bin/true", &target).is_ok();
                    let ran = copied && Command::new(&target).status().is_ok_and(|s| s.success());
                    let _ = fs::remove_file(&target);
                    res.push(format!("{name}={}\n", if !copied { "nocopy" } else if ran { "run" } else { "refused" }));
                };
                try_run(res, "exec_data", &appdata);
                try_run(res, "exec_tmp", Path::new("/tmp"));
                let system = Command::new("/usr/bin/true").status().is_ok_and(|s| s.success());
                res.push(format!("exec_system={}\n", if system { "run" } else { "refused" }));
                Ok(())
            });
            let _ = fs::remove_dir_all(&dir);
            res
        };
        let default = run("", "wx-default");
        assert!(default.contains("exec_data=refused"), "a program written in the folder of the app was executed:\n{default}");
        assert!(default.contains("exec_tmp=refused"), "a program written in /tmp was executed:\n{default}");
        assert!(default.contains("exec_system=run"), "a program of the system no longer runs:\n{default}");
        let asked = run("[permissions]\nexec_written = true\n", "wx-asked");
        assert!(asked.contains("exec_data=run") && asked.contains("exec_tmp=run"), "exec_written does not allow it:\n{asked}");
    }

    /// Protected mode at a start: the manifest in memory loses the network and the programs the app wrote when the installed revision is below the floor
    /// seen for the app, keeps them when the person turned the internet back on (the programs still go), and is left alone in every other case.
    #[test]
    fn a_version_below_the_floor_starts_without_the_network_and_without_written_programs() {
        let root = std::env::temp_dir().join(format!("panpak-protected-start-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let appdir = root.join("apps/org.example.hello");
        fs::create_dir_all(&appdir).unwrap();
        let current = appdir.join("current");
        let manifest = || -> panpak_core::Manifest {
            let text = fake_toml("ed25519:AAAA", "[permissions]\nnetwork = true\nnetwork_allow_tcp = [443]\nexec_written = true\n");
            toml::from_str(std::str::from_utf8(&text).unwrap()).unwrap()
        };
        let net = |m: &panpak_core::Manifest| {
            let p = m.permissions.as_ref().unwrap();
            (p.network, p.network_allow_tcp.clone(), p.exec_written)
        };
        let asked = (Some(true), Some(vec![443]), Some(true));
        let mut m = manifest();
        assert_eq!(protected_start(&mut m, &current), None);
        assert_eq!(net(&m), asked, "no floor seen: left alone");
        panpak_core::release::note_floor(&appdir, 1).unwrap();
        let mut m = manifest();
        assert_eq!(protected_start(&mut m, &current), None);
        assert_eq!(net(&m), asked, "the floor is the installed revision: left alone");
        panpak_core::release::note_floor(&appdir, 2).unwrap();
        let mut m = manifest();
        assert_eq!(protected_start(&mut m, &current), Some(false));
        assert_eq!(net(&m), (Some(false), None, Some(false)));
        panpak_core::release::set_internet_back_on(&appdir, true).unwrap();
        let mut m = manifest();
        assert_eq!(protected_start(&mut m, &current), Some(true));
        assert_eq!(net(&m), (Some(true), Some(vec![443]), Some(false)));
        // A path that is not an installed app is not touched, whatever files lie around.
        let mut m = manifest();
        assert_eq!(protected_start(&mut m, &root.join("somewhere/else.panpak")), None);
        assert_eq!(net(&m), asked);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_tcp_filter_is_left_to_the_network_namespace_of_a_view_without_network() {
        // In the view an app that asks for no network has a network namespace with only `lo`: the filter of ports is not applied.
        assert!(loopback_only_view(true, false));
        // With any network asked for, the filter is the allow-list.
        assert!(!loopback_only_view(true, true));
        // Without the view (no bubblewrap) the filter is the only barrier: it stays.
        assert!(!loopback_only_view(false, false));
        assert!(!loopback_only_view(false, true));
    }

    #[test]
    fn sandbox_scope_signals_and_abstract_sockets() {
        if !crate::testutil::alone() {
            return;
        }
        if !require_abi(landlock::ABI::V6, "scope") {
            return;
        }
        let parent_pid = std::process::id();
        unsafe { std::env::set_var("PANPAK_PARENT_PID", parent_pid.to_string()) };
        // Parent abstract socket: the (scoped) child must not connect.
        let aname = format!("\0panpak-scope-{}", parent_pid);
        let afd = unsafe {
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
            assert!(fd >= 0, "socket failed");
            let mut addr: libc::sockaddr_un = std::mem::zeroed();
            addr.sun_family = libc::AF_UNIX as u16;
            let bytes = aname.as_bytes();
            for (i, b) in bytes.iter().enumerate() {
                addr.sun_path[i] = *b as i8;
            }
            let len = (std::mem::size_of::<libc::sa_family_t>() + bytes.len()) as u32;
            assert_eq!(libc::bind(fd, &addr as *const _ as *const libc::sockaddr, len), 0, "abstract bind failed");
            assert_eq!(libc::listen(fd, 5), 0);
            fd
        };
        let (dir, panpak, tree, appdata) = fixture_tree("scope", "");
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, None)?;
            // Signals: to self ok, to the parent (out of scope) denied.
            let self_ok = unsafe { libc::kill(std::process::id() as i32, 0) == 0 };
            res.push(format!("sig_self={}\n", if self_ok { "ok" } else { "FAIL" }));
            let ppid: i32 = std::env::var("PANPAK_PARENT_PID").unwrap().parse().unwrap();
            denied(res, "sig_parent", unsafe { libc::kill(ppid, 0) } != 0);
            // Abstract: connection denied by the scope.
            let cfd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
            let aname = format!("\0panpak-scope-{ppid}");
            let conn_ok = unsafe {
                let mut addr: libc::sockaddr_un = std::mem::zeroed();
                addr.sun_family = libc::AF_UNIX as u16;
                for (i, b) in aname.as_bytes().iter().enumerate() {
                    addr.sun_path[i] = *b as i8;
                }
                let len = (std::mem::size_of::<libc::sa_family_t>() + aname.len()) as u32;
                libc::connect(cfd, &addr as *const _ as *const libc::sockaddr, len) == 0
            };
            unsafe { libc::close(cfd) };
            denied(res, "abstract_conn", !conn_ok);
            Ok(())
        });
        assert!(res.contains("sig_self=ok"), "self-signal failed:\n{res}");
        assert!(res.contains("sig_parent=deny"), "out-of-scope signal allowed:\n{res}");
        assert!(res.contains("abstract_conn=deny"), "abstract socket reachable:\n{res}");
        unsafe { libc::close(afd) };
        let _ = dir;
    }

    #[test]
    fn x11_keeps_signal_scope() {
        if !crate::testutil::alone() {
            return;
        }
        if !require_abi(landlock::ABI::V6, "X11 signal scope") { return; }
        let (dir, pkg, tree, appdata) = fixture_tree("x11-signal", "[permissions]\nx11 = true\n");
        let parent = std::process::id() as i32;
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            child_sandboxed(res, &pkg, &tree, &appdata, None)?;
            denied(res, "signal_parent", unsafe { libc::kill(parent, 0) } != 0);
            Ok(())
        });
        assert!(res.contains("signal_parent=deny"), "{res}");
        let _ = dir;
    }

    #[test]
    fn sandbox_unix_allow_path() {
        if !crate::testutil::alone() {
            return;
        }
        if !require_abi(landlock::ABI::V9, "unix path") {
            return;
        }
        use std::os::unix::net::UnixListener;
        // Socket under store/ (outside every RW rule): denied without allow,
        // allowed with allow.
        for (tag, listed) in [("deny", false), ("allow", true)] {
            let dir = tmpdir(&format!("ux-{tag}"));
            // Outside /tmp, which the sandbox may write: a socket there passes without any socket rule.
            let outside = PathBuf::from(format!("/var/tmp/panpak-ux-{}-{tag}", std::process::id()));
            fs::create_dir_all(&outside).unwrap();
            let sock = outside.join("sock");
            let dsock = outside.join("dsock");
            let _listener = UnixListener::bind(&sock).unwrap();
            let _dgram = std::os::unix::net::UnixDatagram::bind(&dsock).unwrap();
            let extra = if listed {
                format!("[permissions]\nsockets_allow = [\"{}\", \"{}\"]\n", sock.display(), dsock.display())
            } else {
                String::new()
            };
            let (panpak, tree, appdata) = fixture_tree_in(&dir, &extra);
            let var = format!("PANPAK_USOCK_{tag}");
            unsafe { std::env::set_var(&var, sock.to_str().unwrap()) };
            let dvar = format!("PANPAK_UDSOCK_{tag}");
            unsafe { std::env::set_var(&dvar, dsock.to_str().unwrap()) };
            let expect = if listed { "ok" } else { "deny" };
            let res = fork_lines(&appdata.join("r.txt"), |res| {
                let _man = child_sandboxed(res, &panpak, &tree, &appdata, None)?;
                let p = std::env::var(&var).unwrap();
                let ok = std::os::unix::net::UnixStream::connect(p).is_ok();
                res.push(format!("ux_{tag}={}\n", if ok { "ok" } else { "deny" }));
                let d = std::env::var(&dvar).unwrap();
                let ok = std::os::unix::net::UnixDatagram::unbound().and_then(|u| u.connect(d)).is_ok();
                res.push(format!("uxd_{tag}={}\n", if ok { "ok" } else { "deny" }));
                Ok(())
            });
            assert!(res.contains(&format!("uxd_{tag}={expect}")), "unix datagram socket {tag}: {res}");
            let _ = fs::remove_dir_all(&outside);
            assert!(res.contains(&format!("ux_{tag}={expect}")), "unix socket {tag}: {res}");
            let _ = dir;
        }
    }

    /// .panpak file with arbitrary symlinks (+ one real file for valid targets).
    fn fake_panpak_with_links(
        dir: &Path,
        name: &str,
        links: &[(&str, &str)],
    ) -> (PathBuf, [u8; 32], Vec<Chunk>, Vec<Entry>) {
        let (sk, key) = test_keypair();
        let data = b"content";
        let comp = zstd::encode_all(&data[..], 3).unwrap();
        let chunks = vec![Chunk {
            hash: *blake3::hash(&data[..]).as_bytes(),
            off: b"FAKE-STUB".len() as u64,
            comp: comp.len() as u32,
            raw: data.len() as u32,
        }];
        let mut entries = vec![Entry {
            path: PathBuf::from("bin/hello"),
            mode: 0o755,
            kind: Kind::File { chunks: vec![0] }, info: Default::default(),
        }];
        for (p, t) in links {
            entries.push(Entry {
                path: PathBuf::from(p),
                mode: 0o777,
                kind: Kind::Link(t.to_string()), info: Default::default(),
            });
        }
        let raw_index = encode_index(&chunks, &entries);
        let root = *blake3::hash(&raw_index).as_bytes();
        let path = sign_and_assemble(AssembleIn { dir, name, sk: &sk, key: &key, chunks: &chunks, chunk_blobs: &[comp], entries: &entries, manifest_extra: "" });
        (path, root, chunks, entries)
    }

    #[test]
    fn verify_accepts_valid_file() {
        let dir = tmpdir("ok");
        let (sk, key) = test_keypair();
        let (path, _toml, root) = fake_panpak(&dir, &sk, &key);
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        assert_eq!(ft.root, root);
        let (man, raw) = verify(&mut f, &ft).unwrap();
        assert_eq!(man.app.id, "org.example.hello");
        assert_eq!(blake3::hash(&raw).as_bytes(), &root);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_rejects_tampered_manifest() {
        let dir = tmpdir("tamper");
        let (sk, key) = test_keypair();
        let (path, toml, _root) = fake_panpak(&dir, &sk, &key);
        let mut bytes = fs::read(&path).unwrap();
        let pos = bytes.windows(toml.len()).position(|w| w == toml).unwrap();
        bytes[pos + 10] ^= 0x01;
        fs::write(&path, &bytes).unwrap();
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        assert!(verify(&mut f, &ft).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_rejects_non_adjacent_layout() {
        let dir = tmpdir("layout");
        let (sk, key) = test_keypair();
        let (path, _toml, _root) = fake_panpak(&dir, &sk, &key);
        let mut bytes = fs::read(&path).unwrap();
        let mut f = File::open(&path).unwrap();
        let ft0 = read_footer(&mut f).unwrap();
        // Ghost byte between index and manifest, with man_off bumped by +1:
        // index_off+index_len != man_off stays violated.
        bytes.insert((ft0.index_off + ft0.index_len) as usize, 0xFF);
        let fpos = bytes.len() - FOOTER_LEN as usize + 32;
        bytes[fpos..fpos + 8].copy_from_slice(&(ft0.man_off + 1).to_le_bytes());
        fs::write(&path, &bytes).unwrap();
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        assert!(verify(&mut f, &ft).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A fake package whose signature block is replaced by `block` (the file is rewritten and its footer is kept).
    /// What makes the block: the signature of the right message, the manifest and the root.
    type BlockMaker<'a> = &'a dyn Fn(&[u8; 64], &[u8], &[u8; 32]) -> Vec<u8>;

    fn with_block(tag: &str, block: BlockMaker) -> (PathBuf, PathBuf) {
        let dir = tmpdir(tag);
        let (sk, key) = test_keypair();
        let (path, toml, root) = fake_panpak(&dir, &sk, &key);
        let bytes = fs::read(&path).unwrap();
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        let sig = sk.sign(&panpak_core::package_message(1, 0, &root, &toml)).to_bytes();
        let mut out = bytes[..(ft.man_off + ft.man_len) as usize].to_vec();
        out.extend_from_slice(&block(&sig, &toml, &root));
        out.extend_from_slice(&bytes[bytes.len() - FOOTER_LEN as usize..]);
        let p2 = dir.join("rewritten.panpak");
        fs::write(&p2, &out).unwrap();
        (dir, p2)
    }

    fn entry(ty: u16, flags: u16, data: &[u8]) -> Vec<u8> {
        let mut e = Vec::new();
        e.extend_from_slice(&ty.to_le_bytes());
        e.extend_from_slice(&flags.to_le_bytes());
        e.extend_from_slice(&(data.len() as u32).to_le_bytes());
        e.extend_from_slice(data);
        e
    }

    fn verify_file(p: &Path) -> anyhow::Result<()> {
        let mut f = File::open(p).unwrap();
        let ft = read_footer(&mut f)?;
        verify(&mut f, &ft).map(|_| ())
    }

    fn code_of(e: &anyhow::Error) -> panpak_core::ErrorCode {
        panpak_core::code_of(e, panpak_core::ErrorCode::E1000)
    }

    #[test]
    fn a_signature_over_the_old_unlabeled_message_is_refused() {
        // manifest || root, as signed before the labels: a good signature of the wrong message.
        let (sk, _) = test_keypair();
        let (dir, p) = with_block("old-msg", &|_, toml, root| {
            let mut old = toml.to_vec();
            old.extend_from_slice(root);
            panpak_core::encode_sig_block(&sk.sign(&old).to_bytes())
        });
        assert_eq!(code_of(&verify_file(&p).unwrap_err()), panpak_core::ErrorCode::E1001);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_footer_version_and_flags_are_covered_by_the_signature() {
        let dir = tmpdir("covered");
        let (sk, key) = test_keypair();
        let (path, _toml, _root) = fake_panpak(&dir, &sk, &key);
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        verify(&mut f, &ft).unwrap();
        // A footer that says another version or flags (set by hand: the reader refuses them at the footer today, and the day it does not,
        // the signature still does).
        for (v, fl) in [(1u32, 1u32), (2, 0), (1, 0x8000_0000)] {
            let altered = Footer { version: v, flags: fl, ..ft.clone() };
            assert_eq!(code_of(&verify(&mut f, &altered).unwrap_err()), panpak_core::ErrorCode::E1001, "version {v} flags {fl:#x}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_signature_block_takes_unknown_entries_by_their_critical_bit() {
        // A good block: the package signature alone.
        let (dir, p) = with_block("blk-good", &|sig, _, _| panpak_core::encode_sig_block(sig));
        verify_file(&p).unwrap();
        let _ = fs::remove_dir_all(&dir);
        // An unknown entry that is not critical is ignored, before or after the signature.
        let (dir, p) = with_block("blk-noncrit", &|sig, _, _| {
            let mut b = entry(9, 0, b"a proof of some kind");
            b.extend_from_slice(&panpak_core::encode_sig_block(sig));
            b.extend_from_slice(&entry(10, 0, b""));
            b
        });
        verify_file(&p).unwrap();
        let _ = fs::remove_dir_all(&dir);
        // An unknown entry marked critical refuses the file and says what to do.
        let (dir, p) = with_block("blk-crit", &|sig, _, _| {
            let mut b = panpak_core::encode_sig_block(sig);
            b.extend_from_slice(&entry(9, 1, b"x"));
            b
        });
        let e = verify_file(&p).unwrap_err();
        assert_eq!(code_of(&e), panpak_core::ErrorCode::E0002);
        assert!(format!("{e:#}").contains("update panpak"), "{e:#}");
        let _ = fs::remove_dir_all(&dir);
        // The reserved types 2, 3 and 4 are unknown too: critical refuses.
        for ty in [2u16, 3, 4] {
            let (dir, p) = with_block("blk-reserved", &|sig, _, _| {
                let mut b = panpak_core::encode_sig_block(sig);
                b.extend_from_slice(&entry(ty, 1, b"x"));
                b
            });
            assert_eq!(code_of(&verify_file(&p).unwrap_err()), panpak_core::ErrorCode::E0002, "type {ty}");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_malformed_signature_block_is_refused() {
        type Make = Box<dyn Fn(&[u8; 64]) -> Vec<u8>>;
        type Case = (&'static str, Make);
        let cases: Vec<Case> = vec![
            ("two package signatures", Box::new(|s| [panpak_core::encode_sig_block(s), panpak_core::encode_sig_block(s)].concat())),
            ("no package signature", Box::new(|_| entry(9, 0, &[0u8; 80]))),
            ("package signature not critical", Box::new(|s| entry(1, 0, s))),
            ("package signature of 63 bytes", Box::new(|s| entry(1, 1, &s[..63]))),
            ("flags not defined", Box::new(|s| [panpak_core::encode_sig_block(s), entry(9, 2, b"")].concat())),
            ("an extra byte after the entries", Box::new(|s| [panpak_core::encode_sig_block(s), vec![0u8]].concat())),
            ("an entry longer than the block", Box::new(|s| {
                let mut b = panpak_core::encode_sig_block(s);
                b.extend_from_slice(&entry(9, 0, &[0u8; 4])[..9]);
                b
            })),
            ("a block over 64 KiB", Box::new(|s| [panpak_core::encode_sig_block(s), entry(9, 0, &vec![0u8; 64 * 1024])].concat())),
            ("the old 64 bytes with no block", Box::new(|s| s.to_vec())),
        ];
        for (name, make) in cases {
            let (dir, p) = with_block("blk-bad", &|sig, _, _| make(sig));
            let e = verify_file(&p).unwrap_err();
            assert!(matches!(code_of(&e), panpak_core::ErrorCode::E0003 | panpak_core::ErrorCode::E0002), "{name}: {e:#}");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn verify_rejects_replaced_stub() {
        // Altered stub (first bytes): the signature stays valid but the file
        // must NOT be runnable/installable.
        let dir = tmpdir("stub-tamper");
        let data = b"hello-panpak-contents";
        let (path, _root, chunks, _entries) = fake_panpak_with_file(&dir, data);
        let mut bytes = fs::read(&path).unwrap();
        bytes[3] ^= 0xFF; // inside b"FAKE-STUB"
        fs::write(&path, &bytes).unwrap();
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        // The signature covers manifest+root: still valid (no stub in it).
        let (man, _raw) = verify(&mut f, &ft).unwrap();
        // But the stub identity check fails: no execution.
        let stub_len = chunks.iter().map(|c| c.off).min().unwrap_or(ft.index_off);
        assert!(panpak_core::verify_stub_range(&mut f, stub_len, &man).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Give back the write bit on folders so that a materialized tree can be removed.
    fn make_writable(p: &Path) {
        if let Ok(m) = fs::symlink_metadata(p) {
            if m.is_dir() {
                let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
                for e in fs::read_dir(p).into_iter().flatten().flatten() {
                    make_writable(&e.path());
                }
            }
        }
    }

    /// A package of `n` small files, each with a chunk of its own.
    fn many_files_panpak(dir: &Path, n: usize) -> (PathBuf, [u8; 32], Vec<Chunk>, Vec<Entry>) {
        let (sk, key) = test_keypair();
        let (mut chunks, mut blobs, mut entries) = (Vec::new(), Vec::new(), Vec::new());
        let mut off = b"FAKE-STUB".len() as u64;
        for i in 0..n {
            let data = format!("file number {i} of the regression package").into_bytes();
            let comp = zstd::encode_all(&data[..], 3).unwrap();
            chunks.push(Chunk { hash: *blake3::hash(&data).as_bytes(), off, comp: comp.len() as u32, raw: data.len() as u32 });
            off += comp.len() as u64;
            blobs.push(comp);
            entries.push(Entry { path: PathBuf::from(format!("d{}/f{i}", i % 7)), mode: 0o644, kind: Kind::File { chunks: vec![i as u32] }, info: Default::default() });
        }
        let raw_index = encode_index(&chunks, &entries);
        let root = *blake3::hash(&raw_index).as_bytes();
        let path = sign_and_assemble(AssembleIn { dir, name: "many.panpak", sk: &sk, key: &key, chunks: &chunks, chunk_blobs: &blobs, entries: &entries, manifest_extra: "" });
        (path, root, chunks, entries)
    }

    /// Defect 19: several starts at once on a store that is not materialized yet. They shared one
    /// `.partial` folder, and one removed it while another was writing (PANPAK-E2000, ENOENT).
    #[test]
    fn parallel_first_starts_all_succeed_and_build_the_tree_once() {
        let dir = tmpdir("mat-par");
        let (path, root, chunks, entries) = many_files_panpak(&dir, 400);
        let store = dir.join("store");
        for round in 0..3 {
            let store = store.join(format!("r{round}"));
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let results: Vec<Result<PathBuf>> = std::thread::scope(|sc| {
                let hs: Vec<_> = (0..8)
                    .map(|_| {
                        let (b, path, chunks, entries, store) = (barrier.clone(), &path, &chunks, &entries, &store);
                        sc.spawn(move || {
                            let mut f = File::open(path).unwrap();
                            b.wait();
                            materialize(&mut f, store, &root, chunks, entries)
                        })
                    })
                    .collect();
                hs.into_iter().map(|h| h.join().unwrap()).collect()
            });
            for r in &results {
                assert!(r.is_ok(), "round {round}: {:?}", r.as_ref().err().map(|e| format!("{e:#}")));
            }
            let tree = results[0].as_ref().unwrap().clone();
            assert!(results.iter().all(|r| r.as_ref().unwrap() == &tree));
            assert!(tree.join(".complete").exists());
            assert_eq!(fs::read(tree.join("d3/f10")).unwrap(), b"file number 10 of the regression package");
            let left: Vec<_> = fs::read_dir(store.join("trees")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.contains("partial")).collect();
            assert!(left.is_empty(), "temporary folders left behind: {left:?}");
        }
        make_writable(&dir);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_rebuilds_file_and_verifies_chunks() {
        let dir = tmpdir("mat-ok");
        let data = b"hello-panpak-contents";
        let (path, root, chunks, entries) = fake_panpak_with_file(&dir, data);
        let mut f = File::open(&path).unwrap();
        let tree = materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).unwrap();
        assert_eq!(fs::read(tree.join("bin/hello")).unwrap(), data);
        assert!(tree.join(".complete").exists());
        // A folder has the time of its list and a file the time of its content, both in the one span; the mark keeps the fixed time.
        use std::os::unix::fs::MetadataExt;
        for p in [tree.clone(), tree.join("bin")] {
            let t = fs::symlink_metadata(&p).unwrap().mtime();
            assert!((TREE_MTIME..TREE_MTIME + FILE_TIME_SPAN as i64).contains(&t), "{}: {t}", p.display());
        }
        assert_eq!(fs::symlink_metadata(tree.join(".complete")).unwrap().mtime(), TREE_MTIME);
        let t = fs::symlink_metadata(tree.join("bin/hello")).unwrap().mtime();
        assert!((TREE_MTIME..TREE_MTIME + FILE_TIME_SPAN as i64).contains(&t), "{t}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Chunks held in memory, by hash: a source for the tests of `materialize_from`.
    struct MemChunks(std::collections::HashMap<[u8; 32], Vec<u8>>);

    impl ChunkSource for MemChunks {
        fn raw<'a>(&self, c: &Chunk, _dec: &mut ChunkDecoder, scratch: &'a mut Vec<u8>) -> Result<&'a [u8]> {
            let data = self.0.get(&c.hash).context("no such chunk in the test source")?;
            scratch.clear();
            scratch.extend_from_slice(data);
            panpak_core::verify_chunk(c, scratch)?;
            Ok(scratch)
        }
    }

    /// An index and a source for files made of the given lists of chunks of `size` bytes each (the content of chunk `n` is `n` repeated with
    /// a changing byte, so that two chunks are never alike): `(name, mode, chunk numbers)`.
    fn mem_package(files: &[(&str, u32, &[u8])], size: usize) -> (Vec<Chunk>, Vec<Entry>, MemChunks) {
        let mut chunks: Vec<Chunk> = Vec::new();
        let mut data = std::collections::HashMap::new();
        let mut entries = Vec::new();
        for (name, mode, numbers) in files {
            let mut ids = Vec::new();
            for &n in *numbers {
                let bytes: Vec<u8> = (0..size).map(|i| (i as u8).wrapping_mul(7).wrapping_add(n).wrapping_add((i >> 8) as u8)).collect();
                let hash = *blake3::hash(&bytes).as_bytes();
                let id = match chunks.iter().position(|c| c.hash == hash) {
                    Some(id) => id,
                    None => {
                        chunks.push(Chunk { hash, off: 0, comp: 1, raw: size as u32 });
                        data.insert(hash, bytes);
                        chunks.len() - 1
                    }
                };
                ids.push(id as u32);
            }
            entries.push(Entry { path: PathBuf::from(name), mode: *mode, kind: Kind::File { chunks: ids }, info: Default::default() });
        }
        (chunks, entries, MemChunks(data))
    }

    #[test]
    fn a_large_file_is_a_blob_of_the_store_shared_by_trees_and_checked_when_it_is_reused() {
        use std::os::unix::fs::MetadataExt;
        let dir = tmpdir("large-shared");
        let store = dir.join("store");
        let numbers: Vec<u8> = (0..40).collect();
        let (c1, e1, s1) = mem_package(&[("lib/big.so", 0o755, &numbers)], 300_000);
        let t1 = materialize_from(&s1, &store, &[1u8; 32], &c1, &e1).unwrap();
        // The same file under another name and in another tree, and a source that has no chunks: nothing is decoded, the inode is the same.
        let (c2, e2, _s2) = mem_package(&[("other/dir/renamed.so", 0o755, &numbers)], 300_000);
        let empty = MemChunks(std::collections::HashMap::new());
        let t2 = materialize_from(&empty, &store, &[2u8; 32], &c2, &e2).unwrap();
        let (a, b) = (fs::metadata(t1.join("lib/big.so")).unwrap(), fs::metadata(t2.join("other/dir/renamed.so")).unwrap());
        assert_eq!(a.ino(), b.ino(), "one copy of the large file for both trees");
        assert!(a.nlink() >= 3, "the blob and the two trees");
        assert_eq!(a.permissions().mode() & 0o777, 0o555);
        // No work folder is left in the store.
        assert!(fs::read_dir(store.join("blobs")).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".work-")));
        // A large blob that is damaged is not inherited: with the chunks it is made again, without them the start fails.
        let blob = fs::read_dir(store.join("blobs")).unwrap().flatten().find(|e| e.metadata().unwrap().len() > 8 << 20).unwrap().path();
        fs::set_permissions(&blob, fs::Permissions::from_mode(0o644)).unwrap();
        let mut bad = fs::read(&blob).unwrap();
        bad[1_000_000] ^= 0x01;
        fs::write(&blob, &bad).unwrap();
        assert!(materialize_from(&empty, &store, &[3u8; 32], &c2, &e2).is_err());
        let t4 = materialize_from(&s1, &store, &[4u8; 32], &c2, &e2).unwrap();
        let good = fs::read(t4.join("other/dir/renamed.so")).unwrap();
        assert_ne!(good, bad);
        assert_eq!(good.len(), 12_000_000);
        assert_ne!(fs::metadata(t4.join("other/dir/renamed.so")).unwrap().ino(), a.ino(), "a new blob, not the damaged one");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_has_the_time_of_its_content_the_same_in_every_tree_and_another_for_another_content() {
        use std::os::unix::fs::MetadataExt;
        let dir = tmpdir("content-time");
        let store = dir.join("store");
        // Version 1 and version 2 of an app: `keep` is the same, `change` is not, a folder and a link are there.
        let (c1, e1, s1) = mem_package(&[("lib/keep", 0o644, &[1, 2]), ("lib/change", 0o644, &[3])], 1000);
        let t1 = materialize_from(&s1, &store, &[1u8; 32], &c1, &e1).unwrap();
        let (c2, e2, s2) = mem_package(&[("lib/keep", 0o644, &[1, 2]), ("lib/change", 0o644, &[4])], 1000);
        let t2 = materialize_from(&s2, &store, &[2u8; 32], &c2, &e2).unwrap();
        let m = |t: &Path, f: &str| fs::symlink_metadata(t.join(f)).unwrap().mtime();
        assert_eq!(m(&t1, "lib/keep"), m(&t2, "lib/keep"), "the same content, the same time");
        assert_ne!(m(&t1, "lib/change"), m(&t2, "lib/change"), "another content, another time");
        // A folder has the time of its list: another list, another time.
        assert_ne!(m(&t1, "lib"), m(&t2, "lib"), "a folder whose list changed has another time");
        for (t, f) in [(&t1, "lib/keep"), (&t1, "lib/change"), (&t2, "lib/change"), (&t1, "lib"), (&t2, "lib")] {
            let v = m(t, f);
            assert!((TREE_MTIME..TREE_MTIME + FILE_TIME_SPAN as i64).contains(&v), "{f}: {v}");
        }
        // The same list in another version and another store gives the same time.
        let (c3, e3, s3) = mem_package(&[("lib/keep", 0o644, &[1, 2]), ("lib/change", 0o644, &[3])], 1000);
        let t3 = materialize_from(&s3, &dir.join("store3"), &[3u8; 32], &c3, &e3).unwrap();
        assert_eq!(m(&t1, "lib"), m(&t3, "lib"), "the same list, the same time");
        // The time is the one of the name of the blob (so the same through the hard link), and it is the same on every run.
        assert_eq!(fs::metadata(t1.join("lib/keep")).unwrap().ino(), fs::metadata(t2.join("lib/keep")).unwrap().ino());
        assert_eq!(content_mtime("00000000aaaa-644"), TREE_MTIME);
        assert_eq!(content_mtime("00000001aaaa-644"), TREE_MTIME + 1);
        assert_eq!(content_mtime("ffffffffaaaa-644"), TREE_MTIME + (0xffff_ffffu64 % FILE_TIME_SPAN) as i64);
        assert_eq!(content_mtime("zz"), TREE_MTIME, "a name that is not a hash gives the earliest time");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cache_of_a_folder_is_never_older_than_the_folder_or_any_folder_below() {
        use std::os::unix::fs::MetadataExt;
        let dir = tmpdir("cache-time");
        let (c, e, s) = mem_package(
            &[
                ("icons/hicolor/icon-theme.cache", 0o644, &[9]),
                ("icons/hicolor/16x16/apps/a.png", 0o644, &[1]),
                ("icons/hicolor/32x32/apps/b.png", 0o644, &[2]),
                ("icons/other/c.png", 0o644, &[3]),
            ],
            1000,
        );
        let tree = materialize_from(&s, &dir.join("store"), &[5u8; 32], &c, &e).unwrap();
        let m = |f: &str| fs::symlink_metadata(tree.join(f)).unwrap().mtime();
        let cache = m("icons/hicolor/icon-theme.cache");
        for d in ["icons/hicolor", "icons/hicolor/16x16", "icons/hicolor/16x16/apps", "icons/hicolor/32x32", "icons/hicolor/32x32/apps"] {
            assert!(cache >= m(d), "the cache is older than {d}");
        }
        assert_eq!(cache, ["icons/hicolor", "icons/hicolor/16x16", "icons/hicolor/16x16/apps", "icons/hicolor/32x32", "icons/hicolor/32x32/apps"].iter().map(|d| m(d)).max().unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_start_that_does_not_fit_stops_before_it_writes() {
        let dir = tmpdir("room");
        let (c, e, s) = mem_package(&[("x", 0o644, &[1, 2, 3])], 1000);
        let store = dir.join("store");
        // `ensure_room` is what `make_files` calls with the size of the blobs that are not in the store.
        fs::create_dir_all(store.join("blobs")).unwrap();
        let err = ensure_room(&store.join("blobs"), u64::MAX / 2).unwrap_err();
        assert!(panpak_core::is_storage_full(&err), "{err:#}");
        assert!(format!("{err:#}").contains("nothing was written"), "{err:#}");
        assert!(ensure_room(&store.join("blobs"), 0).is_ok());
        assert!(ensure_room(&store.join("blobs"), 1000).is_ok());
        // And a start that fits makes its tree.
        assert!(materialize_from(&s, &store, &[7u8; 32], &c, &e).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_large_file_is_made_by_several_threads_and_reads_back_whole() {
        let dir = tmpdir("large");
        // 40 chunks of 300,000 bytes: 12 MB, above the size that one thread assembles in memory; and a small file next to it.
        let numbers: Vec<u8> = (0..40).collect();
        let (chunks, entries, src) = mem_package(&[("big", 0o755, &numbers), ("small", 0o644, &[3, 4])], 300_000);
        let root = [9u8; 32];
        let tree = materialize_from(&src, &dir.join("store"), &root, &chunks, &entries).unwrap();
        let mut want = Vec::new();
        for &id in match &entries[0].kind { Kind::File { chunks } => chunks, _ => unreachable!() } {
            want.extend_from_slice(src.0.get(&chunks[id as usize].hash).unwrap());
        }
        assert_eq!(fs::read(tree.join("big")).unwrap(), want, "the parts are written at their places");
        assert_eq!(fs::metadata(tree.join("big")).unwrap().permissions().mode() & 0o777, 0o555, "executable, not writable");
        assert_eq!(fs::read(tree.join("small")).unwrap().len(), 600_000);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn blobs_are_named_by_the_chunks_and_a_file_that_is_there_is_only_linked() {
        let (chunks, entries, _src) = mem_package(&[("a", 0o644, &[1, 2]), ("b", 0o644, &[1, 2]), ("c", 0o755, &[1, 2]), ("d", 0o644, &[2, 1])], 1000);
        let ids = |i: usize| match &entries[i].kind { Kind::File { chunks } => chunks.clone(), _ => unreachable!() };
        assert_eq!(blob_name(&chunks, &ids(0), 0o644), blob_name(&chunks, &ids(1), 0o644), "same chunks, same blob");
        assert_ne!(blob_name(&chunks, &ids(0), 0o644), blob_name(&chunks, &ids(2), 0o755), "the mode is part of the name");
        assert_ne!(blob_name(&chunks, &ids(0), 0o644), blob_name(&chunks, &ids(3), 0o644), "the order of the chunks is part of the name");
        let dir = tmpdir("blobs");
        let store = dir.join("store");
        let (c1, e1, s1) = mem_package(&[("lib", 0o644, &[1, 2, 3])], 4000);
        let t1 = materialize_from(&s1, &store, &[1u8; 32], &c1, &e1).unwrap();
        // A second tree with the same file (and one more): the file is the same inode, and nothing was decoded for it (a source that has no
        // chunks at all serves the file that is there).
        let (c2, e2, s2) = mem_package(&[("lib", 0o644, &[1, 2, 3])], 4000);
        let empty = MemChunks(std::collections::HashMap::new());
        let t2 = materialize_from(&empty, &store, &[2u8; 32], &c2, &e2).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(t1.join("lib")).unwrap().ino(), fs::metadata(t2.join("lib")).unwrap().ino());
        assert_eq!(fs::read(t2.join("lib")).unwrap(), fs::read(t1.join("lib")).unwrap());
        // A blob that is damaged is never inherited by a tree: it is replaced when the chunks are there, and the start fails when they are not.
        let blob = fs::read_dir(store.join("blobs")).unwrap().next().unwrap().unwrap().path();
        fs::set_permissions(&blob, fs::Permissions::from_mode(0o644)).unwrap();
        let mut bad = fs::read(&blob).unwrap();
        bad[0] ^= 0xff;
        fs::write(&blob, &bad).unwrap();
        assert!(materialize_from(&empty, &store, &[3u8; 32], &c2, &e2).is_err(), "a damaged blob and no chunks to make it again: nothing is linked");
        let t4 = materialize_from(&s2, &store, &[4u8; 32], &c2, &e2).unwrap();
        assert_ne!(fs::read(t4.join("lib")).unwrap(), bad);
        assert_eq!(fs::read(t4.join("lib")).unwrap().len(), 12_000);
        assert_eq!(blake3::hash(&fs::read(t4.join("lib")).unwrap()), {
            let mut all = Vec::new();
            for n in [1u8, 2, 3] {
                all.extend_from_slice(s2.0.values().find(|v| v[0] == (0u8.wrapping_add(n))).unwrap());
            }
            blake3::hash(&all)
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_update_takes_the_chunks_it_has_from_the_old_tree_and_the_rest_from_the_download() {
        let dir = tmpdir("staged");
        let store = dir.join("store");
        // Version 1 has chunks 1 2 3 in one file and 4 in another. Version 2 keeps the first file but for chunk 2, which changes to 5.
        let (c1, e1, s1) = mem_package(&[("lib", 0o644, &[1, 2, 3]), ("other", 0o644, &[4])], 5000);
        let old_tree = materialize_from(&s1, &store, &[1u8; 32], &c1, &e1).unwrap();
        let (c2, e2, s2) = mem_package(&[("lib", 0o644, &[1, 5, 3]), ("other", 0o644, &[4])], 5000);
        // The download folder holds the compressed chunk that the old tree does not have.
        let new_hash = *blake3::hash(s2.0.values().find(|v| v[0] == 5).unwrap()).as_bytes();
        let comp = zstd::encode_all(&s2.0[&new_hash][..], 3).unwrap();
        let mut c2 = c2;
        for c in c2.iter_mut() {
            if c.hash == new_hash {
                c.comp = comp.len() as u32;
            }
        }
        let dl = dir.join("download");
        fs::create_dir_all(&dl).unwrap();
        // Not there yet: an error that names the chunk.
        let staged = StagedChunks::new(Some(dl.clone()), Some((old_tree.as_path(), &c1, &e1)));
        let err = materialize_from(&staged, &store, &[2u8; 32], &c2, &e2).unwrap_err();
        assert!(format!("{err:#}").contains(&hex(&new_hash)), "{err:#}");
        // The chunk comes, but with the wrong size: refused.
        fs::write(dl.join(hex(&new_hash)), &comp[..comp.len() - 1]).unwrap();
        assert!(materialize_from(&staged, &store, &[2u8; 32], &c2, &e2).is_err());
        fs::write(dl.join(hex(&new_hash)), &comp).unwrap();
        let new_tree = materialize_from(&staged, &store, &[2u8; 32], &c2, &e2).unwrap();
        let mut want = Vec::new();
        for n in [1u8, 5, 3] {
            want.extend_from_slice(s2.0.values().find(|v| v[0] == n).unwrap());
        }
        assert_eq!(fs::read(new_tree.join("lib")).unwrap(), want);
        // A damaged file of the old tree is not used for the chunks it holds: they must come from the download.
        let mut damaged = fs::read(old_tree.join("other")).unwrap();
        damaged[10] ^= 1;
        fs::set_permissions(old_tree.join("other"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(old_tree.join("other"), &damaged).unwrap();
        let (c4, e4, _s4) = mem_package(&[("again", 0o644, &[4, 9])], 5000);
        let staged = StagedChunks::new(Some(dl.clone()), Some((old_tree.as_path(), &c1, &e1)));
        let err = materialize_from(&staged, &store, &[3u8; 32], &c4, &e4).unwrap_err();
        assert!(format!("{err:#}").contains("neither in the installed files nor in"), "{err:#}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_record_without_its_tree_stops_with_a_code_that_says_how_to_go_on() {
        let dir = tmpdir("no-tree");
        let err = existing_tree(&dir, &[7u8; 32]).unwrap_err();
        assert_eq!(panpak_core::code_of(&err, ErrorCode::E2000), ErrorCode::E2007);
        assert!(format!("{err}").contains("panpak repair"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_makes_the_farm_link_and_check_farm_accepts_only_that() {
        let dir = tmpdir("farm-link");
        let (path, root, chunks, entries) = fake_panpak_with_file(&dir, b"x");
        let mut f = File::open(&path).unwrap();
        let store = dir.join("store");
        let tree = materialize(&mut f, &store, &root, &chunks, &entries).unwrap();
        let rh = hex(&root);
        let link = tree.join(panpak_core::libpath::FARM_LINK);
        assert_eq!(fs::read_link(&link).unwrap().to_str().unwrap(), format!("../../farms/{rh}"));
        let farm = store.join("farms").join(&rh);
        let code = |e: anyhow::Error| panpak_core::code_of(&e, ErrorCode::E2000);
        // The farm folder is made by the resolution; until then the link leads nowhere, and the check says so.
        assert_eq!(code(check_farm(&tree, &farm, &rh).unwrap_err()), ErrorCode::E2006);
        fs::create_dir_all(&farm).unwrap();
        check_farm(&tree, &farm, &rh).unwrap();
        // The farm is a link to another folder: refused.
        fs::remove_dir(&farm).unwrap();
        fs::create_dir_all(dir.join("other")).unwrap();
        std::os::unix::fs::symlink(dir.join("other"), &farm).unwrap();
        let err = check_farm(&tree, &farm, &rh).unwrap_err();
        assert_eq!(code(err), ErrorCode::E2006);
        fs::remove_file(&farm).unwrap();
        fs::create_dir_all(&farm).unwrap();
        // The link of the tree is another one, or gone: refused, with the path of the tree in the message.
        let mut p = fs::symlink_metadata(&tree).unwrap().permissions();
        p.set_mode(0o755);
        fs::set_permissions(&tree, p).unwrap();
        fs::remove_file(&link).unwrap();
        let err = check_farm(&tree, &farm, &rh).unwrap_err();
        assert_eq!(panpak_core::code_of(&err, ErrorCode::E2000), ErrorCode::E2006);
        let text = format!("{err}");
        assert!(text.contains("is missing") && text.contains(&tree.display().to_string()), "{text}");
        std::os::unix::fs::symlink("../../farms/other", &link).unwrap();
        assert_eq!(code(check_farm(&tree, &farm, &rh).unwrap_err()), ErrorCode::E2006);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_library_path_is_relative_and_there_is_no_absolute_list() {
        let dir = tmpdir("ld-env");
        let man: panpak_core::Manifest = toml::from_str("[app]\nid = \"org.example.app\"\nname = \"A\"\nversion = \"1\"\nentry = \"bin/a\"\n").unwrap();
        let (path, root, chunks, entries) = fake_panpak_with_file(&dir, b"x");
        let mut f = File::open(&path).unwrap();
        let store = dir.join("store");
        let tree = materialize(&mut f, &store, &root, &chunks, &entries).unwrap();
        let rh = hex(&root);
        let farm = store.join("farms").join(&rh);
        fs::create_dir_all(&farm).unwrap();
        let plan = panpak_core::abi::Plan { loader: None, ld_path: vec![farm.clone(), tree.join("lib")] };
        // No mark in the root of the tree: no value at all (there is no absolute list any more).
        assert!(library_path_env(&store, &tree, &root, &man, &plan).unwrap().is_none());
        // With the mark (a link in the root named for the id) the value is relative to the program.
        let mut p = fs::symlink_metadata(&tree).unwrap().permissions();
        p.set_mode(0o755);
        fs::set_permissions(&tree, p).unwrap();
        let name = panpak_core::libpath::link_name("org.example.app");
        std::os::unix::fs::symlink(".", tree.join(&name)).unwrap();
        let v = library_path_env(&store, &tree, &root, &man, &plan).unwrap().unwrap();
        assert_eq!(v.to_str().unwrap(), format!("/$ORIGIN/{name}/.farm:/$ORIGIN/{name}/lib"));
        // A program (an ELF entry) with no mark is refused: no absolute list to fall back on.
        let plan0 = panpak_core::abi::Plan { loader: None, ld_path: vec![farm.clone()] };
        let man_elf: panpak_core::Manifest = toml::from_str("[app]\nid = \"org.example.elf\"\nname = \"A\"\nversion = \"1\"\nentry = \"prog\"\n").unwrap();
        fs::write(tree.join("prog"), b"\x7fELF....").unwrap();
        let err = library_path_env_or_refuse(&store, &tree, &root, &man_elf, &plan0).unwrap_err();
        assert_eq!(panpak_core::code_of(&err, ErrorCode::E2000), ErrorCode::E0009);
        assert!(format!("{err:#}").contains("make it again"), "{err:#}");
        // A script with no mark has no value, and is not refused.
        let man_script: panpak_core::Manifest = toml::from_str("[app]\nid = \"org.example.script\"\nname = \"A\"\nversion = \"1\"\nentry = \"bin/hello\"\n").unwrap();
        assert!(library_path_env_or_refuse(&store, &tree, &root, &man_script, &plan0).unwrap().is_none());
        // And a damaged farm link stops the start instead of giving a value that leads nowhere.
        fs::remove_file(tree.join(".farm")).unwrap();
        let err = library_path_env(&store, &tree, &root, &man, &plan).unwrap_err();
        assert_eq!(panpak_core::code_of(&err, ErrorCode::E2000), ErrorCode::E2006);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verification_errors_have_stable_codes() {
        let code = |e: anyhow::Error| panpak_core::code_of(&e, ErrorCode::E2000);
        let dir = tmpdir("codes");
        let (sk, key) = test_keypair();
        let (path, _toml, _root) = fake_panpak(&dir, &sk, &key);
        let good = fs::read(&path).unwrap();
        // Valid package verifies.
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        assert!(verify(&mut f, &ft).is_ok());
        // Flip a byte of the signature (the 64 bytes before the footer).
        let mut bad = good.clone();
        let n = bad.len();
        bad[n - 80 - 1] ^= 0x01;
        let p2 = dir.join("badsig.panpak");
        fs::write(&p2, &bad).unwrap();
        let mut f = File::open(&p2).unwrap();
        let ft = read_footer(&mut f).unwrap();
        assert_eq!(code(verify(&mut f, &ft).unwrap_err()), ErrorCode::E1001);
        // Wrong root in the footer (signature is over manifest || root).
        let mut bad = good.clone();
        bad[n - 80 + 48] ^= 0x01;
        let p3 = dir.join("badroot.panpak");
        fs::write(&p3, &bad).unwrap();
        let mut f = File::open(&p3).unwrap();
        let ft = read_footer(&mut f).unwrap();
        assert_eq!(code(verify(&mut f, &ft).unwrap_err()), ErrorCode::E1001);
        // Wrong magic.
        let mut bad = good.clone();
        bad[n - 80] ^= 0x01;
        let p4 = dir.join("badmagic.panpak");
        fs::write(&p4, &bad).unwrap();
        let mut f = File::open(&p4).unwrap();
        assert_eq!(code(read_footer(&mut f).unwrap_err()), ErrorCode::E0001);
        // A stub that differs from the signed one.
        let mut f = File::open(&path).unwrap();
        let ft = read_footer(&mut f).unwrap();
        let (man, _) = verify(&mut f, &ft).unwrap();
        let e = panpak_core::verify_stub_range(&mut f, 8, &man).unwrap_err();
        assert_eq!(code(e), ErrorCode::E1002);
        // A chunk that does not match its hash.
        let c = Chunk { hash: [1u8; 32], off: 0, comp: 1, raw: 3 };
        assert_eq!(code(panpak_core::verify_chunk(&c, b"abc").unwrap_err()), ErrorCode::E0005);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_rejects_corrupt_chunk() {
        let dir = tmpdir("mat-bad");
        let data = b"hello-panpak-contents-longer-for-cdc";
        let (path, root, chunks, entries) = fake_panpak_with_file(&dir, data);
        // Corrupts one byte of the compressed chunk: hash or zstd must fail.
        let mut bytes = fs::read(&path).unwrap();
        let off = chunks[0].off as usize;
        bytes[off + 2] ^= 0xFF;
        fs::write(&path, &bytes).unwrap();
        let mut f = File::open(&path).unwrap();
        assert!(materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_rejects_absolute_symlink() {
        let dir = tmpdir("link-abs");
        let (path, root, chunks, entries) = fake_panpak_with_links(&dir, "a.panpak", &[("evil", "/etc/passwd")]);
        let mut f = File::open(&path).unwrap();
        assert!(materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_rejects_parent_symlink() {
        let dir = tmpdir("link-dotdot");
        let (path, root, chunks, entries) =
            fake_panpak_with_links(&dir, "d.panpak", &[("sub/evil", "../../x")]);
        let mut f = File::open(&path).unwrap();
        assert!(materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_rejects_composite_symlink_chain() {
        // Individually harmless ("b/passwd", ".."), together they escape the root.
        let dir = tmpdir("link-chain");
        let (path, root, chunks, entries) =
            fake_panpak_with_links(&dir, "c.panpak", &[("a", "b/passwd"), ("b", "..")]);
        let mut f = File::open(&path).unwrap();
        assert!(materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_accepts_relative_symlink() {
        let dir = tmpdir("link-ok");
        let (path, root, chunks, entries) =
            fake_panpak_with_links(&dir, "o.panpak", &[("latest", "bin/hello")]);
        let mut f = File::open(&path).unwrap();
        let tree = materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).unwrap();
        assert_eq!(fs::read_link(tree.join("latest")).unwrap(), PathBuf::from("bin/hello"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_accepts_inner_parent_symlink() {
        // Leading "../" staying inside the root (real Kdenlive case):
        // usr/translations/x -> ../share/... == usr/share/...
        let dir = tmpdir("link-inside");
        let (path, root, chunks, entries) = fake_panpak_with_links(
            &dir,
            "i.panpak",
            &[("usr/translations/karchive6_qt.qm", "../share/locale/ru/LC_MESSAGES/karchive6_qt.qm")],
        );
        let mut f = File::open(&path).unwrap();
        let tree = materialize(&mut f, &dir.join("store"), &root, &chunks, &entries).unwrap();
        assert_eq!(
            fs::read_link(tree.join("usr/translations/karchive6_qt.qm")).unwrap(),
            PathBuf::from("../share/locale/ru/LC_MESSAGES/karchive6_qt.qm")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn abi_parse_ldconfig_and_glob() {
        let sample = "ldconfig mock\n\tlibgtk-3.so.0 (libc6,x86-64) => /lib/x86_64-linux-gnu/libgtk-3.so.0\n\tlibc.so.6 (libc6,x86-64) => /lib/x86_64-linux-gnu/libc.so.6\n";
        let m = abi::parse_ldconfig(sample);
        assert_eq!(m["libgtk-3.so.0"], PathBuf::from("/lib/x86_64-linux-gnu/libgtk-3.so.0"));
        assert!(abi::glob_match("libGL*", "libGL.so.1"));
        assert!(!abi::glob_match("libGL*", "libEGL.so.1"));
        assert!(!abi::glob_match("*vulkan*", "libvulkan.so.1")); // Prefix/suffix only.
        assert!(abi::glob_match("libc.so.6", "libc.so.6"));
    }

    #[test]
    fn abi_decide_host_bundle_prefer() {
        use abi::{decide, Source};
        use std::collections::HashMap;
        let host: HashMap<String, PathBuf> =
            [("a.so", "/lib/a.so"), ("gl.so", "/lib/gl.so")].into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        let bund: HashMap<String, PathBuf> =
            [("a.so", "lib/a.so"), ("b.so", "lib/b.so"), ("gl.so", "lib/gl.so")]
                .into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        // The included copy wins when the package has one; the system copy when it does not.
        let d = decide(&["a.so".into(), "b.so".into(), "c.so".into()], &[], &[], &host_c(&host), &bund).unwrap();
        assert!(d.contains(&("a.so".into(), Source::Bundled("lib/a.so".into()))));
        assert!(d.contains(&("b.so".into(), Source::Bundled("lib/b.so".into()))));
        assert!(d.contains(&("c.so".into(), Source::Host("/lib/c.so".into()))));
        // prefer_host present even in the bundle: stays host.
        let d = decide(&["gl.so".into()], &["gl*".to_string()], &[], &host, &bund).unwrap();
        assert!(d.contains(&("gl.so".into(), Source::Host("/lib/gl.so".into()))));
        // prefer_host missing on the host: error, never substituted.
        assert!(decide(&["missing.so".into()], &["missing*".to_string()], &[], &host, &bund).is_err());
        // needs missing everywhere: error.
        assert!(decide(&["nope.so".into()], &[], &[], &host, &bund).is_err());
        // A library that only modules ask for: left out when nothing provides it, not an error.
        let d = decide(&["nope.so".into()], &[], &["nope*".into()], &host, &bund).unwrap();
        assert!(d.is_empty());
    }

    /// `host` plus a library `c.so` that the bundle does not have.
    fn host_c(host: &std::collections::HashMap<String, PathBuf>) -> std::collections::HashMap<String, PathBuf> {
        let mut h = host.clone();
        h.insert("c.so".into(), "/lib/c.so".into());
        h
    }

    /// A table of cases for the bundle-first rule: (libraries needed, in the system, in the bundle,
    /// expected source of the first one). `H` system, `B` bundled.
    #[test]
    fn abi_bundle_first_table() {
        use abi::{decide, decide_with, Source};
        use std::collections::HashMap;
        let map = |names: &[&str], dir: &str| -> HashMap<String, PathBuf> {
            names.iter().map(|n| (n.to_string(), PathBuf::from(format!("{dir}/{n}")))).collect()
        };
        let who = |name: &str, host: &[&str], bund: &[&str]| -> char {
            let d = decide(&[name.to_string()], &[], &[], &map(host, "/usr/lib"), &map(bund, "lib")).unwrap();
            match &d[0].1 {
                Source::Host(_) => 'H',
                Source::Bundled(_) => 'B',
            }
        };
        // (name, in the system, in the bundle, expected)
        let table: &[(&str, &[&str], &[&str], char)] = &[
            ("libSoundTouch.so.1", &["libSoundTouch.so.1"], &["libSoundTouch.so.1"], 'B'),
            ("libfoo.so.1", &["libfoo.so.1"], &[], 'H'),
            ("libfoo.so.1", &[], &["libfoo.so.1"], 'B'),
            // glibc: the system wins even when the bundle has the library but no libc.
            ("libm.so.6", &["libm.so.6"], &["libm.so.6"], 'H'),
            ("libpthread.so.0", &["libpthread.so.0"], &["libpthread.so.0"], 'H'),
            ("libnss_files.so.2", &["libnss_files.so.2"], &["libnss_files.so.2"], 'H'),
            ("libm.so.6", &[], &["libm.so.6"], 'B'),
            // Graphics stack: the system wins, and the bundle is the fallback.
            ("libGL.so.1", &["libGL.so.1"], &["libGL.so.1"], 'H'),
            ("libGLX.so.0", &["libGLX.so.0"], &["libGLX.so.0"], 'H'),
            ("libEGL.so.1", &["libEGL.so.1"], &["libEGL.so.1"], 'H'),
            ("libGLESv2.so.2", &["libGLESv2.so.2"], &["libGLESv2.so.2"], 'H'),
            ("libvulkan.so.1", &["libvulkan.so.1"], &["libvulkan.so.1"], 'H'),
            ("libgbm.so.1", &["libgbm.so.1"], &["libgbm.so.1"], 'H'),
            ("libdrm.so.2", &["libdrm.so.2"], &["libdrm.so.2"], 'H'),
            ("libdrm_amdgpu.so.1", &["libdrm_amdgpu.so.1"], &["libdrm_amdgpu.so.1"], 'H'),
            ("libnvidia-glcore.so.550", &["libnvidia-glcore.so.550"], &["libnvidia-glcore.so.550"], 'H'),
            ("libGL.so.1", &[], &["libGL.so.1"], 'B'),
            // The Wayland client libraries go with the host's EGL: always the system copy, the bundle only as the fallback.
            ("libwayland-client.so.0", &["libwayland-client.so.0"], &["libwayland-client.so.0"], 'H'),
            ("libwayland-egl.so.1", &["libwayland-egl.so.1"], &["libwayland-egl.so.1"], 'H'),
            ("libwayland-cursor.so.0", &["libwayland-cursor.so.0"], &["libwayland-cursor.so.0"], 'H'),
            ("libwayland-client.so.0", &[], &["libwayland-client.so.0"], 'B'),
            // Not on the list: the included copy wins.
            ("libasound.so.2", &["libasound.so.2"], &["libasound.so.2"], 'B'),
            ("libfreetype.so.6", &["libfreetype.so.6"], &["libfreetype.so.6"], 'B'),
        ];
        for (name, host, bund, want) in table {
            assert_eq!(who(name, host, bund), *want, "{name} host={host:?} bundle={bund:?}");
        }
        // libstdc++ and libgcc_s follow the choice made from the symbol versions; the bundled
        // copy without a choice; the only copy when there is just one.
        let both = |prefs: &HashMap<String, bool>, n: &str| -> char {
            let d = decide_with(&[n.to_string()], &[], &[], &map(&[n], "/usr/lib"), &map(&[n], "lib"), prefs).unwrap();
            if matches!(d[0].1, Source::Host(_)) { 'H' } else { 'B' }
        };
        let none = HashMap::new();
        let host_pref = HashMap::from([("libstdc++.so.6".to_string(), true), ("libgcc_s.so.1".to_string(), false)]);
        assert_eq!(both(&none, "libstdc++.so.6"), 'B');
        assert_eq!(both(&host_pref, "libstdc++.so.6"), 'H');
        assert_eq!(both(&host_pref, "libgcc_s.so.1"), 'B');
        assert_eq!(who("libstdc++.so.6", &["libstdc++.so.6"], &[]), 'H');
        assert_eq!(who("libstdc++.so.6", &[], &["libstdc++.so.6"]), 'B');
        // prefer_host: always the system.
        assert!(!decide(&["libSoundTouch.so.1".into()], &[], &["libSoundTouch*".into()], &map(&["libSoundTouch.so.1"], "/usr/lib"), &map(&["libSoundTouch.so.1"], "lib")).unwrap().is_empty(), "present: decided as usual");
        let d = decide(&["libSoundTouch.so.1".into()], &["libSoundTouch*".into()], &[], &map(&["libSoundTouch.so.1"], "/usr/lib"), &map(&["libSoundTouch.so.1"], "lib")).unwrap();
        assert!(matches!(d[0].1, Source::Host(_)));
        // A bundle with its own glibc (B-world) keeps everything in the bundle, graphics included
        // (it is the case of a package for musl or an old system; prefer_host opts out).
        let d = decide(
            &["libc.so.6".into(), "libGL.so.1".into(), "libz.so.1".into()],
            &[], &[],
            &map(&["libc.so.6", "libGL.so.1", "libz.so.1"], "/usr/lib"),
            &map(&["libc.so.6", "libGL.so.1", "libz.so.1"], "lib"),
        ).unwrap();
        assert!(d.iter().all(|(_, s)| matches!(s, Source::Bundled(_))), "{d:?}");
    }

    /// GLib, GTK, GStreamer and FFmpeg move whole: all included or all from the system.
    #[test]
    fn abi_atomic_families_table() {
        use abi::{decide, family_of, Source};
        use std::collections::HashMap;
        let map = |names: &[&str], dir: &str| -> HashMap<String, PathBuf> {
            names.iter().map(|n| (n.to_string(), PathBuf::from(format!("{dir}/{n}")))).collect()
        };
        let needs = |names: &[&str]| -> Vec<String> { names.iter().map(|n| n.to_string()).collect() };
        let sources = |names: &[&str], host: &[&str], bund: &[&str]| -> String {
            decide(&needs(names), &[], &[], &map(host, "/usr/lib"), &map(bund, "lib"))
                .unwrap()
                .iter()
                .map(|(_, s)| if matches!(s, Source::Host(_)) { 'H' } else { 'B' })
                .collect()
        };
        let glib = ["libglib-2.0.so.0", "libgobject-2.0.so.0", "libgio-2.0.so.0"];
        // The bundle has every GLib library the app needs: the bundle.
        assert_eq!(sources(&glib, &glib, &glib), "BBB");
        // The bundle has only libglib (older) and the system all three: the whole family from the system.
        assert_eq!(sources(&glib, &glib, &glib[..1]), "HHH");
        // Neither side has the whole family: per library, the included one first (mix only if unavoidable).
        assert_eq!(sources(&glib, &glib[1..], &glib[..1]), "BHH");
        // Only the system has them: the system.
        assert_eq!(sources(&glib, &glib, &[]), "HHH");
        // GTK with GDK, GdkPixbuf, ATK, Pango, Cairo.
        let gtk = ["libgtk-3.so.0", "libgdk-3.so.0", "libgdk_pixbuf-2.0.so.0", "libatk-1.0.so.0", "libpango-1.0.so.0", "libcairo.so.2"];
        assert_eq!(sources(&gtk, &gtk, &gtk), "BBBBBB");
        assert_eq!(sources(&gtk, &gtk, &gtk[..3]), "HHHHHH");
        let gtk4 = ["libgtk-4.so.1", "libgdk_pixbuf-2.0.so.0"];
        assert_eq!(sources(&gtk4, &gtk4, &gtk4[1..]), "HH");
        // GStreamer: libgst*.
        let gst = ["libgstreamer-1.0.so.0", "libgstbase-1.0.so.0", "libgstvideo-1.0.so.0"];
        assert_eq!(sources(&gst, &gst, &gst[..2]), "HHH");
        assert_eq!(sources(&gst, &gst, &gst), "BBB");
        // FFmpeg: all or nothing.
        let ff = ["libavcodec.so.60", "libavformat.so.60", "libavutil.so.58", "libswscale.so.7"];
        assert_eq!(sources(&ff, &ff, &ff[..3]), "HHHH");
        assert_eq!(sources(&ff, &ff, &ff), "BBBB");
        // A library of no family is not tied to them.
        assert_eq!(sources(&["libz.so.1", "libglib-2.0.so.0"], &["libz.so.1", "libglib-2.0.so.0"], &["libz.so.1"]), "BH");
        // Membership.
        for (n, fam) in [
            ("libglib-2.0.so.0", Some("GLib")), ("libgmodule-2.0.so.0", Some("GLib")), ("libgtk-x11-2.0.so.0", Some("GTK")),
            ("libgdk-3.so.0", Some("GTK")), ("libatspi.so.0", Some("GTK")), ("libpangocairo-1.0.so.0", Some("GTK")),
            ("libgstapp-1.0.so.0", Some("GStreamer")), ("libavfilter.so.9", Some("FFmpeg")), ("libswresample.so.4", Some("FFmpeg")),
            ("libQt6Core.so.6", Some("Qt6")), ("libKF5Auth.so.5", Some("Qt5")), ("libz.so.1", None), ("libcairo.so.1", None),
        ] {
            assert_eq!(family_of(n), fam, "{n}");
        }
    }

    /// A choice from the symbol check: it moves a library that has two copies, but never the ones the
    /// system must win, and not in a bundle with its own glibc.
    #[test]
    fn abi_symbol_check_choices_are_applied_with_limits() {
        use abi::{decide_with, Source};
        use std::collections::HashMap;
        let map = |names: &[&str], dir: &str| -> HashMap<String, PathBuf> {
            names.iter().map(|n| (n.to_string(), PathBuf::from(format!("{dir}/{n}")))).collect()
        };
        let both = ["libfoo.so.1", "libGL.so.1", "libm.so.6"];
        let needs: Vec<String> = both.iter().map(|n| n.to_string()).collect();
        let prefs: HashMap<String, bool> = both.iter().map(|n| (n.to_string(), true)).collect();
        let d = decide_with(&needs, &[], &[], &map(&both, "/usr/lib"), &map(&both, "lib"), &prefs).unwrap();
        // libfoo follows the choice (system); libGL and libm are the system's anyway.
        assert!(d.iter().all(|(_, s)| matches!(s, Source::Host(_))));
        let back: HashMap<String, bool> = both.iter().map(|n| (n.to_string(), false)).collect();
        let d = decide_with(&needs, &[], &[], &map(&both, "/usr/lib"), &map(&both, "lib"), &back).unwrap();
        // The choice "bundled" is honoured for libfoo only: libGL and libm stay with the system.
        assert!(matches!(d[0].1, Source::Bundled(_)) && matches!(d[1].1, Source::Host(_)) && matches!(d[2].1, Source::Host(_)), "{d:?}");
        // prefer_host is stronger than a choice.
        let d = decide_with(&needs, &["libfoo*".into()], &[], &map(&both, "/usr/lib"), &map(&both, "lib"), &back).unwrap();
        assert!(matches!(d[0].1, Source::Host(_)));
        // A library that only modules ask for is decided like any other when something provides it; it is left out only when nothing does.
        let d = decide_with(&needs, &[], &["libfoo*".into()], &map(&both, "/usr/lib"), &map(&both, "lib"), &prefs).unwrap();
        assert!(d.iter().any(|(n, _)| n == "libfoo.so.1"));
        // A bundle with its own glibc ignores the choices: everything is the bundle's.
        let w = ["libc.so.6", "libfoo.so.1"];
        let wn: Vec<String> = w.iter().map(|n| n.to_string()).collect();
        let d = decide_with(&wn, &[], &[], &map(&w, "/usr/lib"), &map(&w, "lib"), &prefs).unwrap();
        assert!(d.iter().all(|(_, s)| matches!(s, Source::Bundled(_))), "{d:?}");
    }

    #[test]
    fn abi_glibc_family_is_atomic() {
        use abi::{decide, Source};
        use std::collections::HashMap;
        // Bundle with complete 2.36 glibc, host with 2.39: the whole family goes
        // from the bundle (loader included), never mixed.
        let host: HashMap<String, PathBuf> =
            [("libc.so.6", "/lib/libc.so.6"), ("libm.so.6", "/lib/libm.so.6"), ("gtk.so", "/lib/gtk.so")]
                .into_iter().map(|(k, v)| (k.to_string(), PathBuf::from(v))).collect();
        let bund: HashMap<String, PathBuf> =
            [("libc.so.6", "lib/libc.so.6"), ("libm.so.6", "lib/libm.so.6"), ("gtk.so", "lib/gtk.so")]
                .into_iter().map(|(k, v)| (k.to_string(), PathBuf::from(v))).collect();
        let d = decide(&["libc.so.6".into(), "libm.so.6".into(), "gtk.so".into()], &[], &[], &host, &bund).unwrap();
        assert!(d.contains(&("libc.so.6".into(), Source::Bundled("lib/libc.so.6".into()))));
        assert!(d.contains(&("libm.so.6".into(), Source::Bundled("lib/libm.so.6".into()))));
        // B-world: gtk too (in the bundle) comes from the bundle, not the host.
        assert!(d.contains(&("gtk.so".into(), Source::Bundled("lib/gtk.so".into()))));
        // Incomplete family in the bundle: explicit error, no silent mix.
        let mut incomplete = bund.clone();
        incomplete.remove("libm.so.6");
        assert!(decide(&["libm.so.6".into()], &[], &[], &host, &incomplete).is_err());
        // Without libc in the bundle: normal behavior (host).
        let mut no_libc = bund.clone();
        no_libc.remove("libc.so.6");
        no_libc.remove("libm.so.6");
        let d = decide(&["libc.so.6".into()], &[], &[], &host, &no_libc).unwrap();
        assert!(d.contains(&("libc.so.6".into(), Source::Host("/lib/libc.so.6".into()))));
    }

    #[test]
    fn abi_qt_family_is_atomic() {
        use abi::{decide, Source};
        use std::collections::HashMap;
        // Bundle with complete Qt6, host with partial/old Qt6: the whole family goes
        // from the bundle, never mixed (private symbols are not
        // compatible across builds: Qt_PRIVATE_API).
        let host: HashMap<String, PathBuf> =
            [("libQt6Core.so.6", "/lib/libQt6Core.so.6"), ("libQt6Xml.so.6", "/lib/libQt6Xml.so.6")]
                .into_iter().map(|(k, v)| (k.to_string(), PathBuf::from(v))).collect();
        let bund: HashMap<String, PathBuf> =
            [("libQt6Core.so.6", "lib/libQt6Core.so.6"), ("libQt6Xml.so.6", "lib/libQt6Xml.so.6")]
                .into_iter().map(|(k, v)| (k.to_string(), PathBuf::from(v))).collect();
        let d = decide(&["libQt6Core.so.6".into(), "libQt6Xml.so.6".into()], &[], &[], &host, &bund).unwrap();
        assert!(d.contains(&("libQt6Core.so.6".into(), Source::Bundled("lib/libQt6Core.so.6".into()))));
        assert!(d.contains(&("libQt6Xml.so.6".into(), Source::Bundled("lib/libQt6Xml.so.6".into()))));
        // Bundle without QtCore (thin bundle on system Qt): everything from the host.
        let thin: HashMap<String, PathBuf> =
            [("libQt6Xml.so.6", "lib/libQt6Xml.so.6")].into_iter().map(|(k, v)| (k.to_string(), PathBuf::from(v))).collect();
        let d = decide(&["libQt6Core.so.6".into(), "libQt6Xml.so.6".into()], &[], &[], &host, &thin).unwrap();
        assert!(d.contains(&("libQt6Core.so.6".into(), Source::Host("/lib/libQt6Core.so.6".into()))));
        assert!(d.contains(&("libQt6Xml.so.6".into(), Source::Host("/lib/libQt6Xml.so.6".into()))));
        // prefer_host stays supreme even inside the family.
        let d = decide(&["libQt6Core.so.6".into()], &["libQt6Core*".to_string()], &[], &host, &bund).unwrap();
        assert!(d.contains(&("libQt6Core.so.6".into(), Source::Host("/lib/libQt6Core.so.6".into()))));
        // Incomplete bundle (a KF member missing) but complete host:
        // the whole family from the host, never mixed.
        let mut partial = bund.clone();
        partial.remove("libQt6Xml.so.6");
        let d = decide(&["libQt6Core.so.6".into(), "libQt6Xml.so.6".into()], &[], &[], &host, &partial).unwrap();
        assert!(d.contains(&("libQt6Core.so.6".into(), Source::Host("/lib/libQt6Core.so.6".into()))));
        assert!(d.contains(&("libQt6Xml.so.6".into(), Source::Host("/lib/libQt6Xml.so.6".into()))));
    }

    #[test]
    fn split_stub_flags_open() {
        let dir = tmpdir("flags");
        let file = dir.join("a.txt");
        fs::write(&file, b"x").unwrap();
        let canon = fs::canonicalize(&file).unwrap();
        let args = vec![
            "--network-unfiltered".to_string(),
            "--panpak-open".to_string(),
            file.to_string_lossy().into_owned(),
            "--".to_string(),
            "x".to_string(),
        ];
        let (unfiltered, open, app) = split_stub_flags(args).unwrap();
        assert!(unfiltered);
        assert_eq!(open, Some((canon.clone(), OpenGrant { write: true, rename: false })));
        assert_eq!(app, vec!["--".to_string(), "x".to_string()]);
        assert!(split_stub_flags(vec!["--panpak-open".to_string()]).is_err());
        assert!(split_stub_flags(vec!["--panpak-open-readonly".to_string()]).is_err());
        assert!(split_stub_flags(vec!["--panpak-open-rename".to_string()]).is_err());
        let ro = split_stub_flags(vec!["--panpak-open".to_string(), file.to_string_lossy().into_owned(), "--panpak-open-readonly".to_string()]).unwrap();
        assert_eq!(ro.1, Some((canon.clone(), OpenGrant { write: false, rename: false })), "read only on request");
        assert!(split_stub_flags(vec!["--panpak-open".to_string(), file.to_string_lossy().into_owned(), "--panpak-open-readonly".to_string(), "--panpak-open-rename".to_string()]).is_err());
        let old = split_stub_flags(vec!["--panpak-open".to_string(), file.to_string_lossy().into_owned(), "--panpak-open-write".to_string()]).unwrap_err();
        assert!(panpak_core::render(&old, ErrorCode::E2000).contains("--panpak-open-write is no longer needed"));
        // Usage messages: say what is missing, no internal traces.
        let e = split_stub_flags(vec!["--panpak-open".to_string()]).unwrap_err();
        assert_eq!(panpak_core::code_of(&e, ErrorCode::E2000), ErrorCode::E5002, "open");
        let e = split_stub_flags(vec!["--panpak-open-readonly".to_string()]).unwrap_err();
        assert_eq!(panpak_core::code_of(&e, ErrorCode::E2000), ErrorCode::E5001, "readonly");
        let m = panpak_core::render(&e, ErrorCode::E2000);
        assert!(!m.contains("Os {") && !m.contains("panicked"), "internal trace: {m}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sandbox_open_file_only_that_one() {
        if !crate::testutil::alone() {
            return;
        }
        // Fixture outside /tmp/appdata/tree: the `open` rule must
        // cover only the exact file, even in the same folder.
        let (dir, panpak, tree, appdata) = fixture_tree("open-ro", "");
        let outside = outside_tmpdir("open-ro");
        let docs = outside.join("docs");
        fs::create_dir_all(&docs).unwrap();
        // The granted file contains spaces and special characters.
        let note = docs.join("spaces and (parens) [1].txt");
        fs::write(&note, b"content\n").unwrap();
        let sibling = docs.join("secret.txt");
        fs::write(&sibling, b"top-secret\n").unwrap();
        // Symlink to a non-granted path (same folder, other file).
        std::os::unix::fs::symlink(&sibling, docs.join("link-outside")).unwrap();
        // Hardlink to a NON-granted inode (same file as the sibling, other path):
        // stays denied like the target.
        std::fs::hard_link(&sibling, docs.join("secret-copy")).unwrap();
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, Some((&note, OpenGrant { write: false, rename: false })))?;
            let ok = |p: &std::path::Path| std::fs::read(p).is_ok();
            let no = |p: &std::path::Path| std::fs::read(p).is_err();
            res.push(format!("open_granted={}\n", if ok(&note) { "ok" } else { "FAIL" }));
            denied(res, "open_sibling", no(&sibling));
            denied(res, "open_symlink_out", no(&docs.join("link-outside")));
            denied(res, "open_hardlink_alias", no(&docs.join("secret-copy")));
            // Write denied (RO): both on the granted file and elsewhere.
            let no_w = std::fs::OpenOptions::new().write(true).open(&note).is_err();
            denied(res, "write_granted_ro", no_w);
            let no_create = std::fs::File::create(docs.join("new.txt")).is_err();
            denied(res, "create_sibling", no_create);
            Ok(())
        });
        assert!(res.contains("open_granted=ok"), "granted file with spaces/special characters unreadable:\n{res}");
        for op in ["open_sibling", "open_symlink_out", "open_hardlink_alias", "write_granted_ro", "create_sibling"] {
            assert!(res.contains(&format!("{op}=deny")), "{op} NOT blocked:\n{res}");
        }
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn sandbox_open_file_is_saved_in_place_by_default_and_its_folder_stays_closed() {
        if !crate::testutil::alone() {
            return;
        }
        use std::io::Write as _;
        let (dir, panpak, tree, appdata) = fixture_tree("open-rw", "");
        let outside = outside_tmpdir("open-rw");
        let target = outside.join("written.txt");
        fs::write(&target, b"v0\n").unwrap();
        let elsewhere = outside.join("elsewhere.txt");
        fs::write(&elsewhere, b"x\n").unwrap();
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, Some((&target, OpenGrant { write: true, rename: false })))?;
            let ok_w = std::fs::OpenOptions::new().write(true).truncate(true).open(&target)
                .and_then(|mut o| o.write_all(b"v1\n").map(|_| ()))
                .is_ok();
            res.push(format!("write_granted={}\n", if ok_w { "ok" } else { "FAIL" }));
            let no_elsewhere =
                std::fs::OpenOptions::new().write(true).truncate(true).open(&elsewhere).is_err();
            denied(res, "write_elsewhere", no_elsewhere);
            denied(res, "read_elsewhere", std::fs::read(&elsewhere).is_err());
            // Save in place (what a program does with Ctrl+S): rewrite the file and flush it. The default grant is this file and no more.
            let saved = std::fs::OpenOptions::new().write(true).truncate(true).open(&target).and_then(|mut o| o.write_all(b"v2\n").and_then(|_| o.sync_all())).is_ok();
            res.push(format!("save_in_place={}\n", if saved { "ok" } else { "FAIL" }));
            // A save by a temporary file and a rename needs the folder: not given (only `--write-rename` gives it).
            denied(res, "temp_file_in_folder", std::fs::File::create(outside.join("written.txt.tmp")).is_err());
            denied(res, "rename_over_file", std::fs::rename(&elsewhere, &target).is_err());
            denied(res, "remove_file", std::fs::remove_file(&target).is_err());
            Ok(())
        });
        assert!(res.contains("write_granted=ok"), "granted write failed:\n{res}");
        assert!(res.contains("write_elsewhere=deny"), "write elsewhere allowed:\n{res}");
        assert!(res.contains("read_elsewhere=deny"), "read elsewhere allowed:\n{res}");
        assert!(res.contains("save_in_place=ok"), "the file could not be saved in place:\n{res}");
        for op in ["temp_file_in_folder", "rename_over_file", "remove_file"] {
            assert!(res.contains(&format!("{op}=deny")), "{op} NOT blocked:\n{res}");
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"v2\n");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn sandbox_open_rename_without_reading_neighbours() {
        if !crate::testutil::alone() {
            return;
        }
        use std::io::Write as _;
        // Atomic-save pattern (temp+rename): granted only with rename,
        // no reading/listing of neighbors.
        let (dir, panpak, tree, appdata) = fixture_tree("open-rename", "");
        let outside = outside_tmpdir("open-rename");
        let target = outside.join("doc.txt");
        fs::write(&target, b"v0\n").unwrap();
        let sibling = outside.join("neighbour.txt");
        fs::write(&sibling, b"private\n").unwrap();
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, Some((&target, OpenGrant { write: false, rename: true })))?;
            // Atomic save on the granted file: create tmp, write, rename.
            let tmp = outside.join("doc.txt.tmp");
            let created = std::fs::File::create(&tmp).is_ok();
            let written = created
                && std::fs::OpenOptions::new().write(true).open(&tmp)
                    .and_then(|mut o| o.write_all(b"v1\n").map(|_| ()))
                    .is_ok();
            let renamed = written && std::fs::rename(&tmp, &target).is_ok();
            res.push(format!("atomic_save={}\n", if renamed { "ok" } else { "FAIL" }));
            denied(res, "sib_read", std::fs::read(&sibling).is_err());
            denied(res, "sib_list", std::fs::read_dir(&outside).is_err());
            Ok(())
        });
        assert!(res.contains("atomic_save=ok"), "atomic save failed:\n{res}");
        assert!(res.contains("sib_read=deny"), "neighbor readable:\n{res}");
        assert!(res.contains("sib_list=deny"), "directory can be listed:\n{res}");
        assert_eq!(std::fs::read(&target).unwrap(), b"v1\n");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn sandbox_open_rename_denied_without_flag() {
        if !crate::testutil::alone() {
            return;
        }
        // Without rename, the temp+rename pattern is denied (fail-closed).
        let (dir, panpak, tree, appdata) = fixture_tree("open-norename", "");
        let outside = outside_tmpdir("open-norename");
        let target = outside.join("doc.txt");
        fs::write(&target, b"v0\n").unwrap();
        let res = fork_lines(&appdata.join("r.txt"), |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, Some((&target, OpenGrant { write: true, rename: false })))?;
            let tmp = outside.join("doc.txt.tmp");
            denied(res, "tmp_create", std::fs::File::create(&tmp).is_err());
            Ok(())
        });
        assert!(res.contains("tmp_create=deny"), "temp can be created without rename:\n{res}");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn private_tmp_writable_and_var_tmp_denied() {
        if !crate::testutil::alone() {
            return;
        }
        use std::io::Write as _;
        // Under sandbox: SQLite-style (O_EXCL 0600 like etilqs_*) ok in the
        // private tmp folder, denied in /var/tmp (default-deny).
        let (dir, panpak, tree, appdata) = fixture_tree("private-tmp-sandbox", "");
        let private_tmp = appdata.join("tmp");
        fs::create_dir_all(&private_tmp).unwrap();
        let results = appdata.join("results.txt");
        let res = fork_lines(&results, |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, None)?;
            let probe = private_tmp.join("etilqs_probe");
            let ok_tmp = fs::OpenOptions::new().read(true).write(true).create_new(true)
                .open(&probe)
                .and_then(|mut o| o.write_all(b"x").map(|_| ()))
                .is_ok();
            res.push(format!("privatetmp={}\n", if ok_tmp { "ok" } else { "FAIL" }));
            denied(res, "vartmp", File::create(format!("/var/tmp/panpak-privatetmp-{}", std::process::id())).is_err());
            Ok(())
        });
        assert!(res.contains("privatetmp=ok"), "private tmp not writable:\n{res}");
        assert!(res.contains("vartmp=deny"), "/var/tmp writable in sandbox:\n{res}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The ioctl right on devices (Landlock V5): refused on the plain nodes, kept on the terminal and the GPU.
    #[test]
    fn sandbox_device_ioctl_only_where_named() {
        if !crate::testutil::alone() {
            return;
        }
        if !require_abi(landlock::ABI::V5, "device ioctl") {
            return;
        }
        let (dir, panpak, tree, appdata) = fixture_tree("dev-ioctl", "");
        let results = appdata.join("results.txt");
        let res = fork_lines(&results, |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, None)?;
            // TCGETS: a device that allows the ioctl answers ENOTTY (or 0); Landlock refuses with EACCES.
            let ioctl_errno = |path: &str| -> Option<i32> {
                let f = fs::OpenOptions::new().read(true).open(path).ok()?;
                let mut buf = [0u8; 64];
                // SAFETY: TCGETS writes at most a `struct termios` (60 bytes) into the buffer.
                let r = unsafe { libc::ioctl(f.as_raw_fd(), libc::TCGETS as _, buf.as_mut_ptr()) };
                Some(if r == 0 { 0 } else { std::io::Error::last_os_error().raw_os_error().unwrap_or(-1) })
            };
            for node in ["/dev/null", "/dev/zero", "/dev/urandom"] {
                match ioctl_errno(node) {
                    Some(e) => res.push(format!("{node}={}\n", if e == libc::EACCES { "deny" } else { "ALLOW" })),
                    None => res.push(format!("{node}=missing\n")),
                }
            }
            // The terminal: only when the test has one.
            if let Some(e) = ioctl_errno("/dev/tty") {
                res.push(format!("tty={}\n", if e == libc::EACCES { "DENY" } else { "ok" }));
            }
            // The GPU: only when there is a render node the user may open.
            if let Some(node) = fs::read_dir("/dev/dri").ok().and_then(|d| d.flatten().map(|e| e.path()).find(|p| p.to_string_lossy().contains("renderD"))) {
                if let Some(e) = ioctl_errno(&node.to_string_lossy()) {
                    res.push(format!("dri={}\n", if e == libc::EACCES { "DENY" } else { "ok" }));
                }
            }
            Ok(())
        });
        for node in ["/dev/null", "/dev/zero", "/dev/urandom"] {
            assert!(res.contains(&format!("{node}=deny")), "ioctl on {node} not refused:\n{res}");
        }
        assert!(!res.contains("tty=DENY"), "ioctl on /dev/tty refused:\n{res}");
        assert!(!res.contains("dri=DENY"), "ioctl on the render node refused:\n{res}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sandbox_tree_readonly_app_writable() {
        if !crate::testutil::alone() {
            return;
        }
        use std::io::Write as _;
        // Fixture: materialized tree (with write-bit strip) + appdata.
        let (dir, panpak, tree, appdata) = fixture_tree("sandbox-ro", "");
        let blob_before = fs::read(
            fs::read_dir(dir.join("store/blobs")).unwrap().next().unwrap().unwrap().path(),
        )
        .unwrap();
        let target = tree.join("bin/hello");
        let results = appdata.join("results.txt");

        let res = fork_lines(&results, |res| {
            let _man = child_sandboxed(res, &panpak, &tree, &appdata, None)?;
            // 1-2: writing and truncating the tree file.
            denied(res, "write", fs::OpenOptions::new().write(true).open(&target).is_err());
            denied(res, "truncate", fs::OpenOptions::new().write(true).truncate(true).open(&target).is_err());
            // 3: creating a new file in the tree.
            denied(res, "create", File::create(tree.join("new")).is_err());
            // 4-5: renaming and deleting.
            denied(res, "rename", fs::rename(&target, tree.join("moved")).is_err());
            denied(res, "unlink", fs::remove_file(&target).is_err());
            // 6: hardlink to a tree file (REFER denied).
            denied(res, "hardlink", fs::hard_link(&target, appdata.join("hl")).is_err());
            // 6b: $HOME (outside appdata) unlistable: default-deny.
            let home_deny = std::env::var("HOME")
                .map(|h| fs::read_dir(h).is_err())
                .unwrap_or(false);
            denied(res, "homedir", home_deny);
            // 7-9: positive — app data and /tmp stay writable,
            // same-dir rename (atomic-save) and /dev/null ok.
            let ok_data = File::create(appdata.join("out.txt"))
                .and_then(|mut o| o.write_all(b"x").map(|_| ()))
                .is_ok();
            res.push(format!("appdata={}\n", if ok_data { "ok" } else { "FAIL" }));
            let ok_rename_same = fs::rename(appdata.join("out.txt"), appdata.join("out2.txt")).is_ok();
            res.push(format!("rename_same={}\n", if ok_rename_same { "ok" } else { "FAIL" }));
            let ok_null = fs::OpenOptions::new().read(true).write(true).open("/dev/null").is_ok();
            res.push(format!("devnull={}\n", if ok_null { "ok" } else { "FAIL" }));
            let tmpf = std::env::temp_dir().join(format!("panpak-ob1-{}", std::process::id()));
            let ok_tmp = File::create(&tmpf).and_then(|mut o| o.write_all(b"x").map(|_| ())).is_ok();
            let _ = fs::remove_file(&tmpf);
            res.push(format!("tmp={}\n", if ok_tmp { "ok" } else { "FAIL" }));
            Ok(())
        });
        for op in ["write", "truncate", "create", "rename", "unlink", "hardlink", "homedir"] {
            assert!(res.contains(&format!("{op}=deny")), "operation {op} NOT blocked:\n{res}");
        }
        assert!(res.contains("appdata=ok"), "app data not writable:\n{res}");
        assert!(res.contains("tmp=ok"), "/tmp not writable:\n{res}");
        assert!(res.contains("rename_same=ok"), "same-directory rename failed:\n{res}");
        assert!(res.contains("devnull=ok"), "cannot open /dev/null:\n{res}");
        // Defense in depth: blobs unchanged and write-bit-less, marker present.
        let blob_after = fs::read(
            fs::read_dir(dir.join("store/blobs")).unwrap().next().unwrap().unwrap().path(),
        )
        .unwrap();
        assert_eq!(blob_before, blob_after, "shared blob modified");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::symlink_metadata(&target).unwrap().permissions().mode() & 0o222, 0);
        assert!(tree.with_extension("readonly").exists(), "missing readonly marker");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_dedup_hardlink_across_trees() {
        use std::os::unix::fs::MetadataExt;
        let dir = tmpdir("dedup");
        let data = b"shared-content";
        let (path, _, chunks, entries) = fake_panpak_with_file(&dir, data);
        let store = dir.join("store");
        let root_a = [0xAAu8; 32];
        let root_b = [0xBBu8; 32];
        let mut f = File::open(&path).unwrap();
        let ta = materialize(&mut f, &store, &root_a, &chunks, &entries).unwrap();
        let mut f = File::open(&path).unwrap();
        let tb = materialize(&mut f, &store, &root_b, &chunks, &entries).unwrap();
        let (ia, ib) = (
            fs::metadata(ta.join("bin/hello")).unwrap().ino(),
            fs::metadata(tb.join("bin/hello")).unwrap().ino(),
        );
        assert_eq!(ia, ib, "same inode across different trees");
        assert!(fs::metadata(ta.join("bin/hello")).unwrap().nlink() >= 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn decide_net_all_combinations() {
        use NetDecision::*;
        // New ABI: always enforce, the flag is irrelevant.
        assert_eq!(decide_net(true, false, false), Enforce);
        assert_eq!(decide_net(true, true, false), Enforce);
        assert_eq!(decide_net(true, false, true), Enforce);
        // Old kernel + network=true: always refuse (not even with the flag).
        assert!(matches!(decide_net(false, true, false), Refuse(_)));
        assert!(matches!(decide_net(false, true, true), Refuse(_)));
        // Old kernel + network=false: default refusal...
        assert!(matches!(decide_net(false, false, false), Refuse(_)));
        // ...opt-out only explicit, with warning.
        assert_eq!(decide_net(false, false, true), UnfilteredWarn);
    }

    /// A fake `gsettings` printing the given values (and sleeping `delay` seconds first).
    fn fake_gsettings(tag: &str, scheme: &str, theme: &str, delay: &str, exit: i32) -> PathBuf {
        let dir = tmpdir(tag);
        let path = dir.join("gsettings");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nsleep {delay}\ncase \"$3\" in color-scheme) echo \"{scheme}\";; gtk-theme) echo \"{theme}\";; esac\nexit {exit}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn dark_theme_from_gsettings() {
        // A script written and then run fails with ETXTBSY if another thread forks while it is open for writing.
        if !crate::testutil::alone() {
            return;
        }
        // GTK_THEME may be set in the environment of the test run: the function respects it.
        if std::env::var_os("GTK_THEME").is_some() {
            eprintln!("SKIP: GTK_THEME is set");
            return;
        }
        let t = std::time::Duration::from_secs(10);
        let g = fake_gsettings("gs-dark", "'prefer-dark'", "'Yaru'", "0", 0);
        assert_eq!(dark_from(Some(&g), t).as_deref(), Some("Yaru:dark"));
        let g = fake_gsettings("gs-empty", "'prefer-dark'", "''", "0", 0);
        assert_eq!(dark_from(Some(&g), t).as_deref(), Some("Adwaita:dark"));
        let g = fake_gsettings("gs-light", "'default'", "'Yaru'", "0", 0);
        assert_eq!(dark_from(Some(&g), t), None);
        let g = fake_gsettings("gs-odd", "'prefer-dark'", "'a;b'", "0", 0);
        assert_eq!(dark_from(Some(&g), t), None, "theme names with odd characters are not passed on");
        let g = fake_gsettings("gs-fail", "'prefer-dark'", "'Yaru'", "0", 1);
        assert_eq!(dark_from(Some(&g), t), None);
        assert_eq!(dark_from(None, t), None);
        assert_eq!(dark_from(Some(Path::new("/nonexistent/gsettings")), t), None);
    }

    /// The query as the start makes it: started with the program and the time limit, read at the end.
    fn dark_from(g: Option<&Path>, t: std::time::Duration) -> Option<String> {
        gtk_dark_theme(g.map(|g| InterfaceQuery::start(g, t)))
    }

    #[test]
    fn slow_gsettings_is_stopped_at_the_deadline() {
        // A script written and then run fails with ETXTBSY if another thread forks while it is open for writing.
        if !crate::testutil::alone() {
            return;
        }
        if std::env::var_os("GTK_THEME").is_some() {
            return;
        }
        let g = fake_gsettings("gs-slow", "'prefer-dark'", "'Yaru'", "5", 0);
        let t0 = std::time::Instant::now();
        assert_eq!(dark_from(Some(&g), GSETTINGS_TIMEOUT), None);
        let took = t0.elapsed();
        assert!(took < std::time::Duration::from_millis(900), "waited {took:?}");
    }

    #[test]
    fn local_sockets_are_hidden_from_the_environment() {
        use std::ffi::OsStr;
        let state = |cmd: &Command, name: &str| cmd.get_envs().find(|(k, _)| *k == OsStr::new(name)).map(|(_, v)| v.map(|v| v.to_owned()));
        let hidden = Some(Some("disabled:".into()));
        let mut cmd = Command::new("true");
        hide_local_sockets(&mut cmd, false, false);
        for v in ["SSH_AUTH_SOCK", "GPG_AGENT_INFO", "SSH_AGENT_PID"] {
            assert_eq!(state(&cmd, v), Some(None), "{v} must be removed");
        }
        assert_eq!(state(&cmd, "DBUS_SESSION_BUS_ADDRESS"), hidden);
        assert_eq!(state(&cmd, "DBUS_SYSTEM_BUS_ADDRESS"), hidden);
        // Each bus is kept only when the manifest lists it; the agents always go.
        let mut cmd = Command::new("true");
        hide_local_sockets(&mut cmd, true, false);
        assert_eq!(state(&cmd, "DBUS_SESSION_BUS_ADDRESS"), None);
        assert_eq!(state(&cmd, "DBUS_SYSTEM_BUS_ADDRESS"), hidden);
        let mut cmd = Command::new("true");
        hide_local_sockets(&mut cmd, false, true);
        assert_eq!(state(&cmd, "DBUS_SESSION_BUS_ADDRESS"), hidden);
        assert_eq!(state(&cmd, "DBUS_SYSTEM_BUS_ADDRESS"), None);
        assert_eq!(state(&cmd, "SSH_AUTH_SOCK"), Some(None));
    }

    #[test]
    fn gsettings_is_only_looked_for_in_system_paths() {
        // The lookup reads no environment variable: the candidates are the whole input.
        assert!(GSETTINGS_PATHS.iter().all(|p| Path::new(p).is_absolute() && (p.starts_with("/usr/") || p.starts_with("/bin/"))));
        assert!(find_gsettings().is_none_or(|p| GSETTINGS_PATHS.iter().any(|g| Path::new(g) == p)));
        // The end-to-end check, with a program planted in PATH, is in tests/app_dirs.py.
    }

    #[test]
    fn landlock_abi_detected() {
        let abi = panpak_core::detect_abi().expect("Landlock unavailable on this kernel");
        assert!(abi >= landlock::ABI::V1);
        // Visible with --nocapture: needed for the report on the obtained ABI.
        eprintln!("Detected Landlock ABI: {abi:?}");
    }

    #[test]
    fn sockets_allow_requires_v9() {
        // On kernels < V9 the allowlist must fail hard, never pass unnoticed.
        let abi = panpak_core::detect_abi().unwrap();
        if abi >= landlock::ABI::V9 {
            eprintln!("SKIP: kernel supports V9, guard does not trigger here");
            return;
        }
        let (_dir, panpak, tree, appdata) = fixture_tree("ux-guard", "");
        let mut f = File::open(&panpak).unwrap();
        let ft = read_footer(&mut f).unwrap();
        let (mut man, _) = verify(&mut f, &ft).unwrap();
        man.permissions = Some(panpak_core::Permissions {
            x11: None,
            audio: None,
            xdg_dirs: None,
            network: None,
            network_allow_tcp: None,
            network_bind_tcp: None,
            sockets_allow: Some(vec!["/run/dbus.sock".into()]),
            home_paths: None,
            portal: None,
            notifications: None,
            tray: None,
            dbus_own: None,
            mpris: None,
            devices: None,
            exec_written: None,
        });
        let access = Access { rw: vec![appdata.clone()], ..Access::default() };
        let err = build_ruleset(&tree, &access, &man, abi, true, false, None).unwrap_err();
        assert!(format!("{err:?}").contains("V9"), "missing V9 guard: {err:?}");
        let _ = (_dir, panpak, tree, appdata);
    }

    /// Folders for the generated files, as in a store, and a folder outside them that a hostile app would like to write in.
    fn generated_lab(tag: &str) -> (PathBuf, panpak_core::AppDirs, PathBuf) {
        let t = std::env::temp_dir().join(format!("panpak-gen-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let root = t.join("root");
        let d = panpak_core::AppDirs { root: root.clone(), config: root.join("config"), data: root.join("data"), state: root.join("state"), tmp: root.join("tmp"), cache: root.join("cache") };
        for p in [&d.config, &d.data, &d.state, &d.tmp, &d.cache] {
            fs::create_dir_all(p).unwrap();
        }
        let outside = t.join("outside");
        fs::create_dir_all(&outside).unwrap();
        (t, d, outside)
    }

    fn call_fonts(d: &panpak_core::AppDirs) -> Result<()> {
        write_fonts_conf_with(d, Path::new("/home/u/.config"), Path::new("/home/u/.local/share"), Path::new("/home/u"))
    }

    /// PP-02: the files that the outer stub writes in the folders of the app, which the app can write too, never go through a link that the app planted.
    #[test]
    fn generated_files_are_not_made_through_a_folder_that_is_a_link() {
        use std::os::unix::fs::symlink;
        // A folder that is a link: the file would be made in the folder it points to.
        let (t, d, outside) = generated_lab("dir");
        symlink(&outside, d.config.join("fontconfig")).unwrap();
        let _ = call_fonts(&d);
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0, "fonts.conf was written through a link to a folder");
        symlink(&outside, d.data.join("applications")).unwrap();
        let _ = write_opener_config_in(&d, "/x/xdg-open");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0, "the opener entry was written through a link to a folder");
        let _ = fs::remove_dir_all(&t);
    }

    #[test]
    fn generated_files_do_not_overwrite_what_a_planted_link_names_in_the_place_of_the_temporary_file() {
        use std::os::unix::fs::symlink;
        let pid = std::process::id();
        // A link where the temporary file will be made: the file it points to would be overwritten.
        let (t, d, outside) = generated_lab("tmp");
        let victim = outside.join("victim");
        fs::write(&victim, "keep").unwrap();
        fs::create_dir_all(d.config.join("fontconfig")).unwrap();
        symlink(&victim, d.config.join("fontconfig").join(format!(".fonts.conf.{pid}"))).unwrap();
        symlink(&victim, d.config.join(format!(".user-dirs.dirs.{pid}"))).unwrap();
        fs::create_dir_all(d.data.join("applications")).unwrap();
        symlink(&victim, d.config.join(format!("mimeapps.tmp{pid}"))).unwrap();
        symlink(&victim, d.data.join("applications").join(format!("panpak-opener.tmp{pid}"))).unwrap();
        let _ = call_fonts(&d);
        let _ = write_user_dirs(&d, &[]);
        let _ = write_opener_config_in(&d, "/x/xdg-open");
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep", "a file that a link in the folder of the app names was overwritten");
        let _ = fs::remove_dir_all(&t);
    }

    #[test]
    fn generated_files_are_written_and_a_file_of_the_user_stays() {
        let (t2, d2, _) = generated_lab("ok");
        call_fonts(&d2).unwrap();
        write_user_dirs(&d2, &[]).unwrap();
        write_opener_config_in(&d2, "/x/xdg-open").unwrap();
        assert!(d2.config.join("fontconfig/fonts.conf").is_file() && d2.config.join("user-dirs.dirs").is_file() && d2.config.join("mimeapps.list").is_file());
        assert!(d2.data.join("applications/panpak-opener.desktop").is_file());
        // A file of the user's (no marker) stays.
        fs::write(d2.config.join("user-dirs.dirs"), "mine\n").unwrap();
        write_user_dirs(&d2, &[]).unwrap();
        assert_eq!(fs::read_to_string(d2.config.join("user-dirs.dirs")).unwrap(), "mine\n");
        let _ = fs::remove_dir_all(&t2);
    }

    #[test]
    fn the_folder_of_write_rename_is_not_the_home_nor_a_place_that_starts_programs() {
        let home = Path::new("/home/u");
        let refused = |f: &str| rename_scope_refusal(Path::new(f), Some(home)).is_some();
        assert!(refused("/home/u/doc.txt"), "the home itself");
        assert!(refused("/home/doc.txt"), "a folder that holds the home");
        assert!(refused("/doc.txt"));
        assert!(refused("/home/u/.config/app.conf"), "holds .config/autostart");
        assert!(refused("/home/u/.local/x"), "holds .local/bin");
        assert!(refused("/home/u/.ssh/config"), "inside .ssh");
        assert!(refused("/home/u/.config/autostart/a.desktop"), "inside autostart");
        assert!(!refused("/home/u/Documents/note.txt"));
        assert!(!refused("/home/u/projects/x/y/z.txt"));
        assert!(!refused("/home/u/.config/myapp/x.conf"), "a folder of its own below .config");
        assert!(!refused("/mnt/data/note.txt"), "outside the home");
        let why = rename_scope_refusal(Path::new("/home/u/doc.txt"), Some(home)).unwrap();
        assert!(why.contains("the default") && why.contains("folder of its own"), "{why}");
    }

    #[test]
    fn the_open_flags_say_that_the_portal_made_them_unnecessary() {
        let p = PathBuf::from("/home/u/doc.txt");
        let rename = open_flags_note(Some(&(p.clone(), OpenGrant { write: true, rename: true }))).unwrap();
        assert!(rename.contains("--write-rename is not used") && rename.contains("/home/u/doc.txt") && rename.contains("folder is not opened"), "{rename}");
        assert_eq!(open_flags_note(Some(&(p.clone(), OpenGrant { write: true, rename: false }))), None, "writing the file is the default: nothing to say");
        assert_eq!(open_flags_note(Some(&(p, OpenGrant { write: false, rename: false }))), None, "read only: nothing to say");
        assert_eq!(open_flags_note(None), None);
    }
}
