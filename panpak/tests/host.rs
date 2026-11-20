// SPDX-License-Identifier: MIT OR Apache-2.0
// The `[host]` table (PANPAK-FORMAT.md 3.1.11): `pack` writes the architecture and the glibc, an author gives the rest; `install` refuses a package that the system
// cannot run and says what is needed, what is there and what to do; `--force-host` installs it anyway and `doctor` keeps saying it; the start looks again at the
// display and the Landlock ABI.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const ID: &str = "org.example.hostlab";

fn cli_env(home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_panpak"));
    c.args(args)
        .current_dir(home)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/panpak-test-no-bus")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env_remove("PANPAK_SECRET_FILE")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env("PANPAK_TEST_LINK_STORE", "1")
        .env("PANPAK_NO_BACKGROUND", "1")
        .env_remove("LANGUAGE")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn cli(home: &Path, args: &[&str]) -> Output {
    cli_env(home, args, &[])
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

struct Lab {
    home: PathBuf,
    key: PathBuf,
}

impl Drop for Lab {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = Command::new("chmod").args(["-R", "u+w"]).arg(&self.home).output();
            let _ = fs::remove_dir_all(&self.home);
        }
    }
}

fn lab(name: &str) -> Lab {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("host-{name}-{}", std::process::id()));
    let _ = Command::new("chmod").args(["-R", "u+w"]).arg(&home).output();
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join("run")).unwrap();
    fs::set_permissions(home.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
    let key = home.join("secret.key");
    assert!(cli(&home, &["keygen", "-o", key.to_str().unwrap()]).status.success());
    // The runtime that an install makes ready is the real stub next to `panpak` (`cargo build -p panpak-stub` first); the one that `pack` embeds is a script.
    let real = Path::new(env!("CARGO_BIN_EXE_panpak")).with_file_name("panpak-stub");
    assert!(real.is_file(), "{} is missing: build it first (cargo build -p panpak-stub)", real.display());
    let stub = home.join("stub");
    fs::write(&stub, "#!/bin/sh\necho 1\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    Lab { home, key }
}

impl Lab {
    /// Pack `name` with `extra` (tables of `panpak.toml` after `[app]`, such as `[host]`), over `previous` when given.
    fn pack(&self, name: &str, extra: &str, previous: Option<&Path>) -> Result<PathBuf, Output> {
        let tree = self.home.join(format!("tree-{name}"));
        let _ = fs::remove_dir_all(&tree);
        fs::create_dir_all(tree.join("bin")).unwrap();
        fs::write(tree.join("bin/hello"), format!("#!/bin/sh\necho {name}\n")).unwrap();
        fs::set_permissions(tree.join("bin/hello"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(tree.join("panpak.toml"), format!("[app]\nid = \"{ID}\"\nname = \"HostLab\"\nversion = \"{name}\"\nentry = \"bin/hello\"\n{extra}")).unwrap();
        let out = self.home.join(format!("{name}.panpak"));
        let stub = self.home.join("stub");
        let (tree_s, stub_s, key_s, out_s) = (tree.to_str().unwrap(), stub.to_str().unwrap(), self.key.to_str().unwrap(), out.to_str().unwrap());
        let mut args = vec!["pack", tree_s, "--stub", stub_s, "--secret-file", key_s, "-o", out_s];
        if let Some(p) = previous {
            args.extend(["--previous", p.to_str().unwrap()]);
        }
        let o = cli(&self.home, &args);
        if o.status.success() { Ok(out) } else { Err(o) }
    }

    fn pack_ok(&self, name: &str, extra: &str, previous: Option<&Path>) -> PathBuf {
        self.pack(name, extra, previous).unwrap_or_else(|o| panic!("pack {name}: {}", text(&o)))
    }

    fn install(&self, pkg: &Path, flags: &[&str], env: &[(&str, &str)]) -> Output {
        cli_env(&self.home, &[&["install", "--yes", "--trust-first-key"], flags, &[pkg.to_str().unwrap()]].concat(), env)
    }

    fn manifest(&self, pkg: &Path) -> String {
        let out = self.home.join("manifest.toml");
        assert!(cli(&self.home, &["dump-manifest", pkg.to_str().unwrap(), out.to_str().unwrap()]).status.success());
        fs::read_to_string(out).unwrap()
    }

    fn appdir(&self) -> PathBuf {
        self.home.join(".local/share/panpak/apps").join(ID)
    }
}

#[test]
fn pack_writes_the_architecture_and_nobody_may_give_it_or_the_old_glibc_key() {
    let l = lab("write");
    let m = l.manifest(&l.pack_ok("1", "", None));
    assert!(m.contains("[host]") && m.contains(&format!("arch = \"{}\"", std::env::consts::ARCH)), "{m}");
    assert!(!m.contains("glibc_min"), "{m}");
    // `host` is a critical section: its keys are in the list that an older reader refuses on.
    assert!(m.contains("\"host.arch\""), "{m}");
    // The architecture is written by pack.
    let o = l.pack("2", "[host]\narch = \"x86_64\"\n", None).unwrap_err();
    assert!(text(&o).contains("written by `panpak pack`"), "{}", text(&o));
    // The old key of `[abi]` is gone: the message of the schema check names it.
    let o = l.pack("3", "[abi]\nglibc_min = \"2.30\"\n", None).unwrap_err();
    assert!(text(&o).contains("glibc_min"), "{}", text(&o));
    // An author may raise the glibc, with the form of a version, and the forms are checked.
    let m = l.manifest(&l.pack_ok("4", "[host]\nglibc = \"2.20\"\nkernel = \"5.10\"\nlandlock = 1\ndisplay = [\"x11\", \"wayland\"]\nportals = [\"document\"]\nbus = \"none\"\n", None));
    assert!(m.contains("glibc = \"2.20\"") && m.contains("kernel = \"5.10\"") && m.contains("landlock = 1"), "{m}");
    for bad in ["kernel = \"five\"", "landlock = 12", "display = []", "display = [\"x11\", \"x11\"]"] {
        let o = l.pack("5", &format!("[host]\n{bad}\n"), None).unwrap_err();
        assert!(text(&o).contains("host."), "{bad}: {}", text(&o));
    }
}

#[test]
fn a_package_that_the_system_cannot_run_is_refused_with_what_is_needed_what_is_there_and_what_to_do_and_force_host_goes_on() {
    let l = lab("refuse");
    // The display is the session's: it is not part of what an install checks, and the machine is what is refused.
    let pkg = l.pack_ok("1", "[host]\nkernel = \"99.1\"\ndisplay = [\"wayland\"]\nlandlock = 9\n", None);
    let o = l.install(&pkg, &[], &[]);
    let t = text(&o);
    assert!(!o.status.success() && t.contains("PANPAK-E2009"), "{t}");
    assert!(t.contains("HostLab 1 cannot run on this system:") && t.contains("needs Linux 99.1 or newer; found Linux") && t.contains("Update the kernel"), "{t}");
    assert!(!t.contains("display server"), "an install does not look at the display: {t}");
    assert!(t.contains("--force-host"), "{t}");
    assert!(!l.appdir().join("trust").exists() && !l.appdir().join("current").exists(), "nothing was installed");
    // `doctor` on the package says the same.
    let d = text(&cli(&l.home, &["doctor", pkg.to_str().unwrap()]));
    assert!(d.contains("HostLab (org.example.hostlab): needs Linux 99.1 or newer; found Linux"), "{d}");
    assert!(d.contains("NOTE the current session does not give what HostLab needs: needs the display server wayland; found no display server in this session"), "{d}");
    // With --force-host it installs, says so, and the state keeps the choice; `doctor` keeps saying it.
    let o = l.install(&pkg, &["--force-host"], &[]);
    let t = text(&o);
    assert!(o.status.success() && t.contains("installed anyway because of --force-host"), "{t}");
    assert!(fs::read_to_string(l.appdir().join("trust")).unwrap().contains("forced_host=1"));
    let d = text(&cli(&l.home, &["doctor"]));
    assert!(d.contains("HostLab (org.example.hostlab): needs Linux 99.1") && d.contains("(installed with --force-host)"), "{d}");
    // A later version installs over it without the flag (the choice is kept) and a version that needs nothing clears it.
    let v2 = l.pack_ok("2", "[host]\nkernel = \"99.1\"\n", Some(&pkg));
    assert!(l.install(&v2, &[], &[]).status.success());
    let v3 = l.pack_ok("3", "", Some(&v2));
    assert!(l.install(&v3, &[], &[]).status.success());
    assert!(!fs::read_to_string(l.appdir().join("trust")).unwrap().contains("forced_host"), "a package that the system can run clears the choice");
    // And without the flag an app that was never forced is refused again.
    let v4 = l.pack_ok("4", "[host]\nkernel = \"99.1\"\n", Some(&v3));
    assert!(!l.install(&v4, &[], &[]).status.success());
}

#[test]
fn an_install_without_a_display_works_and_the_start_without_one_refuses_with_the_message() {
    let l = lab("display");
    let pkg = l.pack_ok("1", "[host]\ndisplay = [\"x11\"]\nportals = [\"document\"]\n", None);
    // No display and no portal in this session (ssh, a script, the build of an image): installed.
    let o = l.install(&pkg, &[], &[]);
    assert!(o.status.success(), "{}", text(&o));
    // `doctor` says that the session does not give it, as a note and not as a failure.
    let d = text(&cli(&l.home, &["doctor"]));
    assert!(d.contains("NOTE the current session does not give what HostLab needs: needs the display server x11; found no display server in this session"), "{d}");
    assert!(!d.contains("KO   HostLab"), "{d}");
    // The start without a display is refused, with what is needed, what is there and what to do, and nothing about forcing it.
    let launcher = l.home.join(".local/bin").join(ID);
    let start = |display: Option<&str>| {
        let mut c = Command::new(&launcher);
        c.current_dir(&l.home)
            .env("HOME", &l.home)
            .env("XDG_DATA_HOME", l.home.join(".local/share"))
            .env("XDG_RUNTIME_DIR", l.home.join("run"))
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/panpak-test-no-bus")
            .env("PANPAK_NO_BACKGROUND", "1")
            .env("LC_ALL", "C")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY");
        if let Some(d) = display {
            c.env("DISPLAY", d);
        }
        c.output().unwrap()
    };
    let o = start(None);
    let t = text(&o);
    assert!(!o.status.success() && t.contains("HostLab 1 cannot run in this session:") && t.contains("needs the display server x11; found no display server in this session. Start it from a graphical session"), "{t}");
    assert!(t.contains("needs the document portal; found no such portal in this session"), "{t}");
    assert!(!t.contains("--force-host"), "{t}");
    // The same texts in Italian, from the catalog.
    let it = Command::new(&launcher).current_dir(&l.home).env("HOME", &l.home).env("XDG_DATA_HOME", l.home.join(".local/share")).env("XDG_RUNTIME_DIR", l.home.join("run")).env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/panpak-test-no-bus").env("PANPAK_NO_BACKGROUND", "1").env("LC_ALL", "it_IT.UTF-8").env("LANGUAGE", "it").env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY").output().unwrap();
    let t = text(&it);
    assert!(t.contains("non può funzionare in questa sessione") && t.contains("serve il server grafico x11; trovato nessun server grafico in questa sessione"), "{t}");
    // A session with the display and the portal starts the app far enough to pass the check (the document folder of the portal is a plain folder in this test).
    fs::create_dir_all(l.home.join("run/doc/by-app")).unwrap();
    let o = start(Some(":99"));
    assert!(!text(&o).contains("cannot run in this session"), "{}", text(&o));
}

#[test]
fn an_app_that_asks_for_the_private_bus_starts_without_a_session_bus_and_says_what_is_missing() {
    let l = lab("bus");
    let pkg = l.pack_ok("1", "[host]\nbus = \"private\"\n", None);
    assert!(l.install(&pkg, &[], &[]).status.success());
    // `doctor` says what is missing, as a note.
    let d = text(&cli(&l.home, &["doctor"]));
    assert!(d.contains("NOTE HostLab starts without a connection to your session bus, so what goes through it will not work (portals, notifications): needs a session bus and xdg-dbus-proxy"), "{d}");
    assert!(!d.contains("KO   HostLab"), "{d}");
    // The start is not refused, and says the same.
    let o = Command::new(l.home.join(".local/bin").join(ID))
        .current_dir(&l.home)
        .env("HOME", &l.home)
        .env("XDG_DATA_HOME", l.home.join(".local/share"))
        .env("XDG_RUNTIME_DIR", l.home.join("run"))
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/panpak-test-no-bus")
        .env("PANPAK_NO_BACKGROUND", "1")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    let t = text(&o);
    assert!(o.status.success() && t.contains("HostLab starts without a connection to your session bus") && !t.contains("cannot run"), "{t}");
    // The same note in Italian.
    let it = Command::new(l.home.join(".local/bin").join(ID))
        .current_dir(&l.home)
        .env("HOME", &l.home)
        .env("XDG_DATA_HOME", l.home.join(".local/share"))
        .env("XDG_RUNTIME_DIR", l.home.join("run"))
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/panpak-test-no-bus")
        .env("PANPAK_NO_BACKGROUND", "1")
        .env("LC_ALL", "it_IT.UTF-8")
        .env("LANGUAGE", "it")
        .output()
        .unwrap();
    assert!(text(&it).contains("parte senza collegamento al bus della tua sessione"), "{}", text(&it));
}

#[test]
fn resign_makes_the_same_files_a_release_of_another_identity_with_the_real_signing_function() {
    let l = lab("resign");
    let pkg = l.pack_ok("1", "[host]\nkernel = \"5.10\"\n", None);
    let key2 = l.home.join("second.key");
    assert!(cli(&l.home, &["keygen", "-o", key2.to_str().unwrap()]).status.success());
    let out = l.home.join("again.panpak");
    let o = cli(&l.home, &["resign", pkg.to_str().unwrap(), "--secret-file", key2.to_str().unwrap(), "--id", "org.example.resigned", "--version", "2.0", "--revision", "7", "--update-url", "http://127.0.0.1:1/x.panpak", "-o", out.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    // It is a package that every check accepts, with the new identity, the new key, and the table of the old one.
    let d = text(&cli(&l.home, &["doctor", out.to_str().unwrap()]));
    assert!(d.contains("verified package: org.example.resigned 2.0"), "{d}");
    let m = l.manifest(&out);
    assert!(m.contains("id = \"org.example.resigned\"") && m.contains("version = \"2.0\"") && m.contains("revision = 7") && m.contains("url = \"http://127.0.0.1:1/x.panpak\""), "{m}");
    assert!(m.contains("kernel = \"5.10\"") && m.contains("\"host.kernel\""), "the critical list is made again: {m}");
    // The root is the key that signed (not the one of the first package), and the original files are the same.
    let first = l.manifest(&pkg);
    let root = |m: &str| m.lines().find(|x| x.starts_with("root = ")).map(str::to_string);
    assert_ne!(root(&m), root(&first));
    let (ia, ib) = (l.home.join("a.index"), l.home.join("b.index"));
    assert!(cli(&l.home, &["dump-index", pkg.to_str().unwrap(), ia.to_str().unwrap()]).status.success());
    assert!(cli(&l.home, &["dump-index", out.to_str().unwrap(), ib.to_str().unwrap()]).status.success());
    assert_eq!(fs::read(ia).unwrap(), fs::read(ib).unwrap());
    // It installs as the new app.
    let o = cli(&l.home, &["install", "--yes", "--trust-first-key", out.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    // Without a key, or with an unreadable one, nothing is written.
    let nope = l.home.join("nope.panpak");
    let o = cli(&l.home, &["resign", pkg.to_str().unwrap(), "--secret-file", l.home.join("absent.key").to_str().unwrap(), "-o", nope.to_str().unwrap()]);
    assert!(!o.status.success() && !nope.exists());
}
