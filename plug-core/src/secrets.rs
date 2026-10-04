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

/// Whether an environment variable of this name usually holds a credential.
/// A miss leaves the value in the config file, where it was going anyway.
pub fn looks_secret(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    // A name that ends like this says where a credential is, or whose it is.
    let points_elsewhere = [
        "_ID", "_URL", "_URI", "_PATH", "_FILE", "_DIR", "_NAME", "_ACCOUNT", "_USER",
    ]
    .iter()
    .any(|suffix| key.ends_with(suffix));
    if points_elsewhere {
        return false;
    }
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "API_KEY",
        "APIKEY",
        "ACCESS_KEY",
        "PRIVATE_KEY",
    ]
    .iter()
    .any(|word| key.contains(word))
        || key == "KEY"
        || key.ends_with("_KEY")
        || key.ends_with("_PAT")
}

/// The name Plug gives a secret it stores on a server's behalf: the server,
/// a dot, and the field. Removing the server takes these with it.
pub fn name_for(server: &str, field: &str) -> String {
    let safe = |text: &str| -> String {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    };
    let mut name = format!("{}.{}", safe(server), safe(field));
    name.truncate(64);
    name
}

/// A value a person typed, as opposed to a `$VAR`, a redaction placeholder,
/// or nothing.
fn is_literal(value: &str) -> bool {
    !value.is_empty()
        && value != crate::config::REDACTED_SECRET
        && crate::config::expand::extract_env_refs(value).is_empty()
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
    /// Where the isolated test credential backend is installed, the Keychain
    /// is a map in memory, so no test reads or writes the Keychain of the
    /// person running it.
    pub fn builtin() -> Self {
        let keychain: Arc<dyn SecretStore> = if crate::oauth::uses_test_credentials() {
            memory::keychain()
        } else {
            Arc::new(Keychain)
        };
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

    /// The fields of `server` that hold a credential in the clear: `token` for
    /// the bearer token, and the names of the `env` entries that look like
    /// credentials. `server` must be as the file has it, before `$VAR`
    /// expansion.
    pub fn plaintext(&self, server: &ServerConfig) -> Vec<String> {
        let clear = |value: &str| is_literal(value) && self.parse(value).is_none();
        let mut fields: Vec<String> = server
            .env
            .iter()
            .filter(|(key, value)| looks_secret(key) && clear(value))
            .map(|(key, _)| key.clone())
            .collect();
        fields.sort();
        if server
            .auth_token
            .as_ref()
            .is_some_and(|token| clear(token.as_str()))
        {
            fields.insert(0, "token".to_string());
        }
        fields
    }

    /// Move the credentials typed into `server` to the Keychain and leave
    /// references in their place: the bearer token, and the `env` values whose
    /// names say they are credentials. Returns the names stored.
    ///
    /// A value the store will not take stays where it is, so a machine with no
    /// credential store keeps working the way it did.
    pub fn keep(&self, server_name: &str, server: &mut ServerConfig) -> Vec<String> {
        let Some(store) = self.get(KEYCHAIN) else {
            return Vec::new();
        };
        let mut kept = Vec::new();
        let mut put = |field: &str, value: &str| -> Option<String> {
            if !is_literal(value) || self.parse(value).is_some() {
                return None;
            }
            let name = name_for(server_name, field);
            match store.set(&name, &value.to_string().into()) {
                Ok(()) => {
                    kept.push(name.clone());
                    Some(reference(KEYCHAIN, &name))
                }
                Err(error) => {
                    tracing::warn!(server = %server_name, %error, "secret stays in the config file");
                    None
                }
            }
        };
        if let Some(token) = &server.auth_token
            && let Some(reference) = put("token", token.as_str())
        {
            server.auth_token = Some(reference.into());
        }
        for (key, value) in &mut server.env {
            if looks_secret(key)
                && let Some(reference) = put(key, value)
            {
                *value = reference;
            }
        }
        kept
    }

    /// Remove what [`Stores::keep`] stored for `server` and `now` no longer
    /// refers to; `now` is `None` when the server is gone. A secret the
    /// person named themselves may serve other servers, so it is left alone.
    pub fn forget(&self, server_name: &str, server: &ServerConfig, now: Option<&ServerConfig>) {
        fn values(server: &ServerConfig) -> impl Iterator<Item = &str> {
            server
                .auth_token
                .iter()
                .map(|token| token.as_str())
                .chain(server.env.values().map(String::as_str))
        }
        let prefix = name_for(server_name, "");
        for value in values(server) {
            if now.is_some_and(|now| values(now).any(|kept| kept == value)) {
                continue;
            }
            if let Some((id, name)) = self.parse(value)
                && id == KEYCHAIN
                && name.starts_with(&prefix)
                && let Some(store) = self.get(id)
                && let Err(error) = store.remove(name)
            {
                tracing::warn!(server = %server_name, %error, "could not remove a stored secret");
            }
        }
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

/// The stand-in for the Keychain under test.
pub(crate) mod memory {
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

    /// The one store every `Stores::builtin` shares under test.
    pub(crate) fn keychain() -> Arc<Memory> {
        static KEYCHAIN: OnceLock<Arc<Memory>> = OnceLock::new();
        Arc::clone(KEYCHAIN.get_or_init(Arc::default))
    }
}

#[cfg(test)]
mod tests {
    use super::memory::Memory;
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
    fn typed_credentials_move_to_the_keychain_and_leave_references() {
        let memory = Arc::new(Memory::default());
        let stores = Stores::default().with(KEYCHAIN, memory.clone());
        let mut config = server(
            r#"
command = "server"
auth_token = "typed-token"

[env]
GITHUB_TOKEN = "typed-env"
LOG_LEVEL = "debug"
FROM_ENV = "$HOME_TOKEN"
OPENAI_API_KEY = "keychain:mine"
"#,
        );
        assert_eq!(stores.plaintext(&config), ["token", "GITHUB_TOKEN"]);
        let mut kept = stores.keep("my server", &mut config);
        kept.sort();
        assert_eq!(kept, ["my-server.GITHUB_TOKEN", "my-server.token"]);
        assert_eq!(
            config.auth_token.as_ref().unwrap().as_str(),
            "keychain:my-server.token"
        );
        assert_eq!(
            config.env["GITHUB_TOKEN"],
            "keychain:my-server.GITHUB_TOKEN"
        );
        assert_eq!(config.env["LOG_LEVEL"], "debug");
        assert_eq!(config.env["FROM_ENV"], "$HOME_TOKEN");
        assert_eq!(config.env["OPENAI_API_KEY"], "keychain:mine");
        assert_eq!(
            memory.get("my-server.token").unwrap().unwrap().as_str(),
            "typed-token"
        );
        assert!(stores.plaintext(&config).is_empty());
        // A second pass has nothing left to move.
        assert!(stores.keep("my server", &mut config).is_empty());

        memory.set("mine", &"shared".to_string().into()).unwrap();
        let mut edited = config.clone();
        edited.auth_token = None;
        stores.forget("my server", &config, Some(&edited));
        assert!(memory.get("my-server.token").unwrap().is_none());
        assert!(memory.get("my-server.GITHUB_TOKEN").unwrap().is_some());
        stores.forget("my server", &config, None);
        assert!(memory.get("my-server.GITHUB_TOKEN").unwrap().is_none());
        assert!(memory.get("mine").unwrap().is_some());
    }

    #[test]
    fn a_store_that_refuses_leaves_the_value_where_it_was() {
        struct ReadOnly;
        impl SecretStore for ReadOnly {
            fn get(&self, _: &str) -> Result<Option<SecretString>, SecretError> {
                Ok(None)
            }
            fn set(&self, _: &str, _: &SecretString) -> Result<(), SecretError> {
                Err(SecretError::Unavailable {
                    store: KEYCHAIN.to_string(),
                    reason: "no credential store".to_string(),
                })
            }
            fn remove(&self, _: &str) -> Result<(), SecretError> {
                Ok(())
            }
        }
        let mut config = server("command = \"server\"\nauth_token = \"typed-token\"\n");
        let kept = Stores::default()
            .with(KEYCHAIN, Arc::new(ReadOnly))
            .keep("s", &mut config);
        assert!(kept.is_empty());
        assert_eq!(config.auth_token.unwrap().as_str(), "typed-token");
    }

    #[test]
    fn credential_names_are_told_from_ordinary_settings() {
        for key in [
            "GITHUB_TOKEN",
            "api_key",
            "OPENAI_API_KEY",
            "DB_PASSWORD",
            "KEY",
        ] {
            assert!(looks_secret(key), "{key}");
        }
        for key in [
            "LOG_LEVEL",
            "PATH",
            "KEYBOARD",
            "PORT",
            "MONKEY_MODE",
            "OAUTH_CLIENT_ID",
            "OAUTH_KEYCHAIN_ACCOUNT",
            "TOKEN_URL",
            "SECRET_FILE",
        ] {
            assert!(!looks_secret(key), "{key}");
        }
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
