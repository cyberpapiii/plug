use passkey_auth::{AuthenticationState, PasskeyCredential, RegistrationState, Webauthn};
use serde::{Deserialize, Serialize};

use super::DownstreamOauthError;

pub const MAX_OWNER_CREDENTIALS: usize = 5;
pub const MAX_OWNER_CHALLENGES: usize = 10;
pub const OWNER_CEREMONY_LIFETIME_SECS: u64 = 300;

pub type Passkey = PasskeyCredential;
pub type PasskeyRegistration = RegistrationState;
pub type PasskeyAuthentication = AuthenticationState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerCredential {
    pub id: String,
    pub label: String,
    pub passkey: Passkey,
    pub created_at: u64,
    pub last_used_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerBootstrap {
    pub secret_hash: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerRegistrationCeremony {
    pub id: String,
    pub bootstrap_hash: String,
    pub state: PasskeyRegistration,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerAuthenticationCeremony {
    pub id: String,
    pub consent_id: String,
    pub state: PasskeyAuthentication,
    pub expires_at: u64,
}

#[derive(Debug)]
pub struct OwnerSecurity {
    pub webauthn: Webauthn,
    pub rp_id: String,
    pub origin: url::Url,
}

impl OwnerSecurity {
    pub fn new(public_base_url: &str) -> Result<Self, DownstreamOauthError> {
        let origin = url::Url::parse(public_base_url)
            .map_err(|_| DownstreamOauthError::InvalidAuthorizationRequest)?;
        if origin.scheme() != "https"
            || origin.path() != "/"
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(DownstreamOauthError::InvalidAuthorizationRequest);
        }
        let rp_id = match origin.host() {
            Some(url::Host::Domain(host)) => host.to_string(),
            Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)) | None => {
                return Err(DownstreamOauthError::InvalidAuthorizationRequest);
            }
        };
        let browser_origin = origin.origin().ascii_serialization();
        let webauthn = Webauthn::new(&rp_id, "Plug", &browser_origin)
            .require_user_verification(true)
            .strict_base64(true);
        Ok(Self {
            webauthn,
            rp_id,
            origin,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use passkey_auth::{RegistrationResponse, error::Error as PasskeyError};

    #[test]
    fn owner_security_uses_https_hostname_as_rp_id() {
        let security =
            OwnerSecurity::new("https://plug.example.com:8443/").expect("valid owner origin");

        assert_eq!(security.rp_id, "plug.example.com");
        assert_eq!(security.origin.as_str(), "https://plug.example.com:8443/");
    }

    #[test]
    fn owner_security_rejects_non_origin_and_ip_urls() {
        for invalid in [
            "http://plug.example.com",
            "https://plug.example.com/path",
            "https://user@plug.example.com",
            "https://plug.example.com?query=yes",
            "https://plug.example.com#fragment",
            "https://127.0.0.1",
            "https://[::1]",
            "https://",
        ] {
            assert!(
                OwnerSecurity::new(invalid).is_err(),
                "invalid owner origin accepted: {invalid}"
            );
        }
    }

    #[test]
    fn owner_registration_accepts_browser_origin_form_for_default_and_custom_https_ports() {
        for (configured, browser_origin) in [
            ("https://plug.example.com", "https://plug.example.com"),
            (
                "https://plug.example.com:8443",
                "https://plug.example.com:8443",
            ),
        ] {
            let security = OwnerSecurity::new(configured).expect("valid owner origin");
            let (challenge, state) = security.webauthn.start_registration(
                b"owner",
                "owner@plug.local",
                "Plug owner",
                &[],
            );
            let client_data = serde_json::json!({
                "type": "webauthn.create",
                "challenge": challenge.challenge,
                "origin": browser_origin,
                "crossOrigin": false
            });
            let response = RegistrationResponse {
                id: "credential-id".to_string(),
                transports: vec!["internal".to_string()],
                attestation_object: "invalid-attestation".to_string(),
                client_data_json: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                    serde_json::to_vec(&client_data).expect("serialize browser client data"),
                ),
            };

            let error = security
                .webauthn
                .finish_registration(&state, &response)
                .expect_err("invalid attestation must fail after browser origin validation");
            assert!(
                !matches!(error, PasskeyError::OriginMismatch { .. }),
                "browser origin must match configured verifier for {configured}: {error:?}"
            );
        }
    }
}
