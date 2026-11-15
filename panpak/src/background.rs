// SPDX-License-Identifier: MIT OR Apache-2.0
//! The background job of one app (`panpak background <app id>`): PANPAK-FORMAT.md section 6.7.
//!
//! The stub starts it, detached and at the lowest priority, at the start of an app whose `update-state` says it is due. It does two things and neither
//! changes what the app runs: it stages a new version when the update URL has one (6.6: the version is applied at a start when no copy runs), and it reads
//! a part of the installed files, by the budget of the day, to find damage that a disk or a memory left without anyone noticing.
use super::*;
use panpak_core::bgstate::{self, BgState};

/// The most time one run spends reading files.
const VERIFY_SECONDS: u64 = 20;

/// Put the job behind everything the user is doing: the lowest CPU and I/O priority.
fn lower_priority() {
    // SAFETY: plain system calls with integer arguments; a failure only means the job keeps its priority.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
        // ioprio_set(IOPRIO_WHO_PROCESS, this process, class IDLE).
        libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
    }
}

/// What the read of a part of the files found.
pub struct Slice {
    pub next_cursor: u64,
    pub bytes: u64,
    pub bad: Vec<PathBuf>,
    /// The end of the list was passed: a whole round is done.
    pub wrapped: bool,
}

/// Check the files of `entries` from `cursor` on against the signed index, until `budget` bytes have been read or `until` has passed.
/// At least one entry is checked per call, so that a budget smaller than a file still goes forward.
pub fn verify_slice(tree: &Path, chunks: &[panpak_core::Chunk], entries: &[panpak_core::Entry], cursor: u64, budget: u64, until: std::time::Instant) -> Slice {
    let start = if (cursor as usize) < entries.len() { cursor as usize } else { 0 };
    let (mut i, mut bytes, mut bad) = (start, 0u64, Vec::new());
    while i < entries.len() {
        let e = &entries[i];
        if let panpak_core::Kind::File { chunks: ids } = &e.kind {
            bytes += ids.iter().map(|&c| chunks[c as usize].raw as u64).sum::<u64>();
        }
        bad.extend(verify_tree(tree, chunks, std::slice::from_ref(e)).0);
        i += 1;
        if i > start && (bytes >= budget || std::time::Instant::now() >= until) {
            break;
        }
    }
    let wrapped = i >= entries.len();
    Slice { next_cursor: if wrapped { 0 } else { i as u64 }, bytes, bad, wrapped }
}

/// The arguments of `org.freedesktop.portal.Notification.AddNotification (s id, a{sv} notification)`: the id that replaces an older notice of the same app, and the
/// title and the body of the notice.
pub fn notification_args(app_id: &str, name: &str, version: &str) -> Vec<panpak_core::dbus::Value> {
    use panpak_core::dbus::Value;
    let clean = |s: &str| s.chars().filter(|c| !c.is_control()).collect::<String>();
    let title = format!("{}: a security update is ready", clean(name));
    let body = format!("Version {} is ready and is applied the next time {} is started. Close it and open it again to be protected now.", clean(version), clean(name));
    let entry = |k: &str, v: String| Value::Entry(Box::new(Value::str(k)), Box::new(Value::variant(Value::Str(v))));
    vec![Value::Str(format!("panpak-update-{app_id}")), Value::Array("{sv}".into(), vec![entry("title", title), entry("body", body)])]
}

/// Show the notice through the notification portal, with the D-Bus client of panpak. Nothing is shown (and nothing fails) when there is no session bus or no portal.
fn notify(app_id: &str, name: &str, version: &str) {
    let Ok(mut conn) = panpak_core::dbus::Connection::session(std::time::Duration::from_secs(3)) else { return };
    let _ = conn.call(
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Notification",
        "AddNotification",
        notification_args(app_id, name, version),
        vec![],
    );
}

fn copy_runs(app_id: &str) -> bool {
    remove::lock_no_copies(app_id, "the check").is_err()
}

/// `panpak background [--force] [--after-start] [--allow-insecure-http] <app id>`.
pub fn cmd_background(app_id: &str, force: bool, after_start: bool, at_exit: bool, allow_insecure: bool) -> Result<()> {
    panpak_core::validate_app_id(app_id)?;
    let store = store_dir()?;
    let appdir = store.join("apps").join(app_id);
    if fs::symlink_metadata(&appdir).is_err() {
        bail_code!(ErrorCode::E2004, "{app_id} is not installed");
    }
    lower_priority();
    if at_exit {
        // The last copy has just ended: what it left undone, in this order. The staged update is applied (nothing is asked: a version with new permissions waits for the user), then
        // the files found damaged are repaired (this may use the network), then the rest of the job as usual.
        if let Err(e) = cmd_apply(app_id, false, false, true) {
            eprintln!("panpak: {app_id}: the staged update was not applied: {}", panpak_core::err_text(&e));
        }
        cmd_repair_if_damaged(app_id, allow_insecure)?;
        gc::after(&store, &[]);
    }
    if after_start {
        // The start of the app comes first: nothing of this competes with it. (The app may be gone when the wait ends: then there is nothing to do.)
        std::thread::sleep(std::time::Duration::from_secs(20));
        if fs::symlink_metadata(&appdir).is_err() {
            return Ok(());
        }
    }
    // One job per app: a second one that finds the first at work has nothing to do.
    let Some(_job) = job_lock(&store, app_id)? else { return Ok(()) };
    let now = bgstate::now();
    if !force && !bgstate::due(&appdir, now) {
        return Ok(());
    }
    let config = panpak_core::config::load();
    let mut st = bgstate::read(&appdir);

    // 1. The update: staged, never applied here.
    let has_url = fs::read_to_string(appdir.join("update.url")).is_ok_and(|u| !u.trim().is_empty());
    if config.updates && has_url {
        match cmd_update(app_id, false, false, false, allow_insecure, 30, true) {
            Ok(_) => {
                st.failures = 0;
                st.last_check = now;
                st.next_check = now + bgstate::DAY;
            }
            Err(e) => {
                st.failures += 1;
                st.next_check = now + bgstate::backoff(st.failures);
                eprintln!("panpak: the check for an update of {app_id} failed: {}", panpak_core::err_text(&e));
            }
        }
    } else {
        st.next_check = now + bgstate::DAY;
    }

    // 2. A security update that waits because the app runs: one notice per version.
    if let (Some(staged), Some(state)) = (panpak_core::pending::staged_record(&appdir), panpak_core::pending::read_state(&appdir)) {
        if state.security && !state.needs_accept {
            if let Ok(o) = panpak_core::record::open_verified(&staged) {
                let root12: String = panpak_core::hex(&o.footer.root).chars().take(12).collect();
                if st.notified != root12 && copy_runs(app_id) {
                    notify(app_id, &o.manifest.app.name, &o.manifest.app.version);
                    st.notified = root12;
                }
            }
        }
    }

    // 3. The rotating verification of the installed files.
    if config.verify_mb > 0 {
        verify_step(app_id, &store, &appdir, &config, &mut st, now, allow_insecure);
    }
    bgstate::write(&appdir, &st)
}

/// The lock of the job of one app: `<store>/locks/<id>.bg`, taken without waiting. `None` when another job holds it.
fn job_lock(store: &Path, app_id: &str) -> Result<Option<File>> {
    use std::os::unix::io::AsRawFd;
    let dir = store.join("locks");
    fs::create_dir_all(&dir)?;
    let f = fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{app_id}.bg")))?;
    // SAFETY: flock on a descriptor that `f` owns.
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    Ok((r == 0).then_some(f))
}

fn verify_step(app_id: &str, store: &Path, appdir: &Path, config: &panpak_core::config::Config, st: &mut BgState, now: u64, allow_insecure: bool) {
    let today = now / bgstate::DAY;
    if st.verify_day != today {
        st.verify_day = today;
        st.verified_bytes = 0;
    }
    let budget = (config.verify_mb << 20).saturating_sub(st.verified_bytes);
    if budget == 0 {
        return;
    }
    // The app lock keeps a removal or an update from moving the files while they are read.
    let Ok(_app) = panpak_core::applock::lock_for(store, app_id, std::time::Duration::from_secs(2)) else { return };
    let Ok(rec_path) = panpak_core::durable::resolve_link(&appdir.join("current")) else { return };
    let Ok(rec) = panpak_core::record::open_verified(&rec_path) else { return };
    let tree = store.join("trees").join(panpak_core::hex(&rec.footer.root));
    let Ok((chunks, entries)) = panpak_core::parse_index(&rec.raw_index) else { return };
    if !tree.join(".complete").exists() {
        return;
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(VERIFY_SECONDS);
    let slice = verify_slice(&tree, &chunks, &entries, st.cursor, budget, until);
    st.cursor = slice.next_cursor;
    st.verified_bytes += slice.bytes;
    if slice.wrapped {
        st.last_round = now;
    }
    if slice.bad.is_empty() {
        return;
    }
    st.damaged = st.damaged.saturating_add(slice.bad.len() as u32);
    st.damaged_first = slice.bad[0].display().to_string();
    eprintln!("panpak: {app_id}: {} file(s) do not match the signed index, for example {}", slice.bad.len(), st.damaged_first);
    drop(_app);
    // Made right now when no copy of the app runs; otherwise at a start with none (the stub asks for it), or by `panpak repair`.
    if try_repair(app_id, allow_insecure) {
        st.damaged = 0;
        st.damaged_first.clear();
    }
}

/// Repair the installed version if no copy runs. True when it was repaired.
fn try_repair(app_id: &str, allow_insecure: bool) -> bool {
    match remove::lock_no_copies(app_id, "the repair") {
        Ok(l) => drop(l),
        Err(_) => return false,
    }
    match cmd_repair(app_id, None, allow_insecure, 30) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("panpak: {app_id} could not be repaired now: {}", panpak_core::err_text(&e));
            false
        }
    }
}

/// The repair of files that the job found damaged, when no copy of the app runs (nothing is asked). The stub does not run it at a start: the start waits for nothing.
pub fn cmd_repair_if_damaged(app_id: &str, allow_insecure: bool) -> Result<()> {
    panpak_core::validate_app_id(app_id)?;
    let appdir = store_dir()?.join("apps").join(app_id);
    let mut st = bgstate::read(&appdir);
    if st.damaged == 0 {
        return Ok(());
    }
    if try_repair(app_id, allow_insecure) {
        st.damaged = 0;
        st.damaged_first.clear();
        bgstate::write(&appdir, &st)?;
        eprintln!("panpak: {app_id}: the files that did not match were made right");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_has_the_shape_the_portal_wants() {
        use panpak_core::dbus::Value;
        let a = notification_args("org.example.app", "A\nname", "1.2");
        let sig: String = a.iter().map(Value::signature).collect();
        assert_eq!(sig, "sa{sv}");
        assert_eq!(a[0], Value::str("panpak-update-org.example.app"));
        let Value::Array(_, entries) = &a[1] else { panic!() };
        let text = format!("{entries:?}");
        assert!(text.contains("Aname: a security update is ready") && text.contains("Version 1.2 is ready") && !text.contains('\n'), "{text}");
    }
}
