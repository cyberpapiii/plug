//! The daemon's rules the app checks a draft against before sending it, in
//! `testdata/draft_rules.json`. The app reads the same file
//! (`DraftRulesTests` in the app), so a rule changed on one side fails a test
//! until the other side follows.

use std::path::PathBuf;

use plug_core::config::ServerConfig;
use plug_core::downstream_identity::{CUSTOM_CLIENT_PREFIX, link_target_key};
use plug_core::openapi::MAX_OPERATIONS;
use plug_core::operator::{OperatorMutation, apply_operator_mutation};

#[derive(serde::Deserialize)]
struct Rules {
    api_operation_limit: usize,
    accounts: Vec<Account>,
    custom_client_prefix: String,
    custom_clients: Vec<CustomClient>,
    refused_custom_targets: Vec<String>,
}

#[derive(serde::Deserialize)]
struct Account {
    server: String,
    account: String,
    name: Option<String>,
}

#[derive(serde::Deserialize)]
struct CustomClient {
    target: Option<String>,
}

fn rules() -> Rules {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../testdata/draft_rules.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("read fixture")).expect("parse")
}

#[test]
fn an_api_server_takes_the_shared_number_of_operations() {
    assert_eq!(rules().api_operation_limit, MAX_OPERATIONS);
}

#[test]
fn an_account_is_named_by_the_shared_rule() {
    for case in rules().accounts {
        let path = plug_core::paths::test::scratch("draft-rules").join("config.toml");
        let server: ServerConfig =
            toml::from_str("transport = \"http\"\nurl = \"https://example.test/mcp\"\n").unwrap();
        apply_operator_mutation(
            &path,
            OperatorMutation::AddServer {
                name: case.server.clone(),
                server,
            },
        )
        .unwrap();
        let added = apply_operator_mutation(
            &path,
            OperatorMutation::AddAccount {
                server: case.server,
                account: case.account.clone(),
            },
        )
        .ok()
        .and_then(|(_, result)| result.server)
        .map(|server| server.name);
        assert_eq!(added, case.name, "account {:?}", case.account);
    }
}

#[test]
fn a_custom_client_key_follows_the_shared_rule() {
    let rules = rules();
    assert_eq!(rules.custom_client_prefix, CUSTOM_CLIENT_PREFIX);
    for target in rules
        .custom_clients
        .into_iter()
        .filter_map(|case| case.target)
    {
        assert_eq!(link_target_key(&target).as_deref(), Some(target.as_str()));
    }
    for target in rules.refused_custom_targets {
        assert_eq!(link_target_key(&target), None, "{target}");
    }
}
