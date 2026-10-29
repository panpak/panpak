// SPDX-License-Identifier: MIT OR Apache-2.0
//! The part of the address-opening channel that decides: the shape of a request, the rate limit and
//! the answer. No socket, no process: the stub's `opener` module does those, and the fuzz target
//! feeds this module with arbitrary bytes. Rules: PANPAK-FORMAT.md §9.3.
use std::time::Instant;

use crate::openurl::{check_url, MAX_URL};

/// Requests per second and the burst; the rest is refused.
pub const RATE_PER_SEC: f64 = 5.0;
pub const BURST: f64 = 3.0;
/// The longest request: `OPEN `, an address, a line break.
pub const MAX_REQUEST: usize = MAX_URL + "OPEN ".len() + 1;

/// Where addresses go after the checks.
pub trait Target: Send + 'static {
    fn open(&mut self, url: &str) -> Result<(), String>;
}

/// At most `RATE_PER_SEC` per second, with a burst of `BURST`.
pub struct RateLimit {
    tokens: f64,
    last: Instant,
}

impl RateLimit {
    pub fn new(now: Instant) -> Self {
        Self { tokens: BURST, last: now }
    }
    pub fn allow(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * RATE_PER_SEC).min(BURST);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Answer one request, as bytes read from the socket: exactly `OPEN <address>` and one line break, at
/// most `MAX_REQUEST` bytes, UTF-8. The reply is `OK` or `ERR <reason>`. A request counts against the
/// rate limit before anything else is looked at. The target gets an address only if `check_url` accepts it.
pub fn handle(request: &[u8], limit: &mut RateLimit, now: Instant, target: &mut dyn Target) -> String {
    if !limit.allow(now) {
        return "ERR too many requests".into();
    }
    let Some(line) = request.strip_suffix(b"\n").filter(|_| request.len() <= MAX_REQUEST) else { return "ERR bad request".into() };
    let Some(url) = std::str::from_utf8(line).ok().and_then(|l| l.strip_prefix("OPEN ")) else { return "ERR bad request".into() };
    if let Err(e) = check_url(url) {
        return format!("ERR {}", e.to_string().replace(['\n', '\r'], " "));
    }
    match target.open(url) {
        Ok(()) => "OK".into(),
        Err(_) => "ERR cannot open".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Fake(Vec<String>);
    impl Target for Fake {
        fn open(&mut self, url: &str) -> Result<(), String> {
            self.0.push(url.to_string());
            Ok(())
        }
    }

    #[test]
    fn only_good_addresses_reach_the_target() {
        let t0 = Instant::now();
        let mut f = Fake(Vec::new());
        let mut run = |req: &[u8]| {
            let mut l = RateLimit::new(t0);
            handle(req, &mut l, t0, &mut f)
        };
        assert_eq!(run(b"OPEN https://example.org/\n"), "OK");
        for bad in [
            &b"OPEN file:///etc/passwd\n"[..],
            b"OPEN mailto:a@b.org?attach=/etc/passwd\n",
            b"OPEN -x\n",
            b"hello\n",
            b"OPEN  https://example.org/\n",
            b"OPEN https://example.org/",
            b"OPEN https://example.org/\n\n",
            b"OPEN https://example.org/\nOPEN https://example.org/\n",
            b"OPEN https://example.org/\r\n",
            b"open https://example.org/\n",
            b"OPEN https://example.org/\xff\n",
            b"\n",
            b"",
        ] {
            assert!(run(bad).starts_with("ERR"), "{bad:?}");
        }
        let long = [b"OPEN https://e.org/".as_slice(), &vec![b'a'; MAX_REQUEST], b"\n"].concat();
        assert!(run(&long).starts_with("ERR"));
        assert_eq!(f.0, ["https://example.org/"]);
    }

    #[test]
    fn rate_is_five_per_second_with_a_burst_of_three() {
        let t0 = Instant::now();
        let mut l = RateLimit::new(t0);
        let ok: Vec<bool> = (0..5).map(|_| l.allow(t0)).collect();
        assert_eq!(ok, [true, true, true, false, false]);
        let t1 = t0 + Duration::from_millis(200);
        assert!(l.allow(t1));
        assert!(!l.allow(t1));
        let t2 = t1 + Duration::from_secs(60);
        let n = (0..10).filter(|_| l.allow(t2)).count();
        assert_eq!(n, 3);
        let mut l = RateLimit::new(t0);
        let passed = (0..1000).filter(|i| l.allow(t0 + Duration::from_millis(*i))).count();
        assert!(passed <= 8, "{passed}");
    }

    #[test]
    fn a_refused_request_counts_against_the_rate() {
        let t0 = Instant::now();
        let mut l = RateLimit::new(t0);
        let mut f = Fake(Vec::new());
        for _ in 0..3 {
            assert!(handle(b"junk\n", &mut l, t0, &mut f).starts_with("ERR bad"));
        }
        assert_eq!(handle(b"OPEN https://example.org/\n", &mut l, t0, &mut f), "ERR too many requests");
        assert!(f.0.is_empty());
    }
}
