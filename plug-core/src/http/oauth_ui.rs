use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest as _, Sha256};

use crate::downstream_oauth::{AUTH_REQUEST_LIFETIME_SECS, ClientSource, ConsentRequest, paths};

/// Each page's script, after the passkey and request helpers both share.
pub const CONSENT_JAVASCRIPT: &str = concat!(
    include_str!("oauth_ui/passkey.js"),
    include_str!("oauth_ui/consent.js")
);
pub const ENROLL_JAVASCRIPT: &str = concat!(
    include_str!("oauth_ui/passkey.js"),
    include_str!("oauth_ui/enroll.js")
);
pub const STYLESHEET: &str = include_str!("oauth_ui/plug.css");
pub const ICON: &str = include_str!("../../../docs/assets/plug-icon.svg");
pub const JAVASCRIPT_TYPE: &str = "application/javascript; charset=utf-8";
pub const STYLESHEET_TYPE: &str = "text/css; charset=utf-8";
pub const ICON_TYPE: &str = "image/svg+xml";

const HTML_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";
const ASSET_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// The headers every OAuth response carries, page or JSON.
pub fn apply_oauth_security_headers(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "Content-Security-Policy",
        HeaderValue::from_static(HTML_SECURITY_POLICY),
    );
    response
        .headers_mut()
        .insert("Referrer-Policy", HeaderValue::from_static("no-referrer"));
    response.headers_mut().insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("X-Frame-Options", HeaderValue::from_static("DENY"));
}

pub fn consent_page(consent: &ConsentRequest, owner_enrolled: bool) -> Response {
    let client_name = html_escape(&consent.client_name);
    let identity = match consent.client_source {
        ClientSource::MetadataDocument => url::Url::parse(&consent.client_id)
            .ok()
            .and_then(|url| url.host_str().map(ToString::to_string))
            .map(|host| format!("Verified client identity: {}", html_escape(&host)))
            .unwrap_or_else(|| "Verified client metadata".to_string()),
        ClientSource::DynamicRegistration => "Unverified dynamically registered client".to_string(),
    };
    let scopes = consent
        .scopes
        .iter()
        .map(|scope| {
            format!(
                "<li>{} <code>{}</code></li>",
                html_escape(scope_description(scope)),
                html_escape(scope)
            )
        })
        .collect::<String>();
    let callback_url = url::Url::parse(&consent.redirect_uri).ok();
    let callback_destination = callback_url
        .as_ref()
        .and_then(callback_authority)
        .unwrap_or_else(|| consent.redirect_host.clone());
    let callback_warning = callback_url
        .as_ref()
        .and_then(|url| {
            let host = url.host_str()?;
            crate::config::is_loopback_host(host).then(|| {
                format!(
                    "<p><strong>Local app callback:</strong> This approval returns to <code>{}</code> on this device.</p>",
                    html_escape(&callback_destination)
                )
            })
        })
        .unwrap_or_default();
    let allow = if owner_enrolled {
        "<button id=\"allow\" type=\"button\">Allow with Touch ID or passkey</button>"
    } else {
        "<section aria-labelledby=\"setup-heading\"><h2 id=\"setup-heading\">Owner passkey required</h2><p>On the Mac running Plug, run <code>plug auth owner enroll</code>, then start this connection again.</p></section>"
    };
    let script_url = html_escape(&versioned_asset_url(
        paths::CONSENT_SCRIPT,
        CONSENT_JAVASCRIPT,
    ));
    let lifetime_minutes = AUTH_REQUEST_LIFETIME_SECS / 60;
    let head = page_head(&format!("Allow {client_name} to use Plug?"));
    let html = format!(
        "{head}<main id=\"consent\" data-consent-id=\"{}\" data-csrf-token=\"{}\" data-challenge-endpoint=\"{}\" data-decision-endpoint=\"{}\"><h1>Allow {client_name} to use Plug?</h1><p><strong>{client_name}</strong></p><p>{identity}</p><section aria-labelledby=\"destination-heading\"><h2 id=\"destination-heading\">Connection destination</h2><p>Callback: <strong>{}</strong></p><details><summary>Show full callback address</summary><code>{}</code></details>{callback_warning}</section><section aria-labelledby=\"resource-heading\"><h2 id=\"resource-heading\">Plug resource</h2><p>Plug MCP server</p><code>{}</code></section><section aria-labelledby=\"permissions-heading\"><h2 id=\"permissions-heading\">Permissions</h2><ul>{scopes}</ul></section><p>This request expires in {lifetime_minutes} minutes.</p>{allow}<button id=\"deny\" type=\"button\">Deny</button><p id=\"status\" role=\"status\" aria-live=\"polite\"></p><noscript>This page needs JavaScript to verify your passkey. Enable JavaScript and reload this page.</noscript></main><script src=\"{script_url}\"></script></body></html>",
        html_escape(&consent.consent_id),
        html_escape(&consent.csrf_token),
        paths::CONSENT_CHALLENGE,
        paths::CONSENT_DECISION,
        html_escape(&callback_destination),
        html_escape(&consent.redirect_uri),
        html_escape(&consent.resource),
    );
    html_response(StatusCode::OK, html)
}

fn callback_authority(url: &url::Url) -> Option<String> {
    let host = match url.host()? {
        url::Host::Domain(host) => host.to_string(),
        url::Host::Ipv4(address) => address.to_string(),
        url::Host::Ipv6(address) => format!("[{address}]"),
    };
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

pub fn enrollment_page() -> Response {
    let script_url = html_escape(&versioned_asset_url(
        paths::ENROLL_SCRIPT,
        ENROLL_JAVASCRIPT,
    ));
    let challenge = paths::OWNER_ENROLL_CHALLENGE;
    let complete = paths::OWNER_ENROLL_COMPLETE;
    let head = page_head("Set up Plug owner passkey");
    let html = format!(
        "{head}<main id=\"enrollment\" data-challenge-endpoint=\"{challenge}\" data-complete-endpoint=\"{complete}\"><h1>Set up your Plug owner passkey</h1><p>Use Touch ID or another passkey to approve future Plug connections.</p><button id=\"enroll\" type=\"button\">Create owner passkey</button><p id=\"status\" role=\"status\" aria-live=\"polite\"></p><noscript>This page needs JavaScript to create your passkey. Enable JavaScript and reload this page.</noscript></main><script src=\"{script_url}\"></script></body></html>"
    );
    html_response(StatusCode::OK, html)
}

pub fn authorization_error_page(status: StatusCode, code: &str, description: &str) -> Response {
    let head = page_head("Plug authorization failed");
    let html = format!(
        "{head}<main><h1>Plug authorization failed</h1><p><strong>Error: <code>{}</code></strong></p><p>{}</p></main></body></html>",
        html_escape(code),
        html_escape(description),
    );
    html_response(status, html)
}

/// The start of every page: Plug's icon in the tab and at the top, and the
/// page's look, both served by Plug itself.
fn page_head(title: &str) -> String {
    let icon = html_escape(&versioned_asset_url(paths::ICON, ICON));
    let stylesheet = html_escape(&versioned_asset_url(paths::STYLESHEET, STYLESHEET));
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><link rel=\"icon\" type=\"image/svg+xml\" href=\"{icon}\"><link rel=\"stylesheet\" href=\"{stylesheet}\"></head><body><img class=\"brand\" src=\"{icon}\" alt=\"\">",
        html_escape(title)
    )
}

pub fn asset(source: &'static str, content_type: &'static str) -> Response {
    let mut response = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        source,
    )
        .into_response();
    apply_oauth_security_headers(&mut response);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(ASSET_CACHE_CONTROL),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"plug-{}\"", asset_fingerprint(source)))
            .expect("SHA-256 asset ETag is a valid header value"),
    );
    response
}

fn asset_fingerprint(source: &str) -> String {
    hex::encode(Sha256::digest(source.as_bytes()))
}

fn versioned_asset_url(path: &str, source: &str) -> String {
    format!("{path}?v={}", asset_fingerprint(source))
}

fn html_response(status: StatusCode, html: String) -> Response {
    let mut response = (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response();
    apply_oauth_security_headers(&mut response);
    response
}

/// What the consent page says a scope lets a client do.
fn scope_description(scope: &str) -> &'static str {
    // Scopes a client may ask for besides the ones Plug grants by default.
    const ASKED_FOR: [(&str, &str); 2] = [
        (
            crate::events::EVENT_SCOPE,
            "Hear about the events Plug watches for",
        ),
        (
            "offline_access",
            "Stay connected after this browser window closes",
        ),
    ];
    crate::protocol::DEFAULT_DOWNSTREAM_OAUTH_SCOPE_GRANTS
        .iter()
        .chain(&ASKED_FOR)
        .find(|(granted, _)| *granted == scope)
        .map_or("Use the requested Plug capability", |(_, description)| {
            description
        })
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::{consent_page, enrollment_page, page_head, versioned_asset_url};
    use crate::downstream_oauth::{ClientSource, ConsentRequest, paths};

    async fn page_text(response: axum::response::Response) -> String {
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("page body");
        String::from_utf8(body.to_vec()).expect("page HTML")
    }

    fn consent(redirect_uri: &str) -> ConsentRequest {
        ConsentRequest {
            consent_id: "consent".to_string(),
            client_id: "client".to_string(),
            client_name: "Client".to_string(),
            client_source: ClientSource::DynamicRegistration,
            redirect_uri: redirect_uri.to_string(),
            redirect_host: "unused".to_string(),
            scopes: vec!["tools:read".to_string()],
            resource: "https://plug.example.com/mcp".to_string(),
            csrf_token: "csrf".to_string(),
            expires_at: 0,
        }
    }

    #[test]
    fn every_scope_plug_asks_for_is_described() {
        for scope in crate::protocol::DEFAULT_DOWNSTREAM_OAUTH_SCOPES
            .into_iter()
            .chain([crate::events::EVENT_SCOPE, "offline_access"])
        {
            assert_ne!(
                super::scope_description(scope),
                super::scope_description("unknown:scope"),
                "{scope}"
            );
        }
    }

    #[tokio::test]
    async fn an_ipv6_loopback_callback_is_called_local() {
        let page = page_text(consent_page(&consent("http://[::1]:8787/callback"), true)).await;
        assert!(page.contains("Local app callback:"));
        assert!(page.contains("returns to <code>[::1]:8787</code>"));
        let page = page_text(consent_page(
            &consent("https://client.example.com/cb"),
            true,
        ))
        .await;
        assert!(!page.contains("Local app callback:"));
    }

    #[tokio::test]
    async fn pages_name_the_endpoints_their_scripts_post_to() {
        let page = page_text(consent_page(
            &consent("https://client.example.com/cb"),
            true,
        ))
        .await;
        assert!(page.contains(&format!(
            "data-challenge-endpoint=\"{}\" data-decision-endpoint=\"{}\"",
            paths::CONSENT_CHALLENGE,
            paths::CONSENT_DECISION
        )));
        let page = page_text(enrollment_page()).await;
        assert!(page.contains(&format!(
            "data-challenge-endpoint=\"{}\" data-complete-endpoint=\"{}\"",
            paths::OWNER_ENROLL_CHALLENGE,
            paths::OWNER_ENROLL_COMPLETE
        )));
        for script in [super::CONSENT_JAVASCRIPT, super::ENROLL_JAVASCRIPT] {
            assert!(
                !script.contains("/oauth/"),
                "scripts read endpoints from the page"
            );
        }
    }

    #[test]
    fn every_page_carries_plugs_icon_and_look() {
        let head = page_head("A <title>");
        assert!(head.contains("<title>A &lt;title&gt;</title>"));
        assert!(
            head.contains(
                "rel=\"icon\" type=\"image/svg+xml\" href=\"/oauth/assets/plug-icon.svg?v="
            )
        );
        assert!(head.contains("rel=\"stylesheet\" href=\"/oauth/assets/plug.css?v="));
        assert!(head.contains("<img class=\"brand\" src=\"/oauth/assets/plug-icon.svg?v="));
    }

    #[test]
    fn asset_url_fingerprint_changes_with_content() {
        let original = versioned_asset_url("/oauth/assets/consent.js", "const version = 1;");
        let unchanged = versioned_asset_url("/oauth/assets/consent.js", "const version = 1;");
        let updated = versioned_asset_url("/oauth/assets/consent.js", "const version = 2;");

        assert_eq!(original, unchanged);
        assert_ne!(original, updated);
        assert!(original.starts_with("/oauth/assets/consent.js?v="));
    }
}
