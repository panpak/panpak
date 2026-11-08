#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;
use panpak_core::dbus::{message_len, parse_head, ClientId, Limits, Msg, Out, Router, RouterConfig};

// The router of the private bus, fed with what an app and the host can send: a sequence of events, each made of a tag, a connection
// number and a message that is not trusted. It must never panic, and:
// - everything it sends is a message that reads back (header);
// - a message it sends to the host for a call of the app is either the app's own bytes or a call of the router itself (serial in
//   the router's range);
// - a call for a name that the app owns here is never sent to the host;
// - a call to the host registry from the app is never sent to the host.
fuzz_target!(|data: &[u8]| {
    let mut r: Router<()> = Router::new(RouterConfig {
        portal_id: Some("panpak.org.example.App".into()),
        mpris_alias: Some("org.mpris.MediaPlayer2.panpak.org_example_App".into()),
        open_uri: true,
    });
    let lim = Limits { max_message: 1 << 20, ..Limits::default() };
    let mut ids: Vec<ClientId> = Vec::new();
    let mut tokens: Vec<u64> = Vec::new();
    let mut p = data;
    let mut steps = 0;
    while p.len() >= 4 && steps < 64 {
        steps += 1;
        let (tag, who, len) = (p[0] % 6, p[1] as usize, (u16::from_le_bytes([p[2], p[3]]) as usize) % 4096);
        let body_end = (4 + len).min(p.len());
        let payload = p[4..body_end].to_vec();
        p = &p[body_end..];
        if ids.is_empty() || tag == 0 {
            if ids.len() < 6 {
                ids.push(r.new_client());
            }
            if tag == 0 {
                continue;
            }
        }
        let c = ids[who % ids.len()];
        let outs = match tag {
            1 | 2 => {
                let nfds = payload.first().map_or(0, |b| (b % 3) as usize);
                let raw = if payload.len() > 1 { payload[1..].to_vec() } else { Vec::new() };
                let local_before = r.local_names();
                let head = if raw.len() >= 16 { parse_head(&raw, nfds, &lim).ok() } else { None };
                let outs = if tag == 1 { r.from_app(c, Msg { raw: raw.clone(), fds: vec![(); nfds] }) } else { r.from_up(c, Msg { raw: raw.clone(), fds: vec![(); nfds] }) };
                if tag == 1 {
                    if let Some(h) = &head {
                        for o in &outs {
                            if let Out::Up(_, m) = o {
                                let sent = parse_head(&m.raw, m.fds.len(), &Limits { max_message: usize::MAX >> 4, ..lim }).expect("a message sent to the host reads back");
                                if m.raw != raw {
                                    assert!(sent.msg.serial > 0x4000_0000, "a message to the host that is neither the app's nor the router's");
                                }
                                if let Some(d) = &h.msg.destination {
                                    if h.msg.kind == 1 && local_before.contains(d) {
                                        assert!(m.raw != raw, "a call for a name of the app went to the host");
                                    }
                                }
                                if h.msg.interface.as_deref() == Some("org.freedesktop.host.portal.Registry") {
                                    assert!(m.raw != raw, "the app's call to the registry went to the host");
                                }
                            }
                        }
                    }
                }
                outs
            }
            3 => r.client_gone(c),
            4 => {
                let t = tokens.pop().unwrap_or(1);
                r.open_done(t, if payload.first().is_some_and(|b| b % 2 == 0) { Ok(()) } else { Err("no".into()) })
            }
            _ => Vec::new(),
        };
        for o in &outs {
            match o {
                Out::App(_, m) | Out::Up(_, m) => {
                    if m.raw.len() >= 16 {
                        let head: [u8; 16] = m.raw[..16].try_into().unwrap();
                        let _ = message_len(&head, &lim);
                    }
                    parse_head(&m.raw, m.fds.len(), &Limits { max_message: usize::MAX >> 4, ..lim }).expect("the router sent a message that does not read");
                }
                Out::Open { token, .. } => tokens.push(*token),
                Out::Close(id, _) => {
                    let _ = r.client_gone(*id);
                    ids.retain(|x| x != id);
                }
                Out::Log(_) => {}
            }
        }
    }
});
