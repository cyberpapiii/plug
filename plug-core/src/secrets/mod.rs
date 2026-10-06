//! Secret stores.
//!
//! A server's credential in `config.toml` may be a reference, `<store>:<name>`,
//! instead of the value. Only the service follows a reference, and only for the
//! server it is about to start, so a store that cannot answer holds up that one
//! server and nothing else.
//!
//! A store is anything that turns a name into a value. Three are built in:
//! the Keychain, which is the default and does not lock while the person is
//! logged in; the `.env` file; and 1Password, as `op://vault/item/field`. Any
//! other is a command the person names in `[secrets.stores]`.

pub mod command;
pub mod file;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[cfg(not(target_os = "linux"))]
use keyring as platform_keyring;
#[cfg(target_os = "linux")]
use keyring_core as platform_keyring;

use crate::config::ServerConfig;
use crate::types::SecretString;

/// The store a pasted key goes to unless the person chooses another.
pub const KEYCHAIN: &str = "keychain";

pub use command::OP;
pub use file::FILE;

/// How long one read may take unless the store says otherwise. A store that
/// asks a person for approval can wait forever; a server start cannot.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The stores a person adds, under `[secrets]` in `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsConfig {
    /// `[secrets.stores.<id>]`: a reference `<id>:<name>` runs the command.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stores: BTreeMap<String, CommandStoreConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandStoreConfig {
    /// The program and its arguments. `{name}` is replaced by the secret's
    /// name; what the command prints is the value. No shell is involved.
    pub command: Vec<String>,
    /// Seconds the command may take, for a tool that asks for approval.
    #[serde(default = "default_command_timeout")]
    pub timeout_secs: u64,
}

fn default_command_timeout() -> u64 {
    15
}

impl SecretsConfig {
    pub fn is_empty(&self) -> bool {
        self.stores.is_empty()
    }

    /// What is wrong with the stores as written, for config validation.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (id, store) in &self.stores {
            let at = format!("secrets.stores.{id}");
            let plain = !id.is_empty()
                && id.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-')
                });
            if !plain {
                errors.push(format!(
                    "{at}: a store's name is lowercase letters, digits, `_`, and `-`"
                ));
            }
            if [KEYCHAIN, FILE, OP, "http", "https"].contains(&id.as_str()) {
                errors.push(format!("{at}: `{id}` is taken; choose another name"));
            }
            if store.command.first().is_none_or(String::is_empty) {
                errors.push(format!("{at}: `command` needs a program to run"));
            } else if !store
                .command
                .iter()
                .skip(1)
                .any(|arg| arg.contains(command::NAME))
            {
                errors.push(format!(
                    "{at}: `command` needs `{}` where the secret's name goes",
                    command::NAME
                ));
            }
            if !(1..=120).contains(&store.timeout_secs) {
                errors.push(format!("{at}: `timeout_secs` is between 1 and 120"));
            }
        }
        errors
    }
}

/// The stores the running service follows references with. Set from the
/// config when it loads; the built-in ones until then.
static CURRENT: RwLock<Option<Stores>> = RwLock::new(None);

/// The last value each reference gave, so a store that locks after the
/// service started does not take down a server that restarts. Memory only.
static LAST: Mutex<Option<HashMap<String, SecretString>>> = Mutex::new(None);

fn last(reference: &str) -> Option<SecretString> {
    LAST.lock().ok()?.as_ref()?.get(reference).cloned()
}

fn remember(reference: &str, value: Option<&SecretString>) {
    if let Ok(mut last) = LAST.lock() {
        let last = last.get_or_insert_with(HashMap::new);
        match value {
            Some(value) => last.insert(reference.to_string(), value.clone()),
            None => last.remove(reference),
        };
    }
}

/// What to do about a missing secret, for the stores Plug can write.
fn set_hint(store: &str, name: &str) -> String {
    match store {
        KEYCHAIN => format!("; run `plug secret set {name}`"),
        FILE => format!("; run `plug secret set --store file {name}`"),
        _ => String::new(),
    }
}

/// Why a reference did not become a value. The messages name the store and
/// the secret, never a value.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecretError {
    #[error("no secret named `{name}` in {store}{}", set_hint(.store, .name))]
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

    /// How long a read may take.
    fn timeout(&self) -> Duration {
        READ_TIMEOUT
    }

    /// Whether `name` is one this store can be asked for.
    fn accepts(&self, name: &str) -> bool {
        valid_name(name)
    }
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
///
/// A name that would lose something, a character Plug has to replace or a
/// tail that does not fit, ends in a digest of the whole server and field,
/// so two servers or two fields never share one.
pub fn name_for(server: &str, field: &str) -> String {
    let plain = format!("{server}.{field}");
    let fits = |text: &str| {
        text.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    };
    if fits(server) && fits(field) && plain.len() <= MAX_NAME {
        return plain;
    }
    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(format!("{server}\0{field}")));
    let mut name = readable_name(server, field);
    name.truncate(MAX_NAME - NAME_DIGEST - 1);
    format!("{name}.{}", &digest[..NAME_DIGEST])
}

const MAX_NAME: usize = 64;
const NAME_DIGEST: usize = 12;

/// The server and field with what a name cannot hold replaced, cut to fit.
/// Names were once only this, so a stored secret may still go by it.
fn readable_name(server: &str, field: &str) -> String {
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
    name.truncate(MAX_NAME);
    name
}

/// Whether `name` is one Plug made for `field` of `server`, now or under
/// the older naming.
fn made_for(server: &str, field: &str, name: &str) -> bool {
    name == name_for(server, field) || name == readable_name(server, field)
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
    /// is a map in memory and the file is a temporary one, so no test reads
    /// or writes the secrets of the person running it.
    pub fn builtin() -> Self {
        let (keychain, file): (Arc<dyn SecretStore>, _) = if crate::oauth::uses_test_credentials() {
            let path =
                std::env::temp_dir().join(format!("plug-test-secrets-{}", std::process::id()));
            (memory::keychain(), file::File::new(path.join(".env")))
        } else {
            (
                Arc::new(Keychain),
                file::File::new(crate::dotenv::env_file_path()),
            )
        };
        Self::default()
            .with(KEYCHAIN, keychain)
            .with(FILE, Arc::new(file))
            .with(OP, Arc::new(command::Command::one_password()))
    }

    /// The built-in stores and the ones `config` adds.
    pub fn from_config(config: &SecretsConfig) -> Self {
        config
            .stores
            .iter()
            .fold(Self::builtin(), |stores, (id, store)| {
                stores.with(
                    id,
                    Arc::new(command::Command::new(
                        id,
                        store.command.clone(),
                        Duration::from_secs(store.timeout_secs),
                    )),
                )
            })
    }

    /// Make `config`'s stores the ones the service follows references with.
    pub fn configure(config: &SecretsConfig) {
        if let Ok(mut current) = CURRENT.write() {
            *current = Some(Self::from_config(config));
        }
    }

    /// The stores the service follows references with.
    pub fn current() -> Self {
        CURRENT
            .read()
            .ok()
            .and_then(|current| current.clone())
            .unwrap_or_else(Self::builtin)
    }

    /// The stores a value can be put in, for a command's help and errors.
    pub fn ids(&self) -> Vec<&str> {
        self.stores.iter().map(|(id, _)| id.as_str()).collect()
    }

    /// Whether `server` refers to any store, so the caller can prepare what
    /// a store needs before the first read.
    pub fn refers(&self, server: &ServerConfig) -> bool {
        server
            .auth_token
            .iter()
            .map(|token| token.as_str())
            .chain(server.env.values().map(String::as_str))
            .any(|value| self.parse(value).is_some())
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
        self.get(store)
            .is_some_and(|found| found.accepts(name))
            .then_some((store, name))
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
        self.keep_in(KEYCHAIN, server_name, server)
    }

    /// [`Stores::keep`], into the store named `id` in place of the Keychain.
    pub fn keep_in(&self, id: &str, server_name: &str, server: &mut ServerConfig) -> Vec<String> {
        self.keep_staged(id, server_name, server)
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    /// [`Stores::keep_in`], also returning what each name held before, so a
    /// change that is then refused can be undone with [`Stores::put_back`].
    pub fn keep_staged(
        &self,
        id: &str,
        server_name: &str,
        server: &mut ServerConfig,
    ) -> Vec<(String, Option<SecretString>)> {
        let Some(store) = self.get(id) else {
            return Vec::new();
        };
        let mut kept = Vec::new();
        let mut put = |field: &str, value: &str| -> Option<String> {
            if !is_literal(value) || self.parse(value).is_some() {
                return None;
            }
            let name = name_for(server_name, field);
            // Without the old value there is no undoing this, so a store
            // that cannot be read is not written to.
            let before = match store.get(&name) {
                Ok(before) => before,
                Err(error) => {
                    tracing::warn!(server = %server_name, %error, "secret stays in the config file");
                    return None;
                }
            };
            match store.set(&name, &value.to_string().into()) {
                Ok(()) => {
                    kept.push((name.clone(), before));
                    Some(reference(id, &name))
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

    /// Undo [`Stores::keep_staged`] in the store named `id`: each name gets
    /// back what it held, or is removed when it held nothing.
    pub fn put_back(&self, id: &str, staged: Vec<(String, Option<SecretString>)>) {
        let Some(store) = self.get(id) else {
            return;
        };
        for (name, before) in staged {
            let outcome = match &before {
                Some(value) => store.set(&name, value),
                None => store.remove(&name),
            };
            if let Err(error) = outcome {
                tracing::warn!(secret = %name, %error, "could not undo a stored secret");
            }
        }
    }

    /// Remove what [`Stores::keep`] stored for `server` and no server in
    /// `remaining`, the config as it now stands, refers to. A second account
    /// refers to the secrets of the server it was copied from, and those
    /// stay until the last of them is gone. A secret the person named
    /// themselves may serve other servers, so it is left alone.
    pub fn forget<'a>(
        &self,
        server_name: &str,
        server: &ServerConfig,
        remaining: impl IntoIterator<Item = &'a ServerConfig>,
    ) {
        fn fields(server: &ServerConfig) -> impl Iterator<Item = (&str, &str)> {
            server
                .auth_token
                .iter()
                .map(|token| ("token", token.as_str()))
                .chain(
                    server
                        .env
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.as_str())),
                )
        }
        let in_use: std::collections::HashSet<&str> = remaining
            .into_iter()
            .flat_map(|server| fields(server).map(|(_, value)| value))
            .collect();
        // An account copy is named `<server>-<account>` and holds the
        // secrets made for `<server>`.
        let owners = [
            Some(server_name),
            server_name.rsplit_once('-').map(|(base, _)| base),
        ];
        for (field, value) in fields(server) {
            if in_use.contains(value) {
                continue;
            }
            if let Some((id, name)) = self.parse(value)
                && (id == KEYCHAIN || id == FILE)
                && owners
                    .iter()
                    .flatten()
                    .any(|owner| made_for(owner, field, name))
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
        // The store stops itself at its own limit; this is the backstop.
        let limit = store.timeout() + Duration::from_secs(2);
        let owned = name.to_string();
        let read = tokio::task::spawn_blocking(move || store.get(&owned));
        let outcome = match tokio::time::timeout(limit, read).await {
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
            Ok(Ok(Ok(Some(secret)))) => Ok(secret),
        };
        match outcome {
            Ok(secret) => {
                remember(value, Some(&secret));
                Ok(Some(secret))
            }
            // The store answered: the secret is gone, and so is the copy.
            Err(error @ (SecretError::Missing { .. } | SecretError::InvalidName(_))) => {
                remember(value, None);
                Err(error)
            }
            // The store did not answer. A value it gave earlier still works.
            Err(error) => match last(value) {
                Some(secret) => {
                    tracing::warn!(store = %id, %error, "using the value read earlier");
                    Ok(Some(secret))
                }
                None => Err(error),
            },
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

    #[tokio::test]
    async fn a_store_that_stops_answering_still_gives_the_value_it_gave() {
        struct Flaky(std::sync::atomic::AtomicBool);
        impl SecretStore for Flaky {
            fn get(&self, _: &str) -> Result<Option<SecretString>, SecretError> {
                if self.0.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    Err(SecretError::Unavailable {
                        store: "flaky".to_string(),
                        reason: "it is locked".to_string(),
                    })
                } else {
                    Ok(Some("first-read".to_string().into()))
                }
            }
            fn set(&self, _: &str, _: &SecretString) -> Result<(), SecretError> {
                unreachable!()
            }
            fn remove(&self, _: &str) -> Result<(), SecretError> {
                unreachable!()
            }
        }
        let stores = Stores::default().with("flaky", Arc::new(Flaky(Default::default())));
        let config = server("command = \"server\"\nauth_token = \"flaky:kept-across-lock\"\n");
        for _ in 0..2 {
            let resolved = stores.resolve_server(&config).await.unwrap();
            assert_eq!(resolved.auth_token.as_ref().unwrap().as_str(), "first-read");
        }
    }

    #[test]
    fn a_command_in_config_is_a_store_and_one_password_references_are_whole() {
        let config: SecretsConfig =
            toml::from_str("[stores.bw]\ncommand = [\"bw\", \"get\", \"password\", \"{name}\"]\n")
                .unwrap();
        assert!(config.validate().is_empty());
        assert_eq!(config.stores["bw"].timeout_secs, 15);
        let stores = Stores::from_config(&config);
        assert_eq!(stores.parse("bw:github"), Some(("bw", "github")));
        assert_eq!(stores.parse("file:API_KEY"), Some(("file", "API_KEY")));
        assert_eq!(
            stores.parse("op://Private/My Item/password"),
            Some(("op", "//Private/My Item/password"))
        );
        assert_eq!(stores.parse("https://example.com/mcp"), None);

        let bad: SecretsConfig = toml::from_str(
            "[stores.keychain]\ncommand = [\"x\", \"{name}\"]\n[stores.Bad]\ncommand = [\"x\"]\ntimeout_secs = 0\n",
        )
        .unwrap();
        let errors = bad.validate().join("\n");
        assert!(errors.contains("`keychain` is taken"), "{errors}");
        assert!(errors.contains("lowercase letters"), "{errors}");
        assert!(errors.contains("where the secret's name goes"), "{errors}");
        assert!(errors.contains("between 1 and 120"), "{errors}");
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
        let mut kept = stores.keep("my-server", &mut config);
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
        assert!(stores.keep("my-server", &mut config).is_empty());

        memory.set("mine", &"shared".to_string().into()).unwrap();
        let mut edited = config.clone();
        edited.auth_token = None;
        stores.forget("my-server", &config, [&edited]);
        assert!(memory.get("my-server.token").unwrap().is_none());
        assert!(memory.get("my-server.GITHUB_TOKEN").unwrap().is_some());
        stores.forget("my-server", &config, []);
        assert!(memory.get("my-server.GITHUB_TOKEN").unwrap().is_none());
        assert!(memory.get("mine").unwrap().is_some());
    }

    #[test]
    fn no_two_servers_or_fields_share_a_secret_name() {
        // A plain name is kept as it is.
        assert_eq!(name_for("github", "API_TOKEN"), "github.API_TOKEN");
        // Names that read alike once punctuation is replaced.
        assert_ne!(name_for("a.b", "token"), name_for("a-b", "token"));
        assert_ne!(name_for("a b", "token"), name_for("a.b", "token"));
        // A server name long enough to fill the whole name.
        let long = "s".repeat(80);
        let (one, two) = (name_for(&long, "API_TOKEN"), name_for(&long, "OTHER_KEY"));
        assert_ne!(one, two);
        for name in [one, two, name_for("a.b", "token")] {
            assert!(valid_name(&name), "{name}");
        }
    }

    #[test]
    fn a_secret_another_server_still_uses_is_not_removed() {
        let memory = Arc::new(Memory::default());
        let stores = Stores::default().with(KEYCHAIN, memory.clone());
        let mut original = server("command = \"server\"\nauth_token = \"typed-token\"\n");
        stores.keep("workspace", &mut original);
        // A second account is a copy, references and all.
        let copy = original.clone();

        stores.forget("workspace", &original, [&copy]);
        assert!(memory.get("workspace.token").unwrap().is_some());
        // The copy is the last to go and takes the secret with it.
        stores.forget("workspace-personal", &copy, []);
        assert!(memory.get("workspace.token").unwrap().is_none());
    }

    #[test]
    fn a_stored_secret_can_be_put_back_as_it_was() {
        let memory = Arc::new(Memory::default());
        let stores = Stores::default().with(KEYCHAIN, memory.clone());
        memory
            .set("github.token", &"working".to_string().into())
            .unwrap();
        let mut config =
            server("command = \"server\"\nauth_token = \"typed\"\n[env]\nAPI_KEY = \"new\"\n");
        let staged = stores.keep_staged(KEYCHAIN, "github", &mut config);
        assert_eq!(
            memory.get("github.token").unwrap().unwrap().as_str(),
            "typed"
        );

        stores.put_back(KEYCHAIN, staged);
        assert_eq!(
            memory.get("github.token").unwrap().unwrap().as_str(),
            "working"
        );
        assert!(memory.get("github.API_KEY").unwrap().is_none());
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
