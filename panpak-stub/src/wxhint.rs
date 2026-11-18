//! A hint for the log when an app that ended with an error had written a program file.
//!
//! The rule "nothing the app can write is executed" is a Landlock rule, and Landlock tells nobody when it refuses an
//! `execve`: the app gets EACCES and may or may not say why. The outer stub, which stays while the app runs, looks
//! for the likely cause once the app has ended with an error: program files (the execute bit set) that were written
//! since the start, in the private folders of the app. It is a hint, not a proof: it names a file and the permission
//! that would allow it. `panpak trace` (step 13t) is to say it from the refusal itself.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// How many entries are looked at, so that a very large home costs a bounded time at the end of a run.
const LIMIT: usize = 20_000;

/// The files below `roots` with an execute bit that were changed at or after `since`, at most five, and whether the
/// search stopped at the limit. Links are not followed.
pub fn written_programs(roots: &[&Path], since: SystemTime) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut seen = 0usize;
    let mut todo: Vec<PathBuf> = roots.iter().map(|p| p.to_path_buf()).collect();
    while let Some(dir) = todo.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > LIMIT || found.len() >= 5 {
                return found;
            }
            let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
            if m.is_dir() {
                todo.push(e.path());
            } else if m.is_file() && m.permissions().mode() & 0o111 != 0 && m.modified().is_ok_and(|t| t >= since) {
                found.push(e.path());
            }
        }
    }
    found.sort();
    found
}

/// The line for the log, in plain words; None when there is nothing to say.
pub fn message(app: &str, programs: &[PathBuf]) -> Option<String> {
    let first = programs.first()?;
    Some(format!(
        "{app} ended with an error after it wrote the program {}. Programs that an app writes are not run: if it needs to run what it downloads, its package has to ask for the permission exec_written (\"Run programs that it downloads\").",
        first.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, time::Duration};

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-wxhint-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn file(p: &Path, mode: u32) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_program_written_since_the_start_is_named_and_nothing_else_is() {
        let d = dir("a");
        let before = SystemTime::now() - Duration::from_secs(3600);
        file(&d.join("new/tool"), 0o755);
        file(&d.join("new/data.txt"), 0o644);
        symlink("/usr/bin/true", d.join("new/link")).unwrap();
        // Older than the start: not written by this run.
        let old = d.join("old/prog");
        file(&old, 0o755);
        let t = SystemTime::now() - Duration::from_secs(7200);
        fs::File::options().write(true).open(&old).unwrap().set_modified(t).unwrap();
        let found = written_programs(&[&d], before);
        assert_eq!(found, vec![d.join("new/tool")]);
        let m = message("Joplin", &found).unwrap();
        assert!(m.contains("Joplin") && m.contains("new/tool") && m.contains("exec_written"), "{m}");
        assert!(!m.contains("EACCES") && !m.contains("Landlock"), "plain words: {m}");
        assert!(message("Joplin", &[]).is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_search_is_bounded() {
        let d = dir("b");
        for i in 0..8 {
            file(&d.join(format!("p{i}")), 0o755);
        }
        let found = written_programs(&[&d], SystemTime::now() - Duration::from_secs(60));
        assert_eq!(found.len(), 5, "at most five are named");
        let _ = fs::remove_dir_all(&d);
    }
}
