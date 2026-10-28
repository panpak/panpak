// SPDX-License-Identifier: MIT OR Apache-2.0
//! Private folders of an app, what the app may read of the user's own
//! configuration, and how permissions are shown to the user.
//! Layout and rules: PANPAK-FORMAT.md §9.1 and THREAT-MODEL.md §2.2.
use crate::{coded, validate_app_id, ErrorCode, Manifest, PLACEHOLDERS};
use anyhow::{Context, Result};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// First line of the `fonts.conf` the stub writes. A file without it is the user's.
pub const FONTS_CONF_MARKER: &str = "X-Panpak-Generated";

/// The folders an app owns. `root` and `cache` are the only writable roots
/// besides `/tmp` and the `home_paths` of the manifest.
///
/// - `root`  = `$XDG_DATA_HOME/panpak/userdata/<id>`, with `config`, `data`, `state`, `tmp`;
/// - `cache` = `$XDG_CACHE_HOME/panpak/<id>`.
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
    Ok(AppDirs {
        config: root.join("config"),
        data: root.join("data"),
        state: root.join("state"),
        tmp: root.join("tmp"),
        root,
        cache,
    })
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
    /// Create the folders with mode 0700. Run before the sandbox is applied.
    pub fn create(&self) -> Result<()> {
        for parent in [self.root.parent(), self.cache.parent()].into_iter().flatten() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        for d in [&self.root, &self.config, &self.data, &self.state, &self.tmp, &self.cache] {
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
    const CONFIG: &[&str] =
        &["fontconfig", "gtk-3.0", "gtk-4.0", "kdeglobals", "user-dirs.dirs", "mimeapps.list", "qt5ct", "qt6ct", "Trolltech.conf"];
    const DATA: &[&str] = &["fonts", "icons", "themes", "mime"];
    const HOME: &[&str] = &[".fonts", ".icons"];
    let mut all = Vec::new();
    if let Some(c) = dirs::config_dir() {
        all.extend(CONFIG.iter().map(|n| c.join(n)));
    }
    if let Some(d) = dirs::data_dir() {
        all.extend(DATA.iter().map(|n| d.join(n)));
    }
    if let Some(h) = dirs::home_dir() {
        all.extend(HOME.iter().map(|n| h.join(n)));
    }
    all.retain(|p| p.exists());
    all
}

/// `XDG_CONFIG_DIRS` and `XDG_DATA_DIRS` for the app: the existing value, or the default of the
/// XDG Base Directory Specification, then the user's real config and data folder at the end.
/// Landlock lets the app read only part of those two (`readonly_user_paths`).
pub fn xdg_search_dirs(config_dirs: Option<String>, data_dirs: Option<String>) -> Result<(String, String)> {
    let real_config = dirs::config_dir().context("no config dir")?;
    let real_data = dirs::data_dir().context("no data dir")?;
    let join = |current: Option<String>, default: &str, real: &Path| -> Result<String> {
        let base = current.filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string());
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

/// Shown whenever a package asks for the D-Bus socket of the desktop session or system.
pub const DESKTOP_BUS_WARNING: &str =
    "Full access to desktop services (D-Bus): this app is not isolated";

/// Printed by `install` and `update` when the kernel cannot block local sockets.
pub const PARTIAL_ISOLATION_WARNING: &str = "WARNING: on this kernel the sandbox cannot block local sockets (D-Bus, ssh-agent, gpg-agent): this app is only partially isolated";

/// Printed when X11 access leaves the abstract Unix sockets open.
pub const ABSTRACT_SOCKETS_WARNING: &str = "WARNING: X11 access leaves abstract Unix sockets open (some D-Bus and agent sockets): this app is only partially isolated";

/// How well an app is kept away from the local Unix sockets (D-Bus, ssh-agent, gpg-agent,
/// Wayland and the like). One place decides it, from the kernel and the permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketIsolation {
    /// Landlock V9: path sockets are denied unless `sockets_allow` lists them, and the
    /// abstract-socket scope is on.
    Isolated,
    /// The kernel does not filter `connect()` to path sockets (Landlock below V9, or none).
    KernelCannotFilter,
    /// The manifest lists a D-Bus socket.
    ManifestAllowsBus,
    /// The manifest asks for X11, which turns off the abstract-socket scope.
    AbstractSocketsOpen,
}

impl SocketIsolation {
    /// The warning to print, if the app is not fully isolated. `ManifestAllowsBus` has its
    /// own line in `describe_permissions`.
    pub fn warning(self) -> Option<&'static str> {
        match self {
            SocketIsolation::Isolated | SocketIsolation::ManifestAllowsBus => None,
            SocketIsolation::KernelCannotFilter => Some(PARTIAL_ISOLATION_WARNING),
            SocketIsolation::AbstractSocketsOpen => Some(ABSTRACT_SOCKETS_WARNING),
        }
    }
}

/// True if the manifest lists a D-Bus socket in `sockets_allow`. The stub then keeps
/// `DBUS_SESSION_BUS_ADDRESS` as it is.
pub fn allows_desktop_bus(perms: Option<&crate::Permissions>) -> bool {
    perms.is_some_and(|p| p.sockets_allow.iter().flatten().any(|s| is_bus_socket(s)))
}

/// The isolation level of an app from local sockets: `abi` is the Landlock ABI of the kernel
/// (None if Landlock is missing), `perms` the manifest permissions (None for the kernel alone).
pub fn socket_isolation(abi: Option<landlock::ABI>, perms: Option<&crate::Permissions>) -> SocketIsolation {
    if abi.is_none_or(|a| a < landlock::ABI::V9) {
        SocketIsolation::KernelCannotFilter
    } else if allows_desktop_bus(perms) {
        SocketIsolation::ManifestAllowsBus
    } else if perms.is_some_and(|p| p.x11 == Some(true)) {
        SocketIsolation::AbstractSocketsOpen
    } else {
        SocketIsolation::Isolated
    }
}

/// True if a `sockets_allow` entry names a D-Bus socket (`$XDG_RUNTIME_DIR/bus`,
/// `/run/dbus/system_bus_socket`, and the like). The list is a heuristic: a message bus is
/// recognised by a path part `bus`, `dbus` or `system_bus_socket`.
pub fn is_bus_socket(path: &str) -> bool {
    path.split('/').any(|p| matches!(p, "bus" | "dbus" | "dbus-1" | "system_bus_socket"))
}

/// The permissions of a manifest, one line each, for `install` and `update`.
pub fn describe_permissions(man: &Manifest) -> Vec<String> {
    let mut out = Vec::new();
    let Some(p) = &man.permissions else { return out };
    if p.x11 == Some(true) {
        out.push("X11 access: the app can observe other apps' input and screen".to_string());
    }
    let list = |v: &Option<Vec<u16>>| v.iter().flatten().map(u16::to_string).collect::<Vec<_>>().join(", ");
    if p.network == Some(true) || p.network_allow_tcp.is_some() || p.network_bind_tcp.is_some() {
        out.push(format!(
            "network: connect to TCP ports [{}], listen on TCP ports [{}] (filtered by port, not by address)",
            list(&p.network_allow_tcp),
            list(&p.network_bind_tcp)
        ));
    }
    for s in p.sockets_allow.iter().flatten() {
        out.push(format!("Unix socket: {s}"));
    }
    if p.sockets_allow.iter().flatten().any(|s| is_bus_socket(s)) {
        out.push(format!("WARNING: {DESKTOP_BUS_WARNING}"));
    }
    let homes: Vec<String> =
        p.home_paths.iter().flatten().map(|h| format!("{} ({})", h.path, h.access)).collect();
    if !homes.is_empty() {
        out.push(format!("Partial access to your home: {}", homes.join(", ")));
    }
    out
}

/// Permissions in `new` that `old` did not have: a feature turned on, a port, a socket, a home
/// path, or `ro` raised to `rw`. Used to ask before an update.
pub fn added_permissions(old: &Manifest, new: &Manifest) -> Vec<String> {
    let mut out = Vec::new();
    let o = old.permissions.as_ref();
    let Some(n) = &new.permissions else { return out };
    if n.x11 == Some(true) && o.and_then(|p| p.x11) != Some(true) {
        out.push("X11 access".to_string());
    }
    let had_net = o.is_some_and(|p| p.network == Some(true));
    if n.network == Some(true) && !had_net {
        out.push("network access".to_string());
    }
    let ports = |sel: fn(&crate::Permissions) -> &Option<Vec<u16>>, p: Option<&crate::Permissions>| -> Vec<u16> {
        p.map(|p| sel(p).clone().unwrap_or_default()).unwrap_or_default()
    };
    for (what, sel) in [
        ("connect to TCP port", (|p: &crate::Permissions| &p.network_allow_tcp) as fn(&crate::Permissions) -> &Option<Vec<u16>>),
        ("listen on TCP port", |p: &crate::Permissions| &p.network_bind_tcp),
    ] {
        let before = ports(sel, o);
        for port in ports(sel, Some(n)) {
            if !before.contains(&port) {
                out.push(format!("{what} {port}"));
            }
        }
    }
    let had_socks = o.map(|p| p.sockets_allow.clone().unwrap_or_default()).unwrap_or_default();
    for s in n.sockets_allow.iter().flatten() {
        if !had_socks.contains(s) {
            out.push(format!("Unix socket {s}"));
            if is_bus_socket(s) && !had_socks.iter().any(|h| is_bus_socket(h)) {
                out.push(DESKTOP_BUS_WARNING.to_string());
            }
        }
    }
    let had_homes = o.map(|p| p.home_paths.clone().unwrap_or_default()).unwrap_or_default();
    for h in n.home_paths.iter().flatten() {
        let covered = had_homes.iter().any(|x| x.path == h.path && (x.access == "rw" || h.access == "ro"));
        if !covered {
            out.push(format!("access to {} in your home ({})", h.path, h.access));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn man(perm: &str) -> Manifest {
        toml::from_str(&format!(
            "[app]\nid=\"org.example.app\"\nname=\"App\"\nversion=\"1.0\"\nentry=\"bin/app\"\n{perm}[update]\nkey=\"ed25519:{}\"\n",
            "0".repeat(64)
        ))
        .unwrap()
    }

    fn dirs_at(base: &str) -> AppDirs {
        let root = PathBuf::from(base).join("userdata/org.example.app");
        AppDirs {
            config: root.join("config"),
            data: root.join("data"),
            state: root.join("state"),
            tmp: root.join("tmp"),
            root,
            cache: PathBuf::from(base).join("cache/org.example.app"),
        }
    }

    #[test]
    fn expand_replaces_only_known_placeholders() {
        let d = dirs_at("/x");
        assert_eq!(d.expand("--profile=${XDG_CONFIG_HOME}/p").unwrap(), "--profile=/x/userdata/org.example.app/config/p");
        assert_eq!(d.expand("${TMPDIR}:${XDG_CACHE_HOME}").unwrap(), "/x/userdata/org.example.app/tmp:/x/cache/org.example.app");
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
        let (c, d) = xdg_search_dirs(Some("/a:/b".into()), None).unwrap();
        assert!(c.starts_with("/a:/b:"), "{c}");
        assert!(d.starts_with("/usr/local/share:/usr/share:"), "{d}");
        let (c, _) = xdg_search_dirs(Some(String::new()), None).unwrap();
        assert!(c.starts_with("/etc/xdg:"), "{c}");
    }

    #[test]
    fn added_permissions_lists_only_new() {
        let old = man("[permissions]\nnetwork=true\nnetwork_allow_tcp=[443]\n[[permissions.home_paths]]\npath=\"a\"\naccess=\"rw\"\n");
        let same = man("[permissions]\nnetwork=true\nnetwork_allow_tcp=[443]\n[[permissions.home_paths]]\npath=\"a\"\naccess=\"ro\"\n");
        assert!(added_permissions(&old, &same).is_empty());
        let more = man("[permissions]\nx11=true\nnetwork=true\nnetwork_allow_tcp=[443, 8080]\nnetwork_bind_tcp=[2658]\n[[permissions.home_paths]]\npath=\"a\"\naccess=\"rw\"\n[[permissions.home_paths]]\npath=\"b\"\naccess=\"ro\"\n");
        let added = added_permissions(&old, &more);
        assert_eq!(added.len(), 4, "{added:?}");
        let raised = man("[permissions]\nnetwork=true\nnetwork_allow_tcp=[443]\n[[permissions.home_paths]]\npath=\"a\"\naccess=\"rw\"\n");
        let from_ro = man("[permissions]\nnetwork=true\nnetwork_allow_tcp=[443]\n[[permissions.home_paths]]\npath=\"a\"\naccess=\"ro\"\n");
        assert_eq!(added_permissions(&from_ro, &raised).len(), 1);
        assert_eq!(added_permissions(&man(""), &old).len(), 3);
    }

    #[test]
    fn bus_sockets_are_recognised_and_warned_about() {
        for p in ["$XDG_RUNTIME_DIR/bus", "${XDG_RUNTIME_DIR}/bus", "/run/user/1000/bus", "/run/dbus/system_bus_socket", "/var/run/dbus/system_bus_socket"] {
            assert!(is_bus_socket(p), "{p}");
        }
        for p in ["$XDG_RUNTIME_DIR/pulse/native", "/run/user/1000/wayland-0", "$HOME/.cache/business"] {
            assert!(!is_bus_socket(p), "{p}");
        }
        let m = man("[permissions]\nsockets_allow=[\"$XDG_RUNTIME_DIR/bus\"]\n");
        assert!(describe_permissions(&m).iter().any(|l| l.contains(DESKTOP_BUS_WARNING)));
        let plain = man("[permissions]\nsockets_allow=[\"$XDG_RUNTIME_DIR/pulse/native\"]\n");
        assert!(!describe_permissions(&plain).iter().any(|l| l.contains("not isolated")));
        // An update that adds the bus is flagged; one that already had it is not flagged twice.
        assert!(added_permissions(&plain, &m).iter().any(|l| l == DESKTOP_BUS_WARNING));
        assert!(!added_permissions(&m, &m).iter().any(|l| l == DESKTOP_BUS_WARNING));
    }

    #[test]
    fn socket_isolation_depends_on_kernel_and_permissions() {
        use landlock::ABI;
        let plain = man("");
        let bus = man("[permissions]\nsockets_allow=[\"$XDG_RUNTIME_DIR/bus\"]\n");
        let x11 = man("[permissions]\nx11=true\n");
        let wl = man("[permissions]\nsockets_allow=[\"$XDG_RUNTIME_DIR/wayland-0\"]\n");
        for abi in [None, Some(ABI::V1), Some(ABI::V6), Some(ABI::V8)] {
            for m in [&plain, &bus, &x11, &wl] {
                let iso = socket_isolation(abi, m.permissions.as_ref());
                assert_eq!(iso, SocketIsolation::KernelCannotFilter, "{abi:?}");
                assert_eq!(iso.warning(), Some(PARTIAL_ISOLATION_WARNING));
            }
        }
        let v9 = Some(ABI::V9);
        assert_eq!(socket_isolation(v9, None), SocketIsolation::Isolated);
        assert_eq!(socket_isolation(v9, plain.permissions.as_ref()), SocketIsolation::Isolated);
        assert_eq!(socket_isolation(v9, wl.permissions.as_ref()), SocketIsolation::Isolated);
        assert_eq!(socket_isolation(v9, bus.permissions.as_ref()), SocketIsolation::ManifestAllowsBus);
        assert_eq!(socket_isolation(v9, x11.permissions.as_ref()), SocketIsolation::AbstractSocketsOpen);
        assert_eq!(SocketIsolation::Isolated.warning(), None);
        assert_eq!(SocketIsolation::ManifestAllowsBus.warning(), None);
        assert!(allows_desktop_bus(bus.permissions.as_ref()) && !allows_desktop_bus(plain.permissions.as_ref()));
    }

    #[test]
    fn describe_shows_home_line() {
        let m = man("[permissions]\n[[permissions.home_paths]]\npath=\".config/x\"\naccess=\"rw\"\n");
        assert_eq!(describe_permissions(&m), vec!["Partial access to your home: .config/x (rw)".to_string()]);
    }

    #[test]
    fn create_makes_private_folders_and_refuses_links() {
        let base = std::env::temp_dir().join(format!("panpak-layout-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let d = dirs_at(base.to_str().unwrap());
        d.create().unwrap();
        for p in [&d.root, &d.config, &d.data, &d.state, &d.tmp, &d.cache] {
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
