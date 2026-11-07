//! A passkey owner for tests, so a test signs a client in through the
//! consent path the browser takes: CSRF, the owner ceremony, and its cleanup.
//! Compiled only for tests and the `test-helpers` feature.

use base64::Engine as _;
use ciborium::value::Value as CborValue;
use ed25519_dalek::{Signer as _, SigningKey};
use passkey_auth::{AuthenticationResponse, RegistrationResponse};
use rand::RngExt as _;
use sha2::{Digest as _, Sha256};

use super::{AuthorizationRedirect, DownstreamOauthError, DownstreamOauthManager};

/// Approve `consent_id` as the issuer's owner, through the passkey ceremony
/// the consent page runs. An issuer with no owner yet gets this one.
pub async fn approve_consent(
    manager: &DownstreamOauthManager,
    consent_id: &str,
) -> Result<AuthorizationRedirect, DownstreamOauthError> {
    let mut owner = BrowserAuthenticator::from_seed([7; 32]);
    let rp_id = manager.owner_security.rp_id.clone();
    if !manager.owner_enrolled().await {
        let bootstrap = manager.create_owner_bootstrap().await?;
        let challenge = manager.start_owner_registration(&bootstrap).await?;
        let response = owner.registration_response(
            &challenge.public_key.challenge,
            &rp_id,
            manager.base_url(),
            true,
        );
        manager
            .finish_owner_registration(&challenge.ceremony_id, response)
            .await?;
    }
    // The owner passkey counts its uses; each approval must count higher.
    owner.counter = manager
        .state
        .lock()
        .await
        .owner_credentials
        .values()
        .map(|credential| credential.passkey.counter)
        .max()
        .unwrap_or(0);
    let challenge = manager.start_owner_approval(consent_id).await?;
    let assertion = owner.authentication_response(
        &challenge.public_key.challenge,
        &rp_id,
        manager.base_url(),
        true,
    );
    manager
        .finish_owner_approval(&challenge.ceremony_id, assertion)
        .await
}

const FLAG_UP: u8 = 1 << 0;
const FLAG_UV: u8 = 1 << 2;
const FLAG_AT: u8 = 1 << 6;

/// A platform authenticator holding one passkey, answering the browser
/// ceremonies the way WebAuthn does.
pub struct BrowserAuthenticator {
    signing_key: SigningKey,
    pub(crate) credential_id: Vec<u8>,
    counter: u32,
}

impl BrowserAuthenticator {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let mut seed = [0u8; 32];
        rand::rng().fill(&mut seed);
        Self::from_seed(seed)
    }

    fn from_seed(seed: [u8; 32]) -> Self {
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

    #[cfg(test)]
    pub(crate) fn attestation_object(&self, rp_id: &str) -> Vec<u8> {
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

    pub(crate) fn authentication_data(&mut self, rp_id: &str) -> Vec<u8> {
        self.counter += 1;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&Sha256::digest(rp_id.as_bytes()));
        bytes.push(FLAG_UP | FLAG_UV);
        bytes.extend_from_slice(&self.counter.to_be_bytes());
        bytes
    }

    pub(crate) fn sign(&self, authenticator_data: &[u8], client_data: &[u8]) -> Vec<u8> {
        let mut message = authenticator_data.to_vec();
        message.extend_from_slice(&Sha256::digest(client_data));
        self.signing_key.sign(&message).to_bytes().to_vec()
    }

    pub fn registration_response(
        &self,
        challenge: &str,
        rp_id: &str,
        origin: &str,
        user_verified: bool,
    ) -> RegistrationResponse {
        let (client_data_raw, client_data_json) =
            browser_client_data("webauthn.create", challenge, origin);
        let _ = client_data_raw;
        let mut authenticator_data = self.registration_authenticator_data(rp_id);
        if !user_verified {
            authenticator_data[32] &= !FLAG_UV;
        }
        let attestation = CborValue::Map(vec![
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
                CborValue::Bytes(authenticator_data),
            ),
        ]);
        let mut attestation_object = Vec::new();
        ciborium::ser::into_writer(&attestation, &mut attestation_object)
            .expect("serialize attestation");
        RegistrationResponse {
            id: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&self.credential_id),
            transports: vec!["internal".to_string()],
            attestation_object: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(attestation_object),
            client_data_json,
        }
    }

    pub fn authentication_response(
        &mut self,
        challenge: &str,
        rp_id: &str,
        origin: &str,
        user_verified: bool,
    ) -> AuthenticationResponse {
        let mut authenticator_data = self.authentication_data(rp_id);
        if !user_verified {
            authenticator_data[32] &= !FLAG_UV;
        }
        let (client_data_raw, client_data_json) =
            browser_client_data("webauthn.get", challenge, origin);
        let signature = self.sign(&authenticator_data, &client_data_raw);
        AuthenticationResponse {
            id: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&self.credential_id),
            authenticator_data: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(authenticator_data),
            signature: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature),
            client_data_json,
            user_handle: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_counter(&mut self, counter: u32) {
        self.counter = counter;
    }
}

pub(crate) fn browser_client_data(kind: &str, challenge: &str, origin: &str) -> (Vec<u8>, String) {
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
