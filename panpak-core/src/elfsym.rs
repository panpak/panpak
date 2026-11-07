// SPDX-License-Identifier: MIT OR Apache-2.0
//! Needed libraries, symbols and symbol versions of an ELF file, from the section headers
//! (`.dynamic`, `.dynsym`, `.dynstr`, `.gnu.version`, `.gnu.version_r`, `.gnu.version_d`). Used to
//! choose between two copies of `libstdc++.so.6` and `libgcc_s.so.1` (see `abi::cxx_runtime_prefs`),
//! to check that the libraries chosen for an app provide what the app needs (`abi::diagnose`) and by
//! `panpak doctor`.
//! Minimal bounds-checked parser, no external crates: it never panics and reads only what the section
//! headers point to, with caps on every count. A file is read through a `Source`: bytes in memory
//! (`parse_syms`, fuzzed) or a file read with `pread`, only the sections that are needed
//! (`read_elf_info`). Little-endian ELF64 only; any other file (a script, a 32-bit or big-endian ELF)
//! gives an empty result.
use anyhow::{ensure, Context, Result};
use std::{borrow::Cow, collections::HashMap, fs::File, os::unix::fs::FileExt, path::Path};

/// Most symbols read from one file (the largest libraries have a few hundred thousand).
const MAX_SYMS: usize = 1 << 21;
/// Most sections read from one file.
const MAX_SECTIONS: usize = 4096;
/// Longest name kept.
const MAX_NAME: usize = 512;
/// Largest piece read from a file at once.
const MAX_READ: usize = 256 << 20;

/// An undefined symbol that a file needs from another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Needed {
    pub name: String,
    /// The version the symbol is asked with (`GLIBC_2.17`), and the file that must provide it.
    pub version: Option<(String, String)>,
}

/// What the file defines and what it needs.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ElfSyms {
    /// Undefined, non-weak symbols.
    pub undefined: Vec<Needed>,
    /// Defined symbols as hashes (`sym_hash`): one for the name alone and, for a versioned symbol,
    /// one more for `name@version`. Hashes, not names: a large file has hundreds of thousands.
    pub defined: Vec<u64>,
    /// Every `(file, version)` pair in `.gnu.version_r`, also those no symbol uses.
    pub verneed: Vec<(String, String)>,
    /// The version names in `.gnu.version_d`, without the base entry.
    pub verdef: Vec<String>,
}

/// What `read_elf_info` finds in a file.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ElfInfo {
    /// The `DT_NEEDED` names (sorted, without duplicates; a name with `$ORIGIN` reduced to its last part).
    pub needs: Vec<String>,
    /// The folders of `DT_RUNPATH` (or `DT_RPATH`), as written (`$ORIGIN` and all), in order.
    pub runpath: Vec<String>,
    pub syms: ElfSyms,
}

/// A fast 64-bit hash (FNV-1a) of a symbol name, and of `name@version` when there is a version.
pub fn sym_hash(name: &[u8], version: Option<&[u8]>) -> u64 {
    const BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = BASIS;
    for &b in name {
        h = (h ^ b as u64).wrapping_mul(PRIME);
    }
    if let Some(v) = version {
        h = (h ^ b'@' as u64).wrapping_mul(PRIME);
        for &b in v {
            h = (h ^ b as u64).wrapping_mul(PRIME);
        }
    }
    h
}

/// Where the bytes of an ELF file come from.
pub trait Source {
    /// `len` bytes at `off`, or None if they are beyond the end or too many.
    fn read(&self, off: usize, len: usize) -> Option<Cow<'_, [u8]>>;
}

impl Source for [u8] {
    fn read(&self, off: usize, len: usize) -> Option<Cow<'_, [u8]>> {
        self.get(off..off.checked_add(len)?).map(Cow::Borrowed)
    }
}

/// A file read with `pread`.
pub struct FileSource {
    file: File,
    size: usize,
}

impl FileSource {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let size = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
        Ok(Self { file, size })
    }
}

impl Source for FileSource {
    fn read(&self, off: usize, len: usize) -> Option<Cow<'_, [u8]>> {
        if len > MAX_READ || off.checked_add(len)? > self.size {
            return None;
        }
        let mut buf = vec![0u8; len];
        self.file.read_exact_at(&mut buf, off as u64).ok()?;
        Some(Cow::Owned(buf))
    }
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o.checked_add(2)?)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o.checked_add(8)?)?.try_into().ok()?))
}

/// A NUL-terminated string at `off` in the string table `tab`.
fn cstr_bytes(tab: &[u8], off: usize) -> Option<&[u8]> {
    let s = tab.get(off..)?;
    let end = s.iter().position(|&c| c == 0)?;
    (end <= MAX_NAME).then(|| &s[..end])
}

fn cstr(tab: &[u8], off: usize) -> Option<String> {
    cstr_bytes(tab, off).map(|b| String::from_utf8_lossy(b).into_owned())
}

#[derive(Clone, Copy)]
struct Section {
    kind: u32,
    off: usize,
    size: usize,
    link: usize,
    info: u32,
    entsize: usize,
}

const SHT_DYNAMIC: u32 = 6;
const SHT_DYNSYM: u32 = 11;
const SHT_VERDEF: u32 = 0x6fff_fffd;
const SHT_VERNEED: u32 = 0x6fff_fffe;
const SHT_VERSYM: u32 = 0x6fff_ffff;

/// Whether the file is a little-endian ELF64.
fn is_elf64(src: &(impl Source + ?Sized)) -> bool {
    src.read(0, 6).is_some_and(|h| &h[..4] == b"\x7fELF" && h[4] == 2 && h[5] == 1)
}

fn sections(src: &(impl Source + ?Sized)) -> Result<Vec<Section>> {
    if !is_elf64(src) {
        return Ok(Vec::new());
    }
    let Some(head) = src.read(0, 64) else { return Ok(Vec::new()) };
    let shoff = u64_at(&head, 0x28).and_then(|v| usize::try_from(v).ok()).unwrap_or(0);
    let shentsize = u16_at(&head, 0x3A).unwrap_or(0) as usize;
    let shnum = u16_at(&head, 0x3C).unwrap_or(0) as usize;
    if shoff == 0 || shnum == 0 {
        return Ok(Vec::new());
    }
    ensure!(shentsize >= 64 && shnum <= MAX_SECTIONS, "unusable section headers");
    let table = src.read(shoff, shnum * shentsize).context("section header beyond the file")?;
    let mut out = Vec::with_capacity(shnum);
    for i in 0..shnum {
        let o = i * shentsize;
        let field = |v: Option<u64>| v.context("truncated section header");
        let kind = u32_at(&table, o + 4).context("truncated section header")?;
        let (off, size) = (field(u64_at(&table, o + 0x18))?, field(u64_at(&table, o + 0x20))?);
        let link = u32_at(&table, o + 0x28).context("truncated section header")?;
        let info = u32_at(&table, o + 0x2C).context("truncated section header")?;
        let entsize = field(u64_at(&table, o + 0x38))?;
        out.push(Section {
            kind,
            off: usize::try_from(off).unwrap_or(usize::MAX),
            size: usize::try_from(size).unwrap_or(usize::MAX),
            link: link as usize,
            info,
            entsize: usize::try_from(entsize).unwrap_or(0),
        });
    }
    Ok(out)
}

fn body<'a>(src: &'a (impl Source + ?Sized), s: &Section) -> Option<Cow<'a, [u8]>> {
    src.read(s.off, s.size)
}

/// Version index -> (file, name) for the versions a file needs.
type NeedTable = HashMap<u16, (String, String)>;

fn read_verneed(src: &(impl Source + ?Sized), s: &Section, secs: &[Section]) -> (NeedTable, Vec<(String, String)>) {
    let (mut table, mut all) = (HashMap::new(), Vec::new());
    let (Some(data), Some(strs)) = (body(src, s), secs.get(s.link).and_then(|t| body(src, t))) else {
        return (table, all);
    };
    let mut pos = 0usize;
    for _ in 0..(s.info as usize).min(MAX_SECTIONS) {
        let (Some(cnt), Some(file), Some(aux), Some(next)) =
            (u16_at(&data, pos + 2), u32_at(&data, pos + 4), u32_at(&data, pos + 8), u32_at(&data, pos + 12))
        else {
            break;
        };
        let file = cstr(&strs, file as usize).unwrap_or_default();
        let mut a = pos.saturating_add(aux as usize);
        for _ in 0..cnt {
            let (Some(other), Some(name), Some(anext)) = (u16_at(&data, a + 6), u32_at(&data, a + 8), u32_at(&data, a + 12)) else {
                break;
            };
            if let Some(name) = cstr(&strs, name as usize) {
                table.insert(other & 0x7fff, (file.clone(), name.clone()));
                all.push((file.clone(), name));
            }
            if anext == 0 {
                break;
            }
            a = a.saturating_add(anext as usize);
        }
        if next == 0 {
            break;
        }
        pos = pos.saturating_add(next as usize);
    }
    (table, all)
}

fn read_verdef(src: &(impl Source + ?Sized), s: &Section, secs: &[Section]) -> HashMap<u16, String> {
    let mut table = HashMap::new();
    let (Some(data), Some(strs)) = (body(src, s), secs.get(s.link).and_then(|t| body(src, t))) else {
        return table;
    };
    let mut pos = 0usize;
    for _ in 0..(s.info as usize).min(MAX_SECTIONS) {
        let (Some(flags), Some(ndx), Some(aux), Some(next)) =
            (u16_at(&data, pos + 2), u16_at(&data, pos + 4), u32_at(&data, pos + 12), u32_at(&data, pos + 16))
        else {
            break;
        };
        // The base entry (flag 1) names the file itself, not a version.
        if flags & 1 == 0 {
            if let Some(name) = u32_at(&data, pos.saturating_add(aux as usize)).and_then(|n| cstr(&strs, n as usize)) {
                table.insert(ndx & 0x7fff, name);
            }
        }
        if next == 0 {
            break;
        }
        pos = pos.saturating_add(next as usize);
    }
    table
}

fn syms_of(src: &(impl Source + ?Sized), secs: &[Section]) -> Result<ElfSyms> {
    let mut out = ElfSyms::default();
    let (mut needs, mut defs) = (NeedTable::new(), HashMap::new());
    for s in secs.iter().filter(|s| s.kind == SHT_VERNEED) {
        let (t, all) = read_verneed(src, s, secs);
        needs.extend(t);
        out.verneed.extend(all);
    }
    for s in secs.iter().filter(|s| s.kind == SHT_VERDEF) {
        defs.extend(read_verdef(src, s, secs));
    }
    out.verdef = defs.values().cloned().collect();
    out.verdef.sort();
    let Some(dynsym) = secs.iter().find(|s| s.kind == SHT_DYNSYM) else { return Ok(out) };
    let versym = secs.iter().find(|s| s.kind == SHT_VERSYM).and_then(|s| body(src, s));
    let (Some(syms), Some(strs)) = (body(src, dynsym), secs.get(dynsym.link).and_then(|t| body(src, t))) else {
        return Ok(out);
    };
    ensure!(dynsym.entsize == 24 || dynsym.entsize == 0, "unexpected symbol size");
    let count = (syms.len() / 24).min(MAX_SYMS);
    for i in 1..count {
        let o = i * 24;
        let (Some(name), Some(info), Some(shndx)) = (u32_at(&syms, o), syms.get(o + 4), u16_at(&syms, o + 6)) else { break };
        let Some(raw) = cstr_bytes(&strs, name as usize).filter(|n| !n.is_empty()) else { continue };
        let ver = versym.as_ref().and_then(|v| u16_at(v, i * 2)).map(|v| v & 0x7fff).filter(|&v| v > 1);
        if shndx == 0 {
            // Undefined; a weak one may stay unresolved.
            if info >> 4 == 2 {
                continue;
            }
            let version = ver.and_then(|v| needs.get(&v).cloned()).map(|(file, name)| (name, file));
            out.undefined.push(Needed { name: String::from_utf8_lossy(raw).into_owned(), version });
        } else {
            out.defined.push(sym_hash(raw, None));
            if let Some(v) = ver.and_then(|v| defs.get(&v)) {
                out.defined.push(sym_hash(raw, Some(v.as_bytes())));
            }
        }
    }
    Ok(out)
}

fn needs_of(src: &(impl Source + ?Sized), secs: &[Section]) -> Vec<String> {
    let Some(dynamic) = secs.iter().find(|s| s.kind == SHT_DYNAMIC) else { return Vec::new() };
    let (Some(entries), Some(strs)) = (body(src, dynamic), secs.get(dynamic.link).and_then(|t| body(src, t))) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.as_chunks::<16>().0.iter().take(4096) {
        let (Some(tag), Some(val)) = (u64_at(e, 0), u64_at(e, 8)) else { break };
        match tag {
            0 => break,
            1 => {
                if let Some(mut name) = usize::try_from(val).ok().and_then(|o| cstr(&strs, o)) {
                    // A name with $ORIGIN is matched by its last part, as the loader's result would be.
                    if name.contains("$ORIGIN") {
                        name = name.rsplit('/').next().unwrap_or(&name).to_string();
                    }
                    out.push(name);
                }
            }
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Parse the symbols of an ELF file given as bytes.
pub fn parse_syms(b: &[u8]) -> Result<ElfSyms> {
    let secs = sections(b)?;
    syms_of(b, &secs)
}

/// The needs and symbols of an ELF file, reading only the sections that matter. A file that is not
/// a little-endian ELF64 gives an empty result; one without section headers falls back to reading it
/// whole for its `DT_NEEDED` names (it has no symbols to report).
/// The folders of `DT_RUNPATH` (or `DT_RPATH` when there is no `RUNPATH`) of the `.dynamic` section.
fn runpath_of(src: &(impl Source + ?Sized), secs: &[Section]) -> Vec<String> {
    let Some(dynamic) = secs.iter().find(|s| s.kind == SHT_DYNAMIC) else { return Vec::new() };
    let (Some(entries), Some(strs)) = (body(src, dynamic), secs.get(dynamic.link).and_then(|t| body(src, t))) else {
        return Vec::new();
    };
    let (mut rpath, mut runpath) = (None, None);
    for e in entries.as_chunks::<16>().0.iter().take(4096) {
        let (Some(tag), Some(val)) = (u64_at(e, 0), u64_at(e, 8)) else { break };
        let get = || usize::try_from(val).ok().and_then(|o| cstr(&strs, o));
        match tag {
            0 => break,
            15 => rpath = get(),
            29 => runpath = get(),
            _ => {}
        }
    }
    runpath.or(rpath).map(|p| p.split(':').filter(|d| !d.is_empty()).map(str::to_string).collect()).unwrap_or_default()
}

pub fn read_elf_info(path: &Path) -> Result<ElfInfo> {
    let src = FileSource::open(path)?;
    if !is_elf64(&src) {
        return Ok(ElfInfo::default());
    }
    let secs = sections(&src)?;
    if secs.is_empty() {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        return Ok(ElfInfo { needs: crate::elf_needs_bytes(&bytes).unwrap_or_default(), runpath: Vec::new(), syms: ElfSyms::default() });
    }
    Ok(ElfInfo { needs: needs_of(&src, &secs), runpath: runpath_of(&src, &secs), syms: syms_of(&src, &secs)? })
}

/// The libraries of glibc, whose version names (`GLIBC_2.39`) the host must provide.
const GLIBC_FILES: &[&str] = &[
    "libc.so.6", "libm.so.6", "libpthread.so.0", "libdl.so.2", "librt.so.1", "libresolv.so.2", "libutil.so.1", "libanl.so.1",
    "ld-linux-x86-64.so.2", "libmvec.so.1", "libthread_db.so.1",
];

/// The numbers of a `GLIBC_2.39` version name (`[2, 39]`); `GLIBC_PRIVATE` and the others give `None`.
pub fn glibc_version_number(name: &str) -> Option<Vec<u32>> {
    let rest = name.strip_prefix("GLIBC_")?;
    if !rest.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    rest.split('.').map(|p| p.parse::<u32>().ok()).collect()
}

/// The highest `GLIBC_x.y` that an ELF file asks of the glibc libraries (`.gnu.version_r`), reading only that section.
/// `None` when it asks for none (a script, a static file, a file that does not use glibc).
pub fn glibc_needed(path: &Path) -> Result<Option<Vec<u32>>> {
    let src = FileSource::open(path)?;
    if !is_elf64(&src) {
        return Ok(None);
    }
    let mut best: Option<Vec<u32>> = None;
    let secs = sections(&src)?;
    for s in secs.iter().filter(|s| s.kind == SHT_VERNEED) {
        for (file, ver) in read_verneed(&src, s, &secs).1 {
            if !GLIBC_FILES.contains(&file.as_str()) {
                continue;
            }
            if let Some(v) = glibc_version_number(&ver) {
                if best.as_ref().is_none_or(|b| v > *b) {
                    best = Some(v);
                }
            }
        }
    }
    Ok(best)
}

/// The numeric part of a `GLIBCXX_3.4.30`, `CXXABI_1.3.13` or `GCC_4.8.0` version, to compare them.
pub fn version_number(name: &str) -> Option<Vec<u32>> {
    let (_, nums) = name.split_once('_')?;
    nums.split('.').map(|p| p.parse::<u32>().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_and_non_elf_give_empty_results() {
        assert_eq!(parse_syms(b"").unwrap(), ElfSyms::default());
        assert_eq!(parse_syms(b"#!/bin/sh\necho\n").unwrap(), ElfSyms::default());
        let mut elf = vec![0u8; 64];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;
        assert_eq!(parse_syms(&elf).unwrap(), ElfSyms::default());
        // A section header table beyond the file is an error, not a panic.
        elf[0x28..0x30].copy_from_slice(&1_000_000u64.to_le_bytes());
        elf[0x3A..0x3C].copy_from_slice(&64u16.to_le_bytes());
        elf[0x3C..0x3E].copy_from_slice(&3u16.to_le_bytes());
        assert!(parse_syms(&elf).is_err());
    }

    #[test]
    fn truncations_of_a_real_file_never_panic() {
        let real = std::fs::read("/proc/self/exe").unwrap();
        for cut in [0usize, 10, 63, 64, 200, 4096, real.len() / 2, real.len() - 1] {
            let _ = parse_syms(&real[..cut.min(real.len())]);
        }
        let mut bent = real.clone();
        for i in (0..bent.len().min(1 << 16)).step_by(7) {
            bent[i] ^= 0xA5;
        }
        let _ = parse_syms(&bent);
    }

    #[test]
    fn version_names_compare_by_number() {
        let a = version_number("GLIBCXX_3.4.9").unwrap();
        let b = version_number("GLIBCXX_3.4.30").unwrap();
        assert!(b > a);
        assert!(version_number("CXXABI_1.3.13").unwrap() > version_number("CXXABI_1.3.2").unwrap());
        assert_eq!(version_number("GCC_4.8.0"), Some(vec![4, 8, 0]));
        assert_eq!(version_number("GLIBC_PRIVATE"), None);
        assert_eq!(version_number("nonsense"), None);
    }

    #[test]
    fn reading_the_file_and_parsing_its_bytes_agree() {
        // The test binary is static (musl) or dynamic (glibc): both must parse, in both ways.
        let me = std::path::Path::new("/proc/self/exe");
        let from_bytes = parse_syms(&std::fs::read(me).unwrap()).unwrap();
        let info = read_elf_info(me).unwrap();
        assert_eq!(info.syms, from_bytes);
        let needs_from_bytes = crate::elf_needs_bytes(&std::fs::read(me).unwrap()).unwrap();
        assert_eq!(info.needs, needs_from_bytes);
    }

    #[test]
    fn hashes_tell_names_and_versions_apart() {
        assert_ne!(sym_hash(b"foo", None), sym_hash(b"bar", None));
        assert_ne!(sym_hash(b"foo", None), sym_hash(b"foo", Some(b"V1")));
        assert_ne!(sym_hash(b"foo", Some(b"V1")), sym_hash(b"foo", Some(b"V2")));
        assert_eq!(sym_hash(b"foo", Some(b"V1")), sym_hash(b"foo", Some(b"V1")));
    }
}

#[cfg(test)]
mod glibc_tests {
    use super::*;

    /// A minimal ELF64 with section headers: a null section, `.dynamic` (linked to `.dynstr`) and `.dynstr`.
    fn elf_with_dynamic(entries: &[(u64, u64)], strtab: &[u8]) -> Vec<u8> {
        let dynamic: Vec<u8> = entries.iter().chain(&[(0u64, 0u64)]).flat_map(|(t, v)| t.to_le_bytes().into_iter().chain(v.to_le_bytes())).collect();
        let mut f = vec![0u8; 64];
        f[..4].copy_from_slice(b"\x7fELF");
        f[4] = 2;
        f[5] = 1;
        let (dyn_off, str_off) = (64usize, 64 + dynamic.len());
        f.extend(&dynamic);
        f.extend(strtab);
        while f.len() % 8 != 0 {
            f.push(0);
        }
        let shoff = f.len();
        let mut sh = |kind: u32, off: usize, size: usize, link: u32| {
            let mut h = vec![0u8; 64];
            h[4..8].copy_from_slice(&kind.to_le_bytes());
            h[0x18..0x20].copy_from_slice(&(off as u64).to_le_bytes());
            h[0x20..0x28].copy_from_slice(&(size as u64).to_le_bytes());
            h[0x28..0x2C].copy_from_slice(&link.to_le_bytes());
            h
        };
        let table: Vec<u8> = [sh(0, 0, 0, 0), sh(SHT_DYNAMIC, dyn_off, dynamic.len(), 2), sh(3, str_off, strtab.len(), 0)].concat();
        f.extend(table);
        f[0x28..0x30].copy_from_slice(&(shoff as u64).to_le_bytes());
        f[0x3A..0x3C].copy_from_slice(&64u16.to_le_bytes());
        f[0x3C..0x3E].copy_from_slice(&3u16.to_le_bytes());
        f
    }

    #[test]
    fn the_runpath_of_the_dynamic_section_is_read() {
        let dir = std::env::temp_dir().join(format!("panpak-runpath-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // strtab: "\0libfoo.so.1\0$ORIGIN/libproxy:/opt/lib\0"
        let strtab = b"\0libfoo.so.1\0$ORIGIN/libproxy:/opt/lib\0old\0";
        let p = dir.join("a.so");
        std::fs::write(&p, elf_with_dynamic(&[(1, 1), (29, 13)], strtab)).unwrap();
        let info = read_elf_info(&p).unwrap();
        assert_eq!(info.needs, ["libfoo.so.1"]);
        assert_eq!(info.runpath, ["$ORIGIN/libproxy", "/opt/lib"]);
        // DT_RPATH is used only when there is no DT_RUNPATH.
        std::fs::write(&p, elf_with_dynamic(&[(15, 39)], strtab)).unwrap();
        assert_eq!(read_elf_info(&p).unwrap().runpath, ["old"]);
        std::fs::write(&p, elf_with_dynamic(&[(15, 39), (29, 13)], strtab)).unwrap();
        assert_eq!(read_elf_info(&p).unwrap().runpath, ["$ORIGIN/libproxy", "/opt/lib"]);
        // None: an empty list. A value outside the string table: no panic.
        std::fs::write(&p, elf_with_dynamic(&[(1, 1)], strtab)).unwrap();
        assert!(read_elf_info(&p).unwrap().runpath.is_empty());
        std::fs::write(&p, elf_with_dynamic(&[(29, 9999)], strtab)).unwrap();
        assert!(read_elf_info(&p).unwrap().runpath.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_names() {
        assert_eq!(glibc_version_number("GLIBC_2.39"), Some(vec![2, 39]));
        assert_eq!(glibc_version_number("GLIBC_2.2.5"), Some(vec![2, 2, 5]));
        assert_eq!(glibc_version_number("GLIBC_PRIVATE"), None);
        assert_eq!(glibc_version_number("GLIBCXX_3.4.30"), None);
        assert_eq!(glibc_version_number("GLIBC_"), None);
        assert_eq!(glibc_version_number("GLIBC_2.x"), None);
    }

    #[test]
    fn a_program_of_this_system_names_a_glibc_and_a_script_none() {
        let dir = std::env::temp_dir().join(format!("panpak-glibc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("s");
        std::fs::write(&script, b"#!/bin/sh\n").unwrap();
        assert_eq!(glibc_needed(&script).unwrap(), None);
        // The test binary itself is a glibc program on a glibc system (and a static one on musl: nothing to ask).
        if let Some(v) = glibc_needed(&std::env::current_exe().unwrap()).unwrap() {
            assert!(v.len() >= 2 && v[0] == 2, "{v:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
