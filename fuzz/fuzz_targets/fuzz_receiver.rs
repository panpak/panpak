#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;
use panpak_core::frames::{act, encode_request, encode_response, parse_request, parse_response, KIND_OPEN_URI};
use panpak_core::{OpenTarget, RateLimit};
use std::time::Instant;

struct Rec(Vec<String>);
impl OpenTarget for Rec {
    fn open(&mut self, url: &str) -> Result<(), String> {
        self.0.push(url.to_string());
        Ok(())
    }
}

// The frames between the receiver and the actor. Whatever the bytes, the parsers take a whole frame or none, a
// frame that is taken encodes back to the same bytes, and the actor passes on only addresses that the checks
// accept (it does not trust the receiver).
fuzz_target!(|data: &[u8]| {
    let mut rest = data;
    let mut rec = Rec(Vec::new());
    let now = Instant::now();
    while let Ok(Some((req, used))) = parse_request(rest) {
        assert!(used >= 8 && used <= rest.len());
        assert_eq!(encode_request(req.kind, &req.payload).unwrap(), &rest[..used]);
        // A fresh limit for each frame, so that the limit does not hide what the checks would do.
        let before = rec.0.len();
        let resp = act(&req, &mut RateLimit::new(now), now, &mut rec);
        assert!(resp.text.len() <= panpak_core::frames::MAX_TEXT);
        if rec.0.len() > before {
            assert_eq!(req.kind, KIND_OPEN_URI);
            assert!(panpak_core::check_url(&rec.0[before]).is_ok(), "{:?}", rec.0[before]);
        }
        rest = &rest[used..];
    }
    let mut rest = data;
    while let Ok(Some((resp, used))) = parse_response(rest) {
        assert!(used >= 8 && used <= rest.len());
        assert_eq!(encode_response(resp.status, &resp.text), &rest[..used]);
        rest = &rest[used..];
    }
});
