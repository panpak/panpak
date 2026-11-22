// SPDX-License-Identifier: MIT OR Apache-2.0
//! The chunks of an update, fetched with range requests.
//!
//! The chunks that an update needs lie in the package at the server in many small places. Two neighbours are asked in one request even when a few
//! bytes that nobody asked for lie between them (the bytes are dropped: a chunk is checked by its hash when it is used, so nothing that came is
//! trusted), and a few requests are in flight at once. When the server says it is busy (429 or 503), the wait that it asks for (`Retry-After`) is
//! kept, and the rest of the download goes on one request at a time.
use anyhow::{ensure, Context, Result};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Two wanted ranges with at most this many bytes between them are asked in one request.
pub const MERGE_GAP: u64 = 16 << 10;
/// The most that one response holds.
const MAX_RESPONSE: u64 = 4 << 20;
/// Requests in flight at once.
pub const WORKERS: usize = 6;
/// How many times a range is asked again when the server is busy, before the download stops.
pub const TRIES: u32 = 6;
/// The longest wait that a `Retry-After` is obeyed. A server that asks for more is not waited for: the download stops with the time it asked.
const LONGEST_WAIT: Duration = Duration::from_secs(300);

/// The server is busy (HTTP 429 or 503). `retry_after` is what it asked for in `Retry-After`, when it did.
#[derive(Debug)]
pub struct Throttled {
    pub status: u16,
    pub retry_after: Option<Duration>,
}

impl std::fmt::Display for Throttled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.retry_after {
            Some(d) => write!(f, "the server is busy (HTTP {}) and asks to wait {} s", self.status, d.as_secs()),
            None => write!(f, "the server is busy (HTTP {})", self.status),
        }
    }
}

impl std::error::Error for Throttled {}

/// How the chunks are asked for. The defaults are the constants above; the tests choose others.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub workers: usize,
    pub gap: u64,
    pub tries: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Policy { workers: WORKERS, gap: MERGE_GAP, tries: TRIES }
    }
}

impl Policy {
    /// One request at a time, no bytes in between: for a package that is a file here.
    pub fn local() -> Self {
        Policy { workers: 1, gap: 0, tries: 1 }
    }
}

/// The wait that a `Retry-After` header asks for: a number of seconds, or a date (the HTTP format `Sun, 06 Nov 1994 08:49:37 GMT`). A date in the
/// past is no wait. Anything else is not understood (`None`).
pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if v.bytes().all(|b| b.is_ascii_digit()) {
        return v.parse::<u64>().ok().map(Duration::from_secs);
    }
    let at = parse_http_date(v)?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

/// `Sun, 06 Nov 1994 08:49:37 GMT` (the only form that a server MUST generate; RFC 9110, 5.6.7).
fn parse_http_date(v: &str) -> Option<SystemTime> {
    let rest = v.split_once(", ")?.1;
    let mut it = rest.split(' ');
    let day: i64 = it.next()?.parse().ok()?;
    let name = it.next()?;
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| *m == name)? as i64 + 1;
    let year: i64 = it.next()?.parse().ok()?;
    let mut hms = it.next()?.split(':');
    let (h, m, s): (i64, i64, i64) = (hms.next()?.parse().ok()?, hms.next()?.parse().ok()?, hms.next()?.parse().ok()?);
    if it.next()? != "GMT" || it.next().is_some() || !(1..=31).contains(&day) || h > 23 || m > 59 || s > 60 || !(1970..=9999).contains(&year) {
        return None;
    }
    // Days since 1970-01-01 (the civil calendar algorithm of Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + h * 3600 + m * 60 + s;
    u64::try_from(secs).ok().map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

/// Half-open intervals, at most `MAX_RESPONSE` per response. Two intervals with at most `gap` bytes between them are one; with `gap` 0 only
/// adjacent or overlapping intervals are. The bytes in between are fetched and dropped.
pub fn coalesce_ranges(mut ranges: Vec<(u64, u64)>, gap: u64) -> Vec<(u64, u64)> {
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (mut start, end) in ranges {
        while start < end {
            if let Some(last) = out.last_mut() {
                if start <= last.1.saturating_add(gap) && end.saturating_sub(last.0) <= MAX_RESPONSE {
                    last.1 = last.1.max(end);
                    break;
                }
            }
            let stop = end.min(start.saturating_add(MAX_RESPONSE));
            out.push((start, stop));
            start = stop;
        }
    }
    out
}

/// What the workers share about a server that is busy: once it was, the download goes on one request at a time, and no request starts before the
/// time that the server asked for.
struct Pace {
    serial: AtomicBool,
    gate: Mutex<()>,
    not_before: Mutex<Option<Instant>>,
    answers: AtomicU64,
}

impl Pace {
    fn new() -> Self {
        Pace { serial: AtomicBool::new(false), gate: Mutex::new(()), not_before: Mutex::new(None), answers: AtomicU64::new(0) }
    }

    /// Wait until no wait that the server asked for is running.
    fn wait_turn(&self) {
        loop {
            let until = self.not_before.lock().ok().and_then(|g| *g);
            match until {
                Some(t) if t > Instant::now() => std::thread::sleep(t - Instant::now()),
                _ => return,
            }
        }
    }

    /// The server was busy: from now on one request at a time, and none before the wait that it asked for (or, when it asked for none, one that
    /// doubles with every answer of this kind: 1 s, 2 s, 4 s, ...).
    fn slow_down(&self, t: &Throttled) -> Result<()> {
        self.serial.store(true, Ordering::SeqCst);
        let n = self.answers.fetch_add(1, Ordering::SeqCst);
        let wait = t.retry_after.unwrap_or_else(|| Duration::from_secs(1u64 << n.min(5)));
        ensure!(wait <= LONGEST_WAIT, "the server is busy (HTTP {}) and asks to wait {} s: try again later (what came is kept)", t.status, wait.as_secs());
        if let Ok(mut g) = self.not_before.lock() {
            let at = Instant::now() + wait;
            if g.is_none_or(|old| at > old) {
                *g = Some(at);
            }
        }
        Ok(())
    }
}

/// Download the chunks `needed` into `staging`, one file per chunk named by its hash (the compressed bytes as they are in the package; the stub
/// checks each against its hash when it uses it, so nothing here is trusted). A file that is already there with the right size is kept: a download
/// that was interrupted goes on from where it stopped. `fetch(start, end)` returns the bytes `start..end` of the package. Returns how many chunks
/// were fetched now and how many bytes came (the bytes between two neighbours included).
pub fn fetch_chunks(
    staging: &Path,
    needed: &[panpak_core::Chunk],
    index_off: u64,
    policy: Policy,
    fetch: &(dyn Fn(u64, u64) -> Result<Vec<u8>> + Sync),
) -> Result<(u64, u64)> {
    fs::create_dir_all(staging)?;
    let mut seen = std::collections::HashSet::new();
    let mut todo: Vec<&panpak_core::Chunk> = Vec::new();
    for c in needed {
        let end = c.off.checked_add(c.comp as u64).context("chunk exceeds u64")?;
        ensure!(end <= index_off, "chunk outside region: {c:?}");
        if !seen.insert(c.hash) {
            continue;
        }
        if fs::metadata(staging.join(panpak_core::hex(&c.hash))).is_ok_and(|m| m.len() == c.comp as u64) {
            continue;
        }
        todo.push(c);
    }
    todo.sort_by_key(|c| c.off);
    let ranges = coalesce_ranges(todo.iter().map(|c| (c.off, c.off + c.comp as u64)).collect(), policy.gap);
    let next = AtomicUsize::new(0);
    let bytes_in = AtomicU64::new(0);
    let failed: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let pace = Pace::new();
    let one = |&(start, end): &(u64, u64)| -> Result<()> {
        let bytes = ask(&pace, policy.tries, start, end, fetch)?;
        ensure!(bytes.len() as u64 == end - start, "a range came back with {} bytes instead of {}", bytes.len(), end - start);
        bytes_in.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        // The chunks that lie in this range (`todo` is sorted by offset, and a chunk is in exactly one range).
        let from = todo.partition_point(|c| c.off < start);
        for c in todo[from..].iter().take_while(|c| c.off + (c.comp as u64) <= end) {
            let part = &bytes[(c.off - start) as usize..(c.off - start) as usize + c.comp as usize];
            let name = staging.join(panpak_core::hex(&c.hash));
            let tmp = name.with_extension("part");
            fs::write(&tmp, part)?;
            fs::rename(&tmp, &name)?;
        }
        Ok(())
    };
    let workers = policy.workers.clamp(1, ranges.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(r) = ranges.get(i) else { return };
                if failed.lock().map(|f| f.is_some()).unwrap_or(true) {
                    return;
                }
                if let Err(e) = one(r) {
                    if let Ok(mut f) = failed.lock() {
                        f.get_or_insert(e);
                    }
                    return;
                }
            });
        }
    });
    if let Some(e) = failed.into_inner().ok().flatten() {
        return Err(e);
    }
    Ok((todo.len() as u64, bytes_in.load(Ordering::Relaxed)))
}

/// One range, asked again when the server is busy. While it is, only one request at a time runs (the gate), and none starts before the wait that
/// it asked for.
fn ask(pace: &Pace, tries: u32, start: u64, end: u64, fetch: &(dyn Fn(u64, u64) -> Result<Vec<u8>> + Sync)) -> Result<Vec<u8>> {
    let mut left = tries.max(1);
    loop {
        pace.wait_turn();
        let answer = if pace.serial.load(Ordering::SeqCst) {
            let _one_at_a_time = pace.gate.lock().map_err(|_| anyhow::anyhow!("a download thread failed"))?;
            pace.wait_turn();
            fetch(start, end)
        } else {
            fetch(start, end)
        };
        match answer {
            Err(e) => match e.downcast_ref::<Throttled>() {
                Some(t) => {
                    left -= 1;
                    pace.slow_down(t)?;
                    ensure!(left > 0, "the server stayed busy (HTTP {}) for {tries} tries of the same range: try again later (what came is kept)", t.status);
                }
                None => return Err(e),
            },
            ok => return ok,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;

    #[test]
    fn neighbours_are_one_range_up_to_the_gap_and_a_response_is_at_most_4_mib() {
        assert_eq!(coalesce_ranges(vec![(20, 30), (0, 10), (10, 20), (40, 50)], 0), vec![(0, 30), (40, 50)]);
        assert_eq!(coalesce_ranges(vec![(0, 9 << 20)], 0), vec![(0, 4 << 20), (4 << 20, 8 << 20), (8 << 20, 9 << 20)]);
        assert!(coalesce_ranges(vec![(5, 5)], 0).is_empty());
        // The bytes in between: 16 KiB join, one byte more do not.
        let g = MERGE_GAP;
        assert_eq!(coalesce_ranges(vec![(0, 100), (100 + g, 200 + g)], g), vec![(0, 200 + g)]);
        assert_eq!(coalesce_ranges(vec![(0, 100), (101 + g, 200 + g)], g), vec![(0, 100), (101 + g, 200 + g)]);
        // A joined range is never more than 4 MiB.
        let r = coalesce_ranges((0..100u64).map(|i| (i * 100_000, i * 100_000 + 90_000)).collect(), g);
        assert!(r.iter().all(|(s, e)| e - s <= 4 << 20), "{r:?}");
        assert!(r.len() > 1);
    }

    #[test]
    fn retry_after_is_seconds_or_a_date() {
        let now = UNIX_EPOCH + Duration::from_secs(784_111_777); // Sun, 06 Nov 1994 08:49:37 GMT
        assert_eq!(parse_retry_after("120", now), Some(Duration::from_secs(120)));
        assert_eq!(parse_retry_after(" 0 ", now), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", now), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("Sun, 06 Nov 1994 08:51:37 GMT", now), Some(Duration::from_secs(120)));
        assert_eq!(parse_retry_after("Sun, 06 Nov 1994 08:00:00 GMT", now), Some(Duration::ZERO), "a date in the past is no wait");
        for bad in ["", "soon", "-5", "1.5", "Sun, 32 Nov 1994 08:49:37 GMT", "Sun, 06 Nov 1994 08:49:37 UTC", "Sun, 06 Nov 1994 25:49:37 GMT", "99999999999999999999999"] {
            assert_eq!(parse_retry_after(bad, now), None, "{bad:?}");
        }
    }

    /// A server on the loopback that holds the bytes of a package and answers range requests, `delay` after the request has come. It counts the
    /// requests, the most that were in flight at once, and it answers 429 or 503 for the first requests when told to.
    struct Server {
        port: u16,
        requests: Arc<AtomicUsize>,
        most_in_flight: Arc<AtomicUsize>,
        /// The most in flight among the requests that came more than half a second after the first busy answer (the ones before may have been
        /// started before the workers knew).
        most_after_busy: Arc<AtomicUsize>,
    }

    struct Busy {
        status: u16,
        retry_after: Option<&'static str>,
        /// How many of the first requests are answered so.
        first: usize,
    }

    fn serve(data: Vec<u8>, delay: Duration, busy: Option<Busy>, wrong_range_at: Option<usize>) -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let requests = Arc::new(AtomicUsize::new(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let most_after = Arc::new(AtomicUsize::new(0));
        let busy_seen: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let (rq, inf, mo, ma, bs) = (requests.clone(), in_flight, most.clone(), most_after.clone(), busy_seen);
        let data = Arc::new(data);
        let busy = Arc::new(busy);
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut s) = conn else { continue };
                let (rq, inf, mo, ma, bs, data, busy) = (rq.clone(), inf.clone(), mo.clone(), ma.clone(), bs.clone(), data.clone(), busy.clone());
                std::thread::spawn(move || {
                    let n = rq.fetch_add(1, Ordering::SeqCst);
                    let now = inf.fetch_add(1, Ordering::SeqCst) + 1;
                    mo.fetch_max(now, Ordering::SeqCst);
                    if bs.lock().unwrap().is_some_and(|t| t.elapsed() >= Duration::from_millis(500)) {
                        ma.fetch_max(now, Ordering::SeqCst);
                    }
                    let head = read_head(&mut s);
                    std::thread::sleep(delay);
                    let reply = match busy.as_ref() {
                        Some(b) if n < b.first => {
                            bs.lock().unwrap().get_or_insert_with(Instant::now);
                            let ra = b.retry_after.map(|v| format!("Retry-After: {v}\r\n")).unwrap_or_default();
                            format!("HTTP/1.1 {} Busy\r\n{ra}Content-Length: 0\r\nConnection: close\r\n\r\n", b.status).into_bytes()
                        }
                        _ => range_reply(&head, &data, wrong_range_at == Some(n)),
                    };
                    let _ = s.write_all(&reply);
                    inf.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Server { port, requests, most_in_flight: most, most_after_busy: most_after }
    }

    fn read_head(s: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut b = [0u8; 1];
        while !buf.ends_with(b"\r\n\r\n") {
            if s.read(&mut b).unwrap_or(0) == 0 {
                break;
            }
            buf.push(b[0]);
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn range_reply(head: &str, data: &[u8], wrong: bool) -> Vec<u8> {
        let r = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("range: bytes=").map(str::to_string)).unwrap();
        let (a, b) = r.split_once('-').unwrap();
        let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
        let (ca, cb) = if wrong { (a + 1, b + 1) } else { (a, b) };
        let mut out = format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {ca}-{cb}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", data.len(), b - a + 1).into_bytes();
        out.extend_from_slice(&data[a..=b]);
        out
    }

    fn agent() -> ureq::Agent {
        crate::agent_for("http://127.0.0.1:1/x", true, 30).unwrap()
    }

    /// A package of `n` chunks of 1000 bytes at every 1500 bytes (500 bytes of something else between them), the chunk `i` filled with the byte `i`.
    fn package(n: usize) -> (Vec<u8>, Vec<panpak_core::Chunk>) {
        let mut data = vec![0xEEu8; n * 1500 + 4000];
        let mut chunks = Vec::new();
        for i in 0..n {
            let off = i * 1500;
            data[off..off + 1000].fill(i as u8);
            let mut hash = [0u8; 32];
            hash[0] = i as u8;
            hash[1] = (i >> 8) as u8;
            chunks.push(panpak_core::Chunk { hash, off: off as u64, comp: 1000, raw: 1000 });
        }
        (data, chunks)
    }

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("panpak-ranges-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn fetcher(port: u16, total: usize) -> impl Fn(u64, u64) -> Result<Vec<u8>> + Sync {
        let url = format!("http://127.0.0.1:{port}/p.panpak");
        let agent = agent();
        move |start, end| {
            let (b, t) = crate::http_range(&agent, &url, start, end - 1)?;
            ensure!(t == total as u64, "file changed during download");
            Ok(b)
        }
    }

    fn all_there(staging: &Path, chunks: &[panpak_core::Chunk]) {
        for c in chunks {
            let got = fs::read(staging.join(panpak_core::hex(&c.hash))).unwrap();
            assert_eq!(got, vec![c.hash[0]; 1000], "the chunk {}", c.hash[0]);
        }
    }

    #[test]
    fn neighbours_come_in_one_request_and_several_requests_are_in_flight_together() {
        let (data, chunks) = package(60);
        let total = data.len();
        let srv = serve(data, Duration::from_millis(40), None, None);
        let dir = tmpdir("parallel");
        // Every other chunk: the wanted ranges are 2,000 bytes apart, so with a gap of 100 each is a request, and with the real gap they are one.
        let wanted: Vec<_> = chunks.iter().step_by(2).cloned().collect();
        let (n, bytes) = fetch_chunks(&dir, &wanted, total as u64, Policy { workers: 6, gap: 100, tries: 1 }, &fetcher(srv.port, total)).unwrap();
        assert_eq!(n, 30);
        assert_eq!(bytes, 30 * 1000);
        assert_eq!(srv.requests.load(Ordering::SeqCst), 30, "no joining at a gap of 100 bytes");
        let most = srv.most_in_flight.load(Ordering::SeqCst);
        assert!((2..=6).contains(&most), "{most} requests at the same time");
        all_there(&dir, &wanted);
        // The real gap: 2,000 bytes between the wanted chunks is less than 16 KiB, so the whole run is one request (the bytes between come too).
        let dir2 = tmpdir("joined");
        let before = srv.requests.load(Ordering::SeqCst);
        let (n, bytes) = fetch_chunks(&dir2, &wanted, total as u64, Policy::default(), &fetcher(srv.port, total)).unwrap();
        assert_eq!(n, 30);
        assert_eq!(srv.requests.load(Ordering::SeqCst) - before, 1);
        assert_eq!(bytes, 58 * 1500 + 1000);
        all_there(&dir2, &wanted);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&dir2);
    }

    #[test]
    fn a_busy_server_is_waited_for_and_the_rest_goes_one_at_a_time() {
        for (status, retry_after) in [(429u16, Some("1")), (503, None)] {
            let (data, chunks) = package(40);
            let total = data.len();
            let srv = serve(data, Duration::from_millis(30), Some(Busy { status, retry_after, first: 2 }), None);
            let dir = tmpdir(&format!("busy{status}"));
            let wanted: Vec<_> = chunks.iter().step_by(2).cloned().collect();
            let t = Instant::now();
            let (n, _) = fetch_chunks(&dir, &wanted, total as u64, Policy { workers: 6, gap: 100, tries: 4 }, &fetcher(srv.port, total)).unwrap();
            assert_eq!(n, 20);
            all_there(&dir, &wanted);
            assert!(t.elapsed() >= Duration::from_secs(1), "the wait that the server asked for (or the first step of the backoff) was kept: {:?}", t.elapsed());
            assert_eq!(srv.requests.load(Ordering::SeqCst), 20 + 2, "the two refused ranges were asked again, nothing else twice");
            assert!(srv.most_in_flight.load(Ordering::SeqCst) >= 2, "it started in parallel");
            assert!(srv.most_after_busy.load(Ordering::SeqCst) <= 1, "after the busy answer, one request at a time (status {status})");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_server_that_stays_busy_ends_the_download_with_a_message_and_what_came_is_kept() {
        let (data, chunks) = package(10);
        let total = data.len();
        let srv = serve(data, Duration::from_millis(5), Some(Busy { status: 429, retry_after: Some("0"), first: 1000 }), None);
        let dir = tmpdir("stays-busy");
        let e = fetch_chunks(&dir, &chunks[..4], total as u64, Policy { workers: 2, gap: 100, tries: 3 }, &fetcher(srv.port, total)).unwrap_err();
        assert!(format!("{e:#}").contains("stayed busy"), "{e:#}");
        // A server that asks for a very long wait is not waited for.
        let t = Throttled { status: 503, retry_after: Some(Duration::from_secs(3600)) };
        assert!(Pace::new().slow_down(&t).unwrap_err().to_string().contains("try again later"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrong_content_range_in_one_request_stops_the_download_and_the_next_run_asks_only_for_the_rest() {
        let (data, chunks) = package(12);
        let total = data.len();
        let srv = serve(data.clone(), Duration::from_millis(5), None, Some(3));
        let dir = tmpdir("wrong");
        let wanted: Vec<_> = chunks.iter().step_by(2).cloned().collect();
        let e = fetch_chunks(&dir, &wanted, total as u64, Policy { workers: 3, gap: 100, tries: 1 }, &fetcher(srv.port, total)).unwrap_err();
        assert!(format!("{e:#}").contains("Content-Range"), "{e:#}");
        let kept = fs::read_dir(&dir).unwrap().flatten().filter(|e| e.path().extension().is_none()).count();
        assert!(kept < wanted.len(), "not everything came");
        // The next run, with a server that answers well, asks only for what is not there.
        let srv2 = serve(data, Duration::from_millis(1), None, None);
        let (n, _) = fetch_chunks(&dir, &wanted, total as u64, Policy { workers: 3, gap: 100, tries: 1 }, &fetcher(srv2.port, total)).unwrap();
        assert_eq!(n as usize, wanted.len() - kept);
        assert_eq!(srv2.requests.load(Ordering::SeqCst), wanted.len() - kept);
        all_there(&dir, &wanted);
        let _ = fs::remove_dir_all(&dir);
    }
}
