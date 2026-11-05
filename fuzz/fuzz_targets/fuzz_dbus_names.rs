#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;
use panpak_core::dbus::{check_dbus_own, own_name_problem, proxy_args, BusNeeds};

// A `dbus_own` name comes from a manifest. Whatever the text, a name that is accepted is a plain
// D-Bus name of at least three elements, has no wildcard, is not in a service family, and the proxy
// arguments built from it never contain a wildcard, a `--talk` to a portal, or a name of a family.
fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let mut parts = text.split('\n');
    let name = parts.next().unwrap_or("");
    let app = parts.next().unwrap_or("org.example.app");
    let app_name = parts.next().unwrap_or("App");
    if own_name_problem(name, app, app_name, "bin/app").is_none() {
        assert!(!name.contains('*') && name.len() <= 255 && name.split('.').count() >= 3, "{name:?}");
        assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.'), "{name:?}");
        for stem in ["org.freedesktop", "org.gtk", "org.a11y", "ca.desrt.dconf", "com.canonical", "org.ibus", "org.fcitx"] {
            assert!(name != stem && !name.starts_with(&format!("{stem}.")), "{name:?}");
        }
        let names = vec![name.to_string()];
        assert!(check_dbus_own(&names, app, app_name, "bin/app").is_ok());
        let toml = format!("[app]\nid = \"a.b\"\nname = \"x\"\nversion = \"1\"\nentry = \"x\"\n[permissions]\ndbus_own = [{name:?}]\n");
        let man: panpak_core::Manifest = toml::from_str(&toml).unwrap();
        let args = proxy_args(&BusNeeds { app_id: "a.b", app_name: "x", entry: "x", perms: man.permissions.as_ref() });
        assert!(args.iter().all(|a| !a.contains('*') || a.contains("@/org/freedesktop/portal/desktop") || a.contains(".*=") || a.contains(".*@")), "{args:?}");
        assert!(args.iter().all(|a| !a.starts_with("--talk=org.freedesktop.portal")), "{args:?}");
    }
});
