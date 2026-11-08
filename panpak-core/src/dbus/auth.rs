// SPDX-License-Identifier: MIT OR Apache-2.0
//! The server side of the D-Bus authentication, for the router of the private bus: text lines until `BEGIN`.
//!
//! Only `EXTERNAL` is accepted (the connection's credentials are checked by the caller, from the socket, not from what the client says),
//! with the descriptors negotiated as the clients ask. Whatever else is refused. The handshake is bounded: a line, and the whole of it, are short.

const MAX_LINE: usize = 512;
const MAX_TOTAL: usize = 2048;
/// The identifier the router gives itself; clients do not use it for anything.
const GUID: &str = "70616e70616b70616e70616b70616e70";

#[derive(Debug, PartialEq)]
pub enum Step {
    /// More input is needed; send these bytes first (may be empty).
    More(Vec<u8>),
    /// `BEGIN`: the handshake is over. Send `reply`, then bytes in `rest` are the first of the message stream.
    Done { reply: Vec<u8>, rest: Vec<u8> },
    /// Not acceptable: close the connection.
    Refuse(String),
}

#[derive(Default)]
pub struct ServerAuth {
    buf: Vec<u8>,
    total: usize,
    first: bool,
    ok: bool,
    fds: bool,
}

impl ServerAuth {
    pub fn new() -> ServerAuth {
        ServerAuth::default()
    }

    /// Feed bytes read from the client. The very first byte of the connection must be a NUL (the client sends its credentials with it).
    pub fn feed(&mut self, data: &[u8]) -> Step {
        self.total += data.len();
        if self.total > MAX_TOTAL {
            return Step::Refuse("the handshake is too long".into());
        }
        let mut data = data;
        if !self.first {
            match data.first() {
                Some(0) => {
                    self.first = true;
                    data = &data[1..];
                }
                Some(_) => return Step::Refuse("no NUL byte at the start".into()),
                None => return Step::More(Vec::new()),
            }
        }
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            let Some(end) = self.buf.windows(2).position(|w| w == b"\r\n") else {
                if self.buf.len() > MAX_LINE {
                    return Step::Refuse("a line of the handshake is too long".into());
                }
                return Step::More(out);
            };
            if end > MAX_LINE {
                return Step::Refuse("a line of the handshake is too long".into());
            }
            let line: Vec<u8> = self.buf.drain(..end + 2).take(end).collect();
            let Ok(line) = std::str::from_utf8(&line) else { return Step::Refuse("the handshake is not text".into()) };
            let mut words = line.splitn(3, ' ');
            match (words.next(), words.next(), words.next()) {
                (Some("AUTH"), None, _) => out.extend_from_slice(b"REJECTED EXTERNAL\r\n"),
                (Some("AUTH"), Some("EXTERNAL"), None) => out.extend_from_slice(b"DATA\r\n"),
                (Some("AUTH"), Some("EXTERNAL"), Some(hex)) | (Some("DATA"), Some(hex), None) if hex.bytes().all(|b| b.is_ascii_hexdigit()) && hex.len() <= 64 => {
                    self.ok = true;
                    out.extend_from_slice(format!("OK {GUID}\r\n").as_bytes());
                }
                (Some("DATA"), None, _) if !self.ok => {
                    // An empty answer to DATA: the client lets the server use the credentials of the socket.
                    self.ok = true;
                    out.extend_from_slice(format!("OK {GUID}\r\n").as_bytes());
                }
                (Some("NEGOTIATE_UNIX_FD"), None, _) if self.ok => {
                    self.fds = true;
                    out.extend_from_slice(b"AGREE_UNIX_FD\r\n");
                }
                (Some("BEGIN"), None, _) if self.ok => {
                    return Step::Done { reply: out, rest: std::mem::take(&mut self.buf) };
                }
                (Some("CANCEL"), None, _) => {
                    self.ok = false;
                    out.extend_from_slice(b"REJECTED EXTERNAL\r\n");
                }
                _ => return Step::Refuse(format!("not a handshake line: {}", line.chars().take(24).collect::<String>())),
            }
        }
    }

    /// Whether the client asked for descriptors to be passed.
    pub fn fds(&self) -> bool {
        self.fds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usual_handshakes() {
        // libdbus: NUL and AUTH EXTERNAL with the user id, then the descriptors, then BEGIN (all at once, with the first message behind).
        let mut a = ServerAuth::new();
        let Step::Done { reply, rest } = a.feed(b"\0AUTH EXTERNAL 31303030\r\nNEGOTIATE_UNIX_FD\r\nBEGIN\r\nl\x01") else { panic!() };
        assert!(String::from_utf8(reply).unwrap().starts_with(&format!("OK {GUID}\r\nAGREE_UNIX_FD\r\n")));
        assert_eq!(rest, b"l\x01");
        assert!(a.fds());
        // GDBus: asks what is accepted, then AUTH EXTERNAL without data, DATA with it, in pieces.
        let mut a = ServerAuth::new();
        assert_eq!(a.feed(b"\0"), Step::More(vec![]));
        assert_eq!(a.feed(b"AUTH\r\n"), Step::More(b"REJECTED EXTERNAL\r\n".to_vec()));
        assert_eq!(a.feed(b"AUTH EXTERNAL\r\n"), Step::More(b"DATA\r\n".to_vec()));
        assert_eq!(a.feed(b"DATA 30\r\n"), Step::More(format!("OK {GUID}\r\n").into_bytes()));
        assert!(matches!(a.feed(b"BEGIN\r\n"), Step::Done { .. }));
        assert!(!a.fds());
        // A client that sends an empty DATA line.
        let mut a = ServerAuth::new();
        a.feed(b"\0AUTH EXTERNAL\r\n");
        assert!(matches!(a.feed(b"DATA\r\n"), Step::More(r) if r.starts_with(b"OK ")));
    }

    #[test]
    fn what_is_not_a_handshake_is_refused() {
        for bad in [&b"AUTH EXTERNAL\r\n"[..], b"\0BEGIN\r\n", b"\0NEGOTIATE_UNIX_FD\r\n", b"\0AUTH ANONYMOUS\r\n", b"\0AUTH EXTERNAL zz\r\n", b"\0\xff\xfe\r\n"] {
            assert!(matches!(ServerAuth::new().feed(bad), Step::Refuse(_)), "{bad:?}");
        }
        assert!(matches!(ServerAuth::new().feed(&[0u8; 3000]), Step::Refuse(_)));
        let mut long = vec![0u8];
        long.extend(std::iter::repeat_n(b'A', 600));
        assert!(matches!(ServerAuth::new().feed(&long), Step::Refuse(_)));
    }
}
