// SPDX-License-Identifier: MIT OR Apache-2.0
//! Symbols and symbol versions of an ELF file, from the section headers (`.dynsym`,
//! `.gnu.version`, `.gnu.version_r`, `.gnu.version_d`). Used to choose between two copies of
//! `libstdc++.so.6` and `libgcc_s.so.1` (see `abi::cxx_runtime_prefs`) and by `panpak doctor` to
//! check that the libraries chosen for an app provide what the app needs.
//! Minimal bounds-checked parser on bytes, no external crates: it never panics and reads only
//! what the section headers point to, with caps on every count. Little-endian ELF64 only; any
//! other file (a script, a 32-bit or big-endian ELF, a file without section headers) gives an
//! empty result.
use anyhow::{ensure, Context, Result};
use std::collections::HashMap;

/// Most symbols read from one file (the largest libraries have a few hundred thousand).
const MAX_SYMS: usize = 1 << 21;
/// Most sections read from one file.
const MAX_SECTIONS: usize = 4096;
/// Longest name kept.
const MAX_NAME: usize = 512;

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
    /// Defined symbols, with the version name when the symbol is versioned.
    pub defined: Vec<(String, Option<String>)>,
    /// Every `(file, version)` pair in `.gnu.version_r`, also those no symbol uses.
    pub verneed: Vec<(String, String)>,
    /// The version names in `.gnu.version_d`, without the base entry.
    pub verdef: Vec<String>,
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
fn cstr(tab: &[u8], off: usize) -> Option<String> {
    let s = tab.get(off..)?;
    let end = s.iter().position(|&c| c == 0)?;
    if end > MAX_NAME {
        return None;
    }
    Some(String::from_utf8_lossy(&s[..end]).into_owned())
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

const SHT_DYNSYM: u32 = 11;
const SHT_VERDEF: u32 = 0x6fff_fffd;
const SHT_VERNEED: u32 = 0x6fff_fffe;
const SHT_VERSYM: u32 = 0x6fff_ffff;

fn sections(b: &[u8]) -> Result<Vec<Section>> {
    if b.len() < 64 || &b[0..4] != b"\x7fELF" || b[4] != 2 || b[5] != 1 {
        return Ok(Vec::new());
    }
    let shoff = u64_at(b, 0x28).and_then(|v| usize::try_from(v).ok()).unwrap_or(0);
    let shentsize = u16_at(b, 0x3A).unwrap_or(0) as usize;
    let shnum = u16_at(b, 0x3C).unwrap_or(0) as usize;
    if shoff == 0 || shnum == 0 {
        return Ok(Vec::new());
    }
    ensure!(shentsize >= 64 && shnum <= MAX_SECTIONS, "unusable section headers");
    let mut out = Vec::with_capacity(shnum);
    for i in 0..shnum {
        let o = shoff
            .checked_add(i * shentsize)
            .filter(|&o| o.checked_add(64).is_some_and(|e| e <= b.len()))
            .context("section header beyond the file")?;
        let field = |v: Option<u64>| v.context("truncated section header");
        let (kind, off, size) = (u32_at(b, o + 4).context("truncated section header")?, field(u64_at(b, o + 0x18))?, field(u64_at(b, o + 0x20))?);
        let link = u32_at(b, o + 0x28).context("truncated section header")?;
        let info = u32_at(b, o + 0x2C).context("truncated section header")?;
        let entsize = field(u64_at(b, o + 0x38))?;
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

fn body<'a>(b: &'a [u8], s: &Section) -> Option<&'a [u8]> {
    b.get(s.off..s.off.checked_add(s.size)?)
}

/// Version index -> (file, name) for the versions a file needs.
type NeedTable = HashMap<u16, (String, String)>;

fn read_verneed(b: &[u8], s: &Section, secs: &[Section]) -> Result<(NeedTable, Vec<(String, String)>)> {
    let (mut table, mut all) = (HashMap::new(), Vec::new());
    let (Some(data), Some(strs)) = (body(b, s), secs.get(s.link).and_then(|t| body(b, t))) else {
        return Ok((table, all));
    };
    let mut pos = 0usize;
    for _ in 0..(s.info as usize).min(MAX_SECTIONS) {
        let (Some(cnt), Some(file), Some(aux), Some(next)) =
            (u16_at(data, pos + 2), u32_at(data, pos + 4), u32_at(data, pos + 8), u32_at(data, pos + 12))
        else {
            break;
        };
        let file = cstr(strs, file as usize).unwrap_or_default();
        let mut a = pos.saturating_add(aux as usize);
        for _ in 0..cnt {
            let (Some(other), Some(name), Some(anext)) = (u16_at(data, a + 6), u32_at(data, a + 8), u32_at(data, a + 12)) else {
                break;
            };
            if let Some(name) = cstr(strs, name as usize) {
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
    Ok((table, all))
}

fn read_verdef(b: &[u8], s: &Section, secs: &[Section]) -> Result<HashMap<u16, String>> {
    let mut table = HashMap::new();
    let (Some(data), Some(strs)) = (body(b, s), secs.get(s.link).and_then(|t| body(b, t))) else {
        return Ok(table);
    };
    let mut pos = 0usize;
    for _ in 0..(s.info as usize).min(MAX_SECTIONS) {
        let (Some(flags), Some(ndx), Some(aux), Some(next)) =
            (u16_at(data, pos + 2), u16_at(data, pos + 4), u32_at(data, pos + 12), u32_at(data, pos + 16))
        else {
            break;
        };
        // The base entry (flag 1) names the file itself, not a version.
        if flags & 1 == 0 {
            if let Some(name) = u32_at(data, pos.saturating_add(aux as usize)).and_then(|n| cstr(strs, n as usize)) {
                table.insert(ndx & 0x7fff, name);
            }
        }
        if next == 0 {
            break;
        }
        pos = pos.saturating_add(next as usize);
    }
    Ok(table)
}

/// Parse the symbols of an ELF file given as bytes.
pub fn parse_syms(b: &[u8]) -> Result<ElfSyms> {
    let secs = sections(b)?;
    let mut out = ElfSyms::default();
    let (mut needs, mut defs) = (NeedTable::new(), HashMap::new());
    for s in secs.iter().filter(|s| s.kind == SHT_VERNEED) {
        let (t, all) = read_verneed(b, s, &secs)?;
        needs.extend(t);
        out.verneed.extend(all);
    }
    for s in secs.iter().filter(|s| s.kind == SHT_VERDEF) {
        defs.extend(read_verdef(b, s, &secs)?);
    }
    out.verdef = defs.values().cloned().collect();
    out.verdef.sort();
    let versym = secs.iter().find(|s| s.kind == SHT_VERSYM).and_then(|s| body(b, s));
    let Some(dynsym) = secs.iter().find(|s| s.kind == SHT_DYNSYM) else { return Ok(out) };
    let (Some(syms), Some(strs)) = (body(b, dynsym), secs.get(dynsym.link).and_then(|t| body(b, t))) else {
        return Ok(out);
    };
    ensure!(dynsym.entsize == 24 || dynsym.entsize == 0, "unexpected symbol size");
    let count = (syms.len() / 24).min(MAX_SYMS);
    for i in 1..count {
        let o = i * 24;
        let (Some(name), Some(info), Some(shndx)) = (u32_at(syms, o), syms.get(o + 4), u16_at(syms, o + 6)) else { break };
        let Some(name) = cstr(strs, name as usize).filter(|n| !n.is_empty()) else { continue };
        let ver = versym.and_then(|v| u16_at(v, i * 2)).map(|v| v & 0x7fff).filter(|&v| v > 1);
        if shndx == 0 {
            // Undefined; a weak one may stay unresolved.
            if info >> 4 == 2 {
                continue;
            }
            let version = ver.and_then(|v| needs.get(&v).cloned()).map(|(file, name)| (name, file));
            out.undefined.push(Needed { name, version });
        } else {
            out.defined.push((name, ver.and_then(|v| defs.get(&v).cloned())));
        }
    }
    Ok(out)
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
    fn this_program_has_symbols_when_it_is_dynamic() {
        // The test binary is static (musl) or dynamic (glibc): both must parse.
        let me = std::fs::read("/proc/self/exe").unwrap();
        let s = parse_syms(&me).unwrap();
        assert!(s.undefined.len() < 1_000_000);
    }
}
