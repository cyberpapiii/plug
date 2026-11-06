//! Webhook delivery: Standard Webhooks signing and the HTTPS sender.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use axum::http::HeaderMap;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use url::Url;

use super::{EventError, invalid};

pub struct DeliveryResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[allow(
    clippy::double_must_use,
    reason = "async_trait generates redundant must_use on boxed futures"
)]
#[async_trait::async_trait]
pub trait EventDelivery: Send + Sync {
    /// POST `body` with `headers` to the subscriber's `url` once, and say
    /// how it answered. An `Err` names why nothing was delivered.
    async fn post(
        &self,
        url: &str,
        headers: HeaderMap,
        body: Vec<u8>,
    ) -> Result<DeliveryResponse, &'static str>;
}

/// Pins DNS per connection, keeps TLS hostname verification, follows no
/// proxy or redirect, and bounds the response it reads.
pub struct HttpsDelivery;

pub(crate) fn validate_url(value: &str) -> Result<Url, &'static str> {
    let url = Url::parse(value).map_err(|_| "invalid_url")?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default() != Some(443)
        || url.host_str().is_none()
    {
        return Err("invalid_url");
    }
    if let Some(host) = url.host_str()
        && let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>()
        && !public_address(ip)
    {
        return Err("invalid_url");
    }
    Ok(url)
}

// Conservative public-unicast policy. Reject all transition/mapped IPv6 forms,
// documentation, shared, reserved, benchmark and special-purpose IPv4 ranges.
fn public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => public_v6(ip),
    }
}
fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(matches!(a, 0 | 10 | 127)
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && (b == 168 || (b == 0 && matches!(c, 0 | 2)) || (b == 88 && c == 99)))
        || (a == 198 && (matches!(b, 18 | 19) || (b == 51 && c == 100)))
        || (a == 203 && b == 0 && c == 113))
}
fn public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    (s[0] & 0xe000) == 0x2000
        && s[0] != 0x2002
        && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
        && !(s[0] == 0x3fff && s[1] < 0x1000)
}

#[async_trait::async_trait]
impl EventDelivery for HttpsDelivery {
    async fn post(
        &self,
        value: &str,
        headers: HeaderMap,
        body: Vec<u8>,
    ) -> Result<DeliveryResponse, &'static str> {
        // Overall deadline includes DNS, TLS, send, and bounded response collection.
        tokio::time::timeout(Duration::from_secs(10), async {
            let url = validate_url(value)?;
            let host = url
                .host_str()
                .ok_or("invalid_url")?
                .trim_matches(['[', ']']);
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, 443))
                .await
                .map_err(|_| "dns_failed")?
                .collect();
            if addresses.is_empty() || addresses.iter().any(|a| !public_address(a.ip())) {
                return Err("invalid_url");
            }
            // A new non-pooled client pins this connection's resolution. Keep the
            // URL hostname for SNI/certificate validation. Never use system proxies
            // (which could perform an unvalidated second DNS lookup) or redirects.
            let client = crate::tls::http_client_builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(host, &addresses)
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|_| "transport_unavailable")?;
            let mut response = client
                .post(url)
                .headers(headers)
                .body(body)
                .send()
                .await
                .map_err(|_| "delivery_failed")?;
            let status = response.status().as_u16();
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| "delivery_failed")? {
                if bytes.len() + chunk.len() > 4096 {
                    return Err("response_too_large");
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(DeliveryResponse {
                status,
                body: bytes,
            })
        })
        .await
        .map_err(|_| "timeout")?
    }
}

/// The key in a Standard Webhooks `whsec_` secret.
pub(crate) fn signing_key(secret: &str) -> Result<Vec<u8>, EventError> {
    let key = secret
        .strip_prefix("whsec_")
        .and_then(|s| STANDARD.decode(s).ok())
        .ok_or_else(|| invalid("invalid webhook signing secret"))?;
    if !(24..=64).contains(&key.len()) {
        return Err(invalid("invalid webhook signing secret length"));
    }
    Ok(key)
}

pub(crate) fn hmac_sha256(key: &[u8], body: &[u8]) -> Vec<u8> {
    let mut hmac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    hmac.update(body);
    hmac.finalize().into_bytes().to_vec()
}

/// Standard Webhooks headers for one delivery. While a rotated-out secret is
/// still valid, `old` signs too, so the receiver accepts either.
pub(crate) fn signed_headers(
    subscription: &str,
    event_id: &str,
    secret: &str,
    old: Option<&str>,
    body: &[u8],
    timestamp: u64,
) -> Result<HeaderMap, EventError> {
    let mut signing = format!("{event_id}.{timestamp}.").into_bytes();
    signing.extend_from_slice(body);
    let mut signatures = format!(
        "v1,{}",
        STANDARD.encode(hmac_sha256(&signing_key(secret)?, &signing))
    );
    if let Some(old) = old {
        signatures.push_str(&format!(
            " v1,{}",
            STANDARD.encode(hmac_sha256(&signing_key(old)?, &signing))
        ));
    }
    let mut headers = HeaderMap::new();
    for (name, value) in [
        ("content-type", "application/json".to_owned()),
        ("webhook-id", event_id.to_owned()),
        ("webhook-timestamp", timestamp.to_string()),
        ("webhook-signature", signatures),
        ("x-mcp-subscription-id", subscription.to_owned()),
    ] {
        headers.insert(
            axum::http::header::HeaderName::from_static(name),
            value
                .parse()
                .map_err(|_| invalid("invalid webhook header"))?,
        );
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_callback_policy_rejects_internal_targets() {
        for url in [
            "http://example.com/",
            "https://127.0.0.1/",
            "https://10.1.2.3/",
            "https://[::1]/",
            "https://[::ffff:8.8.8.8]/",
            "https://169.254.169.254/",
            "https://user:pass@example.com/",
            "https://example.com:8443/",
            "https://example.com/#fragment",
        ] {
            assert!(validate_url(url).is_err(), "{url}");
        }
        assert!(validate_url("https://example.com/callback").is_ok());
        assert!(public_address("8.8.8.8".parse().unwrap()));
        assert!(!public_address("192.0.2.1".parse().unwrap()));
    }

    #[test]
    fn standard_webhooks_signature_matches_independent_vector() {
        let secret = format!("whsec_{}", STANDARD.encode([7u8; 32]));
        let headers = signed_headers(
            "sub_test",
            "Ev1",
            &secret,
            None,
            br#"{"sample":true}"#,
            1700000000,
        )
        .unwrap();
        assert_eq!(
            headers["webhook-signature"],
            "v1,GZNGj6rLIWZ4DUblEE79gjt6eROPCI+z4HTZS5mqJwk="
        );
    }
}
