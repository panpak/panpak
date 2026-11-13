// SPDX-License-Identifier: MIT OR Apache-2.0
//! What an app may do on the session bus, as arguments for `xdg-dbus-proxy`.
//!
//! The list is closed: the portal is reached only through `--call` rules for named interfaces,
//! never `--talk` (which would open every portal interface), and a name may be owned only if it is
//! the app's own id, a name the manifest declares in `dbus_own` and that passes `own_name_problem`,
//! or a name a permission needs. Pure functions, so that the stub, `doctor`, `install` and the
//! tests all use the same rules.

use super::wire::valid_dotted_name;
use crate::Permissions;
use anyhow::{bail, ensure, Result};

/// At most this many names in `dbus_own`.
pub const MAX_OWN: usize = 8;

/// The portal and its main object.
pub const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// Names (and everything below them) that an app never owns: each is a service of the session, the
/// desktop or the system, and owning it would let the app answer in its place. Matched as the name
/// itself or the name followed by `.` and anything. `org.kde` is handled separately.
const NEVER_OWN: &[(&str, &str)] = &[
    ("org.freedesktop", "session and system services (portals, notifications, secrets, systemd, login, policy kit, screen saver, file manager)"),
    ("org.gtk", "services of GTK (virtual file system, notifications, settings)"),
    ("org.gnome.Shell", "the GNOME shell"),
    ("org.gnome.SessionManager", "the GNOME session"),
    ("org.gnome.Mutter", "the GNOME compositor"),
    ("org.gnome.SettingsDaemon", "the GNOME settings daemons"),
    ("org.gnome.keyring", "the keyring"),
    ("org.gnome.evolution", "the mail, calendar and contacts data server"),
    ("org.gnome.OnlineAccounts", "online accounts"),
    ("org.gnome.ScreenSaver", "the screen saver"),
    ("org.a11y", "the accessibility bus: it sees and drives every window"),
    ("ca.desrt.dconf", "the settings writer: it can change what the session runs"),
    ("com.canonical", "the Ubuntu desktop (global menu, launcher, indicators)"),
    ("com.ubuntu", "the Ubuntu desktop"),
    ("org.ayatana", "desktop indicators"),
    ("org.ibus", "the input method: it sees typed keys"),
    ("org.fcitx", "the input method: it sees typed keys"),
    ("org.mpris", "media player names (a permission, not yet available)"),
    ("org.xfce", "the Xfce desktop"),
    ("org.mate", "the MATE desktop"),
    ("org.cinnamon", "the Cinnamon desktop"),
    ("org.pantheon", "the Pantheon desktop"),
    ("org.lxqt", "the LXQt desktop"),
    ("org.bluez", "bluetooth"),
    ("net.hadess", "power profiles and GPU switching"),
    ("org.pulseaudio", "the sound server"),
    ("org.PulseAudio1", "the sound server"),
    ("org.pipewire", "the sound server"),
    ("fi.w1", "network configuration"),
    ("com.system76", "system management"),
    ("com.redhat", "system management"),
    ("org.fedoraproject", "system management"),
    ("org.opensuse", "system management"),
    ("org.cups", "printing"),
];

/// The watcher a tray icon registers with. Allowed only with the `tray` permission.
pub const TRAY_WATCHER: &str = "org.kde.StatusNotifierWatcher";

fn normalized(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

fn family_problem(name: &str) -> Option<String> {
    for (stem, why) in NEVER_OWN {
        if name == *stem || name.strip_prefix(stem).is_some_and(|r| r.starts_with('.')) {
            return Some(format!("{stem}.* is never granted: {why}"));
        }
    }
    None
}

/// `org.kde.<name>` is the app's own only when `<name>` is one lower-case word that is the last part
/// of the id, the app name or the program name.
fn kde_problem(name: &str, app_id: &str, app_name: &str, entry: &str) -> Option<String> {
    let rest = name.strip_prefix("org.kde.")?;
    let own = [app_id.rsplit('.').next().unwrap_or(""), app_name, entry.rsplit('/').next().unwrap_or("")];
    let ok = !rest.contains('.') && rest.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) && own.iter().any(|w| !w.is_empty() && normalized(w) == rest);
    (!ok).then(|| "org.kde.<name> is granted only when <name> is the app's own name (the last part of the id, the app name or the program name), in lower case".to_string())
}

/// Why `name`, declared in `dbus_own`, may not be owned by this app, or `None`. `app_id`, `app_name`
/// and `entry` give the words that make `org.kde.<word>` the app's own KDE name.
pub fn own_name_problem(name: &str, app_id: &str, app_name: &str, entry: &str) -> Option<String> {
    if name.contains('*') {
        return Some("no wildcards".into());
    }
    if !valid_dotted_name(name, false) {
        return Some("not a valid D-Bus name (elements of letters, digits and `_`, at least two, at most 255 bytes)".into());
    }
    if name.split('.').count() < 3 {
        return Some("at least three elements".into());
    }
    family_problem(name).or_else(|| kde_problem(name, app_id, app_name, entry))
}

/// Why the app's own id cannot be owned for single instance, or `None`. An id was validated as an
/// app id already; only the families above and the KDE rule apply.
pub fn id_own_problem(app_id: &str, app_name: &str, entry: &str) -> Option<String> {
    family_problem(app_id).or_else(|| kde_problem(app_id, app_id, app_name, entry))
}

/// Check the `dbus_own` list of a manifest.
pub fn check_dbus_own(names: &[String], app_id: &str, app_name: &str, entry: &str) -> Result<()> {
    ensure!(names.len() <= MAX_OWN, "more than {MAX_OWN} names");
    for (i, n) in names.iter().enumerate() {
        if let Some(why) = own_name_problem(n, app_id, app_name, entry) {
            let shown: String = n.chars().take(48).collect();
            bail!("{shown:?}: {why}");
        }
        ensure!(!names[..i].contains(n), "{n} listed twice");
    }
    Ok(())
}

/// What the layer is for, from the manifest.
pub struct BusNeeds<'a> {
    pub app_id: &'a str,
    pub app_name: &'a str,
    pub entry: &'a str,
    pub perms: Option<&'a Permissions>,
}

/// The name on the host bus that the app's media player name gets: `org.mpris.MediaPlayer2.panpak.<id>`.
pub fn mpris_alias_for(app_id: &str) -> String {
    super::route::mpris_alias(app_id)
}

/// What the router of the app is told about it (see `route`): the identity for the portal, the alias if the app may have one, and links.
pub fn router_config(app_id: &str, perms: Option<&Permissions>) -> super::route::Config {
    super::route::Config { portal_id: Some(format!("panpak.{app_id}")), mpris_alias: perms.is_some_and(|p| p.mpris == Some(true)).then(|| mpris_alias_for(app_id)), open_uri: true, file_chooser: None }
}

/// The arguments for the session bus part of `xdg-dbus-proxy`, after `ADDRESS PATH`.
///
/// The names of the app (its id, `dbus_own`) are **not** here: they live on the private bus of the app, in the router, and the host bus is never
/// asked for them. The one name the app can have on the host bus is the alias of a media player (permission `mpris`).
pub fn proxy_args(n: &BusNeeds<'_>) -> Vec<String> {
    let mut a = vec!["--filter".to_string()];
    let call = |a: &mut Vec<String>, name: &str, rule: &str, path: &str| a.push(format!("--call={name}={rule}@{path}"));
    let bcast = |a: &mut Vec<String>, name: &str, rule: &str, path: &str| a.push(format!("--broadcast={name}={rule}@{path}"));
    // Appearance, and the requests and sessions every portal call makes.
    call(&mut a, PORTAL, "org.freedesktop.portal.Settings.*", PORTAL_PATH);
    bcast(&mut a, PORTAL, "org.freedesktop.portal.Settings.SettingChanged", PORTAL_PATH);
    call(&mut a, PORTAL, "org.freedesktop.portal.Inhibit.*", PORTAL_PATH);
    call(&mut a, PORTAL, "org.freedesktop.portal.Request.*", "/org/freedesktop/portal/desktop/request/*");
    bcast(&mut a, PORTAL, "org.freedesktop.portal.Request.Response", "/org/freedesktop/portal/desktop/request/*");
    call(&mut a, PORTAL, "org.freedesktop.portal.Session.*", "/org/freedesktop/portal/desktop/session/*");
    bcast(&mut a, PORTAL, "org.freedesktop.portal.Session.Closed", "/org/freedesktop/portal/desktop/session/*");
    bcast(&mut a, PORTAL, "org.freedesktop.portal.Inhibit.StateChanged", "/org/freedesktop/portal/desktop/session/*");
    // The identity: the router registers `panpak.<id>` on each connection, as its first call. The app's own calls to the registry never get here
    // (the router refuses them), so this rule is for the router only.
    call(&mut a, PORTAL, "org.freedesktop.host.portal.Registry.Register", PORTAL_PATH);
    // No Properties.Get/GetAll on the portal: with them GTK finds the FileChooser interface and shows the portal's dialog, which hands the app a path
    // the sandbox cannot read (step 11). OpenURI does not need them: the router answers it. See docs/PRIVATE-BUS.md.
    if n.perms.is_some_and(|p| p.notifications == Some(true)) {
        call(&mut a, PORTAL, "org.freedesktop.portal.Notification.*", PORTAL_PATH);
        for m in ["Notify", "CloseNotification", "GetCapabilities", "GetServerInformation"] {
            call(&mut a, "org.freedesktop.Notifications", &format!("org.freedesktop.Notifications.{m}"), "/org/freedesktop/Notifications");
        }
    }
    // The tray: only the watcher. Measured with Chromium (Zulip 5.13.2) and Qt 6 (qBittorrent 5.3.0): with every
    // `StatusNotifierItem-*` name refused, both register the icon with the unique name of their connection, and the
    // host reads and activates the item through it. No item name is owned, so two apps never compete for one.
    if n.perms.is_some_and(|p| p.tray == Some(true)) {
        a.push(format!("--talk={TRAY_WATCHER}"));
    }
    // A media player: one alias that carries the id of the package, never a name of another player.
    if n.perms.is_some_and(|p| p.mpris == Some(true)) {
        a.push(format!("--own={}", mpris_alias_for(n.app_id)));
    }
    a
}

/// The arguments for the proxy on the accessibility (AT-SPI) bus, after `ADDRESS PATH`: the rules Flatpak uses
/// (`flatpak_run_add_a11y_dbus_args`, Flatpak 1.16.1) **without** `NotifyListenersSync` and `NotifyListenersAsync`, which let an app tell a screen
/// reader that a key was pressed. The app can register with the registry and be read; it cannot read or command the other applications.
pub fn a11y_proxy_args() -> Vec<String> {
    const REG: &str = "org.a11y.atspi.Registry";
    const DEC: &str = "/org/a11y/atspi/registry/deviceeventcontroller";
    vec![
        "--filter".into(),
        "--sloppy-names".into(),
        format!("--broadcast={REG}.EventListenerRegistered=@/org/a11y/atspi/registry"),
        format!("--broadcast={REG}.EventListenerDeregistered=@/org/a11y/atspi/registry"),
        format!("--call={REG}=org.a11y.atspi.Socket.Embed@/org/a11y/atspi/accessible/root"),
        format!("--call={REG}=org.a11y.atspi.Socket.Unembed@/org/a11y/atspi/accessible/root"),
        format!("--call={REG}=org.a11y.atspi.Registry.GetRegisteredEvents@/org/a11y/atspi/registry"),
        format!("--call={REG}=org.a11y.atspi.DeviceEventController.GetKeystrokeListeners@{DEC}"),
        format!("--call={REG}=org.a11y.atspi.DeviceEventController.GetDeviceEventListeners@{DEC}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(n: &str) -> Option<String> {
        own_name_problem(n, "io.github.example.Kdenlive", "Kdenlive Video Editor", "bin/kdenlive")
    }

    #[test]
    fn plain_names_pass() {
        for n in ["io.github.example.Kdenlive", "com.example.app.Instance", "org.example.Foo_Bar"] {
            assert_eq!(problem(n), None, "{n}");
        }
    }

    #[test]
    fn service_families_are_never_granted() {
        for n in [
            "org.freedesktop.Notifications", "org.freedesktop.portal.Desktop", "org.freedesktop.secrets", "org.freedesktop.systemd1", "org.gtk.vfs.Daemon", "org.gnome.Shell.Screenshot",
            "org.gnome.SessionManager", "org.gnome.keyring.Prompter", "org.gnome.evolution.dataserver.Calendar8", "org.a11y.Bus", "org.a11y.atspi.Registry", "ca.desrt.dconf", "com.canonical.AppMenu.Registrar",
            "org.ayatana.indicator.application", "org.ibus.ibus", "org.fcitx.Fcitx5", "org.mpris.MediaPlayer2.vlc", "net.hadess.PowerProfiles", "org.bluez.Foo", "org.pipewire.Foo", "org.PulseAudio1.Foo",
        ] {
            assert!(problem(n).is_some(), "{n} was accepted");
        }
        // Not the whole of org.gnome: an app may be called org.gnome.Calculator.
        assert_eq!(problem("org.gnome.Calculator"), None);
        // A name that only starts with the same letters is not in the family.
        assert_eq!(problem("org.freedesktopfoo.Bar"), None);
    }

    #[test]
    fn the_kde_rule_is_deterministic() {
        assert_eq!(problem("org.kde.kdenlive"), None, "last part of the id");
        assert_eq!(problem("org.kde.kdenlivevideoeditor"), None, "app name");
        assert!(problem("org.kde.StatusNotifierWatcher").is_some());
        assert!(problem("org.kde.plasmashell").is_some());
        assert!(problem("org.kde.kwalletd5").is_some());
        assert!(problem("org.kde.Kdenlive").is_some(), "upper case");
        assert!(problem("org.kde.kdenlive.sub").is_some(), "more than one part");
        assert!(problem("org.kde.kdenlive-1234").is_some(), "per-process name");
        // An id that is itself in the KDE namespace owns its own id.
        assert_eq!(own_name_problem("org.kde.krita", "org.kde.krita", "Krita", "bin/krita"), None);
    }

    #[test]
    fn syntax_is_checked() {
        for n in ["", "a", "a.b", "org.example.*", "org.example.a b", "org.example.1x", ".org.example.x", "org..example.x", "org.example.é", ":1.42"] {
            assert!(problem(n).is_some(), "{n:?}");
        }
        assert!(problem(&format!("org.example.{}", "a".repeat(260))).is_some());
    }

    #[test]
    fn the_list_is_limited_and_without_duplicates() {
        let ok: Vec<String> = (0..8).map(|i| format!("com.example.n{i}")).collect();
        check_dbus_own(&ok, "com.example.app", "App", "app").unwrap();
        let mut nine = ok.clone();
        nine.push("com.example.n9".into());
        assert!(check_dbus_own(&nine, "com.example.app", "App", "app").is_err());
        assert!(check_dbus_own(&["com.example.n0".into(), "com.example.n0".into()], "com.example.app", "App", "app").is_err());
        assert!(check_dbus_own(&["org.freedesktop.Notifications".into()], "com.example.app", "App", "app").is_err());
    }

    fn perms(extra: &str) -> Permissions {
        let t = format!("[app]\nid=\"com.example.app\"\nname=\"App\"\nversion=\"1\"\nentry=\"bin/app\"\n[permissions]\n{extra}");
        toml::from_str::<crate::Manifest>(&t).unwrap().permissions.unwrap()
    }

    fn args(extra: &str) -> Vec<String> {
        let p = perms(extra);
        proxy_args(&BusNeeds { app_id: "com.example.app", app_name: "App", entry: "bin/app", perms: Some(&p) })
    }

    #[test]
    fn the_base_rules_use_call_for_the_portal_and_never_talk() {
        let a = args("");
        assert_eq!(a[0], "--filter");
        assert!(a.iter().all(|x| !x.starts_with("--talk=org.freedesktop")), "{a:?}");
        assert!(a.iter().all(|x| !x.starts_with("--see")), "{a:?}");
        for want in [
            "--call=org.freedesktop.portal.Desktop=org.freedesktop.portal.Settings.*@/org/freedesktop/portal/desktop",
            "--broadcast=org.freedesktop.portal.Desktop=org.freedesktop.portal.Settings.SettingChanged@/org/freedesktop/portal/desktop",
            "--call=org.freedesktop.portal.Desktop=org.freedesktop.portal.Inhibit.*@/org/freedesktop/portal/desktop",
            "--call=org.freedesktop.portal.Desktop=org.freedesktop.portal.Request.*@/org/freedesktop/portal/desktop/request/*",
            "--broadcast=org.freedesktop.portal.Desktop=org.freedesktop.portal.Request.Response@/org/freedesktop/portal/desktop/request/*",
            "--call=org.freedesktop.portal.Desktop=org.freedesktop.portal.Session.*@/org/freedesktop/portal/desktop/session/*",
            "--call=org.freedesktop.portal.Desktop=org.freedesktop.host.portal.Registry.Register@/org/freedesktop/portal/desktop",
        ] {
            assert!(a.iter().any(|x| x == want), "missing {want}");
        }
        assert!(a.iter().all(|x| !x.contains("Properties")), "no Properties call on the portal: {a:?}");
        // The names of the app are on its private bus: the host is never asked to give them.
        assert!(a.iter().all(|x| !x.starts_with("--own=")), "{a:?}");
        assert!(a.iter().all(|x| !x.contains("com.example.app")), "{a:?}");
        // Nothing for the permissions that are off.
        assert!(a.iter().all(|x| !x.contains("Notification") && !x.contains("StatusNotifier") && !x.contains("mpris")), "{a:?}");
    }

    #[test]
    fn permissions_add_exactly_their_rules() {
        let a = args("notifications = true\n");
        assert!(a.iter().any(|x| x.contains("org.freedesktop.portal.Notification.*@/org/freedesktop/portal/desktop")));
        assert!(a.iter().any(|x| x == "--call=org.freedesktop.Notifications=org.freedesktop.Notifications.Notify@/org/freedesktop/Notifications"));
        // The old service's signals are broadcasts seen by every client: not forwarded.
        assert!(a.iter().all(|x| !(x.starts_with("--broadcast=org.freedesktop.Notifications") || x.contains("ActionInvoked") || x.contains("NotificationClosed"))), "{a:?}");
        assert!(a.iter().all(|x| x != "--talk=org.freedesktop.Notifications"));
        // The tray: the watcher only, no item name.
        assert!(args("").iter().all(|x| !x.contains("StatusNotifier")));
        let t = args("tray = true\n");
        assert!(t.iter().any(|x| x == "--talk=org.kde.StatusNotifierWatcher"));
        assert!(t.iter().all(|x| !x.starts_with("--own=") || !x.contains("StatusNotifier")), "{t:?}");
        assert_eq!(t.iter().filter(|x| x.contains("StatusNotifier")).count(), 1);
    }

    #[test]
    fn declared_names_are_not_asked_of_the_host() {
        for extra in ["dbus_own = [\"com.example.app.Second\"]\n", "dbus_own = [\"org.freedesktop.Notifications\"]\n"] {
            let a = args(extra);
            assert!(a.iter().all(|x| !x.starts_with("--own=") && !x.contains("Second") && !x.contains("Notifications")), "{a:?}");
        }
    }

    #[test]
    fn a_media_player_gets_one_alias_with_the_id() {
        assert!(args("").iter().all(|x| !x.contains("mpris")));
        let a = args("mpris = true\n");
        let own: Vec<&String> = a.iter().filter(|x| x.starts_with("--own=")).collect();
        assert_eq!(own, ["--own=org.mpris.MediaPlayer2.panpak.com_example_app"]);
        assert!(a.iter().all(|x| !x.starts_with("--talk=org.mpris")));
        // The names of other players stay unavailable for dbus_own.
        assert!(own_name_problem("org.mpris.MediaPlayer2.vlc", "com.example.app", "App", "bin/app").is_some());
    }

    #[test]
    fn the_router_is_told_the_identity_and_the_alias() {
        let p = perms("mpris = true\n");
        let c = router_config("com.example.app", Some(&p));
        assert_eq!(c.portal_id.as_deref(), Some("panpak.com.example.app"));
        assert_eq!(c.mpris_alias.as_deref(), Some("org.mpris.MediaPlayer2.panpak.com_example_app"));
        assert!(c.open_uri);
        assert_eq!(router_config("com.example.app", None).mpris_alias, None);
    }

    #[test]
    fn the_accessibility_rules_are_flatpaks_without_the_key_notifications() {
        let a = a11y_proxy_args();
        assert_eq!(a[0], "--filter");
        assert_eq!(a.iter().filter(|x| x.starts_with("--call=")).count(), 5);
        assert_eq!(a.iter().filter(|x| x.starts_with("--broadcast=")).count(), 2);
        assert!(a.iter().all(|x| !x.contains("NotifyListeners") && !x.contains("RegisterEvent") && !x.contains("GetChildren") && !x.contains("--talk") && !x.contains("--own")), "{a:?}");
        assert!(a.iter().all(|x| x.starts_with("--filter") || x.starts_with("--sloppy") || x.contains("org.a11y.atspi.Registry")));
    }
}
