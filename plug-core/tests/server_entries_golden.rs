//! The server entries `plug import` reads, in `testdata/server_entries.json`.
//!
//! The app's paste parser reads the same entries when someone pastes a block
//! from another client's settings (`ServerDraftParserTests` in the app), so a
//! shape one side accepts the other accepts too.

use std::collections::BTreeMap;
use std::path::PathBuf;

use plug_core::clients::ClientTarget;
use plug_core::config::TransportType;

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    entry: serde_json::Value,
    server: Option<Expected>,
}

#[derive(serde::Deserialize, Debug, PartialEq)]
struct Expected {
    transport: TransportType,
    command: Option<String>,
    args: Vec<String>,
    url: Option<String>,
    env: BTreeMap<String, String>,
    enabled: bool,
}

#[test]
fn import_reads_every_shared_server_entry_as_pinned() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../testdata/server_entries.json");
    let cases: Vec<Case> =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read fixture")).expect("parse");
    for case in cases {
        let file = serde_json::json!({ "mcpServers": { case.name.clone(): case.entry } });
        let read = ClientTarget::ClaudeDesktop
            .layout()
            .read_servers(&file.to_string())
            .expect("reads");
        let got = read.into_iter().next().map(|(_, entry)| Expected {
            transport: entry.transport,
            command: entry.command,
            args: entry.args.unwrap_or_default(),
            url: entry.url,
            env: entry.env.into_iter().collect(),
            enabled: entry.enabled,
        });
        assert_eq!(got, case.server, "{}", case.name);
    }
}
