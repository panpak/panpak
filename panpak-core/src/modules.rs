//! The report of the modules (PANPAK-FORMAT.md §7.2): which libraries the programs of a package need, and which only the modules (plugins, extension modules,
//! workbenches: files that are loaded on demand) need. `pack` makes it from the ELF files of the tree, signs its hash in `[abi].modules`, and puts it in the tree as
//! `.panpak/modules`. A library that a program needs and that nothing provides is an error where the app starts; one that only modules need is "not available": the
//! app starts and the modules that ask for it do not load. Nothing is declared optional by anyone.

use anyhow::{ensure, Context, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

/// Where the report is in the tree.
pub const PATH: &str = ".panpak/modules";
/// The largest report a reader takes.
pub const MAX_REPORT: usize = 4 << 20;
const HEADER: &str = "panpak modules 1";
/// How many modules are named for a library.
const NAMED: usize = 3;

/// One ELF file of the tree, as `pack` reads it.
pub struct ElfFile {
    pub path: String,
    pub is_program: bool,
    /// The names in its `DT_NEEDED`.
    pub needs: Vec<String>,
}

/// A library that only modules need and that the tree does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unbundled {
    pub lib: String,
    /// How many modules ask for it.
    pub count: usize,
    /// The first of them, in order.
    pub by: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The programs of the tree.
    pub programs: Vec<String>,
    /// Every library name that the programs need, directly or through the libraries of the tree.
    pub required: BTreeSet<String>,
    /// Libraries that only modules need and the tree does not provide.
    pub unbundled: Vec<Unbundled>,
}

/// The report of `files`. `bundled` maps a library name (a file name or a SONAME) to the path of the file of the tree that provides it.
pub fn make(files: &[ElfFile], bundled: &HashMap<String, PathBuf>) -> Report {
    let by_path: HashMap<&str, usize> = files.iter().enumerate().map(|(i, f)| (f.path.as_str(), i)).collect();
    let mut seen = vec![false; files.len()];
    let mut queue: Vec<usize> = Vec::new();
    let mut report = Report::default();
    for (i, f) in files.iter().enumerate() {
        if f.is_program {
            seen[i] = true;
            queue.push(i);
            report.programs.push(f.path.clone());
        }
    }
    while let Some(i) = queue.pop() {
        for n in &files[i].needs {
            report.required.insert(n.clone());
            let next = bundled.get(n).and_then(|p| p.to_str()).and_then(|p| by_path.get(p));
            if let Some(&j) = next {
                if !seen[j] {
                    seen[j] = true;
                    queue.push(j);
                }
            }
        }
    }
    report.programs.sort();
    let mut asked: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (i, f) in files.iter().enumerate() {
        if seen[i] {
            continue;
        }
        for n in &f.needs {
            if !bundled.contains_key(n) && !report.required.contains(n) && !n.starts_with("ld-linux") && n != "ld.so" {
                asked.entry(n).or_default().push(&f.path);
            }
        }
    }
    for (lib, mut by) in asked {
        by.sort_unstable();
        by.dedup();
        report.unbundled.push(Unbundled { lib: lib.to_string(), count: by.len(), by: by.iter().take(NAMED).map(|s| s.to_string()).collect() });
    }
    report
}

impl Report {
    /// The text that goes in the tree: one line for each fact, fields separated by a tab, in a fixed order, so that the same tree gives the same bytes.
    pub fn render(&self) -> String {
        let mut out = format!("{HEADER}\n");
        for p in &self.programs {
            out.push_str(&format!("program\t{p}\n"));
        }
        for n in &self.required {
            out.push_str(&format!("needs\t{n}\n"));
        }
        for u in &self.unbundled {
            out.push_str(&format!("module\t{}\t{}\t{}\n", u.lib, u.count, u.by.join("\t")));
        }
        out
    }

    /// The report from its text. Anything that is not exactly the form above is refused: the file is signed content and a reader does not guess.
    pub fn parse(text: &str) -> Result<Report> {
        ensure!(text.len() <= MAX_REPORT, "the modules report is too large");
        let mut lines = text.split('\n');
        ensure!(lines.next() == Some(HEADER), "the modules report does not start with `{HEADER}`");
        let mut r = Report::default();
        let clean = |s: &str| -> Result<String> {
            ensure!(!s.is_empty() && !s.chars().any(char::is_control), "a field of the modules report is empty or has a control character");
            Ok(s.to_string())
        };
        let mut rest: Vec<&str> = lines.collect();
        ensure!(rest.pop() == Some(""), "the modules report does not end with a newline");
        for line in rest {
            let f: Vec<&str> = line.split('\t').collect();
            match f.as_slice() {
                ["program", p] => r.programs.push(clean(p)?),
                ["needs", n] => {
                    r.required.insert(clean(n)?);
                }
                ["module", lib, count, by @ ..] if !by.is_empty() && by.len() <= NAMED => {
                    let count: usize = count.parse().context("count in the modules report")?;
                    r.unbundled.push(Unbundled { lib: clean(lib)?, count, by: by.iter().map(|s| clean(s)).collect::<Result<_>>()? });
                }
                _ => anyhow::bail!("a line of the modules report is not understood: {line:?}"),
            }
        }
        Ok(r)
    }

    /// Of these library names, the ones that no program needs: an unresolved one is not an error.
    pub fn optional_of(&self, needs: &[String]) -> Vec<String> {
        needs.iter().filter(|n| !self.required.contains(n.as_str())).cloned().collect()
    }
}

/// The hash that `[abi].modules` holds: BLAKE3 of the text, in hex.
pub fn hash(text: &[u8]) -> String {
    crate::hex(blake3::hash(text).as_bytes())
}

/// The report of a tree on disk, checked against the hash in the manifest. `None`: the manifest has no `abi.modules` (the package has no ELF file). A report that
/// is missing or does not match its hash is an error: the tree is not the one that was signed.
pub fn load_from_tree(tree: &std::path::Path, manifest: &crate::Manifest) -> Result<Option<Report>> {
    let Some(want) = manifest.abi.as_ref().and_then(|a| a.modules.as_deref()) else { return Ok(None) };
    let bytes = std::fs::read(tree.join(PATH)).with_context(|| format!("read {}", tree.join(PATH).display()))?;
    check(&bytes, want).map(Some)
}

/// The report of a package file (its chunks), checked against the hash in the manifest.
pub fn load_from_package(f: &mut std::fs::File, raw_index: &[u8], manifest: &crate::Manifest) -> Result<Option<Report>> {
    let Some(want) = manifest.abi.as_ref().and_then(|a| a.modules.as_deref()) else { return Ok(None) };
    let bytes = crate::read_package_file(f, raw_index, PATH, MAX_REPORT)?;
    check(&bytes, want).map(Some)
}

fn check(bytes: &[u8], want: &str) -> Result<Report> {
    ensure!(hash(bytes) == want, "the modules report does not match the hash that the manifest signs");
    Report::parse(std::str::from_utf8(bytes).context("the modules report is not UTF-8")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, program: bool, needs: &[&str]) -> ElfFile {
        ElfFile { path: path.into(), is_program: program, needs: needs.iter().map(|s| s.to_string()).collect() }
    }

    fn tree() -> (Vec<ElfFile>, HashMap<String, PathBuf>) {
        let files = vec![
            file("bin/app", true, &["libcore.so.1", "libc.so.6", "libgl.so.1"]),
            file("lib/libcore.so.1", false, &["libutil.so.2", "libc.so.6"]),
            file("lib/libutil.so.2", false, &["libc.so.6"]),
            file("plugins/a.so", false, &["libcore.so.1", "libplug.so.5", "libgl.so.1"]),
            file("plugins/b.so", false, &["libplug.so.5", "libother.so.1"]),
        ];
        let bundled = [("libcore.so.1", "lib/libcore.so.1"), ("libutil.so.2", "lib/libutil.so.2"), ("a.so", "plugins/a.so"), ("b.so", "plugins/b.so")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), PathBuf::from(v)))
            .collect();
        (files, bundled)
    }

    #[test]
    fn what_programs_need_is_required_and_what_only_modules_need_is_not() {
        let (files, bundled) = tree();
        let r = make(&files, &bundled);
        assert_eq!(r.programs, ["bin/app"]);
        // The programs need these, directly or through libraries of the tree.
        assert!(["libcore.so.1", "libc.so.6", "libgl.so.1", "libutil.so.2"].iter().all(|n| r.required.contains(*n)));
        assert!(!r.required.contains("libplug.so.5") && !r.required.contains("libother.so.1"));
        // Only the modules ask for these, and the tree does not have them; the library that a program also needs is not among them.
        let names: Vec<&str> = r.unbundled.iter().map(|u| u.lib.as_str()).collect();
        assert_eq!(names, ["libother.so.1", "libplug.so.5"]);
        assert_eq!(r.unbundled[1].count, 2);
        assert_eq!(r.unbundled[1].by, ["plugins/a.so", "plugins/b.so"]);
        let optional = r.optional_of(&["libplug.so.5".into(), "libgl.so.1".into()]);
        assert_eq!(optional, ["libplug.so.5"], "a library that a program needs is never optional");
    }

    #[test]
    fn the_text_is_the_same_for_the_same_tree_and_parses_back() {
        let (files, bundled) = tree();
        let r = make(&files, &bundled);
        let text = r.render();
        assert_eq!(text, make(&files, &bundled).render());
        assert_eq!(Report::parse(&text).unwrap(), r);
        assert_eq!(hash(text.as_bytes()).len(), 64);
        // Not exactly the form: refused.
        assert!(Report::parse("panpak modules 2\n").is_err());
        assert!(Report::parse(&text.replace("program\t", "prog\t")).is_err());
        assert!(Report::parse(text.trim_end()).is_err(), "a missing final newline");
        assert!(Report::parse("panpak modules 1\nneeds\ta\u{7}b\n").is_err());
    }

    #[test]
    fn a_tree_with_no_program_has_everything_optional() {
        let files = vec![file("lib/a.so", false, &["libx.so.1"])];
        let r = make(&files, &HashMap::new());
        assert!(r.programs.is_empty() && r.required.is_empty());
        assert_eq!(r.optional_of(&["libx.so.1".into()]), ["libx.so.1"]);
    }
}
