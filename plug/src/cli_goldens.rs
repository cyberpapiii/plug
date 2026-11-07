//! The `--output json` reports Plug.app reads, pinned in `testdata/cli`.
//!
//! The app runs `plug clients`, `plug import`, `plug doctor` and
//! `plug repair` and decodes what they print; its tests decode these same
//! files. Each file is written here from the types the commands print, so a
//! renamed field fails this test. When the change is meant, rerun with
//! `PLUG_REGENERATE_GOLDENS=1` and change the app to match.
//!
//! `commands.json` is the other half: every command line the app runs, or
//! writes into a client's settings, which the app's tests hold its services
//! to and this parses with the CLI's own parser.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::Parser;

use plug_core::clients::ClientTarget;
use plug_core::config::{ServerConfig, TransportType};
use plug_core::doctor::{CheckResult, CheckStatus, DoctorReport};
use plug_core::import::{DiscoveredServer, ImportReport, ScanResult};

use crate::commands::clients::{ClientView, LiveSessionView};
use crate::commands::doctor::{
    ClientRepairItem, ClientRepairReport, PlugLinkDisposition, UnifiedInstallSnapshot,
    doctor_json_value,
};
use crate::views::live::{LiveClientSupport, LiveInventory, LiveSessionTransportCounts};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../testdata/cli")
}

fn check(name: &str, value: serde_json::Value) {
    let contents = format!(
        "{}\n",
        serde_json::to_string_pretty(&value).expect("golden serializes")
    );
    let path = dir().join(name);
    if std::env::var_os("PLUG_REGENERATE_GOLDENS").is_some() {
        std::fs::create_dir_all(dir()).expect("create testdata/cli");
        std::fs::write(&path, contents).unwrap_or_else(|e| panic!("write {name}: {e}"));
        return;
    }
    let golden = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
    assert!(
        golden == contents,
        "testdata/cli/{name} is not what the command prints. If the report is meant to \
         change, rerun with PLUG_REGENERATE_GOLDENS=1 and change the app to match."
    );
}

fn client(target: ClientTarget, linked: Option<&str>, live_sessions: usize) -> ClientView {
    ClientView {
        name: target.display_name().to_string(),
        target: target.slug().to_string(),
        linked: linked.is_some(),
        linked_transport: linked.map(str::to_string),
        linked_endpoint: (linked == Some("http"))
            .then(|| "https://plug.example.com/mcp".to_string()),
        detected: true,
        live: live_sessions > 0,
        live_sessions,
        live_transports: if live_sessions > 0 {
            vec!["daemon_proxy".to_string()]
        } else {
            Vec::new()
        },
        lazy_tool_mode: "standard".to_string(),
        lazy_tool_mode_origin: "auto_default".to_string(),
        lazy_tool_mode_reason: "client handles the full tool list".to_string(),
    }
}

fn clients() -> serde_json::Value {
    let clients = [
        client(ClientTarget::ClaudeCode, Some("stdio"), 2),
        client(ClientTarget::Cursor, Some("http"), 0),
        client(ClientTarget::Zed, None, 0),
    ];
    let sessions = [LiveSessionView {
        transport: "daemon_proxy".to_string(),
        client_id: Some("connector-1".to_string()),
        session_id: "session-1".to_string(),
        client_type: "Claude Code".to_string(),
        target: Some("claude-code".to_string()),
        client_info: Some("claude-code".to_string()),
        host: None,
        key: Some("claude-code".to_string()),
        name: None,
        place: None,
        grant_name: None,
        access_key: None,
        connected_secs: 30,
        last_activity_secs: Some(2),
    }];
    let inventory = LiveInventory {
        session_count: 2,
        session_transports: LiveSessionTransportCounts {
            daemon_proxy: 2,
            http: 0,
        },
        scope: plug_core::ipc::LiveSessionInventoryScope::TransportComplete,
    };
    crate::views::clients::client_list_json(
        &clients,
        &sessions,
        &inventory,
        LiveClientSupport::Supported,
        plug_core::ipc::LiveSessionInventoryScope::TransportComplete,
        None,
        None,
    )
}

fn discovered(source: ClientTarget, name: &str, config: ServerConfig) -> DiscoveredServer {
    DiscoveredServer {
        name: name.to_string(),
        config,
        source,
        source_target: source.slug(),
        source_name: source.display_name(),
    }
}

fn import() -> serde_json::Value {
    let github = discovered(
        ClientTarget::Cursor,
        "github",
        ServerConfig {
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), "@github/mcp".to_string()],
            env: [("GITHUB_TOKEN".to_string(), "$GITHUB_TOKEN".to_string())].into(),
            timeout_secs: 90,
            ..Default::default()
        },
    );
    let notes = discovered(
        ClientTarget::VSCodeCopilot,
        "notes",
        ServerConfig {
            transport: TransportType::Sse,
            url: Some("https://notes.example.com/sse".to_string()),
            ..Default::default()
        },
    );
    let report = ImportReport {
        scanned: vec![
            ScanResult {
                source: ClientTarget::Cursor,
                source_target: ClientTarget::Cursor.slug(),
                source_name: ClientTarget::Cursor.display_name(),
                servers: vec![github.clone()],
                error: None,
            },
            ScanResult {
                source: ClientTarget::VSCodeCopilot,
                source_target: ClientTarget::VSCodeCopilot.slug(),
                source_name: ClientTarget::VSCodeCopilot.display_name(),
                servers: vec![notes.clone()],
                error: None,
            },
            ScanResult {
                source: ClientTarget::Zed,
                source_target: ClientTarget::Zed.slug(),
                source_name: ClientTarget::Zed.display_name(),
                servers: Vec::new(),
                error: Some("settings.json: expected value at line 1 column 1".to_string()),
            },
        ],
        new_servers: vec![github, notes],
        skipped: 1,
        duplicates_merged: 0,
    };
    serde_json::to_value(report).expect("import report serializes")
}

fn doctor() -> serde_json::Value {
    let check = |name: &str, status, fix: Option<&str>| CheckResult {
        name: name.to_string(),
        status,
        message: format!("{name} message"),
        fix_suggestion: fix.map(str::to_string),
    };
    let report = DoctorReport {
        checks: vec![
            check("config_exists", CheckStatus::Pass, None),
            check("port_available", CheckStatus::Warn, Some("Free the port.")),
            check(
                "server_connectivity",
                CheckStatus::Fail,
                Some("Restart it."),
            ),
        ],
        exit_code: 1,
    };
    let snapshot = UnifiedInstallSnapshot {
        app: None,
        shell_resolution: None,
        daemon_version: Some("0.9.0".to_string()),
        daemon_executable: None,
        ownership: plug_core::ipc::DaemonOwnershipMode::AppManaged,
        linked_clients: Vec::new(),
        adapters: Vec::new(),
        shadows: Vec::new(),
        launchd_jobs: Vec::new(),
        daemon_inspection_complete: true,
        adapter_inspection_complete: true,
        launchd_inspection_complete: true,
        app_service_daemon_invocation_verified: true,
        inspection_errors: Vec::new(),
        client_repair_needed: false,
    };
    doctor_json_value(&report, &snapshot)
}

fn repair_item(target: ClientTarget, disposition: PlugLinkDisposition) -> ClientRepairItem {
    ClientRepairItem {
        target: target.slug().to_string(),
        path: PathBuf::from(format!("/Users/example/.{}.json", target.slug())),
        disposition,
        changed: false,
        message: "Inspected.".to_string(),
    }
}

fn repair(needed: bool) -> serde_json::Value {
    let mut items = vec![
        repair_item(ClientTarget::ClaudeCode, PlugLinkDisposition::Canonical),
        repair_item(ClientTarget::Cursor, PlugLinkDisposition::Http),
        repair_item(ClientTarget::Zed, PlugLinkDisposition::Missing),
    ];
    if needed {
        items.push(repair_item(
            ClientTarget::CodexCli,
            PlugLinkDisposition::RecognizedLegacy,
        ));
    }
    serde_json::to_value(ClientRepairReport::new(
        PathBuf::from("/Applications/Plug.app/Contents/Resources/plug"),
        items,
    ))
    .expect("repair report serializes")
}

#[test]
fn the_cli_goldens_are_what_the_commands_print() {
    check("clients.json", clients());
    check("import.json", import());
    check("doctor.json", doctor());
    check("repair_clean.json", repair(false));
    check("repair_needed.json", repair(true));

    let mut files: Vec<String> = std::fs::read_dir(dir())
        .expect("testdata/cli")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "clients.json",
            "commands.json",
            "doctor.json",
            "import.json",
            "repair_clean.json",
            "repair_needed.json"
        ],
        "testdata/cli holds a file nothing checks"
    );
}

/// A flag or subcommand the app uses, renamed or removed here, fails this.
#[test]
fn every_command_the_app_runs_parses() {
    let commands: BTreeMap<String, Vec<String>> = serde_json::from_str(
        &std::fs::read_to_string(dir().join("commands.json")).expect("read commands.json"),
    )
    .expect("commands.json is a map of argument lists");
    assert!(!commands.is_empty());
    for (name, arguments) in commands {
        let argv = std::iter::once("plug".to_string()).chain(arguments.iter().cloned());
        match crate::Cli::try_parse_from(argv) {
            Ok(_) => {}
            Err(error) if error.kind() == clap::error::ErrorKind::DisplayVersion => {}
            Err(error) => panic!(
                "{name}: `plug {}` does not parse: {error}",
                arguments.join(" ")
            ),
        }
    }
}
