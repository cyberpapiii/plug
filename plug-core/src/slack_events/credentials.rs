//! Workspace/app-bound signing credentials. No environment or file fallback.
use crate::types::SecretString;
#[cfg(not(target_os = "linux"))]
use keyring as platform_keyring;
#[cfg(target_os = "linux")]
use keyring_core as platform_keyring;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CredentialError {
    #[error("invalid Slack workspace/app identity")]
    InvalidIdentity,
    #[error("Slack signing secret must contain exactly 32 hexadecimal characters")]
    InvalidSecret,
    #[error("Slack event signing secret is missing; use plug auth slack-events set")]
    Missing,
    #[error("OS credential store unavailable; no signing credential fallback was used")]
    Unavailable,
}

/// Injectable boundary: tests supply an isolated store, never the operator Keychain.
pub trait SigningSecretBackend {
    fn get(&self, account: &str) -> Result<Option<SecretString>, CredentialError>;
    fn set(&self, account: &str, secret: &SecretString) -> Result<(), CredentialError>;
    fn remove(&self, account: &str) -> Result<(), CredentialError>;
}

pub struct KeychainSigningSecrets;
impl KeychainSigningSecrets {
    fn entry(account: &str) -> Result<platform_keyring::Entry, CredentialError> {
        crate::oauth::initialize_platform_keyring().map_err(|_| CredentialError::Unavailable)?;
        platform_keyring::Entry::new("plug", account).map_err(|_| CredentialError::Unavailable)
    }
}
impl SigningSecretBackend for KeychainSigningSecrets {
    fn get(&self, account: &str) -> Result<Option<SecretString>, CredentialError> {
        match Self::entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret.into())),
            Err(platform_keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(CredentialError::Unavailable),
        }
    }
    fn set(&self, account: &str, secret: &SecretString) -> Result<(), CredentialError> {
        Self::entry(account)?
            .set_password(secret.as_str())
            .map_err(|_| CredentialError::Unavailable)
    }
    fn remove(&self, account: &str) -> Result<(), CredentialError> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(platform_keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(CredentialError::Unavailable),
        }
    }
}

pub struct SigningCredential {
    account: String,
}
impl SigningCredential {
    pub fn new(team_id: &str, app_id: &str) -> Result<Self, CredentialError> {
        let valid = |id: &str, prefix| {
            id.starts_with(prefix)
                && id.len() > 1
                && id
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        };
        if !valid(team_id, 'T') || !valid(app_id, 'A') {
            return Err(CredentialError::InvalidIdentity);
        }
        Ok(Self {
            account: format!("slack-events-signing:{team_id}:{app_id}"),
        })
    }
    pub fn validate_secret(secret: &SecretString) -> Result<(), CredentialError> {
        if secret.as_str().len() != 32 || !secret.as_str().bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(CredentialError::InvalidSecret);
        }
        Ok(())
    }
    pub fn load(
        &self,
        backend: &impl SigningSecretBackend,
    ) -> Result<SecretString, CredentialError> {
        let secret = backend
            .get(&self.account)?
            .ok_or(CredentialError::Missing)?;
        Self::validate_secret(&secret)?;
        Ok(secret)
    }
    pub fn save(
        &self,
        backend: &impl SigningSecretBackend,
        secret: &SecretString,
    ) -> Result<(), CredentialError> {
        Self::validate_secret(secret)?;
        backend.set(&self.account, secret)
    }
    pub fn remove(&self, backend: &impl SigningSecretBackend) -> Result<(), CredentialError> {
        backend.remove(&self.account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    #[derive(Default)]
    struct MemoryStore {
        items: Mutex<HashMap<String, SecretString>>,
        unavailable: bool,
    }
    impl SigningSecretBackend for MemoryStore {
        fn get(&self, key: &str) -> Result<Option<SecretString>, CredentialError> {
            if self.unavailable {
                return Err(CredentialError::Unavailable);
            }
            Ok(self.items.lock().unwrap().get(key).cloned())
        }
        fn set(&self, key: &str, value: &SecretString) -> Result<(), CredentialError> {
            if self.unavailable {
                return Err(CredentialError::Unavailable);
            }
            self.items.lock().unwrap().insert(key.into(), value.clone());
            Ok(())
        }
        fn remove(&self, key: &str) -> Result<(), CredentialError> {
            if self.unavailable {
                return Err(CredentialError::Unavailable);
            }
            self.items.lock().unwrap().remove(key);
            Ok(())
        }
    }
    #[test]
    fn signing_credential_round_trip_is_bound_to_workspace_and_app() {
        let store = MemoryStore::default();
        let credential = SigningCredential::new("T123", "A123").unwrap();
        let secret: SecretString = "0123456789abcdef0123456789abcdef".to_owned().into();
        assert_eq!(
            credential.load(&store).unwrap_err(),
            CredentialError::Missing
        );
        credential.save(&store, &secret).unwrap();
        assert_eq!(credential.load(&store).unwrap(), secret);
        for (team, app) in [("T999", "A123"), ("T123", "A999")] {
            assert_eq!(
                SigningCredential::new(team, app)
                    .unwrap()
                    .load(&store)
                    .unwrap_err(),
                CredentialError::Missing
            );
        }
        credential.remove(&store).unwrap();
        credential.remove(&store).unwrap();
        assert_eq!(
            credential.load(&store).unwrap_err(),
            CredentialError::Missing
        );
    }
    #[test]
    fn signing_credential_rejects_bad_input_and_corrupt_store_without_fallback() {
        let store = MemoryStore::default();
        let credential = SigningCredential::new("T123", "A123").unwrap();
        for value in [
            "",
            "too-short",
            "0123456789abcdef0123456789abcdeg",
            "0123456789abcdef0123456789abcdef\n",
        ] {
            assert_eq!(
                credential.save(&store, &value.to_owned().into()),
                Err(CredentialError::InvalidSecret)
            );
        }
        assert!(store.items.lock().unwrap().is_empty());
        store
            .items
            .lock()
            .unwrap()
            .insert(credential.account.clone(), "corrupt".to_owned().into());
        assert_eq!(
            credential.load(&store).unwrap_err(),
            CredentialError::InvalidSecret
        );
        for (team, app) in [
            ("T123/../../", "A123"),
            ("T123", "A123:other"),
            ("t123", "A123"),
            ("T123", "U123"),
        ] {
            assert!(matches!(
                SigningCredential::new(team, app),
                Err(CredentialError::InvalidIdentity)
            ));
        }
        let unavailable = MemoryStore {
            unavailable: true,
            ..Default::default()
        };
        assert_eq!(
            credential.load(&unavailable).unwrap_err(),
            CredentialError::Unavailable
        );
        assert_eq!(
            credential.save(
                &unavailable,
                &"0123456789abcdef0123456789abcdef".to_owned().into()
            ),
            Err(CredentialError::Unavailable)
        );
        assert!(!format!("{:?}", CredentialError::Unavailable).contains("012345"));
    }
}
