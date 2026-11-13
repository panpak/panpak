// SPDX-License-Identifier: MIT OR Apache-2.0
//! The one table of permissions: what each permission is called, how serious it is, which icon it has, and what an app without it cannot do.
//! Everything the user reads about permissions (`install`, `update`, `list`, `inspect`, the README) comes from here, in the language of the user
//! (see `i18n`). The sentences are the English source of the catalogs in `po/`.

use crate::i18n::{self, fill, tr_in, trn_in, Catalog};
use crate::{Manifest, Permissions};
use std::borrow::Cow;

/// How serious a permission is. The order is the order of the list (`High` first when sorted descending).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Normal,
    Attention,
    High,
}

impl Level {
    /// The stable name, for `--json`.
    pub fn key(self) -> &'static str {
        match self {
            Level::Normal => "normal",
            Level::Attention => "attention",
            Level::High => "high",
        }
    }
    /// The name in the language of the catalog.
    pub fn word(self, lang: Option<&Catalog>) -> Cow<'static, str> {
        match self {
            Level::Normal => tr_in(lang, "Normal"),
            Level::Attention => tr_in(lang, "Attention"),
            Level::High => tr_in(lang, "High risk"),
        }
    }
    /// The mark that is always shown with the word, in plain ASCII.
    pub fn mark(self) -> &'static str {
        match self {
            Level::Normal => "[ ]",
            Level::Attention => "[!]",
            Level::High => "[!!]",
        }
    }
}

/// One row of the table.
pub struct Row {
    pub id: &'static str,
    pub level: Level,
    /// The name of a symbolic icon of the freedesktop theme (for the Panpak app).
    pub icon: &'static str,
    /// A symbol for a terminal that shows Unicode.
    pub glyph: &'static str,
    /// The sentence; `{subject}` is the folder, path, socket, scheme or the list of ports.
    pub text: &'static str,
    /// What it means, in a longer sentence.
    pub detail: &'static str,
    /// Why the isolation is only partial (High rows only), as a phrase that goes after "Partial isolation:".
    pub reason: &'static str,
}

const fn row(id: &'static str, level: Level, icon: &'static str, glyph: &'static str, text: &'static str, detail: &'static str, reason: &'static str) -> Row {
    Row { id, level, icon, glyph, text, detail, reason }
}

/// The table. Rows that make a promise the sandbox does not keep yet are not here.
pub static ROWS: &[Row] = &[
    row("x11", Level::High, "dialog-warning-symbolic", "\u{26a0}", "See what other apps show and what you type in them (X11)",
        "X11 has no walls between apps: this app can read the windows and the keyboard input of the others.", "it can watch other apps (X11)"),
    row("desktop-bus", Level::High, "dialog-warning-symbolic", "\u{26a0}", "Use the message bus of your whole desktop",
        "It can talk to every service of your session, also to start programs and to read what other apps publish. The sandbox does not isolate it any more.",
        "it can talk to every service of your desktop"),
    row("home-path-write", Level::High, "user-home-symbolic", "\u{2302}", "Read, change and delete {subject} in your home folder",
        "A place in your home folder that belongs to you, not to the app, is open to it for writing.", "it reaches your home folder"),
    row("home-path-read", Level::High, "user-home-symbolic", "\u{2302}", "Read {subject} in your home folder",
        "A place in your home folder that belongs to you, not to the app, is open to it for reading.", "it reaches your home folder"),
    row("network-connect", Level::Attention, "network-wireless-symbolic", "\u{21c4}", "Connect to the network on the ports {subject}",
        "It can reach any computer on these TCP ports, also on the internet. The filter is by port, not by address.", ""),
    row("network-listen", Level::Attention, "network-wireless-symbolic", "\u{21c4}", "Accept connections from the network on the ports {subject}",
        "Other computers can connect to this app on these TCP ports.", ""),
    row("network-any", Level::Attention, "network-wireless-symbolic", "\u{21c4}", "Use the network",
        "It can use the network. The ports are not listed by the package.", ""),
    row("audio", Level::Attention, "audio-input-microphone-symbolic", "\u{266a}", "Play sound and use the microphone",
        "It can use the sound system of your session: play sounds and record from the microphone.", ""),
    row("folder-write", Level::Attention, "folder-symbolic", "\u{25a3}", "Read, change and delete the files in your {subject} folder",
        "The folder is the one of your user-dirs.dirs, not a copy.", ""),
    row("folder-read", Level::Attention, "folder-symbolic", "\u{25a3}", "Read the files in your {subject} folder",
        "The folder is the one of your user-dirs.dirs. The app cannot change it.", ""),
    row("socket", Level::Attention, "network-server-symbolic", "\u{2301}", "Talk to the local service {subject}",
        "It can connect to this Unix socket, and so to the program that listens on it.", ""),
    row("url-scheme", Level::Normal, "emblem-symbolic-link", "\u{2197}", "Open the links that start with {subject}:",
        "The links of this kind, in your browser or in other apps, are given to this app.", ""),
    row("notifications", Level::Normal, "preferences-system-notifications-symbolic", "\u{25d4}", "Show notifications",
        "It can show messages in the notification area of your desktop, also while it works in the background.", ""),
    row("tray", Level::Normal, "view-pin-symbolic", "\u{25c6}", "Put an icon in the notification area",
        "It can show an icon and a menu there, and it sees which icons are there.", ""),
    row("media-keys", Level::Normal, "multimedia-player-symbolic", "\u{25b6}", "Be controlled by the media keys",
        "The shell of your desktop and the media keys of your keyboard can play, pause and skip in it.", ""),
];

pub fn row_by_id(id: &str) -> Option<&'static Row> {
    ROWS.iter().find(|r| r.id == id)
}

/// A permission a package asks for, ready to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: &'static str,
    pub level: Level,
    pub icon: &'static str,
    pub glyph: &'static str,
    /// The sentence, in the language asked for.
    pub text: String,
    pub detail: String,
    /// The folder, path, socket, scheme or ports that the sentence names; empty for a permission with no subject.
    pub subjects: Vec<String>,
}

fn item(lang: Option<&Catalog>, id: &'static str, subjects: Vec<String>) -> Item {
    let r = row_by_id(id).expect("known permission id");
    let subject = subjects.join(", ");
    Item {
        id,
        level: r.level,
        icon: r.icon,
        glyph: r.glyph,
        text: fill(&tr_in(lang, r.text), &[("subject", subject.clone())]),
        detail: fill(&tr_in(lang, r.detail), &[("subject", subject)]),
        subjects,
    }
}

fn ports(v: &Option<Vec<u16>>) -> Vec<String> {
    v.iter().flatten().map(u16::to_string).collect()
}

/// What a manifest asks for, in the language of the environment, the most serious first.
pub fn items(man: &Manifest) -> Vec<Item> {
    items_in(i18n::current(), man)
}

/// What a manifest asks for. Within a level the order of the table is kept.
pub fn items_in(lang: Option<&Catalog>, man: &Manifest) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    for sc in man.desktop.as_ref().and_then(|d| d.schemes.as_ref()).into_iter().flatten() {
        out.push(item(lang, "url-scheme", vec![sc.clone()]));
    }
    if let Some(p) = &man.permissions {
        out.extend(from_permissions(lang, p));
    }
    // Stable sort: the High rows first, then Attention, then Normal; inside a level, the order of the table.
    let pos = |id: &str| ROWS.iter().position(|r| r.id == id).unwrap_or(usize::MAX);
    out.sort_by(|a, b| b.level.cmp(&a.level).then(pos(a.id).cmp(&pos(b.id))));
    out
}

fn from_permissions(lang: Option<&Catalog>, p: &Permissions) -> Vec<Item> {
    let mut out = Vec::new();
    if p.x11 == Some(true) {
        out.push(item(lang, "x11", vec![]));
    }
    if p.audio == Some(true) {
        out.push(item(lang, "audio", vec![]));
    }
    let (allow, bind) = (ports(&p.network_allow_tcp), ports(&p.network_bind_tcp));
    if !allow.is_empty() {
        out.push(item(lang, "network-connect", allow.clone()));
    }
    if !bind.is_empty() {
        out.push(item(lang, "network-listen", bind.clone()));
    }
    if p.network == Some(true) && allow.is_empty() && bind.is_empty() {
        out.push(item(lang, "network-any", vec![]));
    }
    if p.notifications == Some(true) {
        out.push(item(lang, "notifications", vec![]));
    }
    if p.tray == Some(true) {
        out.push(item(lang, "tray", vec![]));
    }
    if p.mpris == Some(true) {
        out.push(item(lang, "media-keys", vec![]));
    }
    // `dbus_own` is not a permission: since the private bus it has no effect.
    let socks: Vec<&String> = p.sockets_allow.iter().flatten().collect();
    for s in &socks {
        out.push(item(lang, if crate::is_bus_socket(s) { "desktop-bus" } else { "socket" }, if crate::is_bus_socket(s) { vec![] } else { vec![(*s).clone()] }));
    }
    // One line for the bus, however many bus sockets are listed.
    let mut seen_bus = false;
    out.retain(|i| i.id != "desktop-bus" || !std::mem::replace(&mut seen_bus, true));
    for d in p.xdg_dirs.iter().flatten() {
        let id = if d.access == "rw" { "folder-write" } else { "folder-read" };
        out.push(item(lang, id, vec![d.name.clone()]));
    }
    for h in p.home_paths.iter().flatten() {
        let id = if h.access == "rw" { "home-path-write" } else { "home-path-read" };
        out.push(item(lang, id, vec![h.path.clone()]));
    }
    out
}

/// The row that makes `id` unnecessary when the old version had it for the same subject.
fn covered_by(id: &str) -> Option<&'static str> {
    match id {
        "folder-read" => Some("folder-write"),
        "home-path-read" => Some("home-path-write"),
        _ => None,
    }
}

/// The items of `new` that `old` did not have: a permission turned on, a new subject (a port, a folder, a path, a socket, a scheme), or a read
/// raised to a write. An item that only goes away, or goes down, is not listed.
pub fn added(old: &Manifest, new: &Manifest) -> Vec<Item> {
    let (o, n) = (items_in(None, old), items_in(i18n::current(), new));
    let mut out = Vec::new();
    for mut it in n {
        let has = |id: &str, s: &str| o.iter().any(|x| x.id == id && (s.is_empty() || x.subjects.iter().any(|y| y == s)));
        if it.subjects.is_empty() {
            if !has(it.id, "") {
                out.push(it);
            }
            continue;
        }
        // A list of subjects (ports, or one name): keep only the subjects that are new.
        let fresh: Vec<String> = it
            .subjects
            .iter()
            .filter(|s| !has(it.id, s) && !covered_by(it.id).is_some_and(|c| has(c, s)))
            .cloned()
            .collect();
        if fresh.is_empty() {
            continue;
        }
        if fresh.len() != it.subjects.len() {
            it = item(i18n::current(), it.id, fresh);
        }
        out.push(it);
    }
    out
}

/// The highest level of a list of items, if there is one.
pub fn top_level(items: &[Item]) -> Option<Level> {
    items.iter().map(|i| i.level).max()
}

/// The first line of the summary: full isolation, or partial isolation and why. It comes from the table.
pub fn summary(lang: Option<&Catalog>, items: &[Item]) -> String {
    let mut why: Vec<Cow<'static, str>> = Vec::new();
    for i in items.iter().filter(|i| i.level == Level::High) {
        let r = tr_in(lang, row_by_id(i.id).map_or("", |r| r.reason));
        if !why.contains(&r) {
            why.push(r);
        }
    }
    if why.is_empty() {
        tr_in(lang, "Full isolation").into_owned()
    } else {
        format!("{} {}", tr_in(lang, "Partial isolation:"), why.join("; "))
    }
}

/// What the app cannot do, in short sentences, from the permissions it does not have.
pub fn cannot(lang: Option<&Catalog>, man: &Manifest) -> Vec<String> {
    let p = man.permissions.clone();
    let has = |f: &dyn Fn(&Permissions) -> bool| p.as_ref().is_some_and(f);
    let list = |v: &Option<Vec<String>>| v.as_ref().is_some_and(|l| !l.is_empty());
    let net = has(&|p| p.network == Some(true) || !ports(&p.network_allow_tcp).is_empty() || !ports(&p.network_bind_tcp).is_empty());
    let homes = has(&|p| p.home_paths.as_ref().is_some_and(|h| !h.is_empty()));
    let dirs = has(&|p| p.xdg_dirs.as_ref().is_some_and(|d| !d.is_empty()));
    let socks = has(&|p| list(&p.sockets_allow));
    let bus = has(&|p| p.sockets_allow.iter().flatten().any(|s| crate::is_bus_socket(s)));
    let mut out = Vec::new();
    if !net {
        out.push("It cannot use the internet or the network.");
    }
    if !homes {
        out.push(if dirs { "It cannot see your files outside the folders above." } else { "It cannot see your documents, photos or other personal files." });
    }
    if !has(&|p| p.x11 == Some(true)) {
        out.push("It cannot watch other apps or read what you type in them.");
    }
    if !bus {
        out.push("It cannot control your other apps or the services of your desktop.");
    }
    if !has(&|p| p.audio == Some(true)) {
        out.push("It cannot record sound.");
    }
    if !homes && !socks {
        out.push("It cannot read your passwords, keys or the saved logins of other programs.");
    }
    out.into_iter().map(|s| tr_in(lang, s).into_owned()).collect()
}

/// How a terminal is written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub unicode: bool,
    pub color: bool,
}

impl Style {
    pub const PLAIN: Style = Style { unicode: false, color: false };

    /// From the environment: Unicode when the locale is UTF-8 and `TERM` is not `dumb`; color on a terminal when `NO_COLOR` is not set.
    pub fn detect(stdout_is_terminal: bool) -> Style {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let dumb = var("TERM").is_none_or(|t| t == "dumb");
        let loc = var("LC_ALL").or_else(|| var("LC_CTYPE")).or_else(|| var("LANG")).unwrap_or_default().to_ascii_lowercase();
        let utf8 = loc.contains("utf-8") || loc.contains("utf8");
        Style { unicode: utf8 && !dumb, color: stdout_is_terminal && !dumb && std::env::var_os("NO_COLOR").is_none() }
    }
}

fn paint(style: Style, level: Level, s: &str) -> String {
    if !style.color {
        return s.to_string();
    }
    let code = match level {
        Level::Normal => "32",
        Level::Attention => "33",
        Level::High => "1;31",
    };
    format!("\x1b[{code}m{s}\x1b[0m")
}

/// One line for an item: the mark and the word of the level (never the color alone), the glyph, the sentence.
pub fn render_item(lang: Option<&Catalog>, style: Style, it: &Item) -> String {
    let width = [Level::Normal, Level::Attention, Level::High].iter().map(|l| l.word(lang).chars().count()).max().unwrap_or(0);
    let word = it.level.word(lang);
    let pad = " ".repeat(width.saturating_sub(word.chars().count()));
    let tag = paint(style, it.level, &format!("{} {word}", it.level.mark()));
    let glyph = if style.unicode { format!("{} ", it.glyph) } else { String::new() };
    format!("{tag}{pad}  {glyph}{}", it.text)
}

/// The whole block for a manifest: the summary, what the app can do, and what it cannot. Lines are not indented.
pub fn render_block(lang: Option<&Catalog>, style: Style, man: &Manifest) -> Vec<String> {
    let its = items_in(lang, man);
    let mut out = vec![summary(lang, &its)];
    if its.is_empty() {
        out.push(tr_in(lang, "It asks for nothing beyond its own private folders.").into_owned());
    } else {
        out.push(tr_in(lang, "What it can do:").into_owned());
        out.extend(its.iter().map(|i| format!("  {}", render_item(lang, style, i))));
    }
    let no = cannot(lang, man);
    if !no.is_empty() {
        out.push(tr_in(lang, "What it cannot do:").into_owned());
        out.extend(no.iter().map(|s| format!("  - {s}")));
    }
    out
}

/// The count for `list`: the highest level and the number of permissions, such as `Attention, 4 permissions`; `none` for no permission.
pub fn short_summary(lang: Option<&Catalog>, items: &[Item]) -> String {
    match top_level(items) {
        None => tr_in(lang, "none").into_owned(),
        Some(l) => {
            let n = items.len() as u64;
            let count = fill(&trn_in(lang, "{n} permission", "{n} permissions", n), &[("n", n.to_string())]);
            format!("{}, {count}", l.word(lang))
        }
    }
}

/// `[{"id":..,"level":..,"subjects":[..]}]`: ids and values, not sentences, so they do not change with the language.
pub fn items_json(items: &[Item]) -> String {
    let q = |s: &str| {
        let mut o = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => o.push_str("\\\""),
                '\\' => o.push_str("\\\\"),
                c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
                c => o.push(c),
            }
        }
        o.push('"');
        o
    };
    let parts: Vec<String> = items
        .iter()
        .map(|i| format!("{{\"id\":{},\"level\":{},\"subjects\":[{}]}}", q(i.id), q(i.level.key()), i.subjects.iter().map(|s| q(s)).collect::<Vec<_>>().join(",")))
        .collect();
    format!("[{}]", parts.join(","))
}

/// The table as Markdown, in English, for the README. `panpak permissions --markdown` prints it.
pub fn markdown() -> String {
    let mut s = String::from("| Level | What the app may do | What it means |\n|---|---|---|\n");
    for r in ROWS {
        s.push_str(&format!("| {} {} | {} | {} |\n", r.level.mark().replace('[', "\\[").replace(']', "\\]"), r.level.word(None), r.text.replace("{subject}", "*x*"), r.detail));
    }
    s
}

/// Every message of this module that a catalog translates, for the template and its test.
pub fn msgids() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = vec![
        "Normal",
        "Attention",
        "High risk",
        "Full isolation",
        "Partial isolation:",
        "It asks for nothing beyond its own private folders.",
        "What it can do:",
        "What it cannot do:",
        "none",
        "{n} permission",
        "It cannot use the internet or the network.",
        "It cannot see your files outside the folders above.",
        "It cannot see your documents, photos or other personal files.",
        "It cannot watch other apps or read what you type in them.",
        "It cannot control your other apps or the services of your desktop.",
        "It cannot record sound.",
        "It cannot read your passwords, keys or the saved logins of other programs.",
        "This update asks for more than the version you have:",
        "This package asks for more than the version you have:",
        "If you do not accept, you stay on the current version.",
        "Accept them?",
    ];
    for r in ROWS {
        v.push(r.text);
        v.push(r.detail);
        if !r.reason.is_empty() {
            v.push(r.reason);
        }
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// The plural forms of the template: msgid and msgid_plural.
pub const PLURALS: &[(&str, &str)] = &[("{n} permission", "{n} permissions")];

#[cfg(test)]
mod tests {
    use super::*;

    fn man(extra: &str) -> Manifest {
        toml::from_str(&format!(
            "[app]\nid=\"org.example.app\"\nname=\"App\"\nversion=\"1.0\"\nentry=\"bin/app\"\n{extra}[update]\nkey=\"ed25519:{}\"\n",
            "0".repeat(64)
        ))
        .unwrap()
    }

    #[test]
    fn levels_follow_the_values() {
        let m = man("[permissions]\nx11=true\nnotifications=true\n[[permissions.home_paths]]\npath=\"Notes\"\naccess=\"ro\"\n[[permissions.xdg_dirs]]\nname=\"music\"\naccess=\"rw\"\n");
        let its = items_in(None, &m);
        let ids: Vec<_> = its.iter().map(|i| (i.id, i.level)).collect();
        assert_eq!(
            ids,
            [("x11", Level::High), ("home-path-read", Level::High), ("folder-write", Level::Attention), ("notifications", Level::Normal)]
        );
    }

    #[test]
    fn summary_is_full_or_partial_with_the_reason() {
        let ok = items_in(None, &man("[permissions]\nnetwork=true\nnetwork_allow_tcp=[443]\n"));
        assert_eq!(summary(None, &ok), "Full isolation");
        let bad = items_in(None, &man("[permissions]\nx11=true\n[[permissions.home_paths]]\npath=\"a\"\naccess=\"rw\"\n[[permissions.home_paths]]\npath=\"b\"\naccess=\"ro\"\n"));
        assert_eq!(summary(None, &bad), "Partial isolation: it can watch other apps (X11); it reaches your home folder");
    }

    #[test]
    fn dbus_own_is_not_a_permission_and_bus_sockets_are_one_line() {
        assert!(items_in(None, &man("[permissions]\ndbus_own=[\"com.example.app.Second\"]\n")).is_empty());
        let m = man("[permissions]\nsockets_allow=[\"$XDG_RUNTIME_DIR/bus\", \"/run/dbus/system_bus_socket\", \"/run/x/y.sock\"]\n");
        let ids: Vec<_> = items_in(None, &m).iter().map(|i| i.id).collect();
        assert_eq!(ids, ["desktop-bus", "socket"]);
    }

    #[test]
    fn cannot_lines_come_from_the_missing_permissions() {
        let none = cannot(None, &man(""));
        assert_eq!(none.len(), 6, "{none:?}");
        let all = cannot(
            None,
            &man("[permissions]\nx11=true\naudio=true\nnetwork=true\nsockets_allow=[\"$XDG_RUNTIME_DIR/bus\"]\n[[permissions.home_paths]]\npath=\"a\"\naccess=\"ro\"\n"),
        );
        assert!(all.is_empty(), "{all:?}");
        let dirs = cannot(None, &man("[permissions]\n[[permissions.xdg_dirs]]\nname=\"music\"\naccess=\"ro\"\n"));
        assert!(dirs.iter().any(|s| s.contains("outside the folders above")));
    }

    #[test]
    fn added_lists_new_subjects_and_raised_access() {
        let old = man("[permissions]\nnetwork=true\nnetwork_allow_tcp=[443]\n[[permissions.home_paths]]\npath=\"a\"\naccess=\"rw\"\n");
        let same = man("[permissions]\nnetwork=true\nnetwork_allow_tcp=[443]\n[[permissions.home_paths]]\npath=\"a\"\naccess=\"ro\"\n");
        assert!(added(&old, &same).is_empty());
        let more = man("[permissions]\nnetwork=true\nnetwork_allow_tcp=[443, 8080]\n[[permissions.home_paths]]\npath=\"a\"\naccess=\"rw\"\n[[permissions.home_paths]]\npath=\"b\"\naccess=\"ro\"\n");
        let a = added(&old, &more);
        assert_eq!(a.iter().map(|i| (i.id, i.subjects.clone())).collect::<Vec<_>>(), [("home-path-read", vec!["b".to_string()]), ("network-connect", vec!["8080".to_string()])]);
        let from_ro = man("[permissions]\n[[permissions.home_paths]]\npath=\"a\"\naccess=\"ro\"\n");
        let to_rw = man("[permissions]\n[[permissions.home_paths]]\npath=\"a\"\naccess=\"rw\"\n");
        assert_eq!(added(&from_ro, &to_rw).len(), 1);
        assert_eq!(added(&man(""), &old).len(), 2);
        assert!(added(&old, &man("")).is_empty());
    }

    #[test]
    fn lines_always_carry_the_word_and_the_mark() {
        let it = items_in(None, &man("[permissions]\nx11=true\n")).remove(0);
        let plain = render_item(None, Style::PLAIN, &it);
        assert!(plain.starts_with("[!!] High risk") && plain.contains("(X11)") && !plain.contains('\x1b'), "{plain}");
        let rich = render_item(None, Style { unicode: true, color: true }, &it);
        assert!(rich.contains("[!!] High risk") && rich.contains('\x1b') && rich.contains('\u{26a0}'), "{rich}");
    }

    #[test]
    fn list_summary_counts() {
        let its = items_in(None, &man("[permissions]\nnotifications=true\ntray=true\n"));
        assert_eq!(short_summary(None, &its), "Normal, 2 permissions");
        assert_eq!(short_summary(None, &its[..1]), "Normal, 1 permission");
        assert_eq!(short_summary(None, &[]), "none");
    }

    fn po_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("po")
    }

    /// The template, as `xgettext` would write it for these messages.
    fn pot_text() -> String {
        let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"));
        let mut out = String::from(
            "# Template of the translations of Panpak. Written by the test `template_matches_the_messages` (PANPAK_UPDATE_POT=1).\n\
             msgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\"Plural-Forms: nplurals=2; plural=(n != 1);\\n\"\n",
        );
        for id in msgids() {
            out.push_str(&format!("\nmsgid {}\n", q(id)));
            match PLURALS.iter().find(|p| p.0 == id) {
                Some(p) => out.push_str(&format!("msgid_plural {}\nmsgstr[0] \"\"\nmsgstr[1] \"\"\n", q(p.1))),
                None => out.push_str("msgstr \"\"\n"),
            }
        }
        out
    }

    #[test]
    fn template_matches_the_messages() {
        let want = pot_text();
        let path = po_dir().join("panpak.pot");
        if std::env::var_os("PANPAK_UPDATE_POT").is_some() {
            std::fs::write(&path, &want).unwrap();
        }
        let have = std::fs::read_to_string(&path).expect("po/panpak.pot");
        assert_eq!(have, want, "po/panpak.pot is out of date: run the tests with PANPAK_UPDATE_POT=1");
    }

    #[test]
    fn every_catalog_keeps_the_placeholders_and_italian_is_complete() {
        let ids = msgids();
        let cats = i18n::catalogs();
        assert!(cats.iter().any(|c| c.name == "it"), "po/it.po is missing");
        for c in cats {
            for (id, one, two) in c.entries {
                assert!(ids.contains(id), "{}: the message {id:?} is not in the program", c.name);
                assert_eq!(i18n::placeholders(id), i18n::placeholders(one), "{}: placeholders of {id:?}", c.name);
                if !two.is_empty() {
                    assert_eq!(i18n::placeholders(id), i18n::placeholders(two), "{}: plural placeholders of {id:?}", c.name);
                }
            }
        }
        let it = cats.iter().find(|c| c.name == "it").unwrap();
        for id in &ids {
            assert!(it.entries.iter().any(|e| e.0 == *id), "po/it.po has no translation of {id:?}");
        }
        for (one, _) in PLURALS {
            assert!(it.simple_plural && it.entries.iter().any(|e| e.0 == *one && !e.2.is_empty()), "plural of {one:?}");
        }
    }

    #[test]
    fn the_table_is_translated_into_italian_and_levels_stay_in_words() {
        let it = i18n::catalogs().iter().find(|c| c.name == "it");
        let m = man("[permissions]\nx11=true\nnotifications=true\n");
        let lines = render_block(it, Style::PLAIN, &m);
        let (partial, high, normal) = (tr_in(it, "Partial isolation:"), tr_in(it, "High risk"), tr_in(it, "Normal"));
        assert!(partial != "Partial isolation:" && lines[0].starts_with(&*partial), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains(&format!("[!!] {high}"))), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains(&format!("[ ] {normal}"))), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains("High risk")), "{lines:?}");
    }

    #[test]
    fn rows_have_unique_ids_and_valid_levels() {
        let mut ids: Vec<_> = ROWS.iter().map(|r| r.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n);
        for r in ROWS {
            assert_eq!(r.level == Level::High, !r.reason.is_empty(), "{}: a High row has a reason and only a High row", r.id);
        }
    }

    #[test]
    fn markdown_lists_every_row() {
        let md = markdown();
        for r in ROWS {
            assert!(md.contains(&r.text.replace("{subject}", "*x*")), "{}", r.id);
        }
    }

    #[test]
    fn readme_has_the_table() {
        let readme = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("README.md")).unwrap();
        let a = readme.find("<!-- permissions:start -->\n").expect("start marker") + "<!-- permissions:start -->\n".len();
        let b = readme.find("<!-- permissions:end -->").expect("end marker");
        assert_eq!(&readme[a..b], markdown(), "README.md is out of date: paste the output of `panpak permissions --markdown`");
    }
}
