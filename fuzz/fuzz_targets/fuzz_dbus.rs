#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;
use panpak_core::dbus::{encode_message, message_len, parse_message, Limits};

// The first byte says how many descriptors are "waiting"; the rest is a D-Bus message that is not
// trusted. The reader must never panic and never allocate beyond the limits. A message that is
// accepted must encode again and read back as the same message (the oracle for the writer and
// the reader together), and it must respect the limits it was read under.
fuzz_target!(|data: &[u8]| {
    let Some((&n, msg)) = data.split_first() else { return };
    let avail = (n % 20) as usize;
    let lim = Limits::default();
    if msg.len() >= 16 {
        let head: [u8; 16] = msg[..16].try_into().unwrap();
        if let Ok(total) = message_len(&head, &lim) {
            assert!(total <= lim.max_message);
        }
    }
    let Ok(mut m) = parse_message(msg, avail, &lim) else { return };
    assert!(m.unix_fds as usize <= avail && m.unix_fds as usize <= lim.max_fds);
    assert!(msg.len() <= lim.max_message);
    // The sender is not part of what we encode.
    m.sender = None;
    let again = encode_message(&m).expect("a message that was read must encode");
    let back = parse_message(&again, avail, &lim).expect("an encoded message must read back");
    assert_eq!(format!("{m:?}"), format!("{back:?}"));
});
