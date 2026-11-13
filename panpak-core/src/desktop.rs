// SPDX-License-Identifier: MIT OR Apache-2.0
//! The menu entry and the launcher of an installed app, and their lazy update.
//!
//! The entry is a closed template: the package gives the name, the window class and the URL schemes, each
//! checked and escaped, and nothing else. No key of the original `.desktop` file of an app is ever copied
//! (see docs/DESKTOP-INTEGRATION.md, section 3). The entry and the launcher carry one version,
//! [`DESKTOP_VERSION`]; the stub rewrites both at the start of the app when they are older.

use anyhow::{bail, ensure, Context, Result};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// The version of the entry and the launcher. Raise it when the template changes.
pub const DESKTOP_VERSION: u32 = 1;
/// Only this many bytes of the entry are read to find its version.
pub const VERSION_WINDOW: usize = 128;
pub const VERSION_KEY: &str = "X-Panpak-Desktop-Version=";
pub const DESKTOP_MARKER: &str = "X-Panpak-App-Id=";
pub const LAUNCHER_MARKER: &str = "# X-Panpak-App-Id: ";
const LAUNCHER_VERSION: &str = "# X-Panpak-Desktop-Version: ";

/// The per-user store: `$XDG_DATA_HOME/panpak`.
pub fn store_dir() -> Result<PathBuf> {
    Ok(dirs::data_dir().context("no data dir")?.join("panpak"))
}

/// Fixed paths in the store that the launcher and the entry use (docs/DESKTOP-INTEGRATION.md, section 2).
pub fn stable_panpak(store: &Path) -> PathBuf {
    store.join("bin").join("panpak")
}
pub fn stable_stub(store: &Path) -> PathBuf {
    store.join("bin").join("panpak-stub")
}

/// The launcher of an app: `~/.local/bin/<id>`.
pub fn launcher_path(app_id: &str) -> Result<PathBuf> {
    Ok(dirs::home_dir().context("no home dir")?.join(".local/bin").join(app_id))
}

/// The `.desktop` file of an app: `panpak.<id>.desktop`. The portals know the app as `panpak.<id>` (the
/// name of its systemd scope), and look for the file of that name; the prefix also keeps the entry
/// apart from the Flatpak or distribution entry of an app with the same upstream id.
pub fn desktop_path(app_id: &str) -> Result<PathBuf> {
    Ok(dirs::data_dir().context("no data dir")?.join("applications").join(format!("panpak.{app_id}.desktop")))
}

/// The name this entry had before the prefix: `<id>.desktop`.
pub fn legacy_desktop_path(app_id: &str) -> Result<PathBuf> {
    Ok(dirs::data_dir().context("no data dir")?.join("applications").join(format!("{app_id}.desktop")))
}

/// The icon name of an app that has an icon file of its own.
pub fn icon_name(app_id: &str) -> String {
    format!("panpak.{app_id}")
}
/// The themed icon that every icon theme has, for an app without an icon.
pub const GENERIC_ICON: &str = "application-x-executable";

pub fn path_text(p: &Path) -> Result<&str> {
    let s = p.to_str().context("path is not valid UTF-8")?;
    ensure!(!s.chars().any(char::is_control), "control character in path {s:?}");
    Ok(s)
}

/// POSIX shell single quoting.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Value of a Desktop Entry `string` key (Desktop Entry Specification, "Value types"): backslash, newline,
/// carriage return and tab are escaped, other control characters are refused, so a value can never start a new line.
pub fn desktop_string(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => bail!("control character in a desktop entry value"),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// One `Exec` argument (Desktop Entry Specification, "The Exec key"): always double-quoted, with `"`, `` ` ``, `$`
/// and `\` escaped and `%` doubled, then string-escaped like any other value.
pub fn desktop_exec_arg(p: &str) -> Result<String> {
    ensure!(!p.chars().any(char::is_control), "control character in the launcher path");
    let mut q = String::from("\"");
    for c in p.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                q.push('\\');
                q.push(c);
            }
            '%' => q.push_str("%%"),
            c => q.push(c),
        }
    }
    q.push('"');
    desktop_string(&q)
}

/// What the entry is made from.
pub struct Entry<'a> {
    pub app_id: &'a str,
    pub name: &'a str,
    pub launcher: &'a Path,
    pub schemes: &'a [String],
    pub wm_class: Option<&'a str>,
    /// The stable `panpak` of the store, for the menu action "Uninstall". Without one there is no action.
    pub panpak: Option<&'a Path>,
    /// Whether an icon file of the app is installed.
    pub icon: bool,
}

/// The entry declares no MimeType but the URL schemes: the manifest has no field for MIME types, and
/// `application/x-panpak` is the type of the packages themselves, not of the files an app opens.
pub fn desktop_file(e: &Entry) -> Result<String> {
    // The URL schemes the manifest declares; every name is checked again here, whatever the caller did.
    let mut mime = String::new();
    for sc in e.schemes {
        crate::check_scheme_name(sc).map_err(|why| anyhow::anyhow!("scheme {sc}: {why}"))?;
        mime += &format!("x-scheme-handler/{sc};");
    }
    let mime = if mime.is_empty() { String::new() } else { format!("MimeType={mime}\n") };
    // The window class the app sets, so that the desktop groups its windows under this entry.
    let class = match e.wm_class {
        Some(w) => {
            ensure!(crate::valid_wm_class(w), "wm_class {w:?} is not a plain window class");
            format!("StartupWMClass={w}\n")
        }
        None => String::new(),
    };
    let (actions, action) = match e.panpak {
        Some(p) => (
            "Actions=Uninstall;\n",
            format!(
                "\n[Desktop Action Uninstall]\nName=Uninstall\nName[de]=Deinstallieren\nName[es]=Desinstalar\nName[fr]=D\u{e9}sinstaller\nName[it]=Disinstalla\nExec={} remove --ask {}\n",
                desktop_exec_arg(path_text(p)?)?,
                desktop_string(e.app_id)?
            ),
        ),
        None => ("", String::new()),
    };
    let icon = if e.icon { icon_name(e.app_id) } else { GENERIC_ICON.to_string() };
    Ok(format!(
        "[Desktop Entry]\n{VERSION_KEY}{DESKTOP_VERSION}\nType=Application\nName={}\nExec={} %U\nIcon={}\nTerminal=false\nCategories=Utility;\n{mime}{class}{actions}{DESKTOP_MARKER}{}\n{action}",
        desktop_string(e.name)?,
        desktop_exec_arg(path_text(e.launcher)?)?,
        desktop_string(&icon)?,
        desktop_string(e.app_id)?
    ))
}

/// The launcher script. It runs the stable stub of the store on the `current` link of the app.
pub fn launcher_sh(app_id: &str, stub: &Path, current_link: &Path) -> Result<String> {
    let (rt, cur) = (sh_quote(path_text(stub)?), sh_quote(path_text(current_link)?));
    Ok(format!(
        "#!/bin/sh\n# Generated by `panpak install`: do not edit.\n{LAUNCHER_MARKER}{app_id}\n{LAUNCHER_VERSION}{DESKTOP_VERSION}\n# Points to apps/<id>/current; `panpak update`/`rollback` move it.\n# With one existing file (double click): opens ONLY that file\n# (--panpak-open, read-only): use `panpak open --write` for writes.\nif [ \"$#\" -eq 1 ] && [ -f \"$1\" ]; then\n  case \"$1\" in /*) _f=\"$1\";; *) _f=\"$PWD/$1\";; esac\n  exec {rt} run {cur} --panpak-open \"$_f\" \"$_f\"\nfi\nexec {rt} run {cur} \"$@\"\n"
    ))
}

/// A launcher written by panpak for `app_id`: it carries the marker, or it is one written before the marker
/// existed (same two first lines).
pub fn launcher_is_ours(content: &str, app_id: &str) -> bool {
    content.lines().any(|l| l.strip_prefix(LAUNCHER_MARKER) == Some(app_id))
        || content.starts_with("#!/bin/sh\n# Generated by `panpak install`: do not edit.\n# Points to apps/<id>/current")
}

/// A `.desktop` file written by panpak for `app_id`: it has the marker key, or it is one written before the
/// marker existed (panpak's own `Exec` line).
pub fn desktop_is_ours(content: &str, app_id: &str, launcher: &Path) -> bool {
    content.lines().any(|l| l.strip_prefix(DESKTOP_MARKER) == Some(app_id))
        || (content.starts_with("[Desktop Entry]\nType=Application\n") && content.lines().any(|l| l == format!("Exec={} %U", launcher.display())))
}

/// The version in the first bytes of an entry. Only a line that ends inside the window counts; an entry whose
/// version is further in, or that has none, is read as old and gets rewritten.
pub fn entry_version(head: &[u8]) -> Option<u32> {
    let head = &head[..head.len().min(VERSION_WINDOW)];
    let mut lines = head.split(|&b| b == b'\n').peekable();
    while let Some(l) = lines.next() {
        // The last piece has no newline after it inside the window: the line may go on.
        if lines.peek().is_none() {
            break;
        }
        if let Some(v) = l.strip_prefix(VERSION_KEY.as_bytes()) {
            return std::str::from_utf8(v).ok()?.parse().ok();
        }
    }
    None
}

/// Write `data` next to `path` under a hidden temporary name (created with `O_EXCL` and `O_NOFOLLOW`), sync it,
/// rename it over `path` and sync the folder, so that a crash leaves the old file or the new one. The rename
/// replaces a link itself and never follows it.
pub fn write_file_atomic(path: &Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    write_file_with(path, data, mode, true)
}

/// The same without the two syncs (the file, then the folder): for files that can be made again, such as the icons of an app. The new file replaces the old
/// one in one step all the same, but after a crash it may be empty. Each sync costs about 2 ms, and an install writes six icons.
pub fn write_file_lazy(path: &Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    write_file_with(path, data, mode, false)
}

fn write_file_with(path: &Path, data: &[u8], mode: u32, durable: bool) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| std::io::Error::other("destination without a folder"))?;
    let name = path.file_name().ok_or_else(|| std::io::Error::other("destination without a file name"))?.to_string_lossy();
    let tmp = dir.join(format!(".{name}.panpak-tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let res = (|| {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(mode).custom_flags(libc::O_NOFOLLOW).open(&tmp)?;
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.write_all(data)?;
        if durable {
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        // The entry is already in place; a folder that cannot be synced is not an error.
        if durable {
            let _ = fs::File::open(dir).and_then(|d| d.sync_all());
        }
        Ok(())
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// What the check at the start of an app found.
#[derive(Debug, PartialEq, Eq)]
pub enum Refresh {
    /// The entry has the current version, or a newer one.
    Current,
    /// There is no entry (the user removed it, or the app was not installed with one): nothing is made.
    Missing,
    /// The file is a link, not a regular file, too large, or not written by panpak for this app.
    NotOurs,
    /// The entry and the launcher were older and are rewritten.
    Rewritten,
}

fn read_limited(f: &mut fs::File, max: usize) -> Option<Vec<u8>> {
    let mut b = Vec::new();
    f.take(max as u64 + 1).read_to_end(&mut b).ok()?;
    (b.len() <= max).then_some(b)
}

/// The lazy update (docs/DESKTOP-INTEGRATION.md, section 3). In the usual case this opens the entry, reads
/// [`VERSION_WINDOW`] bytes and closes it. `store` is the per-user store, `launcher` the path of the launcher; the stable stub link must be there for
/// the launcher to work, otherwise nothing is touched.
pub fn refresh_entry(store: &Path, entry_dir: &Path, launcher: &Path, app_id: &str, name: &str, schemes: &[String], wm_class: Option<&str>) -> Result<Refresh> {
    let path = entry_dir.join(format!("panpak.{app_id}.desktop"));
    let mut f = match fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Refresh::Missing),
        // A link in the place of the entry (ELOOP) or anything else odd: not ours to touch.
        Err(_) => return Ok(Refresh::NotOurs),
    };
    let mut head = [0u8; VERSION_WINDOW];
    let n = f.read_at(&mut head, 0)?;
    if entry_version(&head[..n]).is_some_and(|v| v >= DESKTOP_VERSION) {
        return Ok(Refresh::Current);
    }
    // Older, or the version is not where it should be: read it all and see whether panpak wrote it.
    let meta = f.metadata()?;
    if !meta.file_type().is_file() {
        return Ok(Refresh::NotOurs);
    }
    let Some(old) = read_limited(&mut f, 1 << 20).and_then(|b| String::from_utf8(b).ok()) else { return Ok(Refresh::NotOurs) };
    if !desktop_is_ours(&old, app_id, launcher) {
        return Ok(Refresh::NotOurs);
    }
    drop(f);
    let stub = stable_stub(store);
    ensure!(fs::symlink_metadata(&stub).is_ok(), "no stable stub in the store at {}", stub.display());
    let have_icon = old.lines().any(|l| l.strip_prefix("Icon=") == Some(icon_name(app_id).as_str()));
    let panpak = stable_panpak(store);
    let panpak = fs::metadata(&panpak).is_ok_and(|m| m.is_file()).then_some(panpak);
    let text = desktop_file(&Entry { app_id, name, launcher, schemes, wm_class, panpak: panpak.as_deref(), icon: have_icon })?;
    // The launcher first: the entry must never name one that is older than itself.
    let current = store.join("apps").join(app_id).join("current");
    let sh = launcher_sh(app_id, &stub, &current)?;
    let launcher_ok = match fs::symlink_metadata(launcher) {
        Ok(m) if m.file_type().is_file() => fs::read(launcher).ok().and_then(|b| String::from_utf8(b).ok()).is_some_and(|s| launcher_is_ours(&s, app_id)),
        Ok(_) => false,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    };
    if launcher_ok {
        write_file_atomic(launcher, sh.as_bytes(), 0o755).with_context(|| format!("write {}", launcher.display()))?;
    }
    write_file_atomic(&path, text.as_bytes(), 0o644).with_context(|| format!("write {}", path.display()))?;
    Ok(Refresh::Rewritten)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry<'a>(l: &'a Path, name: &'a str) -> Entry<'a> {
        Entry { app_id: "org.x", name, launcher: l, schemes: &[], wm_class: None, panpak: None, icon: false }
    }

    #[test]
    fn the_version_is_the_second_line_and_inside_the_window() {
        let l = Path::new("/home/u/.local/bin/org.x");
        let d = desktop_file(&entry(l, "X")).unwrap();
        assert_eq!(d.lines().nth(1), Some("X-Panpak-Desktop-Version=1"));
        assert_eq!(entry_version(d.as_bytes()), Some(DESKTOP_VERSION));
        assert_eq!(entry_version(&d.as_bytes()[..VERSION_WINDOW.min(d.len())]), Some(DESKTOP_VERSION));
    }

    #[test]
    fn a_version_that_is_not_in_the_window_is_old() {
        let pad = "#".repeat(VERSION_WINDOW);
        let late = format!("[Desktop Entry]\n{pad}\n{VERSION_KEY}1\n");
        assert_eq!(entry_version(late.as_bytes()), None);
        // A line that the window cuts in the middle does not count either.
        let cut = format!("[Desktop Entry]\n{}{VERSION_KEY}9", "#".repeat(VERSION_WINDOW - 16 - VERSION_KEY.len() + 10));
        assert_eq!(entry_version(cut.as_bytes()), None);
        assert_eq!(entry_version(b"[Desktop Entry]\nX-Panpak-Desktop-Version=1"), None, "no newline after the number: it may go on");
        assert_eq!(entry_version(b"[Desktop Entry]\nX-Panpak-Desktop-Version=x\n"), None);
        assert_eq!(entry_version(b"[Desktop Entry]\nType=Application\n"), None);
        assert_eq!(entry_version(b""), None);
        assert_eq!(entry_version(b"[Desktop Entry]\nX-Panpak-Desktop-Version=7\n"), Some(7));
    }

    #[test]
    fn the_uninstall_action_needs_a_panpak_path() {
        let l = Path::new("/home/u/.local/bin/org.x");
        let none = desktop_file(&entry(l, "X")).unwrap();
        assert!(!none.contains("Actions=") && !none.contains("[Desktop Action"), "{none}");
        let p = Path::new("/opt/my panpak/panpak");
        let some = desktop_file(&Entry { panpak: Some(p), ..entry(l, "X") }).unwrap();
        assert!(some.contains("Actions=Uninstall;\n") && some.contains("Exec=\"/opt/my panpak/panpak\" remove --ask org.x\n"), "{some}");
        assert!(desktop_file(&Entry { panpak: Some(Path::new("/opt/p\nanpak")), ..entry(l, "X") }).is_err());
    }

    #[test]
    fn the_icon_line_names_the_app_icon_or_the_generic_one() {
        let l = Path::new("/home/u/.local/bin/org.x");
        assert!(desktop_file(&entry(l, "X")).unwrap().contains("\nIcon=application-x-executable\n"));
        assert!(desktop_file(&Entry { icon: true, ..entry(l, "X") }).unwrap().contains("\nIcon=panpak.org.x\n"));
    }

    #[test]
    fn desktop_entry_values_cannot_start_a_line() {
        let l = Path::new("/home/u/.local/bin/org.x");
        let d = desktop_file(&entry(l, "Evil\nExec=/bin/sh\r\nTerminal=true")).unwrap();
        assert_eq!(d.lines().filter(|l| l.starts_with("Exec=")).count(), 1, "{d}");
        assert!(d.contains("Name=Evil\\nExec=/bin/sh\\r\\nTerminal=true\n"), "{d}");
        assert!(desktop_file(&entry(l, "A\\B")).unwrap().contains("Name=A\\\\B\n"));
        for bad in ["a\u{1}b", "a\u{7f}b", "a\u{9b}b"] {
            assert!(desktop_file(&entry(l, bad)).is_err(), "{bad:?}");
        }
        assert!(desktop_file(&entry(Path::new("/home/u\n/x"), "N")).is_err());
    }

    /// The keys an entry may have. Whatever a package says, no other key is written.
    const ALLOWED: &[&str] = &[
        "Type", "Name", "Exec", "Icon", "Terminal", "Categories", "MimeType", "StartupWMClass", "Actions", "X-Panpak-Desktop-Version", "X-Panpak-App-Id",
    ];
    const ACTION_KEYS: &[&str] = &["Name", "Name[de]", "Name[es]", "Name[fr]", "Name[it]", "Exec"];

    fn keys_ok(d: &str) {
        let mut group = "";
        for l in d.lines().filter(|l| !l.is_empty()) {
            if l.starts_with('[') {
                group = l;
                continue;
            }
            let k = l.split('=').next().unwrap();
            let allowed = if group == "[Desktop Entry]" { ALLOWED } else { ACTION_KEYS };
            assert!(allowed.contains(&k), "key {k:?} is not allowed in {group}: {d}");
            assert!(group == "[Desktop Entry]" || group == "[Desktop Action Uninstall]", "group {group}");
        }
    }

    #[test]
    fn only_the_allowed_keys_are_ever_written() {
        let l = Path::new("/home/u/.local/bin/org.x");
        let p = Path::new("/h/.local/share/panpak/bin/panpak");
        let schemes = vec!["org-x".to_string()];
        let full = Entry { schemes: &schemes, wm_class: Some("Org X"), panpak: Some(p), icon: true, ..entry(l, "X") };
        keys_ok(&desktop_file(&full).unwrap());
        keys_ok(&desktop_file(&entry(l, "X")).unwrap());
        // Hostile values give an error or a file with the same keys.
        let hostile = [
            "Evil\nTryExec=/bin/sh",
            "Evil\r\nDBusActivatable=true",
            "x\nX-GNOME-Autostart-enabled=true\n[Desktop Action Evil]\nExec=sh",
            "\n[Desktop Entry]\nExec=sh",
        ];
        for h in hostile {
            if let Ok(d) = desktop_file(&entry(l, h)) {
                keys_ok(&d);
                assert_eq!(d.lines().filter(|l| l.starts_with('[')).count(), 1, "{d}");
            }
            for bad in [Entry { wm_class: Some(h), ..entry(l, "X") }, Entry { schemes: &[h.to_string()], ..entry(l, "X") }] {
                if let Ok(d) = desktop_file(&bad) {
                    keys_ok(&d);
                }
            }
        }
        for w in ["a=b", "a\nb", "a;b", "x\nTryExec=y"] {
            assert!(desktop_file(&Entry { wm_class: Some(w), ..entry(l, "X") }).is_err(), "{w:?}");
        }
        for s in ["a;b", "a/b", "A b", "x\ny", ""] {
            assert!(desktop_file(&Entry { schemes: &[s.to_string()], ..entry(l, "X") }).is_err(), "{s:?}");
        }
    }

    #[test]
    fn desktop_exec_quotes_reserved_characters() {
        let q = |p: &str| desktop_exec_arg(p).unwrap();
        assert_eq!(q("/home/a b/x"), "\"/home/a b/x\"");
        assert_eq!(q("/h/100%/x"), "\"/h/100%%/x\"");
        assert_eq!(q("/h/a\"b"), "\"/h/a\\\\\"b\"");
        assert_eq!(q("/h/$x"), "\"/h/\\\\$x\"");
        assert_eq!(q("/h/`x`"), "\"/h/\\\\`x\\\\`\"");
        assert_eq!(q("/h/a\\b"), "\"/h/a\\\\\\\\b\"");
    }

    #[test]
    fn the_launcher_runs_the_stable_stub() {
        let stub = Path::new("/home/u/.local/share/panpak/bin/panpak-stub");
        let cur = Path::new("/home/u/.local/share/panpak/apps/org.x/current");
        let sh = launcher_sh("org.x", stub, cur).unwrap();
        assert!(sh.starts_with("#!/bin/sh\n") && sh.contains("\n# X-Panpak-App-Id: org.x\n") && sh.ends_with("\"$@\"\n"));
        assert!(sh.contains("exec '/home/u/.local/share/panpak/bin/panpak-stub' run '/home/u/.local/share/panpak/apps/org.x/current' \"$@\""));
        assert!(!sh.contains("runtimes/"), "{sh}");
        assert!(sh.contains("--panpak-open") && sh.contains("[ -f \"$1\" ]"));
        let q = launcher_sh("org.x", Path::new("/h/it's $(id)/rt"), Path::new("/h/`x`/cur")).unwrap();
        assert!(q.contains("exec '/h/it'\\''s $(id)/rt' run '/h/`x`/cur' \"$@\""), "{q}");
        assert!(launcher_sh("org.x", Path::new("/h/a\nb"), Path::new("/c")).is_err());
        assert!(launcher_is_ours(&sh, "org.x") && !launcher_is_ours(&sh, "org.y"));
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-desktop-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A store with the stable stub, an entry folder and a launcher path, in a scratch folder.
    struct Lab {
        root: PathBuf,
        store: PathBuf,
        apps: PathBuf,
        launcher: PathBuf,
    }

    fn lab(name: &str) -> Lab {
        let root = scratch(name);
        let (store, apps) = (root.join("store"), root.join("applications"));
        fs::create_dir_all(store.join("bin")).unwrap();
        fs::create_dir_all(&apps).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        std::os::unix::fs::symlink("../runtimes/1/panpak-stub", stable_stub(&store)).unwrap();
        let launcher = root.join("bin").join("org.x");
        Lab { root, store, apps, launcher }
    }

    impl Lab {
        fn entry_path(&self) -> PathBuf {
            self.apps.join("panpak.org.x.desktop")
        }
        fn current(&self) -> String {
            let e = Entry { app_id: "org.x", name: "X", launcher: &self.launcher, schemes: &[], wm_class: None, panpak: None, icon: false };
            desktop_file(&e).unwrap()
        }
        fn refresh(&self) -> Result<Refresh> {
            refresh_entry(&self.store, &self.apps, &self.launcher, "org.x", "X", &[], None)
        }
    }

    /// An entry as 7a wrote it: no version, no icon.
    fn old_entry(l: &Path) -> String {
        format!("[Desktop Entry]\nType=Application\nName=X\nExec=\"{}\" %U\nTerminal=false\nCategories=Utility;\nX-Panpak-App-Id=org.x\n", l.display())
    }

    #[test]
    fn a_current_entry_is_not_touched() {
        let l = lab("current");
        assert_eq!(l.refresh().unwrap(), Refresh::Missing);
        fs::write(l.entry_path(), l.current()).unwrap();
        let before = fs::metadata(l.entry_path()).unwrap();
        assert_eq!(l.refresh().unwrap(), Refresh::Current);
        let after = fs::metadata(l.entry_path()).unwrap();
        assert_eq!((before.ino_(), before.modified().unwrap()), (after.ino_(), after.modified().unwrap()));
        assert!(!l.launcher.exists(), "the launcher is not made when nothing was old");
        // A newer entry is left alone too: an old runtime must not undo it.
        let newer = l.current().replace("Desktop-Version=1", "Desktop-Version=99");
        fs::write(l.entry_path(), &newer).unwrap();
        assert_eq!(l.refresh().unwrap(), Refresh::Current);
        assert_eq!(fs::read_to_string(l.entry_path()).unwrap(), newer);
        let _ = fs::remove_dir_all(&l.root);
    }

    trait Ino {
        fn ino_(&self) -> u64;
    }
    impl Ino for fs::Metadata {
        fn ino_(&self) -> u64 {
            std::os::unix::fs::MetadataExt::ino(self)
        }
    }

    #[test]
    fn an_old_entry_and_its_launcher_are_rewritten() {
        let l = lab("old");
        fs::write(l.entry_path(), old_entry(&l.launcher)).unwrap();
        fs::write(&l.launcher, "#!/bin/sh\n# Generated by `panpak install`: do not edit.\n# X-Panpak-App-Id: org.x\nexec old\n").unwrap();
        assert_eq!(l.refresh().unwrap(), Refresh::Rewritten);
        assert_eq!(fs::read_to_string(l.entry_path()).unwrap(), l.current());
        let sh = fs::read_to_string(&l.launcher).unwrap();
        assert!(sh.contains(&format!("exec '{}' run '{}'", stable_stub(&l.store).display(), l.store.join("apps/org.x/current").display())), "{sh}");
        assert_eq!(fs::metadata(&l.launcher).unwrap().permissions().mode() & 0o777, 0o755);
        // The second start finds nothing to do.
        assert_eq!(l.refresh().unwrap(), Refresh::Current);
        let _ = fs::remove_dir_all(&l.root);
    }

    #[test]
    fn a_version_that_is_too_far_in_is_taken_as_old() {
        let l = lab("late");
        let mut e = old_entry(&l.launcher);
        e = e.replace("Type=Application\n", &format!("Type=Application\nName[x]={}\n{VERSION_KEY}{DESKTOP_VERSION}\n", "z".repeat(VERSION_WINDOW)));
        fs::write(l.entry_path(), &e).unwrap();
        assert_eq!(l.refresh().unwrap(), Refresh::Rewritten);
        assert_eq!(entry_version(fs::read_to_string(l.entry_path()).unwrap().as_bytes()), Some(DESKTOP_VERSION));
        let _ = fs::remove_dir_all(&l.root);
    }

    #[test]
    fn the_icon_of_an_old_entry_is_kept() {
        let l = lab("icon");
        let e = old_entry(&l.launcher).replace("Terminal=false", "Icon=panpak.org.x\nTerminal=false");
        fs::write(l.entry_path(), e).unwrap();
        assert_eq!(l.refresh().unwrap(), Refresh::Rewritten);
        assert!(fs::read_to_string(l.entry_path()).unwrap().contains("\nIcon=panpak.org.x\n"));
        let _ = fs::remove_dir_all(&l.root);
    }

    #[test]
    fn what_is_not_ours_is_left_alone() {
        let l = lab("foreign");
        // Another program's file of that name.
        fs::write(l.entry_path(), "[Desktop Entry]\nType=Application\nName=Mine\nExec=/bin/mine\n").unwrap();
        assert_eq!(l.refresh().unwrap(), Refresh::NotOurs);
        assert!(fs::read_to_string(l.entry_path()).unwrap().contains("/bin/mine"));
        // Another app's marker.
        fs::write(l.entry_path(), "[Desktop Entry]\nX-Panpak-App-Id=org.other\n").unwrap();
        assert_eq!(l.refresh().unwrap(), Refresh::NotOurs);
        // A link is not followed, not even to read it.
        let target = l.root.join("target");
        fs::write(&target, old_entry(&l.launcher)).unwrap();
        fs::remove_file(l.entry_path()).unwrap();
        std::os::unix::fs::symlink(&target, l.entry_path()).unwrap();
        assert_eq!(l.refresh().unwrap(), Refresh::NotOurs);
        assert_eq!(fs::read_to_string(&target).unwrap(), old_entry(&l.launcher));
        // A folder, and a file that is too large.
        fs::remove_file(l.entry_path()).unwrap();
        fs::write(l.entry_path(), format!("{}{}", old_entry(&l.launcher), "#".repeat(2 << 20))).unwrap();
        assert_eq!(l.refresh().unwrap(), Refresh::NotOurs);
        let _ = fs::remove_dir_all(&l.root);
    }

    #[test]
    fn a_launcher_that_is_not_ours_stays_but_the_entry_is_updated() {
        let l = lab("launcher");
        fs::write(l.entry_path(), old_entry(&l.launcher)).unwrap();
        fs::write(&l.launcher, "#!/bin/sh\necho mine\n").unwrap();
        assert_eq!(l.refresh().unwrap(), Refresh::Rewritten);
        assert_eq!(fs::read_to_string(&l.launcher).unwrap(), "#!/bin/sh\necho mine\n");
        // A missing launcher is made again: the entry points at it.
        fs::remove_file(&l.launcher).unwrap();
        fs::write(l.entry_path(), old_entry(&l.launcher)).unwrap();
        assert_eq!(l.refresh().unwrap(), Refresh::Rewritten);
        assert!(l.launcher.exists());
        let _ = fs::remove_dir_all(&l.root);
    }

    #[test]
    fn without_the_stable_stub_nothing_is_written() {
        let l = lab("nostub");
        fs::remove_file(stable_stub(&l.store)).unwrap();
        fs::write(l.entry_path(), old_entry(&l.launcher)).unwrap();
        assert!(l.refresh().is_err());
        assert_eq!(fs::read_to_string(l.entry_path()).unwrap(), old_entry(&l.launcher));
        assert!(!l.launcher.exists());
        let _ = fs::remove_dir_all(&l.root);
    }

    #[test]
    fn atomic_write_replaces_a_link_without_following_it() {
        let d = scratch("atomic");
        let target = d.join("target");
        fs::write(&target, "keep").unwrap();
        let dest = d.join("dest");
        std::os::unix::fs::symlink(&target, &dest).unwrap();
        write_file_atomic(&dest, b"new", 0o644).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "new");
        assert_eq!(fs::metadata(&dest).unwrap().permissions().mode() & 0o777, 0o644);
        assert_eq!(fs::read_dir(&d).unwrap().count(), 2, "no temporary file stays");
        let _ = fs::remove_dir_all(&d);
    }
}
