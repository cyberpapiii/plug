//! What every HTTP request Plug sends to an upstream has in common: the
//! connect bound, the client builder, the hosts it refuses, and what counts
//! as the same origin.

use std::time::Duration;

use rmcp::transport::auth::{
    OAuthHttpClient, OAuthHttpClientFuture, OAuthHttpRedirectPolicy, OAuthHttpRequest,
};

/// How long an upstream HTTP connect may take before it is abandoned.
///
/// DNS, TCP and TLS to a reachable host settle well inside a second even
/// across an ocean, so a connect still running after this is not slow, it is
/// not happening. Without a bound it runs until the per-server start timeout
/// expires — thirty seconds by default — and startup readiness waits on it,
/// which is how one unreachable remote server delays every other one.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Which redirects an upstream client follows. None of them follows a
/// redirect to a blocked host, so a server cannot reach one by redirecting.
#[derive(Clone, Copy)]
pub(crate) enum Redirects {
    Never,
    SameOrigin,
    AnyHost,
}

/// How many redirects one request follows, as reqwest's default does.
const MAX_REDIRECTS: usize = 10;

/// A client for upstream traffic, following redirects by `redirects`.
pub(crate) fn client(redirects: Redirects) -> anyhow::Result<reqwest::Client> {
    crate::tls::http_client_builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(redirect_policy(redirects))
        .build()
        .map_err(|error| anyhow::anyhow!("could not build the HTTP client: {error}"))
}

fn redirect_policy(redirects: Redirects) -> reqwest::redirect::Policy {
    if matches!(redirects, Redirects::Never) {
        return reqwest::redirect::Policy::none();
    }
    reqwest::redirect::Policy::custom(move |attempt| {
        if let Some(host) = attempt
            .url()
            .host_str()
            .filter(|host| is_blocked_host(host))
        {
            let refusal = format!("redirect to '{host}' is blocked: cloud metadata endpoint");
            return attempt.error(refusal);
        }
        if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        // A redirect may not carry a request, and its credential, anywhere
        // but the address the server is configured for: not another port on
        // the host, and not the same address without TLS.
        let leaves_origin = !attempt
            .previous()
            .last()
            .is_some_and(|previous| same_origin(previous, attempt.url()));
        if matches!(redirects, Redirects::SameOrigin) && leaves_origin {
            return attempt.stop();
        }
        attempt.follow()
    })
}

/// The client rmcp's OAuth manager sends discovery, registration and token
/// requests through. rmcp asks for some of them not to follow redirects.
pub(crate) fn oauth_client() -> anyhow::Result<std::sync::Arc<dyn OAuthHttpClient>> {
    Ok(std::sync::Arc::new(OAuthClient {
        follow: client(Redirects::AnyHost)?,
        stop: client(Redirects::Never)?,
    }))
}

/// The most of an OAuth response body read, as rmcp's own client reads.
const MAX_OAUTH_BODY_BYTES: usize = 1024 * 1024;

struct OAuthClient {
    follow: reqwest::Client,
    stop: reqwest::Client,
}

impl OAuthHttpClient for OAuthClient {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        Box::pin(async move {
            use futures::StreamExt as _;
            let OAuthHttpRequest {
                request,
                redirect_policy,
                timeout,
                ..
            } = request;
            let client = match redirect_policy {
                OAuthHttpRedirectPolicy::Stop => &self.stop,
                _ => &self.follow,
            };
            let mut request = reqwest::Request::try_from(request)?;
            *request.timeout_mut() = timeout;
            let response = client.execute(request).await?;
            let mut builder = http::Response::builder()
                .status(response.status())
                .version(response.version());
            for (name, value) in response.headers() {
                builder = builder.header(name, value);
            }
            let mut body = Vec::new();
            let mut chunks = response.bytes_stream();
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk?;
                if chunk.len() > MAX_OAUTH_BODY_BYTES - body.len() {
                    return Err(format!(
                        "OAuth response body exceeds {MAX_OAUTH_BODY_BYTES} bytes"
                    )
                    .into());
                }
                body.extend_from_slice(&chunk);
            }
            Ok(builder.body(body)?)
        })
    }
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

    /// Serve one route that redirects to `location`. Returns its URL.
    async fn redirecting_to(location: &'static str) -> String {
        let router = axum::Router::new().route(
            "/",
            axum::routing::get(move || async move {
                (
                    axum::http::StatusCode::FOUND,
                    [(axum::http::header::LOCATION, location)],
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://127.0.0.1:{port}/")
    }

    #[tokio::test]
    async fn no_redirect_reaches_a_cloud_metadata_endpoint() {
        for location in [
            "http://169.254.169.254/latest/meta-data/",
            "http://[::ffff:169.254.169.254]/latest/meta-data/",
        ] {
            let url = redirecting_to(location).await;
            for redirects in [Redirects::AnyHost, Redirects::SameOrigin] {
                let error = client(redirects)
                    .unwrap()
                    .get(&url)
                    .timeout(Duration::from_secs(5))
                    .send()
                    .await
                    .expect_err("the redirect is refused");
                assert!(error.is_redirect(), "{location}: {error:?}");
                assert!(
                    format!("{error:?}").contains("cloud metadata endpoint"),
                    "{location}: {error:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_same_origin_client_stops_at_a_redirect_to_another_host() {
        let url = redirecting_to("http://example.invalid/").await;
        let response = client(Redirects::SameOrigin)
            .unwrap()
            .get(&url)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
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
