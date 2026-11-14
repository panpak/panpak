// SPDX-License-Identifier: MIT OR Apache-2.0
// The host side of the file dialog against a fake desktop portal on a private bus: what is asked of the portal, what is shared with which permissions,
// what a link, a cancel and a revoke do. Skipped (with a message) where `dbus-daemon` or python3-gi is missing.
use panpak_core::chooserhost::{run, HostCtx, Outcome};
use panpak_core::dbus::{encode_message, Connection, Message, Value, CALL};
use panpak_core::{docshare, filechooser};
use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

struct Lab {
    dir: PathBuf,
    bus: Child,
    fake: Child,
    log: PathBuf,
    answers: PathBuf,
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = self.fake.kill();
        let _ = self.bus.kill();
        let _ = self.fake.wait();
        let _ = self.bus.wait();
        let _ = Command::new("chmod").args(["-R", "u+rwX"]).arg(&self.dir).output();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn have(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn start() -> Option<Lab> {
    if !have("dbus-daemon", &["--version"]) || !have("python3", &["-c", "import gi; gi.require_version('Gio', '2.0')"]) {
        eprintln!("skipped: the lab needs dbus-daemon and python3-gi");
        return None;
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("chooserhost-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let mut bus = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut addr = String::new();
    BufReader::new(bus.stdout.as_mut().unwrap()).read_line(&mut addr).unwrap();
    let addr = addr.trim().to_string();
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &addr);
    let (log, answers) = (dir.join("log"), dir.join("answers.json"));
    fs::write(&answers, "[]").unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fake_portal_files.py");
    let fake = Command::new("python3").arg(script).arg(&log).arg(&answers).env("DBUS_SESSION_BUS_ADDRESS", &addr).spawn().unwrap();
    let ready = log.with_extension("ready");
    let t0 = Instant::now();
    while !PathBuf::from(format!("{}.ready", log.display())).exists() {
        assert!(t0.elapsed() < Duration::from_secs(20), "the fake portal did not start {ready:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    Some(Lab { dir, bus, fake, log, answers })
}

impl Lab {
    fn answers(&self, json: &str) {
        fs::write(&self.answers, json).unwrap();
    }
    /// The log lines since the last call, parsed.
    fn take_log(&self) -> Vec<String> {
        let text = fs::read_to_string(&self.log).unwrap_or_default();
        let _ = fs::write(&self.log, "");
        text.lines().map(str::to_string).collect()
    }
}

/// The log lines of one call.
fn calls(log: &[String], name: &str) -> Vec<String> {
    log.iter().filter(|l| l.contains(&format!(r#""call": "{name}""#))).cloned().collect()
}

fn entry(k: &str, v: Value) -> Value {
    Value::Entry(Box::new(Value::str(k)), Box::new(Value::variant(v)))
}

fn call(method: &str, opts: Vec<Value>) -> filechooser::Call {
    let m = Message {
        kind: CALL,
        serial: 1,
        path: Some("/org/freedesktop/portal/desktop".into()),
        interface: Some(filechooser::IFACE.into()),
        member: Some(method.into()),
        destination: Some("org.freedesktop.portal.Desktop".into()),
        signature: "ssa{sv}".into(),
        body: vec![Value::str("x11:1f"), Value::str("Pick"), Value::Array("{sv}".into(), opts)],
        ..Default::default()
    };
    filechooser::parse_call(&encode_message(&m).unwrap(), "/run/user/1000/doc/").unwrap()
}

fn ctx() -> HostCtx {
    HostCtx { app_id: "panpak.org.example.App".into(), runtime_dir: "/run/user/1000".into(), writable_roots: Arc::new(OnceLock::new()) }
}

fn uris(o: &Outcome) -> Vec<String> {
    let Outcome::Done(Value::Array(_, e)) = o else { panic!("not done: {o:?}") };
    for i in e {
        if let Value::Entry(k, v) = i {
            if matches!(&**k, Value::Str(s) if s == "uris") {
                if let Value::Variant(inner) = &**v {
                    if let Value::Array(_, items) = &**inner {
                        return items.iter().map(|x| if let Value::Str(s) = x { s.clone() } else { String::new() }).collect();
                    }
                }
            }
        }
    }
    panic!("no uris")
}

fn uri_of(p: &Path) -> String {
    format!("file://{}", filechooser::percent_encode(p.to_string_lossy().as_bytes()))
}

#[test]
fn the_dialog_shares_what_the_user_chose_and_only_that() {
    let Some(lab) = start() else { return };
    let d = lab.dir.join("files");
    fs::create_dir(&d).unwrap();
    let (rw, ro, other) = (d.join("a b.txt"), d.join("ro.txt"), d.join("other.txt"));
    for f in [&rw, &ro, &other] {
        fs::write(f, "x").unwrap();
    }
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o444)).unwrap();

    // 1. One file the user can write: read and write, reuse + persistent, the id of the app as given, and only the known options reach the portal.
    lab.answers(&format!(r#"[{{"code":0,"uris":["{}"],"extra":true}}]"#, uri_of(&rw)));
    let c = call("OpenFile", vec![entry("modal", Value::Bool(true)), entry("accept_label", Value::str("Go")), entry("evil", Value::str("x"))]);
    let out = run(&ctx(), &c);
    assert_eq!(uris(&out), ["file:///run/user/1000/doc/d001/a%20b.txt"]);
    let Outcome::Done(Value::Array(_, entries)) = &out else { unreachable!() };
    let mut keys: Vec<String> = entries.iter().filter_map(|e| if let Value::Entry(k, _) = e { if let Value::Str(s) = &**k { Some(s.clone()) } else { None } } else { None }).collect();
    keys.sort();
    assert_eq!(keys, ["choices", "current_filter", "uris"], "the answer carries the choices and the filter, and not the key the app has no business with");
    let log = lab.take_log();
    assert!(log[0].contains(r#""call": "OpenFile""#) && log[0].contains(r#""parent": "x11:1f""#), "{log:?}");
    assert!(log[0].contains(r#""keys": ["accept_label", "handle_token", "modal"]"#), "only the known options reach the portal: {log:?}");
    assert!(log[1].contains(r#""call": "AddFull""#) && log[1].contains(r#""flags": 3"#) && log[1].contains(r#""app": "panpak.org.example.App""#), "{log:?}");
    assert!(log[1].contains(r#""perms": ["read", "write"]"#), "{log:?}");
    assert!(log[1].contains(&format!(r#""paths": ["{}"]"#, rw.display())), "{log:?}");

    // 2. A file the user cannot write: read only. The app lowering `writable` does the same for a file that could be written.
    lab.answers(&format!(r#"[{{"code":0,"uris":["{}"]}},{{"code":0,"uris":["{}"]}}]"#, uri_of(&ro), uri_of(&other)));
    run(&ctx(), &call("OpenFile", vec![]));
    run(&ctx(), &call("OpenFile", vec![entry("writable", Value::Bool(false))]));
    let adds = calls(&lab.take_log(), "AddFull");
    assert!(adds[0].contains(r#""perms": ["read"]"#) && adds[1].contains(r#""perms": ["read"]"#), "{adds:?}");

    // 3. Several files, some writable and some not: one call for each kind of grant, and the answer keeps the order.
    lab.answers(&format!(r#"[{{"code":0,"uris":["{}","{}","{}"]}}]"#, uri_of(&ro), uri_of(&rw), uri_of(&other)));
    let out = run(&ctx(), &call("OpenFile", vec![entry("multiple", Value::Bool(true))]));
    let u = uris(&out);
    assert!(u[0].ends_with("/ro.txt") && u[1].ends_with("/a%20b.txt") && u[2].ends_with("/other.txt"), "{u:?}");
    assert_eq!(calls(&lab.take_log(), "AddFull").len(), 2);

    // 4. A folder: the export-directory flag.
    lab.answers(&format!(r#"[{{"code":0,"uris":["{}"]}}]"#, uri_of(&d)));
    let out = run(&ctx(), &call("OpenFile", vec![entry("directory", Value::Bool(true))]));
    assert!(uris(&out)[0].ends_with("/files"));
    assert!(calls(&lab.take_log(), "AddFull")[0].contains(r#""flags": 11"#));

    // 5. Save: a name that does not exist yet is shared as a name in its folder; the file is not made.
    let fresh = d.join("new.txt");
    lab.answers(&format!(r#"[{{"code":0,"uris":["{}"]}}]"#, uri_of(&fresh)));
    let out = run(&ctx(), &call("SaveFile", vec![entry("current_name", Value::str("new.txt"))]));
    assert!(uris(&out)[0].ends_with("/new.txt"));
    assert!(!fresh.exists(), "the file appears when the app writes, not before");
    let adds = calls(&lab.take_log(), "AddNamedFull");
    assert!(adds[0].contains(r#""perms": ["read", "write"]"#) && adds[0].contains(&format!(r#""paths": ["{}"]"#, fresh.display())), "{adds:?}");

    // 6. The user closes the dialog: nothing is shared.
    lab.answers(r#"[{"code":1,"uris":[]}]"#);
    assert!(matches!(run(&ctx(), &call("OpenFile", vec![])), Outcome::Ended { code: 1, .. }));
    assert!(calls(&lab.take_log(), "AddFull").is_empty());

    // 7. What the app can already reach (under a folder it can write) is given as it is: the real address, no document, nothing opened by the host. A link there is
    // the app's business: the kernel judges what it opens (Landlock), and there is nothing for the host to be fooled about.
    let link = d.join("notes.txt");
    symlink(&rw, &link).unwrap();
    let rooted = ctx();
    rooted.writable_roots.set(vec![d.clone()]).unwrap();
    let fresh_new = d.join("made-by-the-app.txt");
    lab.answers(&format!(r#"[{{"code":0,"uris":["{}","{}"]}},{{"code":0,"uris":["{}"]}}]"#, uri_of(&rw), uri_of(&link), uri_of(&fresh_new)));
    let Outcome::Done(dict) = run(&rooted, &call("OpenFile", vec![])) else { panic!("not done") };
    let canon = |p: &Path| std::fs::canonicalize(p.parent().unwrap()).unwrap().join(p.file_name().unwrap());
    assert!(format!("{dict:?}").contains(&docshare::real_uri(&canon(&rw))) && format!("{dict:?}").contains(&docshare::real_uri(&canon(&link))), "{dict:?}");
    let Outcome::Done(dict) = run(&rooted, &call("SaveFile", vec![])) else { panic!("not done") };
    assert!(format!("{dict:?}").contains(&docshare::real_uri(&canon(&fresh_new))), "{dict:?}");
    assert!(!fresh_new.exists(), "the host creates nothing");
    let log = lab.take_log();
    assert!(calls(&log, "AddFull").is_empty() && calls(&log, "AddNamedFull").is_empty(), "no document for what the app can reach: {log:?}");
    // A link the user chose elsewhere (a folder the app cannot write) is followed (7b); a Save under the name of a link there is refused.
    let says = format!("the file you chose is a link to {}: choose the original file", rw.display());
    // 7b. The same link in a folder the app cannot write: the user chose it, the file it points to is shared (under its own name). A save under the name
    // of a link is still refused.
    lab.answers(&format!(r#"[{{"code":0,"uris":["{}"]}},{{"code":0,"uris":["{}"]}}]"#, uri_of(&link), uri_of(&link)));
    let Outcome::Done(dict) = run(&ctx(), &call("OpenFile", vec![])) else { panic!("the link was not followed") };
    let text = format!("{dict:?}");
    let own_name = rw.file_name().unwrap().to_string_lossy().replace(' ', "%20");
    assert!(text.contains(&format!("/{own_name}\"")) && !text.contains("notes.txt"), "{text}");
    assert_eq!(calls(&lab.take_log(), "AddFull").len(), 1);
    let Outcome::Ended { user, .. } = run(&ctx(), &call("SaveFile", vec![])) else { panic!("not ended") };
    assert_eq!(user.as_deref(), Some(says.as_str()));
    assert!(calls(&lab.take_log(), "AddNamedFull").is_empty());

    // 8. An address that is not a file, and a file that is gone: ended, with a reason in the log line.
    lab.answers(r#"[{"code":0,"uris":["https://example.org/x"]},{"code":0,"uris":["file:///nonexistent/zzz"]}]"#);
    for _ in 0..2 {
        assert!(matches!(run(&ctx(), &call("OpenFile", vec![])), Outcome::Ended { code: 2, .. }));
    }
    lab.take_log();

    // 8b. Files given at the start: shared before the app runs, and the document paths take their place. Words, options and addresses stay; a link is dropped.
    lab.take_log();
    let args: Vec<String> =
        ["--new-window", "a b.txt", "./new2.txt", &uri_of(&ro), "word", "https://example.org/x.txt", "notes.txt"].iter().map(|s| s.to_string()).collect();
    let shared = panpak_core::startfiles::share(&args, &d, "panpak.org.example.App", "/run/user/1000", &[], || Connection::session(Duration::from_secs(10)));
    assert_eq!(shared.args.len(), 7, "{:?}", shared.args);
    assert_eq!((shared.args[0].as_str(), shared.args[4].as_str(), shared.args[5].as_str()), ("--new-window", "word", "https://example.org/x.txt"));
    assert!(shared.args[6].starts_with("/run/user/1000/doc/") && shared.args[6].ends_with("/a b.txt"), "a link the user could have put is followed: {:?}", shared.args);
    assert!(shared.args[1].starts_with("/run/user/1000/doc/") && shared.args[1].ends_with("/a b.txt"), "a path stays a path: {:?}", shared.args);
    assert!(shared.args[2].starts_with("/run/user/1000/doc/") && shared.args[2].ends_with("/new2.txt"), "{:?}", shared.args);
    assert!(shared.args[3].starts_with("file:///run/user/1000/doc/") && shared.args[3].ends_with("/ro.txt"), "an address stays an address: {:?}", shared.args);
    assert!(shared.args.iter().all(|a| !a.contains(d.to_string_lossy().as_ref())), "no path of the host reaches the app: {:?}", shared.args);
    assert!(shared.dropped.is_empty(), "{:?}", shared.dropped);
    assert!(!d.join("new2.txt").exists(), "the new file appears when the app writes");
    let log = lab.take_log();
    assert_eq!(calls(&log, "AddFull").len(), 2, "one call for the writable file and one for the read-only one: {log:?}");
    assert_eq!(calls(&log, "AddNamedFull").len(), 1);

    // 9. Taking the grants back: `remove` revokes every document of the app, and with the purge deletes those that nobody else holds.
    let mut conn = Connection::session(Duration::from_secs(10)).unwrap();
    let n = docshare::revoke_app(&mut conn, "panpak.org.example.App", false).unwrap();
    assert!(n >= 3, "{n}");
    let log = lab.take_log();
    assert_eq!(calls(&log, "RevokePermissions").len(), n);
    assert!(calls(&log, "Delete").is_empty());
    assert_eq!(docshare::revoke_app(&mut conn, "panpak.org.example.App", true).unwrap(), 0, "nothing is left to revoke");

    // 10. The portal is not there: the dialog ends with a reason, it does not hang or panic.
    drop(conn);
    let mut lab = lab;
    let _ = lab.fake.kill();
    let _ = lab.fake.wait();
    let t0 = Instant::now();
    assert!(matches!(run(&ctx(), &call("OpenFile", vec![])), Outcome::Ended { code: 2, .. }));
    assert!(t0.elapsed() < Duration::from_secs(5));
}
