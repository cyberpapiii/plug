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
        let webauthn = Webauthn::new(&rp_id, "Plug", origin.as_str())
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
}
