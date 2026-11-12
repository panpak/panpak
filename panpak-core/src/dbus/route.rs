// SPDX-License-Identifier: MIT OR Apache-2.0
//! The router of the private bus of an app (step 13p): a state machine, with no I/O, so that it can be tested and fuzzed.
//!
//! One router serves all the copies of an app. Each connection of the app (`ClientId`) has one connection to the host bus behind it (a
//! `xdg-dbus-proxy` of the copy); the router sits between them. It reads the **header** of every message and passes the message on
//! unchanged (bytes and descriptors), with these exceptions:
//!
//! - the calls to the bus daemon about names and match rules (`RequestName`, `ReleaseName`, `GetNameOwner`, `NameHasOwner`,
//!   `ListQueuedOwners`, `StartServiceByName`, `AddMatch`, `RemoveMatch`): names the app owns live **here**, and a name that is not owned here goes to the host;
//! - messages for a name of the app or for the unique name of another connection of the app: delivered here, with the sender field set;
//! - `org.freedesktop.portal.OpenURI` calls: not passed on, handed to the opener (`Out::Open`);
//! - calls to `org.freedesktop.host.portal.Registry` from the app: refused (only the router registers the app's identity with the portal);
//! - the first thing after `Hello`: the router registers the identity `panpak.<id>` and, for a name `org.mpris.MediaPlayer2.*`, asks the host for an alias.
//!
//! The router never decides what the app may do on the host bus: whatever it does not serve goes to the proxy, which filters. It never
//! reads a body except for the calls above, so that a message it does not understand is a message it passes on.

use super::wire::{reply_serial_offset, encode_message, encode_with_body, parse_head, parse_message, valid_dotted_name, Head, Limits, Message, Value, CALL, ERROR, RETURN, SIGNAL};
use std::collections::{HashMap, HashSet, VecDeque};

pub type ClientId = u32;

/// A unique connection name as the daemon gives it: `:` and digits, dots, `_` or `-`, at most 255 bytes.
fn valid_unique(s: &str) -> bool {
    s.len() > 1 && s.len() <= 255 && s.starts_with(':') && s[1..].bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The name of the daemon and its object.
pub const BUS: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
pub const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub const REGISTRY_IFACE: &str = "org.freedesktop.host.portal.Registry";
pub const OPENURI_IFACE: &str = "org.freedesktop.portal.OpenURI";
pub const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// Largest message the router passes (the protocol allows 128 MiB; apps do not send more than a few MiB on the session bus).
pub const MAX_MESSAGE: usize = 16 << 20;
pub const MAX_NAMES_PER_CLIENT: usize = 32;
pub const MAX_RULES_PER_CLIENT: usize = 64;
const MAX_RULE_LEN: usize = 1024;
const MAX_OPENS_PER_CLIENT: usize = 8;
const MAX_SWALLOW: usize = 16;

const FLAG_NO_REPLY: u8 = 0x1;
const NAME_ALLOW_REPLACEMENT: u32 = 1;
const NAME_REPLACE_EXISTING: u32 = 2;
const NAME_DO_NOT_QUEUE: u32 = 4;

/// What the router needs to know about the app.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// The id the portal is told (`panpak.<id>`); `None` registers nothing.
    pub portal_id: Option<String>,
    /// The alias on the host bus for a media player name, if the app may have one.
    pub mpris_alias: Option<String>,
    /// Serve `OpenURI` here (hand it to the opener).
    pub open_uri: bool,
}

/// `org.mpris.MediaPlayer2.panpak.<id>` with dots and dashes of the id turned into `_`.
pub fn mpris_alias(app_id: &str) -> String {
    format!("{MPRIS_PREFIX}panpak.{}", app_id.replace(['.', '-'], "_"))
}

/// A message with the descriptors that belong to it.
#[derive(Debug)]
pub struct Msg<F> {
    pub raw: Vec<u8>,
    pub fds: Vec<F>,
}

/// What to do after an event.
#[derive(Debug)]
pub enum Out<F> {
    /// Send to the app on this connection.
    App(ClientId, Msg<F>),
    /// Send to the host, on the connection behind this one.
    Up(ClientId, Msg<F>),
    /// Hand an address to the opener; answer with `open_done(token, ...)`.
    Open { client: ClientId, token: u64, uri: String },
    /// End this connection (the app sent something that is not D-Bus, or too much).
    Close(ClientId, String),
    /// A line for the log.
    Log(String),
}

fn app<F>(c: ClientId, raw: Vec<u8>) -> Out<F> {
    Out::App(c, Msg { raw, fds: Vec::new() })
}
fn up<F>(c: ClientId, raw: Vec<u8>) -> Out<F> {
    Out::Up(c, Msg { raw, fds: Vec::new() })
}

#[derive(Debug, Clone, PartialEq)]
struct Rule {
    kind: Option<u8>,
    sender: Option<String>,
    interface: Option<String>,
    member: Option<String>,
    path: Option<String>,
    path_namespace: Option<String>,
    destination: Option<String>,
    args: Vec<(usize, String, ArgKind)>,
    /// A key this router does not know: the rule never matches here (the host still gets it).
    unknown: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ArgKind {
    Exact,
    Namespace,
    Path,
}

fn parse_rule(s: &str) -> Option<Rule> {
    if s.len() > MAX_RULE_LEN {
        return None;
    }
    let mut r = Rule { kind: None, sender: None, interface: None, member: None, path: None, path_namespace: None, destination: None, args: Vec::new(), unknown: false };
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let ks = i;
        while i < b.len() && b[i] != b'=' && b[i] != b',' {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' {
            return None;
        }
        let key = &s[ks..i];
        i += 1;
        let mut val = String::new();
        // A value is plain text up to a comma, or quoted with ' in which \' is a quote (the format of the daemon's rules).
        let mut quoted = false;
        while i < b.len() {
            let c = b[i];
            if c == b'\'' {
                quoted = !quoted;
            } else if c == b'\\' && quoted && i + 1 < b.len() && b[i + 1] == b'\'' {
                val.push('\'');
                i += 1;
            } else if c == b',' && !quoted {
                break;
            } else {
                val.push(c as char);
            }
            i += 1;
        }
        if quoted {
            return None;
        }
        i += 1; // the comma
        match key {
            "type" => {
                r.kind = match val.as_str() {
                    "signal" => Some(SIGNAL),
                    "method_call" => Some(CALL),
                    "method_return" => Some(RETURN),
                    "error" => Some(ERROR),
                    _ => return None,
                }
            }
            "sender" => r.sender = Some(val),
            "interface" => r.interface = Some(val),
            "member" => r.member = Some(val),
            "path" => r.path = Some(val),
            "path_namespace" => r.path_namespace = Some(val),
            "destination" => r.destination = Some(val),
            "eavesdrop" => {}
            k if k.starts_with("arg") => {
                let rest = &k[3..];
                let (digits, kind) = if let Some(d) = rest.strip_suffix("namespace") {
                    (d, ArgKind::Namespace)
                } else if let Some(d) = rest.strip_suffix("path") {
                    (d, ArgKind::Path)
                } else {
                    (rest, ArgKind::Exact)
                };
                match digits.parse::<usize>() {
                    Ok(n) if n < 64 => r.args.push((n, val, kind)),
                    _ => r.unknown = true,
                }
            }
            _ => r.unknown = true,
        }
    }
    Some(r)
}

#[derive(Debug)]
struct Owner {
    client: ClientId,
    flags: u32,
}

#[derive(Debug, Default)]
struct Client {
    unique: Option<String>,
    hello: Option<u32>,
    rules: Vec<Rule>,
    names: HashSet<String>,
    serial: u32,
    /// Serials of calls the router made itself on behalf of this connection: their replies are not passed to the app.
    swallow: HashSet<u32>,
    /// The call that asks who owns the portal's name, and the answer: answers from the router carry the owner's unique name, as the portal's do (toolkits
    /// subscribe to the portal's signals by its name, which they resolve to the owner).
    owner_query: Option<u32>,
    portal_owner: Option<String>,
    opens: usize,
    /// Serials of the messages sent to the host side. The proxy of Debian 12 and Ubuntu 24.04 (xdg-dbus-proxy 0.1.5) closes a connection whose serials
    /// do not grow, and the router's own messages are mixed with the app's, so every message that goes up gets the next number and the replies are
    /// given back the number the sender used (`calls`: the new number to the old one, for the calls that expect a reply).
    up_serial: u32,
    calls: std::collections::BTreeMap<u32, u32>,
}

/// The most calls waiting for a reply whose numbers are remembered for a connection.
const MAX_CALLS: usize = 4096;

impl Client {
    fn next_serial(&mut self) -> u32 {
        // Serials of the router's own messages start high, so that they do not meet the app's.
        self.serial = self.serial.wrapping_add(1).max(0x4000_0001);
        self.serial
    }
}

pub struct Router<F> {
    cfg: Config,
    lim: Limits,
    clients: HashMap<ClientId, Client>,
    by_unique: HashMap<String, ClientId>,
    names: HashMap<String, VecDeque<Owner>>,
    opens: HashMap<u64, (ClientId, u32, String)>,
    next_client: ClientId,
    next_token: u64,
    _f: std::marker::PhantomData<F>,
}

fn new_message(kind: u8, serial: u32) -> Message {
    Message { kind, serial, ..Default::default() }
}

impl<F> Router<F> {
    pub fn new(cfg: Config) -> Router<F> {
        let lim = Limits { max_message: MAX_MESSAGE, max_header_fields: 4096, ..Limits::default() };
        Router { cfg, lim, clients: HashMap::new(), by_unique: HashMap::new(), names: HashMap::new(), opens: HashMap::new(), next_client: 1, next_token: 1, _f: std::marker::PhantomData }
    }

    pub fn new_client(&mut self) -> ClientId {
        let id = self.next_client;
        self.next_client = self.next_client.wrapping_add(1).max(1);
        self.clients.insert(id, Client::default());
        id
    }

    /// Names that are owned here now (for the tests and the log).
    pub fn local_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.names.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    pub fn unique_of(&self, c: ClientId) -> Option<&str> {
        self.clients.get(&c)?.unique.as_deref()
    }

    // ---- building messages ---------------------------------------------------------------------------------------------

    fn reply_msg(&mut self, to: ClientId, call: &Message, sig: &str, body: Vec<Value>) -> Option<Vec<u8>> {
        let cl = self.clients.get_mut(&to)?;
        let mut m = new_message(RETURN, cl.next_serial());
        m.reply_serial = Some(call.serial);
        m.destination = cl.unique.clone();
        m.sender = Some(call.destination.clone().filter(|d| !d.starts_with(':')).unwrap_or_else(|| BUS.to_string()));
        m.signature = sig.to_string();
        m.body = body;
        encode_message(&m).ok()
    }

    fn error_msg(&mut self, to: ClientId, call: &Message, name: &str, text: &str) -> Option<Vec<u8>> {
        if call.flags & FLAG_NO_REPLY != 0 {
            return None;
        }
        let cl = self.clients.get_mut(&to)?;
        let mut m = new_message(ERROR, cl.next_serial());
        m.error_name = Some(name.to_string());
        m.reply_serial = Some(call.serial);
        m.destination = cl.unique.clone();
        m.sender = Some(BUS.to_string());
        m.signature = "s".into();
        m.body = vec![Value::str(text)];
        encode_message(&m).ok()
    }

    fn bus_signal(&mut self, to: Option<ClientId>, member: &str, sig: &str, body: Vec<Value>) -> Option<Vec<u8>> {
        let mut m = new_message(SIGNAL, 1);
        m.path = Some(BUS_PATH.into());
        m.interface = Some(BUS.into());
        m.member = Some(member.into());
        m.sender = Some(BUS.into());
        m.signature = sig.into();
        m.body = body;
        if let Some(c) = to {
            let cl = self.clients.get_mut(&c)?;
            m.serial = cl.next_serial();
            m.destination = cl.unique.clone();
        }
        encode_message(&m).ok()
    }

    /// A message of the router's own to the host, on behalf of a connection; the reply is not passed to the app.
    #[allow(clippy::too_many_arguments)]
    fn own_call(&mut self, c: ClientId, dest: &str, path: &str, iface: &str, member: &str, sig: &str, body: Vec<Value>) -> Option<Vec<u8>> {
        let cl = self.clients.get_mut(&c)?;
        let serial = cl.next_serial();
        if cl.swallow.len() >= MAX_SWALLOW {
            cl.swallow.clear();
        }
        cl.swallow.insert(serial);
        let mut m = new_message(CALL, serial);
        m.flags = 0;
        m.destination = Some(dest.into());
        m.path = Some(path.into());
        m.interface = Some(iface.into());
        m.member = Some(member.into());
        m.signature = sig.into();
        m.body = body;
        encode_message(&m).ok()
    }

    // ---- delivery --------------------------------------------------------------------------------------------------------

    /// The message with `sender` set, for a connection of the app. `None` if the message cannot be rewritten (big endian).
    fn restamp(h: &Head, raw: &[u8], sender: &str) -> Option<Vec<u8>> {
        if h.big {
            return None;
        }
        let mut m = h.msg.clone();
        m.sender = Some(sender.to_string());
        encode_with_body(&m, &raw[h.body_start..h.body_start + h.body_len]).ok()
    }

    fn rule_matches(&self, rule: &Rule, h: &Head, raw: &[u8], sender: &str) -> bool {
        let m = &h.msg;
        if rule.unknown {
            return false;
        }
        if rule.kind.is_some_and(|k| k != m.kind) {
            return false;
        }
        if let Some(s) = &rule.sender {
            let resolved = self.names.get(s).and_then(|q| q.front()).and_then(|o| self.clients.get(&o.client)).and_then(|c| c.unique.as_deref());
            if s != sender && resolved != Some(sender) {
                return false;
            }
        }
        if rule.interface.as_deref().is_some_and(|i| m.interface.as_deref() != Some(i)) || rule.member.as_deref().is_some_and(|x| m.member.as_deref() != Some(x)) {
            return false;
        }
        if rule.path.as_deref().is_some_and(|p| m.path.as_deref() != Some(p)) {
            return false;
        }
        if let Some(ns) = &rule.path_namespace {
            let p = m.path.as_deref().unwrap_or("");
            if !(p == ns || (ns != "/" && p.starts_with(&format!("{ns}/"))) || ns == "/") {
                return false;
            }
        }
        if rule.destination.as_deref().is_some_and(|d| m.destination.as_deref() != Some(d)) {
            return false;
        }
        if !rule.args.is_empty() {
            let Ok(full) = parse_message(raw, m.unix_fds as usize, &self.lim) else { return false };
            for (n, want, kind) in &rule.args {
                let Some(Value::Str(s)) = full.body.get(*n) else { return false };
                let ok = match kind {
                    ArgKind::Exact => s == want,
                    ArgKind::Namespace => s == want || s.strip_prefix(want.as_str()).is_some_and(|r| r.starts_with('.')),
                    ArgKind::Path => s == want || (s.ends_with('/') && want.starts_with(s.as_str())) || (want.ends_with('/') && s.starts_with(want.as_str())),
                };
                if !ok {
                    return false;
                }
            }
        }
        true
    }

    fn deliver_local(&mut self, from: ClientId, to: ClientId, h: &Head, msg: Msg<F>, outs: &mut Vec<Out<F>>) {
        let Some(sender) = self.clients.get(&from).and_then(|c| c.unique.clone()) else {
            outs.push(Out::Log("a message from a connection that has not said Hello".into()));
            return;
        };
        match Self::restamp(h, &msg.raw, &sender) {
            Some(raw) => outs.push(Out::App(to, Msg { raw, fds: msg.fds })),
            None => outs.push(Out::Close(from, "big endian messages are not delivered between connections".into())),
        }
    }

    // ---- names -------------------------------------------------------------------------------------------------------------

    fn name_changed(&mut self, name: &str, old: &str, new: &str, outs: &mut Vec<Out<F>>) {
        let Some(raw) = self.bus_signal(None, "NameOwnerChanged", "sss", vec![Value::str(name), Value::str(old), Value::str(new)]) else { return };
        let Ok(h) = parse_head(&raw, 0, &self.lim) else { return };
        let ids: Vec<ClientId> = self.clients.keys().copied().collect();
        for id in ids {
            let hit = self.clients.get(&id).is_some_and(|c| c.rules.iter().any(|r| self.rule_matches(r, &h, &raw, BUS)));
            if hit {
                outs.push(app(id, raw.clone()));
            }
        }
    }

    fn acquired(&mut self, c: ClientId, name: &str, outs: &mut Vec<Out<F>>) {
        if let Some(raw) = self.bus_signal(Some(c), "NameAcquired", "s", vec![Value::str(name)]) {
            outs.push(app(c, raw));
        }
    }

    fn lost(&mut self, c: ClientId, name: &str, outs: &mut Vec<Out<F>>) {
        if let Some(raw) = self.bus_signal(Some(c), "NameLost", "s", vec![Value::str(name)]) {
            outs.push(app(c, raw));
        }
    }

    fn unique(&self, c: ClientId) -> String {
        self.clients.get(&c).and_then(|x| x.unique.clone()).unwrap_or_default()
    }

    /// `RequestName`: the reply code.
    fn request_name(&mut self, c: ClientId, name: &str, flags: u32, outs: &mut Vec<Out<F>>) -> u32 {
        let me = self.unique(c);
        let mut replaced: Option<ClientId> = None;
        let q = self.names.entry(name.to_string()).or_default();
        let code = if q.iter().any(|o| o.client == c) {
            4 // already owner, or already in the queue
        } else if q.is_empty() {
            q.push_back(Owner { client: c, flags });
            1
        } else if flags & NAME_REPLACE_EXISTING != 0 && q.front().is_some_and(|o| o.flags & NAME_ALLOW_REPLACEMENT != 0) {
            let old = q.pop_front().expect("checked above");
            replaced = Some(old.client);
            let keep = old.flags & NAME_DO_NOT_QUEUE == 0;
            q.push_front(Owner { client: c, flags });
            if keep {
                q.insert(1, old);
            }
            1
        } else if flags & NAME_DO_NOT_QUEUE != 0 {
            3
        } else {
            q.push_back(Owner { client: c, flags });
            2
        };
        if code == 1 || code == 2 {
            if let Some(cl) = self.clients.get_mut(&c) {
                cl.names.insert(name.to_string());
            }
        }
        if let Some(old) = replaced {
            let still = self.names.get(name).is_some_and(|q| q.iter().any(|o| o.client == old));
            if !still {
                if let Some(cl) = self.clients.get_mut(&old) {
                    cl.names.remove(name);
                }
            }
            self.lost(old, name, outs);
        }
        if code == 1 {
            let old_unique = replaced.map(|p| self.unique(p)).unwrap_or_default();
            self.acquired(c, name, outs);
            self.name_changed(name, &old_unique, &me, outs);
        }
        code
    }

    fn release_name(&mut self, c: ClientId, name: &str, outs: &mut Vec<Out<F>>) -> u32 {
        let Some(q) = self.names.get_mut(name) else { return 2 };
        let Some(pos) = q.iter().position(|o| o.client == c) else { return 3 };
        let was_owner = pos == 0;
        q.remove(pos);
        if let Some(cl) = self.clients.get_mut(&c) {
            cl.names.remove(name);
        }
        let me = self.unique(c);
        if was_owner {
            let next = self.names.get(name).and_then(|q| q.front()).map(|o| o.client);
            self.lost(c, name, outs);
            match next {
                Some(n) => {
                    let nu = self.unique(n);
                    self.acquired(n, name, outs);
                    self.name_changed(name, &me, &nu, outs);
                }
                None => {
                    self.names.remove(name);
                    self.name_changed(name, &me, "", outs);
                }
            }
        }
        1
    }

    fn owner_unique(&self, name: &str) -> Option<String> {
        let o = self.names.get(name)?.front()?;
        self.clients.get(&o.client)?.unique.clone()
    }

    // ---- events ----------------------------------------------------------------------------------------------------------

    /// A connection of the app ended (or the router dropped it): release its names.
    pub fn client_gone(&mut self, c: ClientId) -> Vec<Out<F>> {
        let mut outs = Vec::new();
        let Some(cl) = self.clients.get(&c) else { return outs };
        let names: Vec<String> = cl.names.iter().cloned().collect();
        for n in names {
            self.release_name(c, &n, &mut outs);
        }
        if let Some(cl) = self.clients.remove(&c) {
            if let Some(u) = cl.unique {
                self.by_unique.remove(&u);
            }
        }
        self.opens.retain(|_, (id, _, _)| *id != c);
        outs
    }

    /// A message from the host for this connection (through the proxy).
    pub fn from_up(&mut self, c: ClientId, msg: Msg<F>) -> Vec<Out<F>> {
        let msg = self.give_back_serial(c, msg);
        let outs = self.handle_up(c, msg);
        self.number_up(outs)
    }

    /// A reply from the host side carries the number this router gave to the call: put the number of the sender back.
    fn give_back_serial(&mut self, c: ClientId, mut msg: Msg<F>) -> Msg<F> {
        if !matches!(msg.raw.get(1), Some(&RETURN) | Some(&ERROR)) || msg.raw.first() != Some(&b'l') {
            return msg;
        }
        let (Some(cl), Some(at)) = (self.clients.get_mut(&c), reply_serial_offset(&msg.raw)) else { return msg };
        let Some(bytes) = msg.raw.get(at..at + 4) else { return msg };
        let now = u32::from_le_bytes(bytes.try_into().unwrap());
        if let Some(old) = cl.calls.remove(&now) {
            msg.raw[at..at + 4].copy_from_slice(&old.to_le_bytes());
        }
        msg
    }

    /// Give the messages that go to the host side their numbers, in the order they go.
    fn number_up(&mut self, mut outs: Vec<Out<F>>) -> Vec<Out<F>> {
        for o in outs.iter_mut() {
            let Out::Up(c, m) = o else { continue };
            let Some(cl) = self.clients.get_mut(c) else { continue };
            if m.raw.len() < 16 || m.raw[0] != b'l' {
                continue;
            }
            let old = u32::from_le_bytes(m.raw[8..12].try_into().unwrap());
            cl.up_serial += 1;
            let new = cl.up_serial;
            m.raw[8..12].copy_from_slice(&new.to_le_bytes());
            if m.raw[1] == CALL && m.raw[2] & 1 == 0 {
                cl.calls.insert(new, old);
                if cl.calls.len() > MAX_CALLS {
                    cl.calls.pop_first();
                }
            }
        }
        outs
    }

    fn handle_up(&mut self, c: ClientId, msg: Msg<F>) -> Vec<Out<F>> {
        let mut outs = Vec::new();
        let h = match parse_head(&msg.raw, msg.fds.len(), &self.lim) {
            Ok(h) => h,
            Err(e) => {
                outs.push(Out::Close(c, format!("bad message from the host side: {e:#}")));
                return outs;
            }
        };
        let m = &h.msg;
        let mut first_reply = false;
        {
            let Some(cl) = self.clients.get_mut(&c) else { return outs };
            if let (RETURN | ERROR, Some(rs)) = (m.kind, m.reply_serial) {
                if cl.owner_query == Some(rs) {
                    cl.owner_query = None;
                    if m.kind == RETURN {
                        if let Ok(full) = parse_message(&msg.raw, 0, &self.lim) {
                            if let Some(Value::Str(o)) = full.body.first() {
                                if valid_unique(o) {
                                    cl.portal_owner = Some(o.clone());
                                }
                            }
                        }
                    }
                }
                if cl.swallow.remove(&rs) {
                    if m.kind == ERROR {
                        outs.push(Out::Log(format!("a call of the router failed: {}", m.error_name.as_deref().unwrap_or("?"))));
                    }
                    return outs;
                }
                first_reply = cl.unique.is_none() && cl.hello == Some(rs) && m.kind == RETURN;
            }
        }
        if first_reply {
            if let Ok(full) = parse_message(&msg.raw, 0, &self.lim) {
                if let Some(Value::Str(u)) = full.body.first() {
                    if !valid_unique(u) || self.by_unique.contains_key(u) {
                        outs.push(Out::Close(c, "the host gave this connection a name that cannot be one".into()));
                        return outs;
                    }
                    if let Some(cl) = self.clients.get_mut(&c) {
                        cl.unique = Some(u.clone());
                    }
                    self.by_unique.insert(u.clone(), c);
                    // The identity first, so that the portal knows the connection before the app's first call: the reply to Hello goes to the
                    // app after the call is on its way to the host (the order of `outs`).
                    if let Some(id) = self.cfg.portal_id.clone() {
                        let body = vec![Value::str(&id), Value::Array("{sv}".into(), Vec::new())];
                        if let Some(raw) = self.own_call(c, PORTAL, PORTAL_PATH, REGISTRY_IFACE, "Register", "sa{sv}", body) {
                            outs.push(up(c, raw));
                        }
                    }
                    if self.cfg.open_uri {
                        if let Some(raw) = self.own_call(c, BUS, BUS_PATH, BUS, "GetNameOwner", "s", vec![Value::str(PORTAL)]) {
                            if let (Some(cl), Ok(h)) = (self.clients.get_mut(&c), parse_head(&raw, 0, &self.lim)) {
                                cl.owner_query = Some(h.msg.serial);
                            }
                            outs.push(up(c, raw));
                        }
                    }
                }
            }
        }
        outs.push(Out::App(c, msg));
        outs
    }

    /// A message from the app on this connection.
    pub fn from_app(&mut self, c: ClientId, msg: Msg<F>) -> Vec<Out<F>> {
        let outs = self.handle_app(c, msg);
        self.number_up(outs)
    }

    fn handle_app(&mut self, c: ClientId, msg: Msg<F>) -> Vec<Out<F>> {
        let mut outs = Vec::new();
        if !self.clients.contains_key(&c) {
            return outs;
        }
        let h = match parse_head(&msg.raw, msg.fds.len(), &self.lim) {
            Ok(h) => h,
            Err(e) => {
                outs.push(Out::Close(c, format!("bad message from the app: {e:#}")));
                return outs;
            }
        };
        if h.big {
            outs.push(Out::Close(c, "big endian messages are not supported".into()));
            return outs;
        }
        let m = h.msg.clone();
        let dest = m.destination.clone();
        match m.kind {
            CALL => {
                let iface = m.interface.clone().unwrap_or_default();
                if iface == REGISTRY_IFACE {
                    if let Some(raw) = self.error_msg(c, &m, "org.freedesktop.DBus.Error.AccessDenied", "the identity is registered by the runtime, not by the app") {
                        outs.push(app(c, raw));
                    }
                    outs.push(Out::Log("the app called the host registry: refused".into()));
                    return outs;
                }
                if iface == OPENURI_IFACE && self.cfg.open_uri {
                    self.open_uri(c, &h, &msg, &mut outs);
                    return outs;
                }
                // GTK reads the version of the OpenURI interface before it uses the portal (without it, a click on a link does nothing). The router answers
                // for that one interface; every other interface stays with the host (where the rules refuse it), so the file chooser is not reached.
                if iface == "org.freedesktop.DBus.Properties" && self.cfg.open_uri && self.is_portal(c, dest.as_deref()) && m.path.as_deref() == Some(PORTAL_PATH) {
                    if let Some(raw) = self.openuri_properties(c, &m, &msg) {
                        outs.push(app(c, raw));
                        return outs;
                    }
                }
                if dest.as_deref() == Some(BUS) {
                    self.bus_call(c, &h, msg, &mut outs);
                    return outs;
                }
            }
            SIGNAL if dest.is_none() => {
                // A broadcast: the matching rules of the connections of this app, then the host (which filters).
                if msg.fds.is_empty() {
                    let sender = self.unique(c);
                    let ids: Vec<ClientId> = self.clients.keys().copied().collect();
                    for id in ids {
                        let hit = self.clients.get(&id).is_some_and(|x| x.rules.iter().any(|r| self.rule_matches(r, &h, &msg.raw, &sender)));
                        if hit {
                            if let Some(raw) = Self::restamp(&h, &msg.raw, &sender) {
                                outs.push(app(id, raw));
                            }
                        }
                    }
                }
                outs.push(Out::Up(c, msg));
                return outs;
            }
            _ => {}
        }
        // Calls, replies and errors, and signals with a destination: to a name or a connection of this app, or to the host.
        if let Some(d) = dest {
            let target = if d.starts_with(':') { self.by_unique.get(&d).copied() } else { self.names.get(&d).and_then(|q| q.front()).map(|o| o.client) };
            if let Some(t) = target {
                if self.clients.contains_key(&t) {
                    self.deliver_local(c, t, &h, msg, &mut outs);
                    return outs;
                }
            }
            if m.kind == CALL && !d.starts_with(':') && self.names.contains_key(&d) {
                // A name that was owned here a moment ago (the queue is empty or being emptied): not the host's business either.
                if let Some(raw) = self.error_msg(c, &m, "org.freedesktop.DBus.Error.ServiceUnknown", &format!("the name {d} has no owner")) {
                    outs.push(app(c, raw));
                }
                return outs;
            }
        }
        outs.push(Out::Up(c, msg));
        outs
    }

    // ---- calls to the daemon -------------------------------------------------------------------------------------------

    /// A call to the daemon: answered here, or passed on (then `outs` has the `Up`).
    fn bus_call(&mut self, c: ClientId, h: &Head, msg: Msg<F>, outs: &mut Vec<Out<F>>) {
        let m = &h.msg;
        let member = m.member.as_deref().unwrap_or("");
        let full = match parse_message(&msg.raw, msg.fds.len(), &self.lim) {
            Ok(f) => f,
            Err(e) => {
                outs.push(Out::Close(c, format!("bad call to the bus: {e:#}")));
                return;
            }
        };
        let s = |i: usize| match full.body.get(i) {
            Some(Value::Str(x)) => Some(x.clone()),
            _ => None,
        };
        let u = |i: usize| match full.body.get(i) {
            Some(Value::U32(x)) => Some(*x),
            _ => None,
        };
        match member {
            "Hello" => {
                if let Some(cl) = self.clients.get_mut(&c) {
                    if cl.hello.is_none() {
                        cl.hello = Some(m.serial);
                    }
                }
                outs.push(Out::Up(c, msg));
            }
            "RequestName" => {
                let (Some(name), Some(flags)) = (s(0), u(1)) else {
                    if let Some(raw) = self.error_msg(c, m, "org.freedesktop.DBus.Error.InvalidArgs", "RequestName takes a name and flags") {
                        outs.push(app(c, raw));
                    }
                    return;
                };
                let local_count = self.clients.get(&c).map_or(0, |x| x.names.len());
                if name.starts_with(':') || !valid_dotted_name(&name, true) || (local_count >= MAX_NAMES_PER_CLIENT && !self.names.contains_key(&name)) {
                    if let Some(raw) = self.error_msg(c, m, "org.freedesktop.DBus.Error.InvalidArgs", "not a name this bus can give") {
                        outs.push(app(c, raw));
                    }
                    return;
                }
                let code = self.request_name(c, &name, flags, outs);
                if code == 1 && name.starts_with(MPRIS_PREFIX) {
                    if let Some(alias) = self.cfg.mpris_alias.clone() {
                        if name != alias {
                            if let Some(raw) = self.own_call(c, BUS, BUS_PATH, BUS, "RequestName", "su", vec![Value::str(&alias), Value::U32(NAME_DO_NOT_QUEUE)]) {
                                outs.push(up(c, raw));
                            }
                        }
                    }
                }
                if let Some(raw) = self.reply_msg(c, m, "u", vec![Value::U32(code)]) {
                    outs.push(app(c, raw));
                }
            }
            "ReleaseName" => {
                let Some(name) = s(0) else { return self.bad_args(c, m, outs) };
                let code = self.release_name(c, &name, outs);
                if let Some(raw) = self.reply_msg(c, m, "u", vec![Value::U32(code)]) {
                    outs.push(app(c, raw));
                }
            }
            "GetNameOwner" | "NameHasOwner" | "ListQueuedOwners" | "StartServiceByName" => {
                let Some(name) = s(0) else { return self.bad_args(c, m, outs) };
                if self.names.contains_key(&name) {
                    let reply = match member {
                        "GetNameOwner" => self.owner_unique(&name).map(|o| ("s", vec![Value::str(&o)])),
                        "NameHasOwner" => Some(("b", vec![Value::Bool(self.owner_unique(&name).is_some())])),
                        "StartServiceByName" => Some(("u", vec![Value::U32(1)])), // already running
                        _ => {
                            let list: Vec<Value> = self.names.get(&name).map(|q| q.iter().filter_map(|o| self.clients.get(&o.client).and_then(|x| x.unique.clone())).map(|u| Value::str(&u)).collect()).unwrap_or_default();
                            Some(("as", vec![Value::Array("s".into(), list)]))
                        }
                    };
                    match reply {
                        Some((sig, body)) => {
                            if let Some(raw) = self.reply_msg(c, m, sig, body) {
                                outs.push(app(c, raw));
                            }
                        }
                        None => {
                            if let Some(raw) = self.error_msg(c, m, "org.freedesktop.DBus.Error.NameHasNoOwner", "no owner") {
                                outs.push(app(c, raw));
                            }
                        }
                    }
                } else {
                    outs.push(Out::Up(c, msg));
                }
            }
            "AddMatch" => {
                let Some(text) = s(0) else { return self.bad_args(c, m, outs) };
                if let Some(rule) = parse_rule(&text) {
                    if let Some(cl) = self.clients.get_mut(&c) {
                        if cl.rules.len() < MAX_RULES_PER_CLIENT {
                            cl.rules.push(rule);
                        }
                    }
                }
                outs.push(Out::Up(c, msg));
            }
            "RemoveMatch" => {
                if let Some(rule) = s(0).and_then(|t| parse_rule(&t)) {
                    if let Some(cl) = self.clients.get_mut(&c) {
                        if let Some(p) = cl.rules.iter().position(|r| *r == rule) {
                            cl.rules.remove(p);
                        }
                    }
                }
                outs.push(Out::Up(c, msg));
            }
            _ => outs.push(Out::Up(c, msg)),
        }
    }

    fn bad_args(&mut self, c: ClientId, m: &Message, outs: &mut Vec<Out<F>>) {
        if let Some(raw) = self.error_msg(c, m, "org.freedesktop.DBus.Error.InvalidArgs", "wrong arguments") {
            outs.push(app(c, raw));
        }
    }

    // ---- OpenURI ---------------------------------------------------------------------------------------------------------

    fn open_uri(&mut self, c: ClientId, h: &Head, msg: &Msg<F>, outs: &mut Vec<Out<F>>) {
        let m = &h.msg;
        if m.member.as_deref() != Some("OpenURI") {
            if let Some(raw) = self.error_msg(c, m, "org.freedesktop.DBus.Error.NotSupported", "this runtime opens addresses (http, https, mailto) only") {
                outs.push(app(c, raw));
            }
            return;
        }
        let full = parse_message(&msg.raw, msg.fds.len(), &self.lim);
        let (uri, token) = match &full {
            Ok(f) if f.signature == "ssa{sv}" => {
                let Value::Str(uri) = &f.body[1] else { return };
                let mut tok = None;
                if let Value::Array(_, items) = &f.body[2] {
                    for it in items {
                        if let Value::Entry(k, v) = it {
                            if let (Value::Str(k), Value::Variant(v)) = (&**k, &**v) {
                                if k == "handle_token" {
                                    if let Value::Str(t) = &**v {
                                        tok = Some(t.clone());
                                    }
                                }
                            }
                        }
                    }
                }
                (uri.clone(), tok)
            }
            _ => {
                if let Some(raw) = self.error_msg(c, m, "org.freedesktop.DBus.Error.InvalidArgs", "OpenURI takes (parent_window, uri, options)") {
                    outs.push(app(c, raw));
                }
                return;
            }
        };
        let busy = self.clients.get(&c).is_none_or(|x| x.opens >= MAX_OPENS_PER_CLIENT);
        if busy {
            if let Some(raw) = self.error_msg(c, m, "org.freedesktop.DBus.Error.LimitsExceeded", "too many addresses at once") {
                outs.push(app(c, raw));
            }
            return;
        }
        let unique = self.unique(c);
        // The handle the portal would give: the connection's name with `.` as `_`, and the app's token (or one of ours).
        let token_s = token.filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')).unwrap_or_else(|| format!("panpak{}", m.serial));
        let handle = format!("/org/freedesktop/portal/desktop/request/{}/{}", unique.trim_start_matches(':').replace('.', "_"), token_s);
        let t = self.next_token;
        self.next_token += 1;
        self.opens.insert(t, (c, m.serial, handle));
        if let Some(cl) = self.clients.get_mut(&c) {
            cl.opens += 1;
        }
        outs.push(Out::Open { client: c, token: t, uri });
    }

    /// The opener answered. `Ok` means the address was handed to the system; `Err(text)` says why not.
    /// The destination is the portal: its well-known name, or the unique name of its owner (a GDBusProxy resolves the name once and then uses the owner).
    fn is_portal(&self, c: ClientId, dest: Option<&str>) -> bool {
        match dest {
            Some(PORTAL) => true,
            Some(d) => self.clients.get(&c).and_then(|cl| cl.portal_owner.as_deref()) == Some(d),
            None => false,
        }
    }

    /// `Properties.Get` and `GetAll` of `org.freedesktop.portal.OpenURI`: `version` is 3. None for anything else.
    fn openuri_properties(&mut self, c: ClientId, m: &Message, msg: &Msg<F>) -> Option<Vec<u8>> {
        let member = m.member.as_deref()?;
        if member != "Get" && member != "GetAll" {
            return None;
        }
        let full = parse_message(&msg.raw, msg.fds.len(), &self.lim).ok()?;
        let Some(Value::Str(i)) = full.body.first() else { return None };
        if i != OPENURI_IFACE || (member == "Get" && !matches!(full.body.get(1), Some(Value::Str(p)) if p == "version")) {
            return None;
        }
        let version = Value::Variant(Box::new(Value::U32(3)));
        let (sig, body) = if member == "Get" {
            ("v", vec![version])
        } else {
            ("a{sv}", vec![Value::Array("{sv}".into(), vec![Value::Entry(Box::new(Value::Str("version".into())), Box::new(version))])])
        };
        let cl = self.clients.get_mut(&c)?;
        let mut r = new_message(RETURN, cl.next_serial());
        r.reply_serial = Some(m.serial);
        r.destination = cl.unique.clone();
        r.sender = Some(cl.portal_owner.clone().unwrap_or_else(|| PORTAL.to_string()));
        r.signature = sig.into();
        r.body = body;
        encode_message(&r).ok()
    }

    pub fn open_done(&mut self, token: u64, result: Result<(), String>) -> Vec<Out<F>> {
        let outs = self.open_done_inner(token, result);
        self.number_up(outs)
    }

    fn open_done_inner(&mut self, token: u64, result: Result<(), String>) -> Vec<Out<F>> {
        let mut outs = Vec::new();
        let Some((c, serial, handle)) = self.opens.remove(&token) else { return outs };
        let Some(cl) = self.clients.get_mut(&c) else { return outs };
        cl.opens = cl.opens.saturating_sub(1);
        let unique = cl.unique.clone();
        let from = cl.portal_owner.clone().unwrap_or_else(|| PORTAL.to_string());
        // The method return: the handle of the request.
        let mut r = new_message(RETURN, cl.next_serial());
        r.reply_serial = Some(serial);
        r.destination = unique.clone();
        r.sender = Some(from.clone());
        r.signature = "o".into();
        r.body = vec![Value::Path(handle.clone())];
        if let Ok(raw) = encode_message(&r) {
            outs.push(app(c, raw));
        }
        // The answer: 0 done, 2 ended in some other way; the reason goes to the log of the runtime (the portal has no field for it).
        let mut s = new_message(SIGNAL, cl.next_serial());
        s.path = Some(handle);
        s.interface = Some("org.freedesktop.portal.Request".into());
        s.member = Some("Response".into());
        s.destination = unique;
        s.sender = Some(from);
        s.signature = "ua{sv}".into();
        s.body = vec![Value::U32(if result.is_ok() { 0 } else { 2 }), Value::Array("{sv}".into(), Vec::new())];
        if let Ok(raw) = encode_message(&s) {
            outs.push(app(c, raw));
        }
        if let Err(e) = result {
            outs.push(Out::Log(format!("OpenURI refused: {e}")));
        }
        outs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type R = Router<()>;

    /// The messages with the serial field set to 0: the router gives its own numbers to what goes to the host side.
    fn unnumbered(v: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        v.into_iter().map(|mut m| { m[8..12].fill(0); m }).collect()
    }

    fn msg(raw: Vec<u8>) -> Msg<()> {
        Msg { raw, fds: Vec::new() }
    }

    fn call(serial: u32, dest: &str, path: &str, iface: &str, member: &str, sig: &str, body: Vec<Value>) -> Vec<u8> {
        let mut m = new_message(CALL, serial);
        m.destination = Some(dest.into());
        m.path = Some(path.into());
        m.interface = Some(iface.into());
        m.member = Some(member.into());
        m.signature = sig.into();
        m.body = body;
        encode_message(&m).unwrap()
    }

    fn bus_call(serial: u32, member: &str, sig: &str, body: Vec<Value>) -> Vec<u8> {
        call(serial, BUS, BUS_PATH, BUS, member, sig, body)
    }

    fn reply_to(serial: u32, dest: &str, sig: &str, body: Vec<Value>) -> Vec<u8> {
        let mut m = new_message(RETURN, 77);
        m.reply_serial = Some(serial);
        m.destination = Some(dest.into());
        m.sender = Some(BUS.into());
        m.signature = sig.into();
        m.body = body;
        encode_message(&m).unwrap()
    }

    fn signal(serial: u32, path: &str, iface: &str, member: &str, sig: &str, body: Vec<Value>) -> Vec<u8> {
        let mut m = new_message(SIGNAL, serial);
        m.path = Some(path.into());
        m.interface = Some(iface.into());
        m.member = Some(member.into());
        m.signature = sig.into();
        m.body = body;
        encode_message(&m).unwrap()
    }

    fn parsed(raw: &[u8]) -> Message {
        parse_message(raw, 0, &Limits::default()).unwrap()
    }

    fn apps(outs: &[Out<()>], c: ClientId) -> Vec<Message> {
        outs.iter().filter_map(|o| if let Out::App(id, m) = o { (*id == c).then(|| parsed(&m.raw)) } else { None }).collect()
    }

    fn ups(outs: &[Out<()>], c: ClientId) -> Vec<Vec<u8>> {
        outs.iter().filter_map(|o| if let Out::Up(id, m) = o { (*id == c).then(|| m.raw.clone()) } else { None }).collect()
    }

    fn config() -> Config {
        Config { portal_id: Some("panpak.org.example.App".into()), mpris_alias: Some(mpris_alias("org.example.App")), open_uri: true }
    }

    /// A connection that said Hello and got `:1.<n>`.
    fn connect(r: &mut R, n: u32) -> ClientId {
        let c = r.new_client();
        let outs = r.from_app(c, msg(bus_call(1, "Hello", "", vec![])));
        assert_eq!(ups(&outs, c).len(), 1);
        let outs = r.from_up(c, msg(reply_to(1, &format!(":1.{n}"), "s", vec![Value::str(&format!(":1.{n}"))])));
        assert!(!apps(&outs, c).is_empty());
        // The router asked who owns the portal; the host answers (it is not for the app).
        for raw in ups(&outs, c) {
            let m = parsed(&raw);
            if m.member.as_deref() == Some("GetNameOwner") {
                assert!(apps(&r.from_up(c, msg(reply_to(m.serial, &format!(":1.{n}"), "s", vec![Value::str(":1.74")]))), c).is_empty());
            }
        }
        assert_eq!(r.unique_of(c), Some(format!(":1.{n}").as_str()));
        c
    }

    fn request(r: &mut R, c: ClientId, serial: u32, name: &str, flags: u32) -> (u32, Vec<Out<()>>) {
        let outs = r.from_app(c, msg(bus_call(serial, "RequestName", "su", vec![Value::str(name), Value::U32(flags)])));
        let code = apps(&outs, c).iter().find(|m| m.kind == RETURN && m.reply_serial == Some(serial)).map(|m| match m.body[0] {
            Value::U32(x) => x,
            _ => 0,
        });
        (code.expect("a reply"), outs)
    }

    #[test]
    fn hello_registers_the_identity_before_the_app_goes_on() {
        let mut r = R::new(config());
        let c = r.new_client();
        let hello = bus_call(1, "Hello", "", vec![]);
        let outs = r.from_app(c, msg(hello.clone()));
        assert_eq!(ups(&outs, c), vec![hello], "Hello goes to the host unchanged");
        let outs = r.from_up(c, msg(reply_to(1, ":1.9", "s", vec![Value::str(":1.9")])));
        // First the calls of the router (the identity, and who owns the portal), then the reply to the app.
        assert!(matches!(outs[0], Out::Up(..)) && matches!(outs[1], Out::Up(..)) && matches!(outs[2], Out::App(..)), "{outs:?}");
        let reg = parsed(&ups(&outs, c)[0]);
        assert_eq!((reg.destination.as_deref(), reg.interface.as_deref(), reg.member.as_deref()), (Some(PORTAL), Some(REGISTRY_IFACE), Some("Register")));
        assert_eq!(reg.body[0], Value::str("panpak.org.example.App"));
        // Its reply is not for the app; an error is only logged.
        let rs = reg.serial;
        let outs = r.from_up(c, msg(reply_to(rs, ":1.9", "", vec![])));
        assert!(apps(&outs, c).is_empty());
    }

    #[test]
    fn a_message_that_is_not_ours_is_passed_on_byte_for_byte() {
        let mut r = R::new(config());
        let c = connect(&mut r, 5);
        let raw = call(9, PORTAL, PORTAL_PATH, "org.freedesktop.portal.Settings", "ReadOne", "ss", vec![Value::str("org.freedesktop.appearance"), Value::str("color-scheme")]);
        let outs = r.from_app(c, msg(raw.clone()));
        assert_eq!(unnumbered(ups(&outs, c)), unnumbered(vec![raw]));
        assert_eq!(outs.len(), 1);
        // And what comes back is passed to the app unchanged.
        let back = reply_to(9, ":1.5", "s", vec![Value::str("x")]);
        let outs = r.from_up(c, msg(back.clone()));
        assert!(matches!(&outs[0], Out::App(id, m) if *id == c && m.raw == back));
    }

    #[test]
    fn the_app_cannot_register_an_identity_itself() {
        let mut r = R::new(config());
        let c = connect(&mut r, 5);
        for dest in [PORTAL, ":1.74"] {
            let raw = call(12, dest, PORTAL_PATH, REGISTRY_IFACE, "Register", "sa{sv}", vec![Value::str("org.mozilla.firefox"), Value::Array("{sv}".into(), vec![])]);
            let outs = r.from_app(c, msg(raw));
            assert!(ups(&outs, c).is_empty(), "the call reached the host: {outs:?}");
            let e = &apps(&outs, c)[0];
            assert_eq!((e.kind, e.error_name.as_deref()), (ERROR, Some("org.freedesktop.DBus.Error.AccessDenied")));
        }
    }

    #[test]
    fn a_name_of_the_app_stays_in_the_app() {
        let mut r = R::new(config());
        let c = connect(&mut r, 5);
        let (code, outs) = request(&mut r, c, 2, "com.example.Other", NAME_DO_NOT_QUEUE);
        assert_eq!(code, 1);
        assert!(ups(&outs, c).is_empty(), "a name must not be asked from the host: {outs:?}");
        assert_eq!(r.local_names(), vec!["com.example.Other"]);
        // Who owns it: answered here. Another name: the host is asked.
        let outs = r.from_app(c, msg(bus_call(3, "GetNameOwner", "s", vec![Value::str("com.example.Other")])));
        assert_eq!(apps(&outs, c)[0].body[0], Value::str(":1.5"));
        let raw = bus_call(4, "GetNameOwner", "s", vec![Value::str("com.example.Foreign")]);
        let outs = r.from_app(c, msg(raw.clone()));
        assert_eq!(ups(&outs, c), vec![raw]);
        // A call to a name of the app is never sent to the host, even from another connection of the app.
        let d = connect(&mut r, 6);
        let outs = r.from_app(d, msg(call(5, "com.example.Other", "/x", "com.example.I", "Ping", "", vec![])));
        assert!(ups(&outs, d).is_empty());
        let delivered = apps(&outs, c);
        assert_eq!(delivered.len(), 1);
        assert_eq!((delivered[0].sender.as_deref(), delivered[0].member.as_deref(), delivered[0].serial), (Some(":1.6"), Some("Ping"), 5));
        // The answer goes back to the caller by its unique name.
        let outs = r.from_app(c, msg(reply_to(5, ":1.6", "", vec![])));
        assert!(ups(&outs, c).is_empty());
        assert_eq!(apps(&outs, d)[0].sender.as_deref(), Some(":1.5"));
    }

    #[test]
    fn a_name_is_taken_once_and_queued_and_released() {
        let mut r = R::new(config());
        let (a, b) = (connect(&mut r, 5), connect(&mut r, 6));
        assert_eq!(request(&mut r, a, 2, "org.example.App", 0).0, 1);
        assert_eq!(request(&mut r, b, 2, "org.example.App", NAME_DO_NOT_QUEUE).0, 3, "single instance: the second copy is refused");
        assert_eq!(request(&mut r, a, 3, "org.example.App", 0).0, 4);
        assert_eq!(request(&mut r, b, 3, "org.example.App", 0).0, 2, "queued");
        // The owner goes: the next one gets the name.
        let outs = r.client_gone(a);
        let got = apps(&outs, b);
        assert!(got.iter().any(|m| m.member.as_deref() == Some("NameAcquired") && m.body[0] == Value::str("org.example.App")), "{got:?}");
        let outs = r.from_app(b, msg(bus_call(4, "GetNameOwner", "s", vec![Value::str("org.example.App")])));
        assert_eq!(apps(&outs, b)[0].body[0], Value::str(":1.6"));
        // The last one goes: the name is free and nothing is left.
        r.client_gone(b);
        assert!(r.local_names().is_empty());
        assert_eq!(r.client_count(), 0);
    }

    #[test]
    fn replacing_an_owner_that_allows_it() {
        let mut r = R::new(config());
        let (a, b) = (connect(&mut r, 5), connect(&mut r, 6));
        assert_eq!(request(&mut r, a, 2, "org.example.R", NAME_ALLOW_REPLACEMENT).0, 1);
        let (code, outs) = request(&mut r, b, 2, "org.example.R", NAME_REPLACE_EXISTING);
        assert_eq!(code, 1);
        assert!(apps(&outs, a).iter().any(|m| m.member.as_deref() == Some("NameLost")));
        // The old owner stays in the queue (it did not say do-not-queue).
        let outs = r.client_gone(b);
        assert!(apps(&outs, a).iter().any(|m| m.member.as_deref() == Some("NameAcquired")));
        // Without the permission to be replaced: refused, queued or not.
        let (c, d) = (connect(&mut r, 7), connect(&mut r, 8));
        assert_eq!(request(&mut r, c, 2, "org.example.S", 0).0, 1);
        assert_eq!(request(&mut r, d, 2, "org.example.S", NAME_REPLACE_EXISTING | NAME_DO_NOT_QUEUE).0, 3);
    }

    #[test]
    fn match_rules_decide_what_other_connections_of_the_app_get() {
        let mut r = R::new(config());
        let (a, b) = (connect(&mut r, 5), connect(&mut r, 6));
        let add = bus_call(2, "AddMatch", "s", vec![Value::str("type='signal',interface='com.example.I',member='Changed',arg0='yes'")]);
        let outs = r.from_app(b, msg(add.clone()));
        assert_eq!(unnumbered(ups(&outs, b)), unnumbered(vec![add]), "the host gets the rule too");
        let hit = signal(3, "/x", "com.example.I", "Changed", "s", vec![Value::str("yes")]);
        let miss = signal(4, "/x", "com.example.I", "Changed", "s", vec![Value::str("no")]);
        let other = signal(5, "/x", "com.example.J", "Changed", "s", vec![Value::str("yes")]);
        let outs = r.from_app(a, msg(hit.clone()));
        assert_eq!(apps(&outs, b).len(), 1);
        assert_eq!(apps(&outs, b)[0].sender.as_deref(), Some(":1.5"));
        assert_eq!(unnumbered(ups(&outs, a)), unnumbered(vec![hit]), "a broadcast also goes to the host, which filters it");
        for raw in [miss, other] {
            let outs = r.from_app(a, msg(raw));
            assert!(apps(&outs, b).is_empty());
        }
        // A rule that was removed does not match any more.
        r.from_app(b, msg(bus_call(6, "RemoveMatch", "s", vec![Value::str("type='signal',interface='com.example.I',member='Changed',arg0='yes'")])));
        let outs = r.from_app(a, msg(signal(7, "/x", "com.example.I", "Changed", "s", vec![Value::str("yes")])));
        assert!(apps(&outs, b).is_empty());
    }

    #[test]
    fn name_changes_reach_the_connections_that_watch_for_them() {
        let mut r = R::new(config());
        let (a, b) = (connect(&mut r, 5), connect(&mut r, 6));
        r.from_app(b, msg(bus_call(2, "AddMatch", "s", vec![Value::str("type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0namespace='org.example'")])));
        let (_, outs) = request(&mut r, a, 2, "org.example.App", 0);
        let seen: Vec<Message> = apps(&outs, b).into_iter().filter(|m| m.member.as_deref() == Some("NameOwnerChanged")).collect();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].body, vec![Value::str("org.example.App"), Value::str(""), Value::str(":1.5")]);
        let (_, outs) = request(&mut r, a, 3, "com.other.X", 0);
        assert!(apps(&outs, b).iter().all(|m| m.member.as_deref() != Some("NameOwnerChanged")), "outside the namespace");
        let outs = r.client_gone(a);
        assert!(apps(&outs, b).iter().any(|m| m.member.as_deref() == Some("NameOwnerChanged") && m.body[2] == Value::str("")));
    }

    #[test]
    fn a_media_player_name_gets_an_alias_on_the_host() {
        let mut r = R::new(config());
        let c = connect(&mut r, 5);
        let (code, outs) = request(&mut r, c, 2, "org.mpris.MediaPlayer2.vlc", NAME_DO_NOT_QUEUE);
        assert_eq!(code, 1);
        let u = ups(&outs, c);
        assert_eq!(u.len(), 1, "one call to the host");
        let m = parsed(&u[0]);
        assert_eq!((m.destination.as_deref(), m.member.as_deref()), (Some(BUS), Some("RequestName")));
        assert_eq!(m.body[0], Value::str("org.mpris.MediaPlayer2.panpak.org_example_App"));
        assert_eq!(m.body[1], Value::U32(NAME_DO_NOT_QUEUE));
        // The answer from the host is not for the app.
        let outs = r.from_up(c, msg(reply_to(m.serial, ":1.5", "u", vec![Value::U32(1)])));
        assert!(apps(&outs, c).is_empty());
        // A second name of the same family, or a name of another family: no second alias / none.
        let (_, outs) = request(&mut r, c, 3, "com.example.Plain", 0);
        assert!(ups(&outs, c).is_empty());
        // Without the permission there is no alias.
        let mut r2 = R::new(Config { mpris_alias: None, ..config() });
        let c2 = connect(&mut r2, 5);
        let (_, outs) = request(&mut r2, c2, 2, "org.mpris.MediaPlayer2.vlc", 0);
        assert!(ups(&outs, c2).is_empty());
    }

    #[test]
    fn serials_toward_the_host_grow_and_replies_get_the_number_of_the_sender() {
        // xdg-dbus-proxy 0.1.5 closes a connection whose serials do not grow; the router's own messages (the identity, the owner of the portal) come between the app's.
        let mut r = R::new(config());
        let c = connect(&mut r, 5);
        let mut last = 0;
        let mut ups_seen = Vec::new();
        for (serial, member) in [(2u32, "Ping"), (3, "Ping"), (9, "Ping")] {
            let raw = call(serial, "org.example.Other", "/", "org.freedesktop.DBus.Peer", member, "", vec![]);
            for raw in ups(&r.from_app(c, msg(raw)), c) {
                let m = parsed(&raw);
                assert!(m.serial > last, "serial {} after {last}", m.serial);
                last = m.serial;
                ups_seen.push((serial, m.serial));
            }
        }
        // The reply to the call the app numbered 9 comes back with the router's number and reaches the app with 9.
        let (_, up9) = *ups_seen.last().unwrap();
        let outs = r.from_up(c, msg(reply_to(up9, ":1.5", "", vec![])));
        let got = apps(&outs, c);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].reply_serial, Some(9));
    }

    #[test]
    fn the_version_of_openuri_is_answered_and_nothing_else_of_the_portal() {
        let mut r = R::new(config());
        let c = connect(&mut r, 5);
        for (member, sig, args) in [("Get", "ss", vec![Value::str(OPENURI_IFACE), Value::str("version")]), ("GetAll", "s", vec![Value::str(OPENURI_IFACE)])] {
            let raw = call(30, PORTAL, PORTAL_PATH, "org.freedesktop.DBus.Properties", member, sig, args);
            let outs = r.from_app(c, msg(raw));
            assert!(outs.iter().all(|o| !matches!(o, Out::Up(..))), "{member}: {outs:?}");
            assert_eq!(apps(&outs, c).len(), 1, "{member}");
        }
        // Another interface, or another property: to the host (where it is refused).
        for args in [vec![Value::str("org.freedesktop.portal.FileChooser"), Value::str("version")], vec![Value::str(OPENURI_IFACE), Value::str("other")]] {
            let raw = call(31, PORTAL, PORTAL_PATH, "org.freedesktop.DBus.Properties", "Get", "ss", args);
            let outs = r.from_app(c, msg(raw));
            assert!(outs.iter().any(|o| matches!(o, Out::Up(..))), "{outs:?}");
        }
    }

    #[test]
    fn links_go_to_the_opener_and_the_app_is_told_the_truth() {
        let mut r = R::new(config());
        let c = connect(&mut r, 5);
        let opts = Value::Array("{sv}".into(), vec![Value::Entry(Box::new(Value::str("handle_token")), Box::new(Value::variant(Value::str("tk1"))))]);
        let raw = call(20, PORTAL, PORTAL_PATH, OPENURI_IFACE, "OpenURI", "ssa{sv}", vec![Value::str(""), Value::str("https://example.org/a"), opts.clone()]);
        let outs = r.from_app(c, msg(raw));
        assert!(ups(&outs, c).is_empty(), "OpenURI is not the portal's here");
        let Some(Out::Open { token, uri, client }) = outs.into_iter().next() else { panic!("no Open") };
        assert_eq!((client, uri.as_str()), (c, "https://example.org/a"));
        let outs = r.open_done(token, Ok(()));
        let got = apps(&outs, c);
        assert_eq!(got[0].kind, RETURN);
        assert_eq!(got[0].sender.as_deref(), Some(":1.74"), "answers carry the unique name of the portal's owner");
        assert_eq!(got[1].sender.as_deref(), Some(":1.74"));
        assert_eq!(got[0].body[0], Value::Path("/org/freedesktop/portal/desktop/request/1_5/tk1".into()));
        assert_eq!((got[1].member.as_deref(), got[1].path.as_deref()), (Some("Response"), Some("/org/freedesktop/portal/desktop/request/1_5/tk1")));
        assert_eq!(got[1].body[0], Value::U32(0));
        // A refusal: the code is 2 and the reason is logged.
        let raw = call(21, PORTAL, PORTAL_PATH, OPENURI_IFACE, "OpenURI", "ssa{sv}", vec![Value::str(""), Value::str("file:///etc/passwd"), opts]);
        let outs = r.from_app(c, msg(raw));
        let Some(Out::Open { token, .. }) = outs.into_iter().next() else { panic!() };
        let outs = r.open_done(token, Err("scheme file is not allowed".into()));
        assert_eq!(apps(&outs, c)[1].body[0], Value::U32(2));
        assert!(outs.iter().any(|o| matches!(o, Out::Log(l) if l.contains("not allowed"))));
        // The other calls of the interface are not served.
        let raw = call(22, PORTAL, PORTAL_PATH, OPENURI_IFACE, "OpenFile", "sha{sv}", vec![Value::str(""), Value::Fd(0), Value::Array("{sv}".into(), vec![])]);
        let mut x = msg(raw);
        x.fds.push(());
        let outs = r.from_app(c, x);
        assert_eq!(apps(&outs, c)[0].error_name.as_deref(), Some("org.freedesktop.DBus.Error.NotSupported"));
        // With OpenURI not served by the router the call goes to the host unchanged.
        let mut r2 = R::new(Config { open_uri: false, ..config() });
        let c2 = connect(&mut r2, 5);
        let raw = call(23, PORTAL, PORTAL_PATH, OPENURI_IFACE, "OpenURI", "ssa{sv}", vec![Value::str(""), Value::str("https://example.org"), Value::Array("{sv}".into(), vec![])]);
        assert_eq!(unnumbered(ups(&r2.from_app(c2, msg(raw.clone())), c2)), unnumbered(vec![raw]));
    }

    #[test]
    fn a_message_that_is_not_dbus_ends_the_connection() {
        let mut r = R::new(config());
        let c = connect(&mut r, 5);
        for bad in [vec![0u8; 3], vec![b'x'; 40], {
            let mut v = bus_call(2, "ListNames", "", vec![]);
            v[0] = b'B';
            v
        }] {
            let outs = r.from_app(c, msg(bad));
            assert!(matches!(outs[0], Out::Close(id, _) if id == c), "{outs:?}");
        }
        // Too many names are refused, not granted.
        let d = connect(&mut r, 6);
        for i in 0..MAX_NAMES_PER_CLIENT {
            assert_eq!(request(&mut r, d, 10 + i as u32, &format!("com.example.N{i}"), 0).0, 1);
        }
        let outs = r.from_app(d, msg(bus_call(99, "RequestName", "su", vec![Value::str("com.example.Extra"), Value::U32(0)])));
        assert_eq!(apps(&outs, d)[0].error_name.as_deref(), Some("org.freedesktop.DBus.Error.InvalidArgs"));
        // A unique name cannot be requested.
        let outs = r.from_app(d, msg(bus_call(100, "RequestName", "su", vec![Value::str(":1.99"), Value::U32(0)])));
        assert_eq!(apps(&outs, d)[0].kind, ERROR);
    }

    #[test]
    fn a_unique_name_that_cannot_be_one_is_not_taken() {
        // Found by the fuzzer: the host side answered Hello with a string that is no connection name, and replies built for it did not read.
        let mut r = R::new(config());
        let c = r.new_client();
        r.from_app(c, msg(bus_call(1, "Hello", "", vec![])));
        let outs = r.from_up(c, msg(reply_to(1, "x", "s", vec![Value::str("x")])));
        assert!(matches!(outs[0], Out::Close(id, _) if id == c), "{outs:?}");
        // And two connections cannot have the same name.
        let a = connect(&mut r, 5);
        let b = r.new_client();
        r.from_app(b, msg(bus_call(1, "Hello", "", vec![])));
        let outs = r.from_up(b, msg(reply_to(1, ":1.5", "s", vec![Value::str(":1.5")])));
        assert!(matches!(outs[0], Out::Close(id, _) if id == b));
        assert_eq!(r.unique_of(a), Some(":1.5"));
    }

    #[test]
    fn a_message_for_an_unknown_connection_goes_to_the_host() {
        let mut r = R::new(config());
        let c = connect(&mut r, 5);
        // The host may answer a call to a unique name that is not ours (an error, or a reply to a call from the host).
        let raw = reply_to(3, ":1.77", "", vec![]);
        assert_eq!(unnumbered(ups(&r.from_app(c, msg(raw.clone())), c)), unnumbered(vec![raw]));
    }
}
