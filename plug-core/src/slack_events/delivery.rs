use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use axum::http::HeaderMap;
use url::Url;

use super::{DeliveryResponse, EventDelivery};

pub struct HttpsDelivery;

pub(super) fn validate_url(value: &str) -> Result<Url, &'static str> {
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
pub(super) fn public_address(address: IpAddr) -> bool {
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
            let client = reqwest::Client::builder()
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
