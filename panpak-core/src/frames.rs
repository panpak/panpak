// SPDX-License-Identifier: MIT OR Apache-2.0
//! The frames between the two pieces of the resident stub: the receiver (a process with no rights, which
//! reads what the app sends) and the actor (the outer stub, which does the things the app may ask for).
//! No socket, no process: the stub's `receiver` and `opener` modules do those, and the fuzz target feeds this
//! module with arbitrary bytes. Rules: PANPAK-FORMAT.md §9.7.
//!
//! A request is `PKRQ`, a version byte (1), a kind byte, a 16-bit length (big endian) and that many bytes. A response is
//! `PKRS`, the version, a status byte, a 16-bit length and a short text. The receiver sends requests, the actor
//! answers each with one response, in order. Descriptors never cross: no frame carries one, and the app's own
//! descriptors stop at the receiver.
use crate::openchan::{RateLimit, Target};
use crate::openurl::check_url;
use std::time::Instant;

pub const REQUEST_MAGIC: [u8; 4] = *b"PKRQ";
pub const RESPONSE_MAGIC: [u8; 4] = *b"PKRS";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 8;
/// The largest payload of a request: an address is at most 2048 bytes.
pub const MAX_PAYLOAD: usize = 4096;
/// The longest text of a response.
pub const MAX_TEXT: usize = 256;
/// The largest payload of a file chooser request (the token of the router and the call of the app) and the longest text of its response.
pub const MAX_CHOOSER_PAYLOAD: usize = 4 + crate::filechooser::MAX_CALL;
pub const MAX_CHOOSER_TEXT: usize = 60_000;

/// What a request asks for. Only `OPEN_URI` is done today; the other numbers are kept for what comes
/// next (a file chooser through the portal is 2, a browser for the sign-in of an app is 3) and are refused
/// as unsupported until then.
pub const KIND_OPEN_URI: u8 = 1;
pub const KIND_FILE_CHOOSER: u8 = 2;
pub const KIND_BROWSER: u8 = 3;

/// How a request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Done.
    Done = 0,
    /// Refused by a rule: the request was not valid, or too many came.
    Refused = 1,
    /// Valid, but it could not be done.
    Failed = 2,
    /// The kind is not one this version does.
    Unsupported = 3,
    /// The answer of a file chooser request (it comes when the user has chosen, not in the order of the requests): the text is
    /// `<token> <code> [<hex of the a{sv}>]` (`filechooser::response_text`).
    Chooser = 4,
}

impl Status {
    fn from_byte(b: u8) -> Option<Status> {
        Some(match b {
            0 => Status::Done,
            1 => Status::Refused,
            2 => Status::Failed,
            3 => Status::Unsupported,
            4 => Status::Chooser,
            _ => return None,
        })
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Request {
    pub kind: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Response {
    pub status: Status,
    pub text: String,
}

/// Why bytes are not a frame. The stream cannot be followed after any of these: the side that gets one ends
/// the conversation.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    Magic,
    Version(u8),
    Length(usize),
    Status(u8),
    Text,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Magic => write!(f, "not a frame (bad magic)"),
            FrameError::Version(v) => write!(f, "frame version {v} is not supported"),
            FrameError::Length(n) => write!(f, "frame length {n} is over the limit"),
            FrameError::Status(s) => write!(f, "unknown status {s}"),
            FrameError::Text => write!(f, "the text of a response is not printable ASCII"),
        }
    }
}

impl std::error::Error for FrameError {}

fn max_payload(kind: u8) -> usize {
    if kind == KIND_FILE_CHOOSER {
        MAX_CHOOSER_PAYLOAD
    } else {
        MAX_PAYLOAD
    }
}

pub fn encode_request(kind: u8, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > max_payload(kind) {
        return Err(FrameError::Length(payload.len()));
    }
    let mut v = Vec::with_capacity(HEADER_LEN + payload.len());
    v.extend_from_slice(&REQUEST_MAGIC);
    v.extend_from_slice(&[VERSION, kind]);
    v.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    v.extend_from_slice(payload);
    Ok(v)
}

pub fn encode_response(status: Status, text: &str) -> Vec<u8> {
    // The text is cut to the limit and reduced to printable ASCII: it ends up on the screen of the app.
    let t: String = text.chars().map(|c| if (' '..='~').contains(&c) { c } else { '?' }).take(MAX_TEXT).collect();
    let mut v = Vec::with_capacity(HEADER_LEN + t.len());
    v.extend_from_slice(&RESPONSE_MAGIC);
    v.extend_from_slice(&[VERSION, status as u8]);
    v.extend_from_slice(&(t.len() as u16).to_be_bytes());
    v.extend_from_slice(t.as_bytes());
    v
}

/// The answer of a file chooser request: the text is `filechooser::response_text`, not cut to printable ASCII and not limited to `MAX_TEXT`.
pub fn encode_chooser_response(text: &str) -> Result<Vec<u8>, FrameError> {
    if text.len() > MAX_CHOOSER_TEXT || !text.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        return Err(FrameError::Text);
    }
    let mut v = Vec::with_capacity(HEADER_LEN + text.len());
    v.extend_from_slice(&RESPONSE_MAGIC);
    v.extend_from_slice(&[VERSION, Status::Chooser as u8]);
    v.extend_from_slice(&(text.len() as u16).to_be_bytes());
    v.extend_from_slice(text.as_bytes());
    Ok(v)
}

/// The header of a frame: checks the magic, the version and the length, and returns the payload length.
fn header(buf: &[u8], magic: &[u8; 4], max: usize) -> Result<Option<(u8, usize)>, FrameError> {
    // Decide as early as the bytes allow: a wrong magic is an error after the first different byte.
    let seen = buf.len().min(4);
    if buf[..seen] != magic[..seen] {
        return Err(FrameError::Magic);
    }
    if buf.len() < HEADER_LEN {
        return Ok(None);
    }
    if buf[4] != VERSION {
        return Err(FrameError::Version(buf[4]));
    }
    let len = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    if len > max {
        return Err(FrameError::Length(len));
    }
    Ok(Some((buf[5], len)))
}

/// Take the first request off `buf`: `Ok(None)` while it is not complete, otherwise the request and the number
/// of bytes it took.
pub fn parse_request(buf: &[u8]) -> Result<Option<(Request, usize)>, FrameError> {
    let Some((kind, len)) = header(buf, &REQUEST_MAGIC, MAX_CHOOSER_PAYLOAD)? else { return Ok(None) };
    if len > max_payload(kind) {
        return Err(FrameError::Length(len));
    }
    if buf.len() < HEADER_LEN + len {
        return Ok(None);
    }
    Ok(Some((Request { kind, payload: buf[HEADER_LEN..HEADER_LEN + len].to_vec() }, HEADER_LEN + len)))
}

/// Take the first response off `buf`, as `parse_request` does for a request.
pub fn parse_response(buf: &[u8]) -> Result<Option<(Response, usize)>, FrameError> {
    let Some((status, len)) = header(buf, &RESPONSE_MAGIC, MAX_CHOOSER_TEXT)? else { return Ok(None) };
    let status = Status::from_byte(status).ok_or(FrameError::Status(status))?;
    if status != Status::Chooser && len > MAX_TEXT {
        return Err(FrameError::Length(len));
    }
    if buf.len() < HEADER_LEN + len {
        return Ok(None);
    }
    let text = &buf[HEADER_LEN..HEADER_LEN + len];
    if !text.iter().all(|b| (b' '..=b'~').contains(b)) {
        return Err(FrameError::Text);
    }
    Ok(Some((Response { status, text: String::from_utf8_lossy(text).into_owned() }, HEADER_LEN + len)))
}

/// What the actor does with a request. The actor does not trust the receiver: the address is checked again, and
/// the rate limit is its own. A kind that is not done yet is `Unsupported`.
/// At most `MAX_TEXT` bytes, cut at a character (an error message may repeat the scheme of an address, which can be 2 KiB).
fn bounded(mut text: String) -> String {
    if text.len() > MAX_TEXT {
        let mut end = MAX_TEXT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

pub fn act(req: &Request, limit: &mut RateLimit, now: Instant, target: &mut dyn Target) -> Response {
    match req.kind {
        KIND_OPEN_URI => {
            if !limit.allow(now) {
                return Response { status: Status::Refused, text: "too many requests".into() };
            }
            let Ok(url) = std::str::from_utf8(&req.payload) else { return Response { status: Status::Refused, text: "the address is not UTF-8".into() } };
            if let Err(e) = check_url(url) {
                return Response { status: Status::Refused, text: bounded(e.to_string().replace(['\n', '\r'], " ")) };
            }
            match target.open(url) {
                Ok(()) => Response { status: Status::Done, text: String::new() },
                Err(e) => Response { status: Status::Failed, text: format!("cannot open: {}", e.replace(['\n', '\r'], " ").chars().take(160).collect::<String>()) },
            }
        }
        KIND_FILE_CHOOSER | KIND_BROWSER => Response { status: Status::Unsupported, text: "not done by this version".into() },
        _ => Response { status: Status::Unsupported, text: "unknown kind".into() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rec(Vec<String>);
    impl Target for Rec {
        fn open(&mut self, url: &str) -> Result<(), String> {
            self.0.push(url.to_string());
            Ok(())
        }
    }

    #[test]
    fn an_error_text_is_bounded_even_when_it_repeats_a_long_scheme() {
        let url = format!("{}:x", "a".repeat(2000));
        let mut limit = RateLimit::new(Instant::now());
        let r = act(&Request { kind: KIND_OPEN_URI, payload: url.into_bytes() }, &mut limit, Instant::now(), &mut Rec(Vec::new()));
        assert_eq!(r.status, Status::Refused);
        assert!(r.text.len() <= MAX_TEXT && r.text.starts_with("scheme aaaa"), "{} bytes", r.text.len());
        assert!(bounded("é".repeat(300)).len() <= MAX_TEXT);
        // And it encodes: a text above the limit would not.
        assert!(!encode_response(r.status, &r.text).is_empty());
    }

    #[test]
    fn request_round_trip_and_partial() {
        let b = encode_request(KIND_OPEN_URI, b"https://example.org/").unwrap();
        assert_eq!(&b[..8], b"PKRQ\x01\x01\x00\x14");
        let (r, used) = parse_request(&b).unwrap().unwrap();
        assert_eq!((r.kind, r.payload.as_slice(), used), (1, b"https://example.org/".as_slice(), b.len()));
        for cut in 0..b.len() {
            assert_eq!(parse_request(&b[..cut]).unwrap(), None, "cut {cut}");
        }
        // Two frames in one buffer: the first is taken, the rest stays.
        let mut two = b.clone();
        two.extend(&b);
        assert_eq!(parse_request(&two).unwrap().unwrap().1, b.len());
    }

    #[test]
    fn response_round_trip_and_text_is_cleaned() {
        let b = encode_response(Status::Refused, "bad\naddress\u{e9}");
        let (r, used) = parse_response(&b).unwrap().unwrap();
        assert_eq!(used, b.len());
        assert_eq!(r, Response { status: Status::Refused, text: "bad?address?".into() });
        let long = encode_response(Status::Failed, &"a".repeat(1000));
        assert_eq!(parse_response(&long).unwrap().unwrap().0.text.len(), MAX_TEXT);
        for cut in 0..b.len() {
            assert_eq!(parse_response(&b[..cut]).unwrap(), None);
        }
    }

    #[test]
    fn bad_frames_are_errors() {
        assert_eq!(parse_request(b"XKRQ"), Err(FrameError::Magic));
        assert_eq!(parse_request(b"PKRS\x01\x01\x00\x00"), Err(FrameError::Magic), "a response is not a request");
        assert_eq!(parse_request(b"PKRQ\x02\x01\x00\x00"), Err(FrameError::Version(2)));
        assert_eq!(parse_request(b"PKRQ\x01\x01\x10\x01"), Err(FrameError::Length(0x1001)));
        assert_eq!(encode_request(1, &vec![0; MAX_PAYLOAD + 1]), Err(FrameError::Length(MAX_PAYLOAD + 1)));
        assert_eq!(parse_response(b"PKRS\x01\x09\x00\x00"), Err(FrameError::Status(9)));
        assert_eq!(parse_response(b"PKRS\x01\x00\x01\x01"), Err(FrameError::Length(0x101)));
        assert_eq!(parse_response(b"PKRS\x01\x00\x00\x01\x07"), Err(FrameError::Text));
    }

    #[test]
    fn act_checks_again_and_limits() {
        let t0 = Instant::now();
        let mut rec = Rec(Vec::new());
        let mut lim = RateLimit::new(t0);
        let ok = act(&Request { kind: KIND_OPEN_URI, payload: b"https://example.org/".to_vec() }, &mut lim, t0, &mut rec);
        assert_eq!(ok.status, Status::Done);
        for bad in [&b"file:///etc/passwd"[..], b"-x", b"https://example.org/ x", b"\xff\xfe", b"", b"mailto:a@b.org?attach=/etc/passwd"] {
            let r = act(&Request { kind: KIND_OPEN_URI, payload: bad.to_vec() }, &mut RateLimit::new(t0), t0, &mut rec);
            assert_eq!(r.status, Status::Refused, "{bad:?}");
        }
        assert_eq!(rec.0, vec!["https://example.org/".to_string()], "only the good address reached the target");
        for k in [KIND_FILE_CHOOSER, KIND_BROWSER, 0, 4, 255] {
            let r = act(&Request { kind: k, payload: vec![] }, &mut RateLimit::new(t0), t0, &mut rec);
            assert_eq!(r.status, Status::Unsupported, "kind {k}");
        }
        // A flood: the burst passes, the rest is refused.
        let mut lim = RateLimit::new(t0);
        let done = (0..30).filter(|_| act(&Request { kind: KIND_OPEN_URI, payload: b"https://example.org/".to_vec() }, &mut lim, t0, &mut rec).status == Status::Done).count();
        assert!(done <= 3, "{done}");
    }
}
