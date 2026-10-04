//! Secret stores.
//!
//! A server's credential in `config.toml` may be a reference, `<store>:<name>`,
//! instead of the value. Only the service follows a reference, and only for the
//! server it is about to start, so a store that cannot answer holds up that one
//! server and nothing else.
//!
//! A store is anything that turns a name into a value. The Keychain is built
//! in and is the default; it does not lock while the person is logged in.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

#[cfg(not(target_os = "linux"))]
use keyring as platform_keyring;
#[cfg(target_os = "linux")]
use keyring_core as platform_keyring;

use crate::config::ServerConfig;
use crate::types::SecretString;

/// The store a pasted key goes to unless the person chooses another.
pub const KEYCHAIN: &str = "keychain";

/// How long one read may take. A store that asks a person for approval can
/// wait forever; a server start cannot.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a reference did not become a value. The messages name the store and
/// the secret, never a value.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecretError {
    #[error("no secret named `{name}` in {store}; run `plug secret set {name}`")]
    Missing { store: String, name: String },
    #[error("waiting for {store}: it did not answer in time")]
    TimedOut { store: String },
    #[error("waiting for {store}: {reason}")]
    Unavailable { store: String, reason: String },
    #[error(
        "`{0}` is not a usable secret name; use letters, digits, `_`, `-`, and `.`, at most 64"
    )]
    InvalidName(String),
}

/// Something that keeps secrets by name. Calls may block.
pub trait SecretStore: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError>;
    fn set(&self, name: &str, value: &SecretString) -> Result<(), SecretError>;
    fn remove(&self, name: &str) -> Result<(), SecretError>;
}

/// A secret name is safe to print, to pass to a command, and to use as a
/// Keychain account.
pub fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

/// The reference a config value holds for `name` in `store`.
pub fn reference(store: &str, name: &str) -> String {
    format!("{store}:{name}")
}

/// The OS credential store, under the same service name as Plug's OAuth
/// tokens.
pub struct Keychain;

impl Keychain {
    fn entry(name: &str) -> Result<platform_keyring::Entry, SecretError> {
        let unavailable = |reason: String| SecretError::Unavailable {
            store: KEYCHAIN.to_string(),
            reason,
        };
        crate::oauth::initialize_platform_keyring().map_err(unavailable)?;
        platform_keyring::Entry::new("plug", &format!("secret:{name}"))
            .map_err(|error| unavailable(error.to_string()))
    }

    fn unavailable(error: platform_keyring::Error) -> SecretError {
        SecretError::Unavailable {
            store: KEYCHAIN.to_string(),
            reason: error.to_string(),
        }
    }
}

impl SecretStore for Keychain {
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError> {
        match Self::entry(name)?.get_password() {
            Ok(value) => Ok(Some(value.into())),
            Err(platform_keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(Self::unavailable(error)),
        }
    }

    fn set(&self, name: &str, value: &SecretString) -> Result<(), SecretError> {
        Self::entry(name)?
            .set_password(value.as_str())
            .map_err(Self::unavailable)
    }

    fn remove(&self, name: &str) -> Result<(), SecretError> {
        match Self::entry(name)?.delete_credential() {
            Ok(()) | Err(platform_keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(Self::unavailable(error)),
        }
    }
}

/// The stores a reference may name.
#[derive(Clone, Default)]
pub struct Stores {
    stores: Vec<(String, Arc<dyn SecretStore>)>,
}

impl Stores {
    /// The stores every Plug has.
    ///
    /// Under test the Keychain is a map in memory, so no unit test reads or
    /// writes the Keychain of the person running it.
    pub fn builtin() -> Self {
        #[cfg(test)]
        let keychain: Arc<dyn SecretStore> = testing::keychain();
        #[cfg(not(test))]
        let keychain: Arc<dyn SecretStore> = Arc::new(Keychain);
        Self::default().with(KEYCHAIN, keychain)
    }

    pub fn with(mut self, id: &str, store: Arc<dyn SecretStore>) -> Self {
        self.stores.retain(|(existing, _)| existing != id);
        self.stores.push((id.to_string(), store));
        self
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn SecretStore>> {
        self.stores
            .iter()
            .find(|(existing, _)| existing == id)
            .map(|(_, store)| store)
    }

    /// Split `value` into a store and a name, when the whole value is a
    /// reference to a store that exists. Anything else, a URL or an ordinary
    /// value with a colon in it, is not a reference.
    pub fn parse<'a>(&self, value: &'a str) -> Option<(&'a str, &'a str)> {
        let (store, name) = value.split_once(':')?;
        (self.get(store).is_some() && valid_name(name)).then_some((store, name))
    }

    /// The value `value` refers to, or `None` when it is not a reference.
    async fn follow(&self, value: &str) -> Result<Option<SecretString>, SecretError> {
        let Some((id, name)) = self.parse(value) else {
            return Ok(None);
        };
        let store = Arc::clone(self.get(id).expect("parse checked the store"));
        let owned = name.to_string();
        let read = tokio::task::spawn_blocking(move || store.get(&owned));
        match tokio::time::timeout(READ_TIMEOUT, read).await {
            Err(_) => Err(SecretError::TimedOut {
                store: id.to_string(),
            }),
            Ok(Err(join)) => Err(SecretError::Unavailable {
                store: id.to_string(),
                reason: join.to_string(),
            }),
            Ok(Ok(Err(error))) => Err(error),
            Ok(Ok(Ok(None))) => Err(SecretError::Missing {
                store: id.to_string(),
                name: name.to_string(),
            }),
            Ok(Ok(Ok(Some(secret)))) => Ok(Some(secret)),
        }
    }

    /// `config` with every reference in it replaced by its value: the bearer
    /// token and the values of `env`. A config with no reference is returned
    /// as it is, and no store is touched.
    pub async fn resolve_server<'a>(
        &self,
        config: &'a ServerConfig,
    ) -> Result<Cow<'a, ServerConfig>, SecretError> {
        let mut resolved = Cow::Borrowed(config);
        if let Some(token) = &config.auth_token
            && let Some(secret) = self.follow(token.as_str()).await?
        {
            resolved.to_mut().auth_token = Some(secret);
        }
        for (key, value) in &config.env {
            if let Some(secret) = self.follow(value).await? {
                resolved
                    .to_mut()
                    .env
                    .insert(key.clone(), secret.as_str().to_string());
            }
        }
        Ok(resolved)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    #[derive(Default)]
    pub(crate) struct Memory(Mutex<HashMap<String, String>>);

    impl SecretStore for Memory {
        fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .map(SecretString::from))
        }
        fn set(&self, name: &str, value: &SecretString) -> Result<(), SecretError> {
            self.0
                .lock()
                .unwrap()
                .insert(name.to_string(), value.as_str().to_string());
            Ok(())
        }
        fn remove(&self, name: &str) -> Result<(), SecretError> {
            self.0.lock().unwrap().remove(name);
            Ok(())
        }
    }

    /// The store `Stores::builtin` uses as the Keychain under test.
    pub(crate) fn keychain() -> Arc<Memory> {
        static KEYCHAIN: OnceLock<Arc<Memory>> = OnceLock::new();
        Arc::clone(KEYCHAIN.get_or_init(Arc::default))
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Memory;
    use super::*;

    struct Locked;

    impl SecretStore for Locked {
        fn get(&self, _: &str) -> Result<Option<SecretString>, SecretError> {
            Err(SecretError::Unavailable {
                store: "vault".to_string(),
                reason: "it is locked".to_string(),
            })
        }
        fn set(&self, _: &str, _: &SecretString) -> Result<(), SecretError> {
            unreachable!()
        }
        fn remove(&self, _: &str) -> Result<(), SecretError> {
            unreachable!()
        }
    }

    fn stores() -> Stores {
        let memory = Memory::default();
        memory
            .set("github", &SecretString::from("ghp_value".to_string()))
            .unwrap();
        Stores::default()
            .with(KEYCHAIN, Arc::new(memory))
            .with("vault", Arc::new(Locked))
    }

    fn server(toml: &str) -> ServerConfig {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn only_a_whole_value_naming_a_real_store_is_a_reference() {
        let stores = stores();
        assert_eq!(
            stores.parse("keychain:github"),
            Some(("keychain", "github"))
        );
        assert_eq!(stores.parse("https://example.com/mcp"), None);
        assert_eq!(stores.parse("unknown:github"), None);
        assert_eq!(stores.parse("keychain:"), None);
        assert_eq!(stores.parse("keychain:two words"), None);
        assert_eq!(stores.parse("Bearer keychain:github"), None);
        assert_eq!(reference(KEYCHAIN, "github"), "keychain:github");
    }

    #[tokio::test]
    async fn a_server_gets_the_values_its_references_name() {
        let config = server(
            r#"
command = "server"
auth_token = "keychain:github"

[env]
TOKEN = "keychain:github"
LEVEL = "debug:verbose"
"#,
        );
        let resolved = stores().resolve_server(&config).await.unwrap();
        assert_eq!(resolved.auth_token.as_ref().unwrap().as_str(), "ghp_value");
        assert_eq!(resolved.env["TOKEN"], "ghp_value");
        assert_eq!(resolved.env["LEVEL"], "debug:verbose");
        // The config the person wrote still holds the reference.
        assert_eq!(config.env["TOKEN"], "keychain:github");
    }

    #[tokio::test]
    async fn a_server_with_no_reference_touches_no_store() {
        let config = server("command = \"server\"\nauth_token = \"plain\"\n");
        let resolved = Stores::default()
            .with(KEYCHAIN, Arc::new(Locked))
            .resolve_server(&config)
            .await
            .unwrap();
        assert!(matches!(resolved, Cow::Borrowed(_)));
    }

    #[tokio::test]
    async fn a_reference_that_cannot_be_read_says_which_and_why() {
        let missing = server("command = \"server\"\nauth_token = \"keychain:absent\"\n");
        let error = stores().resolve_server(&missing).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "no secret named `absent` in keychain; run `plug secret set absent`"
        );

        let locked = server("command = \"server\"\n[env]\nKEY = \"vault:api\"\n");
        let error = stores().resolve_server(&locked).await.unwrap_err();
        assert_eq!(error.to_string(), "waiting for vault: it is locked");
    }

    #[test]
    fn names_are_limited_to_what_is_safe_to_print_and_pass_on() {
        assert!(valid_name("github.token-2_a"));
        assert!(!valid_name(""));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("a b"));
        assert!(!valid_name(&"a".repeat(65)));
    }
}
