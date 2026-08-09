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
    use ciborium::value::Value as CborValue;
    use ed25519_dalek::{Signer as _, SigningKey};
    use passkey_auth::{
        AuthenticationResponse, RegistrationResponse, error::Error as PasskeyError,
    };
    use rand::RngExt as _;
    use sha2::{Digest as _, Sha256};

    const FLAG_UP: u8 = 1 << 0;
    const FLAG_UV: u8 = 1 << 2;
    const FLAG_AT: u8 = 1 << 6;

    struct BrowserAuthenticator {
        signing_key: SigningKey,
        credential_id: Vec<u8>,
        counter: u32,
    }

    impl BrowserAuthenticator {
        fn new() -> Self {
            let mut seed = [0u8; 32];
            rand::rng().fill(&mut seed);
            Self {
                signing_key: SigningKey::from_bytes(&seed),
                credential_id: b"plug-owner-credential".to_vec(),
                counter: 0,
            }
        }

        fn cose_public_key(&self) -> Vec<u8> {
            let map = CborValue::Map(vec![
                (CborValue::Integer(1.into()), CborValue::Integer(1.into())),
                (
                    CborValue::Integer(3.into()),
                    CborValue::Integer((-8).into()),
                ),
                (
                    CborValue::Integer((-1).into()),
                    CborValue::Integer(6.into()),
                ),
                (
                    CborValue::Integer((-2).into()),
                    CborValue::Bytes(self.signing_key.verifying_key().to_bytes().to_vec()),
                ),
            ]);
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(&map, &mut bytes).expect("serialize COSE key");
            bytes
        }

        fn registration_authenticator_data(&self, rp_id: &str) -> Vec<u8> {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&Sha256::digest(rp_id.as_bytes()));
            bytes.push(FLAG_UP | FLAG_UV | FLAG_AT);
            bytes.extend_from_slice(&self.counter.to_be_bytes());
            bytes.extend_from_slice(&[0u8; 16]);
            bytes.extend_from_slice(&(self.credential_id.len() as u16).to_be_bytes());
            bytes.extend_from_slice(&self.credential_id);
            bytes.extend_from_slice(&self.cose_public_key());
            bytes
        }

        fn attestation_object(&self, rp_id: &str) -> Vec<u8> {
            let map = CborValue::Map(vec![
                (
                    CborValue::Text("fmt".to_string()),
                    CborValue::Text("none".to_string()),
                ),
                (
                    CborValue::Text("attStmt".to_string()),
                    CborValue::Map(Vec::new()),
                ),
                (
                    CborValue::Text("authData".to_string()),
                    CborValue::Bytes(self.registration_authenticator_data(rp_id)),
                ),
            ]);
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(&map, &mut bytes).expect("serialize attestation");
            bytes
        }

        fn authentication_data(&mut self, rp_id: &str) -> Vec<u8> {
            self.counter += 1;
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&Sha256::digest(rp_id.as_bytes()));
            bytes.push(FLAG_UP | FLAG_UV);
            bytes.extend_from_slice(&self.counter.to_be_bytes());
            bytes
        }

        fn sign(&self, authenticator_data: &[u8], client_data: &[u8]) -> Vec<u8> {
            let mut message = authenticator_data.to_vec();
            message.extend_from_slice(&Sha256::digest(client_data));
            self.signing_key.sign(&message).to_bytes().to_vec()
        }
    }

    fn browser_client_data(kind: &str, challenge: &str, origin: &str) -> (Vec<u8>, String) {
        let raw = serde_json::to_vec(&serde_json::json!({
            "type": kind,
            "challenge": challenge,
            "origin": origin,
            "crossOrigin": false
        }))
        .expect("serialize browser client data");
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&raw);
        (raw, encoded)
    }

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

    #[test]
    fn owner_full_browser_registration_and_authentication_survive_state_serialization() {
        let security = OwnerSecurity::new("https://plug.example.com").expect("owner security");
        let mut authenticator = BrowserAuthenticator::new();
        let (registration_challenge, registration_state) =
            security
                .webauthn
                .start_registration(b"owner", "owner@plug.local", "Plug owner", &[]);
        let registration_state: PasskeyRegistration = serde_json::from_slice(
            &serde_json::to_vec(&registration_state).expect("serialize registration state"),
        )
        .expect("deserialize registration state");
        let (_, registration_client_data) = browser_client_data(
            "webauthn.create",
            &registration_challenge.challenge,
            "https://plug.example.com",
        );
        let credential = security
            .webauthn
            .finish_registration(
                &registration_state,
                &RegistrationResponse {
                    id: base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(&authenticator.credential_id),
                    transports: vec!["internal".to_string()],
                    attestation_object: base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(authenticator.attestation_object(&security.rp_id)),
                    client_data_json: registration_client_data,
                },
            )
            .expect("complete browser registration");

        let (authentication_challenge, authentication_state) = security
            .webauthn
            .start_authentication(std::slice::from_ref(&credential.id));
        let authentication_state: PasskeyAuthentication = serde_json::from_slice(
            &serde_json::to_vec(&authentication_state).expect("serialize authentication state"),
        )
        .expect("deserialize authentication state");
        let authenticator_data = authenticator.authentication_data(&security.rp_id);
        let (authentication_client_data_raw, authentication_client_data) = browser_client_data(
            "webauthn.get",
            &authentication_challenge.challenge,
            "https://plug.example.com",
        );
        let signature = authenticator.sign(&authenticator_data, &authentication_client_data_raw);
        let success = security
            .webauthn
            .finish_authentication(
                &authentication_state,
                &AuthenticationResponse {
                    id: base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(&authenticator.credential_id),
                    authenticator_data: base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(authenticator_data),
                    signature: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature),
                    client_data_json: authentication_client_data,
                    user_handle: None,
                },
                &credential,
            )
            .expect("complete browser authentication");
        assert!(success.user_verified);
        assert_eq!(success.new_counter, 1);

        let (wrong_origin_challenge, wrong_origin_state) = security
            .webauthn
            .start_authentication(std::slice::from_ref(&credential.id));
        let wrong_origin_data = authenticator.authentication_data(&security.rp_id);
        let (wrong_origin_raw, wrong_origin_client_data) = browser_client_data(
            "webauthn.get",
            &wrong_origin_challenge.challenge,
            "https://evil.example.com",
        );
        let wrong_origin_signature = authenticator.sign(&wrong_origin_data, &wrong_origin_raw);
        let error = security
            .webauthn
            .finish_authentication(
                &wrong_origin_state,
                &AuthenticationResponse {
                    id: base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(&authenticator.credential_id),
                    authenticator_data: base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(wrong_origin_data),
                    signature: base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(wrong_origin_signature),
                    client_data_json: wrong_origin_client_data,
                    user_handle: None,
                },
                &credential,
            )
            .expect_err("wrong browser origin must fail");
        assert!(matches!(error, PasskeyError::OriginMismatch { .. }));
    }
}
