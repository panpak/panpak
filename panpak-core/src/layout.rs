// SPDX-License-Identifier: MIT OR Apache-2.0
//! Private folders of an app, what the app may read of the user's own
//! configuration, and how permissions are shown to the user.
//! Layout and rules: PANPAK-FORMAT.md §9.1 and THREAT-MODEL.md §2.2.
use crate::{coded, validate_app_id, ErrorCode, PLACEHOLDERS};
use anyhow::{Context, Result};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// First line of the `fonts.conf` the stub writes. A file without it is the user's.
pub const FONTS_CONF_MARKER: &str = "X-Panpak-Generated";

/// `TMPDIR` of an app in the isolated view: the folder for large temporary files, mounted here (a short path, whatever the home).
pub const VIEW_TMP: &str = "/var/tmp";

/// The folders an app owns. `root` and `cache` are the only writable roots
/// besides `/tmp` and the `home_paths` of the manifest.
///
/// - `root`  = `$XDG_DATA_HOME/panpak/userdata/<id>`: the private home of the app, the only folder of its data. It holds
///   `.config`, `.local/share` and `.local/state`. In the isolated view it is mounted at the path of the real home;
///   without the view it is the `HOME` of the app;
/// - `cache` = `$XDG_CACHE_HOME/panpak/<id>`, mounted as `~/.cache` in the view; `cache/tmp` is the folder for large
///   temporary files.
#[derive(Debug, Clone)]
pub struct AppDirs {
    pub root: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
    pub tmp: PathBuf,
    pub cache: PathBuf,
}

/// Paths of the app folders for `id`, from the XDG variables of the caller.
/// Nothing is created.
pub fn app_dirs(id: &str) -> Result<AppDirs> {
    validate_app_id(id)?;
    let root = dirs::data_dir().context("no data dir")?.join("panpak").join("userdata").join(id);
    let cache = dirs::cache_dir().context("no cache dir")?.join("panpak").join(id);
    Ok(AppDirs::below(root, cache))
}

/// Directory with mode 0700. An existing real directory is kept (and its mode
/// tightened); a link or a file in its place is refused.
fn private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => {
            if m.permissions().mode() & 0o077 != 0 {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            }
            Ok(())
        }
        Ok(_) => Err(coded(
            ErrorCode::E3000,
            format!("{} is a link or a file, not a folder: refused (remove or move it)", path.display()),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new().mode(0o700).create(path).with_context(|| format!("create {}", path.display()))
        }
        Err(e) => Err(e).with_context(|| format!("check {}", path.display())),
    }
}

impl AppDirs {
    /// The folders of an app whose private home is `root` and whose cache is `cache`.
    fn below(root: PathBuf, cache: PathBuf) -> AppDirs {
        AppDirs {
            config: root.join(".config"),
            data: root.join(".local").join("share"),
            state: root.join(".local").join("state"),
            tmp: cache.join("tmp"),
            root,
            cache,
        }
    }

    /// The same folders as the isolated view shows them (`TMPDIR` is `/var/tmp` there: a short path, whatever the home): the private home is mounted at `home` and the cache at
    /// `home/.cache`. The variables of the app and the paths in its launch values use these.
    pub fn seen_at(&self, home: &Path) -> AppDirs {
        AppDirs { tmp: PathBuf::from(VIEW_TMP), ..AppDirs::below(home.to_path_buf(), home.join(".cache")) }
    }

    /// Create the folders with mode 0700. Run before the sandbox is applied.
    pub fn create(&self) -> Result<()> {
        for parent in [self.root.parent(), self.cache.parent()].into_iter().flatten() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let local = self.root.join(".local");
        for d in [&self.root, &self.config, &local, &self.data, &self.state, &self.cache, &self.tmp] {
            private_dir(d)?;
        }
        Ok(())
    }

    /// The two roots, persistent first.
    pub fn roots(&self) -> [&Path; 2] {
        [&self.root, &self.cache]
    }

    fn placeholder(&self, name: &str) -> Option<&Path> {
        Some(match name {
            "XDG_CONFIG_HOME" => &self.config,
            "XDG_DATA_HOME" => &self.data,
            "XDG_STATE_HOME" => &self.state,
            "XDG_CACHE_HOME" => &self.cache,
            "TMPDIR" => &self.tmp,
            _ => return None,
        })
    }

    /// Replace the placeholders of `PLACEHOLDERS` in one pass. Any other `${...}` is an
    /// error (the manifest validation refuses it before, this is the safety net).
    pub fn expand(&self, s: &str) -> Result<String> {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find("${") {
            out.push_str(&rest[..i]);
            let after = &rest[i + 2..];
            let end = after.find('}').context("unterminated '${' in launch value")?;
            let dir = self
                .placeholder(&after[..end])
                .with_context(|| format!("unknown placeholder in launch value (allowed: {})", PLACEHOLDERS.join(", ")))?;
            out.push_str(dir.to_str().context("app folder path is not UTF-8")?);
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    /// Delete the persistent folders and the cache of the app. Links inside are removed, never
    /// followed, and a root that is a link is refused. Returns the roots that existed.
    pub fn clear(&self) -> Result<Vec<PathBuf>> {
        let mut removed = Vec::new();
        for root in self.roots() {
            match fs::symlink_metadata(root) {
                Ok(m) if m.is_dir() => {
                    fs::remove_dir_all(root).with_context(|| format!("delete {}", root.display()))?;
                    removed.push(root.to_path_buf());
                }
                Ok(_) => {
                    return Err(coded(
                        ErrorCode::E3000,
                        format!("{} is a link or a file, not a folder: refused", root.display()),
                    ))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("check {}", root.display())),
            }
        }
        Ok(removed)
    }
}

/// What the app may read of the user's own settings, for theme, fonts and icons. Only
/// entries that exist: a missing one is skipped. Never a whole config or data folder.
pub fn readonly_user_paths() -> Vec<PathBuf> {
    readonly_user_paths_in(dirs::config_dir().as_deref(), dirs::data_dir().as_deref(), dirs::home_dir().as_deref())
}

/// The user's icon folders hold themes, one folder each. The app may read every theme but `hicolor`.
///
/// `hicolor` is the theme that the toolkits always search, and it is where `panpak install` puts the icons of the apps
/// for the menu of the desktop. That folder has no `icon-theme.cache`, so Qt looks up every icon name file by file in each
/// size folder: QOwnNotes made 40,247 failed `access()` calls and needed 100 ms more to show its window (against 470 calls
/// without the folder). The same happens with `~/.icons/hicolor`; a theme that is not in use costs nothing. The app has no use
/// for the icons of the menu, so `hicolor` of the user stays out of the view; the themes the user chose stay in.
const HIDDEN_ICON_THEME: &str = "hicolor";

fn icon_themes(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_name() != HIDDEN_ICON_THEME)
        .map(|e| e.path())
        // A theme is a folder (a link to one counts); a loose file is not mounted.
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

pub fn readonly_user_paths_in(config: Option<&Path>, data: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    const CONFIG: &[&str] =
        &["fontconfig", "gtk-3.0", "gtk-4.0", "kdeglobals", "user-dirs.dirs", "mimeapps.list", "qt5ct", "qt6ct", "Trolltech.conf"];
    const DATA: &[&str] = &["fonts", "themes", "mime"];
    let mut all = Vec::new();
    if let Some(c) = config {
        all.extend(CONFIG.iter().map(|n| c.join(n)));
    }
    if let Some(d) = data {
        all.extend(DATA.iter().map(|n| d.join(n)));
    }
    if let Some(h) = home {
        all.push(h.join(".fonts"));
    }
    all.retain(|p| p.exists());
    for icons in data.map(|d| d.join("icons")).into_iter().chain(home.map(|h| h.join(".icons"))) {
        all.extend(icon_themes(&icons));
    }
    all
}

/// What the isolated view shows of the user's own environment (fonts, themes, icons, cursors, GTK, Qt and KDE settings)
/// at the real paths, inside the private home.
///
/// Every folder is mounted read-only, as it is (a bind mount of the user's folder): the app sees the same files as every other
/// program and a change the user makes meanwhile (another theme) is seen by the running app, and the app cannot change them.
/// Only entries that exist.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EnvLayers {
    /// Folders mounted read-only.
    pub dirs: Vec<PathBuf>,
    /// Single files, mounted read-only. A file is the inode that is there when the app starts: a program that replaces it by a rename
    /// is not seen until the next start (`kdeglobals` is written that way by KDE).
    pub files: Vec<PathBuf>,
    /// Files inside those folders that are private to the user and are replaced by an empty file.
    pub hidden: Vec<PathBuf>,
}

impl EnvLayers {
    /// Number of mounts the layers cost.
    pub fn mounts(&self) -> usize {
        self.dirs.len() + self.files.len() + self.hidden.len()
    }
}

/// The layers for the folders of the user (`config`, `data`, `home`; others in tests). The reason of each entry:
///
/// - `fontconfig`, `.fonts`, `.local/share/fonts`: the fonts of the user and the rules that choose them;
/// - `gtk-3.0`, `gtk-4.0`: GTK settings (`settings.ini`, `gtk.css`) and, in `gtk-3.0`, the bookmarks, which are hidden;
/// - `qt5ct`, `qt6ct`, `Trolltech.conf`, `kdeglobals`: Qt and KDE settings (style, fonts, colours);
/// - `.local/share/themes`: GTK themes of the user; each theme folder of `.local/share/icons` and `.icons` except `hicolor`
///   (one mount each, not the whole folder: see `HIDDEN_ICON_THEME`; measured, a `hicolor` that is there, even empty, costs
///   QOwnNotes 50 to 170 ms);
/// - `.local/share/mime`: the types of the user, for names and icons of files.
pub fn env_layers_in(config: Option<&Path>, data: Option<&Path>, home: Option<&Path>) -> EnvLayers {
    const CONFIG_DIRS: &[&str] = &["fontconfig", "gtk-3.0", "gtk-4.0", "qt5ct", "qt6ct"];
    const CONFIG_FILES: &[&str] = &["kdeglobals", "Trolltech.conf"];
    const DATA_DIRS: &[&str] = &["fonts", "themes", "mime"];
    // Files of the user's folders that tell what the user has been doing, not how things look.
    const PRIVATE: &[&str] = &["gtk-3.0/bookmarks", "gtk-3.0/servers", "gtk-4.0/bookmarks"];
    let mut out = EnvLayers::default();
    if let Some(c) = config {
        out.dirs.extend(CONFIG_DIRS.iter().map(|n| c.join(n)));
        out.files.extend(CONFIG_FILES.iter().map(|n| c.join(n)));
        out.hidden.extend(PRIVATE.iter().map(|n| c.join(n)));
    }
    if let Some(d) = data {
        out.dirs.extend(DATA_DIRS.iter().map(|n| d.join(n)));
        out.dirs.extend(icon_themes(&d.join("icons")));
    }
    if let Some(h) = home {
        out.dirs.push(h.join(".fonts"));
        out.dirs.extend(icon_themes(&h.join(".icons")));
    }
    out.dirs.retain(|p| p.is_dir());
    out.files.retain(|p| p.is_file());
    let dirs = out.dirs.clone();
    out.hidden.retain(|p| p.is_file() && dirs.iter().any(|o| p.starts_with(o)));
    out
}

/// `env_layers_in` for the user of this process.
pub fn env_layers() -> EnvLayers {
    env_layers_in(dirs::config_dir().as_deref(), dirs::data_dir().as_deref(), dirs::home_dir().as_deref())
}

/// System caches outside `/usr` that the app reads at start, read-only. The view has an empty `/var`, and
/// Landlock lets an app read only a few system folders, so each of these is mounted into the view and given one
/// read rule. `/var/cache/fontconfig` holds the font index of the system fonts: without it the first start of
/// every app (the private cache folder is empty) scans every font file again. Only entries that exist.
pub fn system_cache_paths() -> Vec<PathBuf> {
    const CACHES: &[&str] = &["/var/cache/fontconfig"];
    CACHES.iter().map(PathBuf::from).filter(|p| p.is_dir()).collect()
}

/// `XDG_CONFIG_DIRS` and `XDG_DATA_DIRS` for the app: the existing value, or the default of the
/// XDG Base Directory Specification, then (`with_real`) the user's real config and data folder at the end.
/// Landlock lets the app read only part of those two (`readonly_user_paths`).
pub fn xdg_search_dirs(config_dirs: Option<String>, data_dirs: Option<String>, with_real: bool) -> Result<(String, String)> {
    let real_config = dirs::config_dir().context("no config dir")?;
    let real_data = dirs::data_dir().context("no data dir")?;
    // With the private home of the view the real folders of the user are the folders of the app (`XDG_CONFIG_HOME` and
    // `XDG_DATA_HOME`, with the environment mounted in them), so they are not added a second time.
    let join = |current: Option<String>, default: &str, real: &Path| -> Result<String> {
        let base = current.filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string());
        if !with_real {
            return Ok(base);
        }
        Ok(format!("{base}:{}", real.to_str().context("XDG folder is not UTF-8")?))
    };
    Ok((join(config_dirs, "/etc/xdg", &real_config)?, join(data_dirs, "/usr/local/share:/usr/share", &real_data)?))
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `fonts.conf` for the private config folder. It includes the user's own fontconfig files and
/// adds the font folders of the user, which the app cannot find through its private XDG folders.
pub fn fonts_conf(real_config: &Path, real_data: &Path, home: &Path) -> String {
    let cfg = xml(&real_config.join("fontconfig").to_string_lossy());
    format!(
        "<?xml version=\"1.0\"?>\n\
<!DOCTYPE fontconfig SYSTEM \"urn:fontconfig:fonts.dtd\">\n\
<!-- {FONTS_CONF_MARKER}: written by panpak-stub at every start. Delete this line to keep your own copy. -->\n\
<fontconfig>\n\
  <include ignore_missing=\"yes\">{cfg}/fonts.conf</include>\n\
  <include ignore_missing=\"yes\">{cfg}/conf.d</include>\n\
  <dir>{}</dir>\n\
  <dir>{}</dir>\n\
</fontconfig>\n",
        xml(&real_data.join("fonts").to_string_lossy()),
        xml(&home.join(".fonts").to_string_lossy()),
    )
}

/// Printed by `install` and `update` when the kernel cannot block local sockets.
pub const PARTIAL_ISOLATION_WARNING: &str = "WARNING: on this kernel the sandbox cannot block local sockets (D-Bus, ssh-agent, gpg-agent): this app is only partially isolated";

/// Printed when abstract Unix sockets stay open: X11 access turns the Landlock scope off, and a
/// kernel below Landlock V6 has no scope. The isolated view cannot hide them (they live in the
/// network namespace, which the app shares).
pub const ABSTRACT_SOCKETS_WARNING: &str = "WARNING: abstract Unix sockets stay open (X11 access, or a kernel without the Landlock scope; some D-Bus and agent sockets): this app is only partially isolated";

/// How well an app is kept away from the local Unix sockets (D-Bus, ssh-agent, gpg-agent,
/// Wayland and the like). One place decides it, from the kernel, the isolated view and the
/// permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketIsolation {
    /// Path sockets are denied unless the policy lists them (by the isolated view, by Landlock V9,
    /// or both), and abstract sockets are blocked by the Landlock scope.
    Isolated,
    /// Nothing blocks path sockets: no isolated view, and Landlock below V9 or none.
    KernelCannotFilter,
    /// The manifest lists a D-Bus socket.
    ManifestAllowsBus,
    /// Abstract sockets stay reachable: X11 access turns the Landlock scope off, and below
    /// Landlock V6 there is no scope.
    AbstractSocketsOpen,
}

impl SocketIsolation {
    /// The warning to print, if the app is not fully isolated. `ManifestAllowsBus` has its
    /// own line in the table of permissions.
    pub fn warning(self) -> Option<&'static str> {
        match self {
            SocketIsolation::Isolated | SocketIsolation::ManifestAllowsBus => None,
            SocketIsolation::KernelCannotFilter => Some(PARTIAL_ISOLATION_WARNING),
            SocketIsolation::AbstractSocketsOpen => Some(ABSTRACT_SOCKETS_WARNING),
        }
    }
}

/// The answer of [`socket_isolation`]: the level and the layers that are active. The layers are
/// independent: each one works without the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsolationReport {
    pub level: SocketIsolation,
    /// The isolated view (bubblewrap) hides the sockets that the policy does not list.
    pub view: bool,
    /// Landlock V9 denies connecting to path sockets that the policy does not list.
    pub landlock_paths: bool,
    /// The Landlock scope (V6) blocks abstract Unix sockets (off with X11 access).
    pub landlock_abstract: bool,
}

impl IsolationReport {
    /// The active layers in words, for `doctor` and `install`.
    pub fn layers(&self) -> String {
        let mut v = Vec::new();
        if self.view {
            v.push("isolated view (bubblewrap)");
        }
        if self.landlock_paths {
            v.push("Landlock path sockets (V9)");
        }
        if self.landlock_abstract {
            v.push("Landlock abstract-socket scope (V6)");
        }
        if v.is_empty() {
            "none".to_string()
        } else {
            v.join(" + ")
        }
    }
}

/// True if a `sockets_allow` entry names the D-Bus system bus (`.../system_bus_socket`).
pub fn is_system_bus_socket(path: &str) -> bool {
    path.split('/').any(|p| p == "system_bus_socket")
}

/// True if the manifest lists a session D-Bus socket in `sockets_allow` (a bus socket that is not
/// the system bus). The stub then keeps `DBUS_SESSION_BUS_ADDRESS` as it is.
pub fn allows_session_bus(perms: Option<&crate::Permissions>) -> bool {
    perms.is_some_and(|p| p.sockets_allow.iter().flatten().any(|s| is_bus_socket(s) && !is_system_bus_socket(s)))
}

/// True if the manifest lists the system bus in `sockets_allow`. The stub then keeps
/// `DBUS_SYSTEM_BUS_ADDRESS` as it is.
pub fn allows_system_bus(perms: Option<&crate::Permissions>) -> bool {
    perms.is_some_and(|p| p.sockets_allow.iter().flatten().any(|s| is_system_bus_socket(s)))
}

/// True if the manifest lists any D-Bus socket.
pub fn allows_desktop_bus(perms: Option<&crate::Permissions>) -> bool {
    allows_session_bus(perms) || allows_system_bus(perms)
}

/// The isolation of an app from local sockets: `abi` is the Landlock ABI of the kernel (None if
/// Landlock is missing), `perms` the manifest permissions (None for the kernel alone) and `view`
/// whether the isolated view can be built here (`view_status().usable()`).
pub fn socket_isolation(abi: Option<landlock::ABI>, perms: Option<&crate::Permissions>, view: bool) -> IsolationReport {
    let landlock_paths = abi.is_some_and(|a| a >= landlock::ABI::V9);
    let x11 = perms.is_some_and(|p| p.x11 == Some(true));
    let landlock_abstract = abi.is_some_and(|a| a >= landlock::ABI::V6) && !x11;
    let level = if !view && !landlock_paths {
        SocketIsolation::KernelCannotFilter
    } else if allows_desktop_bus(perms) {
        SocketIsolation::ManifestAllowsBus
    } else if !landlock_abstract {
        SocketIsolation::AbstractSocketsOpen
    } else {
        SocketIsolation::Isolated
    };
    IsolationReport { level, view, landlock_paths, landlock_abstract }
}

/// True if a `sockets_allow` entry names a D-Bus socket (`$XDG_RUNTIME_DIR/bus`,
/// `/run/dbus/system_bus_socket`, and the like). The list is a heuristic: a message bus is
/// recognised by a path part `bus`, `dbus` or `system_bus_socket`.
pub fn is_bus_socket(path: &str) -> bool {
    path.split('/').any(|p| matches!(p, "bus" | "dbus" | "dbus-1" | "system_bus_socket"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_user_hicolor_theme_stays_out_of_the_view_and_the_other_themes_in() {
        let t = std::env::temp_dir().join(format!("panpak-layout-icons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let (data, home) = (t.join("data"), t.join("home"));
        for d in ["icons/hicolor/48x48/apps", "icons/MyTheme", "icons/Other", "fonts", ".fonts-not", "x"] {
            std::fs::create_dir_all(data.join(d)).unwrap();
        }
        for d in [".icons/hicolor/16x16/apps", ".icons/Old", ".fonts"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        std::fs::write(data.join("icons/loose.png"), "x").unwrap();
        std::os::unix::fs::symlink(data.join("icons/MyTheme"), data.join("icons/Linked")).unwrap();
        let got = readonly_user_paths_in(None, Some(&data), Some(&home));
        let rel = |p: &PathBuf| p.strip_prefix(&t).unwrap().display().to_string();
        let got: Vec<String> = got.iter().map(rel).collect();
        // `icons` and `.icons` themselves are never granted; their themes are, except hicolor; loose files are not.
        assert_eq!(got, ["data/fonts", "home/.fonts", "data/icons/Linked", "data/icons/MyTheme", "data/icons/Other", "home/.icons/Old"]);
        assert!(!got.iter().any(|p| p.contains("hicolor") || p.ends_with("/icons") || p.ends_with("/.icons") || p.contains("loose")));
        // Without any user icon folder nothing is added, and a folder with only hicolor adds nothing.
        std::fs::remove_dir_all(data.join("icons")).unwrap();
        std::fs::create_dir_all(data.join("icons/hicolor")).unwrap();
        assert!(readonly_user_paths_in(None, Some(&data), None).iter().all(|p| !p.starts_with(data.join("icons"))));
        let _ = std::fs::remove_dir_all(&t);
    }

    use super::*;
    use crate::Manifest;

    fn man(perm: &str) -> Manifest {
        toml::from_str(&format!(
            "[app]\nid=\"org.example.app\"\nname=\"App\"\nversion=\"1.0\"\nentry=\"bin/app\"\n{perm}[update]\nkey=\"ed25519:{}\"\n",
            "0".repeat(64)
        ))
        .unwrap()
    }

    fn dirs_at(base: &str) -> AppDirs {
        AppDirs::below(PathBuf::from(base).join("userdata/org.example.app"), PathBuf::from(base).join("cache/org.example.app"))
    }

    #[test]
    fn expand_replaces_only_known_placeholders() {
        let d = dirs_at("/x");
        assert_eq!(d.expand("--profile=${XDG_CONFIG_HOME}/p").unwrap(), "--profile=/x/userdata/org.example.app/.config/p");
        assert_eq!(d.expand("${TMPDIR}:${XDG_CACHE_HOME}").unwrap(), "/x/cache/org.example.app/tmp:/x/cache/org.example.app");
        assert_eq!(d.expand("a$b").unwrap(), "a$b");
        assert!(d.expand("${HOME}").is_err());
        assert!(d.expand("${XDG_CONFIG_HOME").is_err());
    }

    #[test]
    fn fonts_conf_has_marker_includes_and_escapes() {
        let c = fonts_conf(Path::new("/h/.config"), Path::new("/h/.local/share"), Path::new("/h&x"));
        assert!(c.contains(FONTS_CONF_MARKER));
        assert!(c.contains("<include ignore_missing=\"yes\">/h/.config/fontconfig/fonts.conf</include>"));
        assert!(c.contains("<dir>/h/.local/share/fonts</dir>"));
        assert!(c.contains("<dir>/h&amp;x/.fonts</dir>"));
    }

    #[test]
    fn xdg_dirs_keep_existing_or_default_and_append_real() {
        let (c, d) = xdg_search_dirs(Some("/a:/b".into()), None, true).unwrap();
        assert!(c.starts_with("/a:/b:"), "{c}");
        assert!(d.starts_with("/usr/local/share:/usr/share:"), "{d}");
        let (c, _) = xdg_search_dirs(Some(String::new()), None, true).unwrap();
        assert!(c.starts_with("/etc/xdg:"), "{c}");
    }

    #[test]
    fn bus_sockets_are_recognised() {
        for p in ["$XDG_RUNTIME_DIR/bus", "${XDG_RUNTIME_DIR}/bus", "/run/user/1000/bus", "/run/dbus/system_bus_socket", "/var/run/dbus/system_bus_socket"] {
            assert!(is_bus_socket(p), "{p}");
        }
        for p in ["$XDG_RUNTIME_DIR/pulse/native", "/run/user/1000/wayland-0", "$HOME/.cache/business"] {
            assert!(!is_bus_socket(p), "{p}");
        }
    }

    #[test]
    fn socket_isolation_depends_on_kernel_view_and_permissions() {
        use landlock::ABI;
        let plain = man("");
        let bus = man("[permissions]\nsockets_allow=[\"$XDG_RUNTIME_DIR/bus\"]\n");
        let x11 = man("[permissions]\nx11=true\n");
        let wl = man("[permissions]\nsockets_allow=[\"$XDG_RUNTIME_DIR/wayland-0\"]\n");
        // No view and Landlock below V9: nothing blocks path sockets.
        for abi in [None, Some(ABI::V1), Some(ABI::V6), Some(ABI::V8)] {
            for m in [&plain, &bus, &x11, &wl] {
                let r = socket_isolation(abi, m.permissions.as_ref(), false);
                assert_eq!(r.level, SocketIsolation::KernelCannotFilter, "{abi:?}");
                assert_eq!(r.level.warning(), Some(PARTIAL_ISOLATION_WARNING));
                assert!(!r.view && !r.landlock_paths);
                assert!(!r.layers().contains("Landlock path"));
            }
        }
        // Landlock V9 alone.
        let v9 = Some(ABI::V9);
        let r = socket_isolation(v9, None, false);
        assert_eq!((r.level, r.view, r.landlock_paths, r.landlock_abstract), (SocketIsolation::Isolated, false, true, true));
        assert_eq!(socket_isolation(v9, plain.permissions.as_ref(), false).level, SocketIsolation::Isolated);
        assert_eq!(socket_isolation(v9, wl.permissions.as_ref(), false).level, SocketIsolation::Isolated);
        assert_eq!(socket_isolation(v9, bus.permissions.as_ref(), false).level, SocketIsolation::ManifestAllowsBus);
        assert_eq!(socket_isolation(v9, x11.permissions.as_ref(), false).level, SocketIsolation::AbstractSocketsOpen);
        // The view alone, with the Landlock of this kernel (V8) or a bare kernel.
        let r = socket_isolation(Some(ABI::V8), plain.permissions.as_ref(), true);
        assert_eq!((r.level, r.view, r.landlock_paths, r.landlock_abstract), (SocketIsolation::Isolated, true, false, true));
        assert_eq!(r.layers(), "isolated view (bubblewrap) + Landlock abstract-socket scope (V6)");
        assert_eq!(socket_isolation(Some(ABI::V8), x11.permissions.as_ref(), true).level, SocketIsolation::AbstractSocketsOpen);
        assert_eq!(socket_isolation(Some(ABI::V8), bus.permissions.as_ref(), true).level, SocketIsolation::ManifestAllowsBus);
        // Without the V6 scope the abstract sockets are open even with the view.
        assert_eq!(socket_isolation(Some(ABI::V5), None, true).level, SocketIsolation::AbstractSocketsOpen);
        assert_eq!(socket_isolation(None, None, true).level, SocketIsolation::AbstractSocketsOpen);
        // Both layers.
        let r = socket_isolation(v9, None, true);
        assert_eq!(r.layers(), "isolated view (bubblewrap) + Landlock path sockets (V9) + Landlock abstract-socket scope (V6)");
        assert_eq!(socket_isolation(None, None, false).layers(), "none");
        assert_eq!(SocketIsolation::Isolated.warning(), None);
        assert_eq!(SocketIsolation::ManifestAllowsBus.warning(), None);
        assert!(allows_desktop_bus(bus.permissions.as_ref()) && !allows_desktop_bus(plain.permissions.as_ref()));
    }

    #[test]
    fn session_and_system_bus_are_told_apart() {
        let sess = man("[permissions]\nsockets_allow=[\"$XDG_RUNTIME_DIR/bus\"]\n");
        let sys = man("[permissions]\nsockets_allow=[\"/run/dbus/system_bus_socket\"]\n");
        let both = man("[permissions]\nsockets_allow=[\"$XDG_RUNTIME_DIR/bus\", \"/var/run/dbus/system_bus_socket\"]\n");
        let none = man("[permissions]\nsockets_allow=[\"$XDG_RUNTIME_DIR/wayland-0\"]\n");
        let f = |m: &Manifest| (allows_session_bus(m.permissions.as_ref()), allows_system_bus(m.permissions.as_ref()));
        assert_eq!(f(&sess), (true, false));
        assert_eq!(f(&sys), (false, true));
        assert_eq!(f(&both), (true, true));
        assert_eq!(f(&none), (false, false));
        assert_eq!(f(&man("")), (false, false));
        assert!(is_system_bus_socket("/run/dbus/system_bus_socket") && !is_system_bus_socket("/run/user/1000/bus"));
        // Either bus listed makes the isolation level "manifest allows the bus" on a V9 kernel.
        let v9 = Some(landlock::ABI::V9);
        assert_eq!(socket_isolation(v9, sys.permissions.as_ref(), false).level, SocketIsolation::ManifestAllowsBus);
        assert_eq!(socket_isolation(v9, sess.permissions.as_ref(), false).level, SocketIsolation::ManifestAllowsBus);
    }

    #[test]
    fn the_private_home_is_one_folder_with_the_standard_folders_in_it() {
        let d = dirs_at("/x");
        let root = Path::new("/x/userdata/org.example.app");
        assert_eq!((d.config.as_path(), d.data.as_path(), d.state.as_path()), (root.join(".config").as_path(), root.join(".local/share").as_path(), root.join(".local/state").as_path()));
        // The cache is its own folder, and the large temporary files go below it.
        assert_eq!((d.cache.as_path(), d.tmp.as_path()), (Path::new("/x/cache/org.example.app"), Path::new("/x/cache/org.example.app/tmp")));
        // As the view shows them: the home is the private home, `~/.cache` the cache.
        let s = d.seen_at(Path::new("/home/u"));
        assert_eq!((s.root.as_path(), s.config.as_path(), s.data.as_path(), s.state.as_path()), (Path::new("/home/u"), Path::new("/home/u/.config"), Path::new("/home/u/.local/share"), Path::new("/home/u/.local/state")));
        assert_eq!((s.cache.as_path(), s.tmp.as_path()), (Path::new("/home/u/.cache"), Path::new("/var/tmp")));
        assert_eq!(s.expand("${XDG_STATE_HOME}/a").unwrap(), "/home/u/.local/state/a");
    }

    #[test]
    fn the_layers_name_whole_folders_and_hide_the_private_files() {
        let base = std::env::temp_dir().join(format!("panpak-envlayers-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (cfg, data, home) = (base.join(".config"), base.join(".local/share"), base.clone());
        for d in ["gtk-3.0", "fontconfig", "qt5ct"] {
            fs::create_dir_all(cfg.join(d)).unwrap();
        }
        fs::write(cfg.join("gtk-3.0/bookmarks"), "file:///secret\n").unwrap();
        fs::write(cfg.join("gtk-3.0/settings.ini"), "[Settings]\n").unwrap();
        fs::write(cfg.join("kdeglobals"), "x").unwrap();
        for d in ["icons/hicolor", "icons/Adwaita", "fonts", "themes"] {
            fs::create_dir_all(data.join(d)).unwrap();
        }
        fs::create_dir_all(home.join(".icons/hicolor")).unwrap();
        assert!(!env_layers_in(Some(&cfg), Some(&data), Some(&home)).dirs.iter().any(|p| p.ends_with("hicolor")), "hicolor is never mounted");
        let l = env_layers_in(Some(&cfg), Some(&data), Some(&home));
        assert_eq!(l.dirs, [cfg.join("fontconfig"), cfg.join("gtk-3.0"), cfg.join("qt5ct"), data.join("fonts"), data.join("themes"), data.join("icons/Adwaita")]);
        assert_eq!(l.files, [cfg.join("kdeglobals")]);
        assert_eq!(l.hidden, [cfg.join("gtk-3.0/bookmarks")]);
        // About ten mounts for a full environment, not one per file or theme.
        assert!(l.mounts() <= 12, "{}", l.mounts());
        // Nothing exists: nothing is mounted.
        assert_eq!(env_layers_in(Some(&base.join("none")), Some(&base.join("none")), None), EnvLayers::default());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn create_makes_private_folders_and_refuses_links() {
        let base = std::env::temp_dir().join(format!("panpak-layout-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let d = dirs_at(base.to_str().unwrap());
        d.create().unwrap();
        for p in [&d.root, &d.config, &d.root.join(".local"), &d.data, &d.state, &d.tmp, &d.cache] {
            assert_eq!(fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o700, "{p:?}");
        }
        // clear removes the content, never follows a link inside.
        let outside = base.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), b"x").unwrap();
        std::os::unix::fs::symlink(&outside, d.data.join("link")).unwrap();
        assert_eq!(d.clear().unwrap().len(), 2);
        assert!(outside.join("keep").exists());
        assert!(!d.root.exists() && !d.cache.exists());
        // a link in place of a root is refused by create and clear.
        fs::create_dir_all(d.root.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside, &d.root).unwrap();
        assert!(d.create().is_err());
        assert!(d.clear().is_err());
        assert!(outside.join("keep").exists());
        let _ = fs::remove_dir_all(&base);
    }
}
