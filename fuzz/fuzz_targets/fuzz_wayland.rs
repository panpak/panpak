#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;
use panpak_core::wayland::{parse_event, parse_stream, Event, MAX_MESSAGE};

// The bytes that a compositor sends are read by the client that registers the security context of an app. Whatever
// they are, the parser neither panics nor takes more than a message at a time, a message it accepts is whole
// (its size is a multiple of 4, between 8 and the limit), and the strings in the events have no NUL.
fuzz_target!(|data: &[u8]| {
    let mut rest = data;
    loop {
        match parse_event(rest) {
            Ok(Some((ev, used))) => {
                assert!(used >= 8 && used % 4 == 0 && used <= MAX_MESSAGE && used <= rest.len());
                match &ev {
                    Event::Global { interface, .. } => assert!(!interface.contains('\0')),
                    Event::Error { message, .. } => assert!(!message.contains('\0')),
                    _ => {}
                }
                rest = &rest[used..];
            }
            Ok(None) | Err(_) => break,
        }
    }
    // The same result when the stream arrives in two pieces (the client reads in chunks).
    if let Ok(all) = parse_stream(data) {
        let mid = data.len() / 2;
        let mut pending = data[..mid].to_vec();
        let mut got = Vec::new();
        for piece in [&data[mid..], &[][..]] {
            pending.extend_from_slice(piece);
            while let Ok(Some((ev, used))) = parse_event(&pending) {
                got.push(ev);
                pending.drain(..used);
            }
        }
        assert_eq!(got, all);
    }
});
