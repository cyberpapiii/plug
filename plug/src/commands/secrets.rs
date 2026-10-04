//! `plug secret`: put a value in a secret store, or take it out.
//!
//! The value is read from a hidden prompt or from standard input. It is never
//! an argument, so it never reaches shell history or the process list, and no
//! command prints one back.

use std::io::{IsTerminal, Read};

use plug_core::secrets::{self, KEYCHAIN, SecretError, Stores};
use plug_core::types::SecretString;

use crate::ui;

pub(crate) async fn cmd_secret(
    config_path: Option<&std::path::PathBuf>,
    command: crate::SecretCommands,
) -> anyhow::Result<()> {
    let stores = Stores::builtin();
    let keychain = stores
        .get(KEYCHAIN)
        .expect("the Keychain store is built in");
    match command {
        crate::SecretCommands::Set { name } => {
            let name = checked(name)?;
            let value = read_value(&name)?;
            keychain.set(&name, &value)?;
            ui::print_success_line(format!(
                "Stored `{name}` in the Keychain. Use it in a server as `{}`.",
                secrets::reference(KEYCHAIN, &name)
            ));
        }
        crate::SecretCommands::Rm { name } => {
            let name = checked(name)?;
            keychain.remove(&name)?;
            ui::print_success_line(format!(
                "Removed `{name}` from the Keychain. A running server keeps the value until it restarts."
            ));
        }
        crate::SecretCommands::Move => move_keys(config_path, &stores).await?,
    }
    Ok(())
}

/// Hand each server that has a key in the clear back to the service, which
/// stores the key and writes a reference, the same as when a key is typed.
async fn move_keys(
    config_path: Option<&std::path::PathBuf>,
    stores: &Stores,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        config_path.is_none(),
        "`plug secret move` works on the config Plug is running, not on --config"
    );
    let (path, config) = super::config::load_editable_config(None)?;
    let mut servers: Vec<_> = config
        .servers
        .into_iter()
        .filter(|(_, server)| !stores.plaintext(server).is_empty())
        .collect();
    servers.sort_by(|a, b| a.0.cmp(&b.0));
    if servers.is_empty() {
        ui::print_info_line("No keys are written in the config file.");
        return Ok(());
    }
    for (name, server) in servers {
        let fields = stores.plaintext(&server).join(", ");
        super::servers::apply_server_mutation(
            None,
            plug_core::operator::OperatorMutation::UpdateServer {
                name: name.clone(),
                server,
            },
        )
        .await?;
        let left = plug_core::operator::load_editable_config(&path)?
            .servers
            .get(&name)
            .map(|server| stores.plaintext(server))
            .unwrap_or_default();
        if left.is_empty() {
            ui::print_success_line(format!("{name}: moved {fields} to the Keychain."));
        } else {
            ui::print_warning_line(format!(
                "{name}: the Keychain did not take {}; left in the config file.",
                left.join(", ")
            ));
        }
    }
    Ok(())
}

fn checked(name: String) -> Result<String, SecretError> {
    if secrets::valid_name(&name) {
        Ok(name)
    } else {
        Err(SecretError::InvalidName(name))
    }
}

/// A hidden prompt at a terminal; otherwise standard input, so another
/// password manager can hand the value over without it being typed.
fn read_value(name: &str) -> anyhow::Result<SecretString> {
    let value = if std::io::stdin().is_terminal() {
        dialoguer::Password::new()
            .with_prompt(format!("Value for `{name}` (input hidden)"))
            .interact()?
    } else {
        let mut piped = String::new();
        std::io::stdin().read_to_string(&mut piped)?;
        piped
    };
    from_input(value)
}

/// The value as entered, without the line ending a pipe or a paste adds.
fn from_input(value: String) -> anyhow::Result<SecretString> {
    let value = value.trim_end_matches(['\r', '\n']);
    anyhow::ensure!(!value.is_empty(), "no value given; nothing was stored");
    Ok(value.to_string().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_piped_value_loses_only_its_line_ending() {
        assert_eq!(from_input(" a b \r\n".into()).unwrap().as_str(), " a b ");
        assert!(from_input("\n".into()).is_err());
    }

    #[test]
    fn a_name_that_is_not_safe_is_refused_before_any_store_is_touched() {
        assert!(checked("github".into()).is_ok());
        assert!(matches!(
            checked("../x".into()),
            Err(SecretError::InvalidName(_))
        ));
    }
}
