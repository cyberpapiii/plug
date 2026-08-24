use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest as _, Sha256};

use crate::downstream_oauth::{ClientSource, ConsentRequest};

pub const CONSENT_JAVASCRIPT: &str = include_str!("oauth_ui/consent.js");
pub const ENROLL_JAVASCRIPT: &str = include_str!("oauth_ui/enroll.js");

const HTML_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";
const ASSET_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

pub fn apply_oauth_html_security_headers(response: &mut Response) {
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

pub fn apply_oauth_json_security_headers(response: &mut Response) {
    apply_oauth_html_security_headers(response);
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
            matches!(host, "localhost" | "127.0.0.1" | "::1").then(|| {
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
        "/oauth/assets/consent.js",
        CONSENT_JAVASCRIPT,
    ));
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Allow {client_name} to use Plug?</title></head><body><main id=\"consent\" data-consent-id=\"{}\" data-csrf-token=\"{}\" data-challenge-endpoint=\"/oauth/consent/challenge\" data-decision-endpoint=\"/oauth/consent/decision\"><h1>Allow {client_name} to use Plug?</h1><p><strong>{client_name}</strong></p><p>{identity}</p><section aria-labelledby=\"destination-heading\"><h2 id=\"destination-heading\">Connection destination</h2><p>Callback: <strong>{}</strong></p><details><summary>Show full callback address</summary><code>{}</code></details>{callback_warning}</section><section aria-labelledby=\"resource-heading\"><h2 id=\"resource-heading\">Plug resource</h2><p>Plug MCP server</p><code>{}</code></section><section aria-labelledby=\"permissions-heading\"><h2 id=\"permissions-heading\">Permissions</h2><ul>{scopes}</ul></section><p>This request expires in 5 minutes.</p>{allow}<button id=\"deny\" type=\"button\">Deny</button><p id=\"status\" role=\"status\" aria-live=\"polite\"></p><noscript>This page needs JavaScript to verify your passkey. Enable JavaScript and reload this page.</noscript></main><script src=\"{script_url}\"></script></body></html>",
        html_escape(&consent.consent_id),
        html_escape(&consent.csrf_token),
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
        "/oauth/assets/enroll.js",
        ENROLL_JAVASCRIPT,
    ));
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Set up Plug owner passkey</title></head><body><main id=\"enrollment\"><h1>Set up your Plug owner passkey</h1><p>Use Touch ID or another passkey to approve future Plug connections.</p><button id=\"enroll\" type=\"button\">Create owner passkey</button><p id=\"status\" role=\"status\" aria-live=\"polite\"></p><noscript>This page needs JavaScript to create your passkey. Enable JavaScript and reload this page.</noscript></main><script src=\"{script_url}\"></script></body></html>"
    );
    html_response(StatusCode::OK, html)
}

pub fn authorization_error_page(status: StatusCode, code: &str, description: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Plug authorization failed</title></head><body><main><h1>Plug authorization failed</h1><p><strong>Error: <code>{}</code></strong></p><p>{}</p></main></body></html>",
        html_escape(code),
        html_escape(description),
    );
    html_response(status, html)
}

pub fn javascript_asset(source: &'static str) -> Response {
    let mut response = (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        source,
    )
        .into_response();
    apply_oauth_html_security_headers(&mut response);
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
    apply_oauth_html_security_headers(&mut response);
    response
}

fn scope_description(scope: &str) -> &'static str {
    match scope {
        "tools:read" => "Use Plug tools",
        "resources:read" => "Read resources available through Plug",
        "prompts:read" => "Use prompts available through Plug",
        "completion:use" => "Use argument completion through Plug",
        "tasks:use" => "Run and manage long-running tasks through Plug",
        "subscriptions:listen" => "Receive change notifications from Plug",
        "offline_access" => "Stay connected after this browser window closes",
        _ => "Use the requested Plug capability",
    }
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
    use super::versioned_asset_url;

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
