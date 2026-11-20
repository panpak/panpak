// SPDX-License-Identifier: MIT OR Apache-2.0
//! Asking the domain of an author to confirm a key (PANPAK-FORMAT.md §3.2.1). The rules for what the file says are in `panpak_core::domain`; this is the request:
//! HTTPS only, one address (`https://<domain>/.well-known/panpak.toml`), a small file, a short time, and no redirect to another host.

use panpak_core::domain;
use std::io::Read;
use std::net::SocketAddr;
use std::time::Duration;
use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

/// How long the whole request may take.
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// How many redirects inside the same host are followed.
const MAX_HOPS: usize = 3;

/// Why the file could not be had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The network, the certificate, a status that is not 200, a time out: said in a few words.
    Unreachable(String),
    /// A redirect to another host, to a scheme that is not https, or too many.
    Redirected,
    TooBig,
}

/// Test builds only (the feature `failpoints`, like the test points; a release has none, and `scripts/check-no-failpoints.sh` looks for the name in it): the environment
/// variable `PANPAK_TEST_RESOLVE=<domain>=<ip>:<port>` makes the connection for that name go to a local server, while the name in the address and in the certificate stays the
/// domain. It is a way for a test to reach a server of its own, and a way for whoever sets the environment to send the check elsewhere, which is why a release does not read it.
#[cfg(feature = "failpoints")]
fn test_target() -> Option<(String, SocketAddr)> {
    let v = std::env::var("PANPAK_TEST_RESOLVE").ok()?;
    let (name, addr) = v.split_once('=')?;
    Some((name.to_string(), addr.parse().ok()?))
}

#[cfg(not(feature = "failpoints"))]
fn test_target() -> Option<(String, SocketAddr)> {
    None
}

#[derive(Debug)]
struct DomainResolver {
    test: Option<(String, SocketAddr)>,
}

impl Resolver for DomainResolver {
    fn resolve(&self, uri: &Uri, config: &Config, timeout: NextTimeout) -> Result<ResolvedSocketAddrs, ureq::Error> {
        if let (Some((name, addr)), Some(host)) = (&self.test, uri.host()) {
            if name.eq_ignore_ascii_case(host) {
                let mut one = self.empty();
                one.push(*addr);
                return Ok(one);
            }
        }
        crate::happy::HappyEyeballs.resolve(uri, config, timeout)
    }
}

/// The file of `domain`, as text. `tls` is the trust that the settings give (`network.ca_file`), or the built-in list.
pub fn fetch(domain: &str, tls: Option<ureq::tls::TlsConfig>) -> Result<String, Failure> {
    let mut builder = Config::builder()
        .https_only(true)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .user_agent("panpak");
    if let Some(t) = tls {
        builder = builder.tls_config(t);
    }
    let agent = ureq::Agent::with_parts(builder.build(), DefaultConnector::default(), DomainResolver { test: test_target() });
    let mut url = domain::url_of(domain);
    for _ in 0..=MAX_HOPS {
        let resp = agent.get(&url).call().map_err(|e| Failure::Unreachable(short(&e.to_string())))?;
        let status = resp.status().as_u16();
        if (301..=308).contains(&status) && status != 304 && status != 306 {
            let loc = resp.headers().get("location").and_then(|l| l.to_str().ok()).ok_or(Failure::Redirected)?;
            url = same_host(&url, loc).ok_or(Failure::Redirected)?;
            continue;
        }
        if status != 200 {
            return Err(Failure::Unreachable(format!("HTTP {status}")));
        }
        if resp.headers().get("content-length").and_then(|l| l.to_str().ok()).and_then(|l| l.parse::<usize>().ok()).is_some_and(|n| n > domain::MAX_FILE) {
            return Err(Failure::TooBig);
        }
        let mut body = Vec::new();
        resp.into_body().into_reader().take(domain::MAX_FILE as u64 + 1).read_to_end(&mut body).map_err(|e| Failure::Unreachable(short(&e.to_string())))?;
        if body.len() > domain::MAX_FILE {
            return Err(Failure::TooBig);
        }
        return String::from_utf8(body).map_err(|_| Failure::Unreachable("the file is not UTF-8".into()));
    }
    Err(Failure::Redirected)
}

/// The address a redirect leads to, when it stays on the host of `from` and on https.
fn same_host(from: &str, location: &str) -> Option<String> {
    let base: Uri = from.parse().ok()?;
    let target: Uri = if location.starts_with("https://") {
        location.parse().ok()?
    } else if location.starts_with('/') && !location.starts_with("//") {
        format!("https://{}{location}", base.authority()?).parse().ok()?
    } else {
        return None;
    };
    // The address is made again from the authority that was asked, so that the case of the host in the answer changes nothing.
    (target.scheme_str() == Some("https") && target.authority()?.as_str().eq_ignore_ascii_case(base.authority()?.as_str()))
        .then(|| format!("https://{}{}", base.authority().map(|a| a.as_str()).unwrap_or_default(), target.path_and_query().map_or("/", |p| p.as_str())))
}

fn short(s: &str) -> String {
    let one: String = s.chars().filter(|c| !c.is_control()).take(120).collect();
    one
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redirect_stays_on_the_host_and_on_https() {
        let from = "https://example.org/.well-known/panpak.toml";
        assert_eq!(same_host(from, "/other/panpak.toml").as_deref(), Some("https://example.org/other/panpak.toml"));
        assert_eq!(same_host(from, "https://EXAMPLE.org/x").as_deref(), Some("https://example.org/x"));
        for bad in ["https://evil.example/x", "http://example.org/x", "//evil.example/x", "https://example.org:8443/x", "https://example.org.evil.example/x", "x", ""] {
            assert_eq!(same_host(from, bad), None, "{bad}");
        }
    }
}
