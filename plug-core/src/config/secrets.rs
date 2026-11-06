//! The `[secrets]` section, and the ids of the stores built into Plug.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The store a pasted key goes to unless the person chooses another.
pub const KEYCHAIN: &str = "keychain";
/// The `.env` file beside `config.toml`.
pub const FILE: &str = "file";
/// The 1Password store's id: `op://vault/item/field` is a reference as it is.
pub const OP: &str = "op";
/// Where `{name}` goes in a store's command.
pub const NAME: &str = "{name}";

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

/// How long a store's command may take unless it says otherwise.
pub(crate) const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 15;

fn default_command_timeout() -> u64 {
    DEFAULT_COMMAND_TIMEOUT_SECS
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
            } else if !store.command.iter().skip(1).any(|arg| arg.contains(NAME)) {
                errors.push(format!(
                    "{at}: `command` needs `{}` where the secret's name goes",
                    NAME
                ));
            }
            if !(1..=120).contains(&store.timeout_secs) {
                errors.push(format!("{at}: `timeout_secs` is between 1 and 120"));
            }
        }
        errors
    }
}
