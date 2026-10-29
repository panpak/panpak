#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;
use std::time::{Duration, Instant};

struct Record(Vec<String>);

impl panpak_core::OpenTarget for Record {
    fn open(&mut self, url: &str) -> Result<(), String> {
        self.0.push(url.to_string());
        Ok(())
    }
}

// The server of the opener channel takes only `OPEN <address>` and one line break. Whatever bytes
// arrive, the target sees an address only when the request has exactly that shape and the address
// passes `check_url`, and the rate limit holds.
fuzz_target!(|data: &[u8]| {
    let t0 = Instant::now();
    let mut rec = Record(Vec::new());
    let mut limit = panpak_core::RateLimit::new(t0);
    let reply = panpak_core::handle_open_request(data, &mut limit, t0, &mut rec);
    assert!(reply == "OK" || reply.starts_with("ERR "), "reply {reply:?}");
    assert_eq!(reply == "OK", rec.0.len() == 1);
    assert!(data.len() <= panpak_core::MAX_REQUEST || rec.0.is_empty(), "too long a request was passed on");
    if let Some(url) = rec.0.first() {
        assert_eq!(data, format!("OPEN {url}\n").as_bytes(), "the request was not in the minimal format");
        assert!(panpak_core::check_url(url).is_ok());
        assert!(!url.to_ascii_lowercase().starts_with("file:"));
    }
    // A flood of the same bytes: at most the burst passes in an instant, and little more in a second.
    let mut limit = panpak_core::RateLimit::new(t0);
    let mut rec = Record(Vec::new());
    for i in 0..200u64 {
        panpak_core::handle_open_request(data, &mut limit, t0 + Duration::from_millis(i * 5), &mut rec);
    }
    assert!(rec.0.len() <= 3 + 5 + 1, "{} requests passed in one second", rec.0.len());
});
