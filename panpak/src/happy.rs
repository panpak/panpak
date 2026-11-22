// SPDX-License-Identifier: MIT OR Apache-2.0
//! Name resolution for the downloads, with the address chosen the way RFC 8305 (Happy Eyeballs) does it.
//!
//! The resolver of `ureq` hands every address of a host to the connector, which tries them one after the other; an IPv6 address that is
//! not reachable (a network with an address and no route) holds the connection for the whole connect timeout, and the IPv4 address that works
//! is never tried. Here the addresses are put in turn (IPv6, IPv4, IPv6, ...), a connection to the first is started, a connection to the next
//! is started 250 ms later (at once if the first has failed), and the first that opens is the one the download uses. A connection that does not
//! open in a few seconds is given up. The race opens one extra connection; the download opens its own to the winner.
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

/// Wait this long for an attempt before starting the next one (the RFC suggests 250 ms).
pub const STAGGER: Duration = Duration::from_millis(250);
/// Give an address this long to open a connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

/// The addresses in the order of the attempts: IPv6 first, then alternating families, each family in the order the resolver gave.
pub fn interleave(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
    let (v6, v4): (Vec<SocketAddr>, Vec<SocketAddr>) = addrs.iter().partition(|a| a.is_ipv6());
    let (mut a, mut b) = (v6.into_iter(), v4.into_iter());
    let mut out = Vec::with_capacity(addrs.len());
    loop {
        match (a.next(), b.next()) {
            (None, None) => return out,
            (x, y) => out.extend(x.into_iter().chain(y)),
        }
    }
}

/// Start an attempt per address (in the order given), `stagger` apart, and return the first address that connects. `connect` is the
/// attempt itself. If none connects, the error of the last attempt that failed.
pub fn race<F>(addrs: &[SocketAddr], stagger: Duration, patience: Duration, connect: F) -> io::Result<SocketAddr>
where
    F: Fn(SocketAddr) -> io::Result<()> + Send + Sync + 'static,
{
    if addrs.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "the host has no address"));
    }
    let connect = Arc::new(connect);
    let (tx, rx) = mpsc::channel::<(SocketAddr, io::Result<()>)>();
    let (mut started, mut done) = (0usize, 0usize);
    let mut last_err = io::Error::new(io::ErrorKind::TimedOut, "no address answered");
    let give_up = Instant::now() + patience + stagger * addrs.len() as u32;
    while done < addrs.len() {
        if started < addrs.len() {
            let (addr, tx, connect) = (addrs[started], tx.clone(), connect.clone());
            started += 1;
            std::thread::spawn(move || {
                let _ = tx.send((addr, connect(addr)));
            });
        }
        // Wait for an answer; with more addresses to try, only until the stagger has passed.
        let wait = if started < addrs.len() { stagger } else { give_up.saturating_duration_since(Instant::now()) };
        loop {
            match rx.recv_timeout(wait) {
                Ok((addr, Ok(()))) => return Ok(addr),
                Ok((_, Err(e))) => {
                    done += 1;
                    last_err = e;
                    if started < addrs.len() {
                        break; // the next attempt starts at once
                    }
                    if done == addrs.len() {
                        return Err(last_err);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if started >= addrs.len() {
                        return Err(last_err);
                    }
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(last_err),
            }
        }
    }
    Err(last_err)
}

/// The resolver of the downloads: the addresses of the host, one of them chosen by `race`.
#[derive(Debug)]
pub struct HappyEyeballs;

impl Resolver for HappyEyeballs {
    fn resolve(&self, uri: &Uri, config: &Config, timeout: NextTimeout) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let all = DefaultResolver::default().resolve(uri, config, timeout)?;
        let addrs = interleave(&all.iter().copied().collect::<Vec<_>>());
        let winner = match addrs.len() {
            // A single address: nothing to choose, no extra connection.
            1 => addrs[0],
            _ => race(&addrs, STAGGER, CONNECT_TIMEOUT, |a| TcpStream::connect_timeout(&a, CONNECT_TIMEOUT).map(drop)).map_err(ureq::Error::Io)?,
        };
        let mut one: ResolvedSocketAddrs = self.empty();
        one.push(winner);
        Ok(one)
    }
}

/// An agent for the downloads of builds: HTTPS only, a few redirects, the resolver above.
pub fn agent(timeout_secs: u64) -> ureq::Agent {
    let config = Config::builder().https_only(true).max_redirects(5).timeout_global(Some(Duration::from_secs(timeout_secs))).build();
    ureq::Agent::with_parts(config, DefaultConnector::default(), HappyEyeballs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn a(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn families_alternate_starting_with_ipv6() {
        let all = [a("192.0.2.1:443"), a("192.0.2.2:443"), a("[2001:db8::1]:443"), a("[2001:db8::2]:443"), a("[2001:db8::3]:443")];
        assert_eq!(interleave(&all), [a("[2001:db8::1]:443"), a("192.0.2.1:443"), a("[2001:db8::2]:443"), a("192.0.2.2:443"), a("[2001:db8::3]:443")]);
        assert_eq!(interleave(&[a("192.0.2.1:443")]), [a("192.0.2.1:443")]);
        assert!(interleave(&[]).is_empty());
    }

    /// Run `f` on a thread of its own and fail with a message when it does not return in 60 s. The tests below do not read the clock: a
    /// test that waits for a time (or a machine that stalls for some seconds) must not decide whether `race` is right, and a `race` that waits for
    /// the wrong thing must end as a failure, not as a hang.
    fn with_deadline<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(60)).unwrap_or_else(|_| panic!("{what}: no answer in 60 s"))
    }

    #[test]
    fn a_hung_ipv6_address_does_not_hold_back_the_working_ipv4_one() {
        // The attempt at the IPv6 address waits until the attempt at the IPv4 address has been made (it is released by it, or gives up after 60 s)
        // and then fails. The IPv4 attempt must be made while the IPv6 one is still waiting: the order of the events is checked, not a time.
        let (release, wait) = mpsc::channel::<()>();
        let wait = std::sync::Mutex::new(wait);
        let v6_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let v4_saw_v6_waiting = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (d, saw) = (v6_done.clone(), v4_saw_v6_waiting.clone());
        let got = with_deadline("a hung IPv6 address", move || {
            race(&[a("[2001:db8::1]:443"), a("192.0.2.1:443")], Duration::from_millis(100), Duration::from_secs(120), move |addr| {
                if addr.is_ipv6() {
                    let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(60));
                    d.store(true, std::sync::atomic::Ordering::SeqCst);
                    Err(io::Error::new(io::ErrorKind::TimedOut, "no route"))
                } else {
                    saw.store(!d.load(std::sync::atomic::Ordering::SeqCst), std::sync::atomic::Ordering::SeqCst);
                    let _ = release.send(());
                    Ok(())
                }
            })
        })
        .unwrap();
        assert_eq!(got, a("192.0.2.1:443"));
        assert!(v4_saw_v6_waiting.load(std::sync::atomic::Ordering::SeqCst), "the IPv4 attempt was made only after the IPv6 one had ended");
    }

    #[test]
    fn a_failed_attempt_starts_the_next_at_once_and_a_host_with_one_family_works() {
        // A stagger of an hour: when the next attempt does not start at the failure of the first, but at the end of the stagger, the deadline ends the test.
        let got = with_deadline("a failed attempt", || {
            race(&[a("[2001:db8::1]:443"), a("192.0.2.1:443")], Duration::from_secs(3600), Duration::from_secs(5), |addr| {
                if addr.is_ipv6() { Err(io::Error::from(io::ErrorKind::ConnectionRefused)) } else { Ok(()) }
            })
        })
        .unwrap();
        assert_eq!(got, a("192.0.2.1:443"));
        // Only IPv4 (a host with no AAAA record).
        assert_eq!(race(&[a("192.0.2.1:443")], STAGGER, CONNECT_TIMEOUT, |_| Ok(())).unwrap(), a("192.0.2.1:443"));
        // The first answer wins even if it comes late and the others fail.
        let late = race(&[a("[2001:db8::1]:443"), a("192.0.2.1:443")], Duration::from_millis(50), Duration::from_secs(5), |addr| {
            if addr.is_ipv6() { std::thread::sleep(Duration::from_millis(300)); Ok(()) } else { Err(io::Error::from(io::ErrorKind::ConnectionRefused)) }
        });
        assert_eq!(late.unwrap(), a("[2001:db8::1]:443"));
    }

    #[test]
    fn nothing_answers_is_an_error_not_a_hang() {
        let e = with_deadline("no answer at all", || {
            race(&[a("[2001:db8::1]:443"), a("192.0.2.1:443")], Duration::from_millis(50), Duration::from_secs(5), |_| Err(io::Error::from(io::ErrorKind::ConnectionRefused))).unwrap_err()
        });
        assert_eq!(e.kind(), io::ErrorKind::ConnectionRefused);
        assert!(race(&[], STAGGER, CONNECT_TIMEOUT, |_| Ok(())).is_err());
    }

    #[test]
    fn with_real_sockets_an_unreachable_ipv6_and_a_listening_ipv4() {
        // 2001:db8::/32 is reserved for documentation: nothing answers there (or the system has no route and the attempt fails at once). The attempt at
        // the IPv6 address may wait for 300 s: a `race` that waited for it would end at the deadline of the test.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let got = with_deadline("an unreachable IPv6 address", move || {
            race(&[a(&format!("[2001:db8::1]:{port}")), a(&format!("127.0.0.1:{port}"))], Duration::from_millis(250), Duration::from_secs(400), |addr| {
                TcpStream::connect_timeout(&addr, Duration::from_secs(if addr.is_ipv6() { 300 } else { 4 })).map(drop)
            })
        })
        .unwrap();
        assert_eq!(got, a(&format!("127.0.0.1:{port}")));
    }
}
