// SPDX-License-Identifier: MIT OR Apache-2.0
//! The sequence of releases of an app (PANPAK-FORMAT.md section 3.1.10): what a client checks when a package enters (install, update,
//! apply, rollback) against the one that is installed, and the history file that makes a fork visible.
//!
//! A release has a `revision`, a number that grows. It is the only thing compared: the version text is for people, a date depends on the
//! clock, and a parsed version has no order across apps. The `floor` says "nothing below this revision enters": it only grows (the
//! client keeps the highest one it has seen for the app), and it stops an install, an update and a rollback to a lower revision. The
//! `.panpak/history` file lists the earlier releases of the channel (revision and hash of the signed message), so that an update can
//! tell a version that continues the installed one from one that does not (a fork).
use crate::{coded, ErrorCode, Release};
use anyhow::{bail, Result};

/// The place of the history in the tree of a package.
pub const HISTORY_PATH: &str = ".panpak/history";
const HISTORY_MAGIC: &str = "panpak-history 1";
/// At most this many earlier releases are listed (one line of 75 bytes each).
pub const MAX_HISTORY: usize = 4096;
/// The history file is read only up to this size.
pub const MAX_HISTORY_FILE: usize = 1 << 20;

/// One earlier release: its revision and the BLAKE3 hash of its signed message (`package_message`).
pub type Entry = (u64, [u8; 32]);

/// The hash that names a release in a history: BLAKE3 of the message its signature covers.
pub fn signed_hash(version: u32, flags: u32, root: &[u8; 32], manifest: &[u8]) -> [u8; 32] {
    *blake3::hash(&crate::package_message(version, flags, root, manifest)).as_bytes()
}

/// The text of the history file: the magic line, then `<revision> <hash>` per line, in rising revisions.
pub fn encode_history(entries: &[Entry]) -> String {
    let mut s = format!("{HISTORY_MAGIC}\n");
    for (rev, hash) in entries {
        s.push_str(&format!("{rev} {}\n", crate::hex(hash)));
    }
    s
}

/// The BLAKE3 hash of a history file, as `[release].history` names it.
pub fn history_hash(text: &[u8]) -> String {
    crate::hex(blake3::hash(text).as_bytes())
}

/// Read a history file. Strict: the magic line, rising revisions from 1, lowercase hex hashes, no blank or extra line.
pub fn parse_history(bytes: &[u8]) -> Result<Vec<Entry>> {
    let bad = |m: &str| coded(ErrorCode::E4006, format!("the history of the release is malformed ({m}): the package is damaged or was made by a faulty tool"));
    if bytes.len() > MAX_HISTORY_FILE {
        return Err(bad("too large"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| bad("not text"))?;
    let mut lines = text.split('\n');
    if lines.next() != Some(HISTORY_MAGIC) {
        return Err(bad("no magic line"));
    }
    let mut out: Vec<Entry> = Vec::new();
    let rest: Vec<&str> = lines.collect();
    if rest.last() != Some(&"") {
        return Err(bad("the last line does not end"));
    }
    for line in &rest[..rest.len() - 1] {
        let (rev_text, hash) = line.split_once(' ').ok_or_else(|| bad("a line has no hash"))?;
        let rev: u64 = rev_text.parse().map_err(|_| bad("a revision is not a number"))?;
        if rev == 0 || rev.to_string() != rev_text || out.last().is_some_and(|(last, _)| rev <= *last) {
            return Err(bad("revisions are not rising from 1"));
        }
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(bad("a hash is not 64 lowercase hex characters"));
        }
        let mut h = [0u8; 32];
        for (i, d) in h.iter_mut().enumerate() {
            *d = u8::from_str_radix(&hash[2 * i..2 * i + 2], 16).map_err(|_| bad("a hash is not hex"))?;
        }
        out.push((rev, h));
        if out.len() > MAX_HISTORY {
            return Err(bad("too many entries"));
        }
    }
    Ok(out)
}

/// What a client knows of the installed release and of the highest floor it has seen for the app.
#[derive(Debug, Clone)]
pub struct Installed {
    pub revision: u64,
    pub channel: String,
    /// The hash of the signed message of the installed record.
    pub signed: [u8; 32],
    /// The history file of the installed tree, if it has one.
    pub history: Option<Vec<Entry>>,
    /// The highest floor seen for the app (at least the floor of the installed release).
    pub floor: u64,
}

/// The options that let a person do on purpose what the checks refuse.
#[derive(Debug, Clone, Copy, Default)]
pub struct Allow {
    pub downgrade: bool,
    pub switch_channel: bool,
}

/// What the revision of a candidate says about it, compared with the installed release.
#[derive(Debug, PartialEq, Eq)]
pub enum Standing {
    /// A later revision: it can enter.
    Newer,
    /// An earlier revision that a person allowed (`--allow-downgrade`).
    Older,
    /// The very release that is installed (the same signed message): nothing to do.
    Same,
    /// A package of another channel that a person chose (`--switch-channel`): it enters whatever its revision, the history of the old channel is not
    /// asked of it (the counter of each channel is its own), and only the floor still holds. `same_revision` is set when it has the revision of the installed
    /// release and other bytes: with one counter for every channel that should not happen, and it is an error of the author, which `doctor` says.
    Switch { same_revision: bool },
}

/// A candidate that enters over an installed release (an `install` of a file, an `update`, an `apply`): its revision, its channel and
/// the floor. `candidate_signed` is the hash of its signed message.
pub fn check_entering(installed: &Installed, cand: &Release, candidate_signed: &[u8; 32], allow: Allow) -> Result<Standing> {
    if cand.channel != installed.channel && !allow.switch_channel {
        return Err(coded(
            ErrorCode::E4005,
            format!(
                "this package is of the channel `{}` and the installed app follows `{}`: a change of channel is a choice of yours (use --switch-channel)",
                cand.channel, installed.channel
            ),
        ));
    }
    if cand.revision < installed.floor {
        return Err(coded(
            ErrorCode::E4003,
            format!("revision {} is below the floor {} that the author set for this app: nothing below the floor is installed, updated to or rolled back to", cand.revision, installed.floor),
        ));
    }
    if cand.channel != installed.channel {
        return Ok(Standing::Switch { same_revision: cand.revision == installed.revision && *candidate_signed != installed.signed });
    }
    if cand.revision < installed.revision {
        if !allow.downgrade {
            return Err(coded(ErrorCode::E4001, format!("revision {} is older than the installed {}: refused (use --allow-downgrade)", cand.revision, installed.revision)));
        }
        return Ok(Standing::Older);
    }
    if cand.revision == installed.revision {
        if *candidate_signed == installed.signed {
            return Ok(Standing::Same);
        }
        return Err(coded(
            ErrorCode::E4004,
            format!("two different publications carry the revision {}: one of them is not what the author meant to publish (a fork or a replay); nothing was changed. A new build of your own app is a new release: make it with `panpak pack --previous <the package that is installed>`, or give it a higher revision", cand.revision),
        ));
    }
    Ok(Standing::Newer)
}

/// A rollback to the release `previous`: below the floor it is refused.
pub fn check_rollback(floor: u64, previous_revision: u64) -> Result<()> {
    if previous_revision < floor {
        return Err(coded(
            ErrorCode::E4003,
            format!("the previous version has revision {previous_revision}, below the floor {floor} that the author set for this app: a rollback to it is refused"),
        ));
    }
    Ok(())
}

/// A version that continues the installed one: the installed release is in the history of the candidate, and the history that the installed
/// release had is the beginning of the candidate's. An app whose installed release has no history and a candidate that has none claim
/// nothing, and pass; once there is a history it cannot be dropped.
pub fn check_continuity(installed: &Installed, cand_revision: u64, cand_history: Option<&[Entry]>) -> Result<()> {
    let fork = |m: String| coded(ErrorCode::E4006, format!("{m}: this is not a continuation of the installed version, it may be a fork or a forgery; nothing was changed"));
    let Some(h) = cand_history else {
        if installed.history.is_some() {
            return Err(fork("the new version has no history, and the installed one has".to_string()));
        }
        return Ok(());
    };
    if h.last().is_some_and(|(r, _)| *r >= cand_revision) {
        return Err(fork(format!("the history of the new version lists revisions up to {}, not below its own {cand_revision}", h.last().map(|e| e.0).unwrap_or(0))));
    }
    if !h.iter().any(|(r, s)| *r == installed.revision && *s == installed.signed) {
        return Err(fork(format!("the installed version (revision {}) is not in the history of the new one", installed.revision)));
    }
    if let Some(old) = &installed.history {
        if h.len() < old.len() || h[..old.len()] != old[..] {
            return Err(fork("the history of the new version does not begin with the history of the installed one".to_string()));
        }
    }
    Ok(())
}

/// The history of a release, read from its tree. A release whose manifest names no history has none; one that names it must have the
/// file, regular and with the hash that the signed manifest gives.
pub fn read_tree_history(tree: &std::path::Path, rel: &Release) -> Result<Option<Vec<Entry>>> {
    let Some(want) = &rel.history else { return Ok(None) };
    let path = tree.join(HISTORY_PATH);
    let regular = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file() && m.len() as usize <= MAX_HISTORY_FILE);
    if !regular {
        return Err(coded(ErrorCode::E4006, format!("the history file of the release is missing from {} or is not a regular file: `panpak repair` the app", tree.display())));
    }
    let bytes = std::fs::read(&path)?;
    check_history_hash(&bytes, want)?;
    Ok(Some(parse_history(&bytes)?))
}

/// The history of a package that is a file: read from its tree entry, with every chunk checked.
pub fn read_package_history(f: &mut std::fs::File, raw_index: &[u8], rel: &Release) -> Result<Option<Vec<Entry>>> {
    let Some(want) = &rel.history else { return Ok(None) };
    let bytes = crate::read_package_file(f, raw_index, HISTORY_PATH, MAX_HISTORY_FILE)?;
    check_history_hash(&bytes, want)?;
    Ok(Some(parse_history(&bytes)?))
}

fn check_history_hash(bytes: &[u8], want: &str) -> Result<()> {
    if history_hash(bytes) != want {
        return Err(coded(ErrorCode::E4006, "the history file does not match the hash that the signed manifest gives (the package is damaged or was changed)"));
    }
    Ok(())
}

/// The highest floor seen for an app, kept in its trust record (`<app folder>/trust`, the line `floor=`). A floor only grows: it is the highest `[release].floor` of
/// any release of the app whose signature was verified here (an install, an update that was refused too, an apply).
pub fn seen_floor(appdir: &std::path::Path) -> u64 {
    crate::trust::read(appdir).map_or(0, |s| s.floor)
}

/// Remember a floor if it is higher than the one kept.
pub fn note_floor(appdir: &std::path::Path, floor: u64) -> Result<()> {
    note_release(appdir, floor, 0)
}

/// Remember the floor and the revision of a release whose signature was verified here: both only grow.
pub fn note_release(appdir: &std::path::Path, floor: u64, revision: u64) -> Result<()> {
    let have = crate::trust::read(appdir);
    if floor > have.as_ref().map_or(0, |s| s.floor) || revision > have.as_ref().map_or(0, |s| s.seen_revision) {
        crate::trust::modify(appdir, |s| {
            s.floor = s.floor.max(floor);
            s.seen_revision = s.seen_revision.max(revision);
        })?;
    }
    Ok(())
}

/// An app is taken as graphical when its package says it has windows of its own to show: a window class or icons (`[desktop]`), or X11. Any other app is a
/// program of the terminal. The manifest has no field that says it outright; when it has one (step 14), that field is the rule. It decides how protected mode
/// speaks to the person: a notification for a graphical app, the terminal for the other.
pub fn is_graphical(man: &crate::Manifest) -> bool {
    let desktop = man.desktop.as_ref().is_some_and(|d| d.wm_class.is_some() || d.icons.as_ref().is_some_and(|i| !i.is_empty()));
    desktop || man.permissions.as_ref().and_then(|p| p.x11) == Some(true)
}

/// The file of `<app folder>` that exists while the installed version is in protected mode (its content is the floor). A start looks for this one file and
/// nothing else; the restrictions themselves are decided by the floor and the revision (`protected_floor`), so a file that is out of date costs one missed
/// question, never a missed restriction.
pub const PROTECTED_MARK: &str = "protected";

/// Make the mark say what the floor and the installed revision say. Returns whether the state changed (the menu entry of the app follows it).
pub fn sync_protected(appdir: &std::path::Path, installed_revision: u64) -> Result<bool> {
    let path = appdir.join(PROTECTED_MARK);
    let had = std::fs::symlink_metadata(&path).is_ok();
    match protected_floor(appdir, installed_revision) {
        Some(floor) => crate::durable::write_file(&path, format!("{floor}\n").as_bytes(), 0o644)?,
        None => {
            if let Err(e) = std::fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(e.into());
                }
            }
        }
    }
    Ok(had != protected_floor(appdir, installed_revision).is_some())
}

/// Whether the mark is there (a regular file of the app folder).
pub fn is_marked_protected(appdir: &std::path::Path) -> bool {
    std::fs::symlink_metadata(appdir.join(PROTECTED_MARK)).is_ok_and(|m| m.file_type().is_file())
}

/// What the menu entry of the app shows: `None` when the app is not in protected mode, `Some(true)` when the person turned the internet back on.
pub fn menu_state(appdir: &std::path::Path) -> Option<bool> {
    is_marked_protected(appdir).then(|| internet_is_back_on(appdir))
}

/// The file of `<app folder>` that says the person turned the internet back on for an app that is in protected mode.
pub const INTERNET_BACK_ON: &str = "protected-net";

/// The floor that puts the installed version in protected mode: the highest floor seen for the app, when the installed revision is below it.
/// Nothing is read but the one small file; an app that never saw a floor above its own revision is not in protected mode.
pub fn protected_floor(appdir: &std::path::Path, installed_revision: u64) -> Option<u64> {
    let floor = seen_floor(appdir);
    (installed_revision < floor).then_some(floor)
}

/// Whether the person turned the internet back on for this app (a regular file of the app folder; a link or a folder of that name counts for nothing).
pub fn internet_is_back_on(appdir: &std::path::Path) -> bool {
    std::fs::symlink_metadata(appdir.join(INTERNET_BACK_ON)).is_ok_and(|m| m.file_type().is_file())
}

/// Turn the internet back on (`true`) or off again (`false`) for an app in protected mode. It is the person's act and is kept in the state of the client,
/// not in the manifest; it ends with the mode (`end_protection`).
pub fn set_internet_back_on(appdir: &std::path::Path, on: bool) -> Result<()> {
    let path = appdir.join(INTERNET_BACK_ON);
    if on {
        crate::durable::write_file(&path, b"1\n", 0o644)?;
    } else if let Err(e) = std::fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(e.into());
        }
    }
    Ok(())
}

/// What protected mode takes away from the permissions of the manifest in memory (the signed manifest is not changed): no program that the app wrote is
/// run, and no network, not even the ports it listed, unless the person turned the internet back on. The network that stays is the one inside the app's
/// own view (its loopback), which is what an app without any network permission has.
pub fn protect(perms: &mut crate::Permissions, internet_back_on: bool) {
    perms.exec_written = Some(false);
    if !internet_back_on {
        perms.network = Some(false);
        perms.network_allow_tcp = None;
        perms.network_bind_tcp = None;
    }
}

/// The history that a package made after `previous` carries: the previous one, then the previous release itself.
pub fn extend_history(previous_history: Option<&[Entry]>, previous_revision: u64, previous_signed: [u8; 32]) -> Result<Vec<Entry>> {
    let mut h: Vec<Entry> = previous_history.map(|h| h.to_vec()).unwrap_or_default();
    if h.last().is_some_and(|(r, _)| *r >= previous_revision) {
        bail!("the history of the previous package lists revisions that are not below its own {previous_revision}");
    }
    h.push((previous_revision, previous_signed));
    if h.len() > MAX_HISTORY {
        bail!("the history would list more than {MAX_HISTORY} releases");
    }
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn rel(revision: u64, channel: &str) -> Release {
        Release { revision, floor: 0, security: 0, channel: channel.into(), history: None }
    }

    fn installed(revision: u64, hist: Option<Vec<Entry>>) -> Installed {
        Installed { revision, channel: "stable".into(), signed: h(revision as u8), history: hist, floor: 0 }
    }

    fn code(e: anyhow::Error) -> ErrorCode {
        crate::code_of(&e, ErrorCode::E4000)
    }

    #[test]
    fn protected_mode_is_a_floor_above_the_installed_revision_and_takes_the_network_and_written_programs() {
        let d = std::env::temp_dir().join(format!("panpak-protected-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(protected_floor(&d, 1), None, "no floor seen: not protected");
        note_floor(&d, 3).unwrap();
        assert_eq!(protected_floor(&d, 2), Some(3));
        assert_eq!(protected_floor(&d, 3), None, "the revision of the floor itself is fine");
        assert_eq!(protected_floor(&d, 9), None);
        assert!(!internet_is_back_on(&d));
        set_internet_back_on(&d, true).unwrap();
        assert!(internet_is_back_on(&d));
        set_internet_back_on(&d, false).unwrap();
        set_internet_back_on(&d, false).unwrap();
        assert!(!internet_is_back_on(&d));
        // A link or a folder of that name is not the person's act.
        std::os::unix::fs::symlink("/etc/passwd", d.join(INTERNET_BACK_ON)).unwrap();
        assert!(!internet_is_back_on(&d));
        let _ = std::fs::remove_dir_all(&d);

        let asked = || crate::Permissions {
            network: Some(true),
            network_allow_tcp: Some(vec![443]),
            network_bind_tcp: Some(vec![8080]),
            exec_written: Some(true),
            ..crate::Permissions::default()
        };
        let mut p = asked();
        protect(&mut p, false);
        assert_eq!((p.network, p.network_allow_tcp, p.network_bind_tcp, p.exec_written), (Some(false), None, None, Some(false)));
        let mut p = asked();
        protect(&mut p, true);
        assert_eq!((p.network, p.network_allow_tcp.clone(), p.network_bind_tcp.clone(), p.exec_written), (Some(true), Some(vec![443]), Some(vec![8080]), Some(false)), "the internet back on keeps the network that the app asked for, not the programs it wrote");
    }

    #[test]
    fn a_history_is_written_and_read_back_and_a_malformed_one_is_refused() {
        let list = vec![(1, h(1)), (2, h(2)), (7, h(7))];
        let text = encode_history(&list);
        assert!(text.starts_with("panpak-history 1\n1 0101"), "{text}");
        assert_eq!(parse_history(text.as_bytes()).unwrap(), list);
        assert_eq!(parse_history(encode_history(&[]).as_bytes()).unwrap(), vec![]);
        let one = crate::hex(&h(1));
        for (name, bad) in [
            ("no magic", format!("1 {one}\n")),
            ("not ending", format!("panpak-history 1\n1 {one}")),
            ("a blank line", format!("panpak-history 1\n1 {one}\n\n")),
            ("revision 0", format!("panpak-history 1\n0 {one}\n")),
            ("leading zero", format!("panpak-history 1\n01 {one}\n")),
            ("a plus", format!("panpak-history 1\n+1 {one}\n")),
            ("not rising", format!("panpak-history 1\n2 {one}\n2 {one}\n")),
            ("falling", format!("panpak-history 1\n3 {one}\n2 {one}\n")),
            ("short hash", "panpak-history 1\n1 abcd\n".to_string()),
            ("upper hash", format!("panpak-history 1\n1 {}\n", one.to_uppercase().replace('0', "A"))),
            ("no hash", "panpak-history 1\n1\n".to_string()),
        ] {
            let e = parse_history(bad.as_bytes()).expect_err(name);
            assert_eq!(code(e), ErrorCode::E4006, "{name}");
        }
        assert!(parse_history(&[b'x'; MAX_HISTORY_FILE + 1]).is_err());
        assert!(parse_history(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn a_candidate_is_told_by_its_revision_and_never_by_its_version_text() {
        let i = installed(5, None);
        let go = |c: &Release, s: u8, a: Allow| check_entering(&i, c, &h(s), a);
        assert_eq!(go(&rel(6, "stable"), 6, Allow::default()).unwrap(), Standing::Newer);
        // The same release again: nothing to do.
        assert_eq!(go(&rel(5, "stable"), 5, Allow::default()).unwrap(), Standing::Same);
        // The same number with other bytes: two publications under one number.
        assert_eq!(code(go(&rel(5, "stable"), 99, Allow::default()).unwrap_err()), ErrorCode::E4004);
        // Older: a downgrade, which only an explicit option allows.
        assert_eq!(code(go(&rel(4, "stable"), 4, Allow::default()).unwrap_err()), ErrorCode::E4001);
        assert_eq!(go(&rel(4, "stable"), 4, Allow { downgrade: true, ..Allow::default() }).unwrap(), Standing::Older);
    }

    #[test]
    fn nothing_below_the_floor_enters_not_even_with_allow_downgrade() {
        let mut i = installed(8, None);
        i.floor = 6;
        let allow = Allow { downgrade: true, switch_channel: false };
        assert_eq!(code(check_entering(&i, &rel(5, "stable"), &h(5), allow).unwrap_err()), ErrorCode::E4003);
        assert!(check_entering(&i, &rel(6, "stable"), &h(6), allow).is_ok());
        // A rollback is the same rule: the previous version below the floor is refused, one at the floor is allowed.
        assert_eq!(code(check_rollback(6, 5).unwrap_err()), ErrorCode::E4003);
        assert!(check_rollback(6, 6).is_ok() && check_rollback(0, 1).is_ok());
    }

    #[test]
    fn a_channel_does_not_change_by_itself() {
        let i = installed(5, None);
        let e = check_entering(&i, &rel(9, "beta"), &h(9), Allow::default()).unwrap_err();
        assert_eq!(code(e), ErrorCode::E4005);
        let sw = Allow { switch_channel: true, ..Allow::default() };
        assert_eq!(check_entering(&i, &rel(9, "beta"), &h(9), sw).unwrap(), Standing::Switch { same_revision: false });
    }

    #[test]
    fn a_switch_of_channel_may_go_down_and_skips_the_history_but_not_the_floor() {
        let mut i = installed(8, None);
        i.floor = 3;
        let sw = Allow { switch_channel: true, ..Allow::default() };
        // Beta (revision 8) to stable (revision 5): lower, and no downgrade option is needed.
        assert_eq!(check_entering(&i, &rel(5, "beta2"), &h(5), sw).unwrap(), Standing::Switch { same_revision: false });
        // The same revision with other bytes is another channel's own release, not a replay.
        assert_eq!(check_entering(&i, &rel(8, "beta2"), &h(99), sw).unwrap(), Standing::Switch { same_revision: true });
        // The floor still holds.
        assert_eq!(code(check_entering(&i, &rel(2, "beta2"), &h(2), sw).unwrap_err()), ErrorCode::E4003);
        // Without the option the channel is refused first.
        assert_eq!(code(check_entering(&i, &rel(5, "beta2"), &h(5), Allow::default()).unwrap_err()), ErrorCode::E4005);
    }

    #[test]
    fn a_version_must_continue_the_installed_one() {
        let inst = installed(3, None);
        // The installed release is in the history: it continues.
        assert!(check_continuity(&inst, 4, Some(&[(1, h(1)), (3, h(3))])).is_ok());
        // It is not: a fork (revision 3 with another message), or a history that skips it.
        assert_eq!(code(check_continuity(&inst, 4, Some(&[(1, h(1)), (3, h(9))])).unwrap_err()), ErrorCode::E4006);
        assert_eq!(code(check_continuity(&inst, 4, Some(&[(1, h(1)), (2, h(2))])).unwrap_err()), ErrorCode::E4006);
        // A history that reaches its own revision or beyond is not the history of that release.
        assert_eq!(code(check_continuity(&inst, 3, Some(&[(3, h(3))])).unwrap_err()), ErrorCode::E4006);
        // Neither has a history: nothing is claimed. Once there is one it cannot be dropped.
        assert!(check_continuity(&inst, 4, None).is_ok());
        let chained = installed(3, Some(vec![(1, h(1)), (2, h(2))]));
        assert_eq!(code(check_continuity(&chained, 4, None).unwrap_err()), ErrorCode::E4006);
        // The history of the installed release is the beginning of the new one's, not another past.
        assert!(check_continuity(&chained, 4, Some(&[(1, h(1)), (2, h(2)), (3, h(3))])).is_ok());
        assert_eq!(code(check_continuity(&chained, 4, Some(&[(1, h(1)), (2, h(8)), (3, h(3))])).unwrap_err()), ErrorCode::E4006);
        assert_eq!(code(check_continuity(&chained, 4, Some(&[(2, h(2)), (3, h(3))])).unwrap_err()), ErrorCode::E4006);
    }

    #[test]
    fn the_floor_kept_for_an_app_only_grows() {
        let d = std::env::temp_dir().join(format!("panpak-floor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(seen_floor(&d), 0);
        note_floor(&d, 4).unwrap();
        assert_eq!(seen_floor(&d), 4);
        note_floor(&d, 2).unwrap();
        assert_eq!(seen_floor(&d), 4);
        note_floor(&d, 9).unwrap();
        assert_eq!(seen_floor(&d), 9);
        std::fs::write(d.join(crate::trust::FILE), "floor=junk\n").unwrap();
        assert_eq!(seen_floor(&d), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_history_of_a_tree_is_the_file_that_the_manifest_hash_names() {
        let d = std::env::temp_dir().join(format!("panpak-histtree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".panpak")).unwrap();
        let text = encode_history(&[(1, h(1))]);
        let mut r = rel(2, "stable");
        // No hash in the manifest: no history, whatever the tree holds.
        assert_eq!(read_tree_history(&d, &r).unwrap(), None);
        r.history = Some(history_hash(text.as_bytes()));
        // The file is missing: the tree is incomplete.
        assert_eq!(code(read_tree_history(&d, &r).unwrap_err()), ErrorCode::E4006);
        std::fs::write(d.join(HISTORY_PATH), &text).unwrap();
        assert_eq!(read_tree_history(&d, &r).unwrap(), Some(vec![(1, h(1))]));
        // Changed after the signature: the hash does not match.
        std::fs::write(d.join(HISTORY_PATH), encode_history(&[(1, h(2))])).unwrap();
        assert_eq!(code(read_tree_history(&d, &r).unwrap_err()), ErrorCode::E4006);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_history_is_made_from_the_previous_package() {
        let list = extend_history(None, 1, h(1)).unwrap();
        assert_eq!(list, vec![(1, h(1))]);
        let list = extend_history(Some(&list), 2, h(2)).unwrap();
        assert_eq!(list, vec![(1, h(1)), (2, h(2))]);
        // A previous package whose own history is not below it is not one to build on.
        assert!(extend_history(Some(&list), 2, h(3)).is_err());
        assert!(extend_history(Some(&list), 1, h(3)).is_err());
    }

    #[test]
    fn the_release_table_is_checked_in_a_signed_manifest() {
        use crate::{check_release, Manifest};
        let base = "[app]\nid = \"a.b\"\nname = \"N\"\nversion = \"1\"\nentry = \"x\"\n";
        let man = |extra: &str| -> Manifest { toml::from_str(&format!("{base}{extra}")).unwrap() };
        // No table: a package of a test version.
        assert_eq!(code(check_release(&man("")).unwrap_err()), ErrorCode::E0011);
        assert!(check_release(&man("[release]\nrevision = 3\n")).is_ok());
        assert!(check_release(&man("[release]\nrevision = 3\nfloor = 3\nsecurity = 2\nchannel = \"beta-2\"\nhistory = \"00000000000000000000000000000000000000000000000000000000000000ab\"\n")).is_ok());
        for bad in [
            "[release]\nrevision = 0\n",
            "[release]\nrevision = 3\nfloor = 4\n",
            "[release]\nrevision = 3\nsecurity = 4\n",
            "[release]\nrevision = 3\nchannel = \"\"\n",
            "[release]\nrevision = 3\nchannel = \"Stable\"\n",
            "[release]\nrevision = 3\nchannel = \"-x\"\n",
            "[release]\nrevision = 3\nhistory = \"abc\"\n",
        ] {
            assert!(check_release(&man(bad)).is_err(), "{bad}");
        }
        // The table is closed.
        assert!(toml::from_str::<Manifest>(&format!("{base}[release]\nrevision = 1\nfuture = 1\n")).is_err());
    }
}
