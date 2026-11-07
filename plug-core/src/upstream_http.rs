//! What every HTTP request Plug sends to an upstream has in common: the
//! connect bound, the client builder, the hosts it refuses, and what counts
//! as the same origin.

use std::time::Duration;

/// How long an upstream HTTP connect may take before it is abandoned.
///
/// DNS, TCP and TLS to a reachable host settle well inside a second even
/// across an ocean, so a connect still running after this is not slow, it is
/// not happening. Without a bound it runs until the per-server start timeout
/// expires — thirty seconds by default — and startup readiness waits on it,
/// which is how one unreachable remote server delays every other one.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A client for upstream traffic, following redirects by `redirect`.
pub(crate) fn client(redirect: reqwest::redirect::Policy) -> anyhow::Result<reqwest::Client> {
    crate::tls::http_client_builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(redirect)
        .build()
        .map_err(|error| anyhow::anyhow!("could not build the HTTP client: {error}"))
}

/// Whether `host` is a cloud metadata endpoint, which no upstream may be.
///
/// Loopback and private addresses are allowed: every server is one the owner
/// configured, and local servers are common. A hostname that resolves to a
/// metadata address is not caught; that would need resolving at connect time.
pub(crate) fn is_blocked_host(host: &str) -> bool {
    if host == "metadata.google.internal" {
        return true;
    }
    let host = host.trim_start_matches('[').trim_end_matches(']');
    // AWS, GCP and Azure all answer on this one address.
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.to_canonical() == std::net::IpAddr::from([169, 254, 169, 254]))
}

/// Refuse `url` when its host is a cloud metadata endpoint.
pub(crate) fn refuse_blocked(url: &reqwest::Url) -> anyhow::Result<()> {
    match url.host_str() {
        Some(host) if is_blocked_host(host) => {
            anyhow::bail!("URL host '{host}' is blocked: cloud metadata endpoint")
        }
        _ => Ok(()),
    }
}

/// Whether `a` and `b` share scheme, host and port, counting a default port
/// written out the same as one left implicit.
pub(crate) fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_cloud_metadata_hosts_are_blocked() {
        for host in [
            "169.254.169.254",
            "[169.254.169.254]",
            "[::ffff:169.254.169.254]",
            "metadata.google.internal",
        ] {
            assert!(is_blocked_host(host), "{host}");
        }
        for host in [
            "127.0.0.1",
            "[::1]",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.0.1",
            "8.8.8.8",
            "example.com",
            "localhost",
        ] {
            assert!(!is_blocked_host(host), "{host}");
        }
    }

    #[test]
    fn a_written_default_port_is_the_same_origin() {
        let url = |s: &str| reqwest::Url::parse(s).unwrap();
        assert!(same_origin(
            &url("https://a.example/x"),
            &url("https://a.example:443/y")
        ));
        assert!(!same_origin(
            &url("https://a.example/x"),
            &url("http://a.example/x")
        ));
        assert!(!same_origin(
            &url("https://a.example/x"),
            &url("https://a.example:8443/x")
        ));
        assert!(!same_origin(
            &url("https://a.example/x"),
            &url("https://b.example/x")
        ));
    }
}
