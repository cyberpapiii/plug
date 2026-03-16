use dialoguer::console::style;

use crate::OutputFormat;
use crate::commands::clients::{cmd_link, execute_export};
use crate::ui::{
    cli_prompt_theme, print_banner, print_heading, print_info_line, print_success_line,
};

pub(crate) fn cmd_import(
    config_path: Option<&std::path::PathBuf>,
    clients: Option<Vec<String>>,
    _all: bool,
    dry_run: bool,
    yes: bool,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    use dialoguer::MultiSelect;
    use plug_core::import::{self, ClientSource};

    let sources = match clients {
        Some(names) => names
            .iter()
            .filter_map(|n| match n.as_str() {
                "claude-desktop" => Some(ClientSource::ClaudeDesktop),
                "claude-code" => Some(ClientSource::ClaudeCode),
                "cursor" => Some(ClientSource::Cursor),
                "windsurf" => Some(ClientSource::Windsurf),
                "vscode" => Some(ClientSource::VSCodeCopilot),
                "gemini-cli" => Some(ClientSource::GeminiCli),
                "codex-cli" => Some(ClientSource::CodexCli),
                "opencode" => Some(ClientSource::OpenCode),
                "zed" => Some(ClientSource::Zed),
                "cline" => Some(ClientSource::Cline),
                "cline-cli" => Some(ClientSource::ClineCli),
                "roocode" => Some(ClientSource::RooCode),
                "factory" => Some(ClientSource::Factory),
                "nanobot" => Some(ClientSource::Nanobot),
                "junie" => Some(ClientSource::Junie),
                "kilo" => Some(ClientSource::Kilo),
                "antigravity" => Some(ClientSource::Antigravity),
                "goose" => Some(ClientSource::Goose),
                _ => None,
            })
            .collect(),
        None => ClientSource::all().to_vec(),
    };

    let existing = match plug_core::config::load_config(config_path) {
        Ok(cfg) => cfg.servers,
        Err(_) => std::collections::HashMap::new(),
    };

    if matches!(output, OutputFormat::Text) {
        print_banner(
            "◆",
            "Import",
            "Scan existing AI client configs for MCP servers",
        );
        print_info_line(style("Scanning for MCP servers...").bold());
    }
    let report = import::import(&existing, &sources);

    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            for res in &report.scanned {
                if let Some(ref e) = res.error {
                    eprintln!(
                        "  {} {:<16} {}",
                        style("!").yellow().bold(),
                        res.source,
                        style(e).red()
                    );
                }
            }
            if report.new_servers.is_empty() {
                println!();
                print_success_line("No new servers found.");
                return Ok(());
            }
            if dry_run {
                println!();
                print_success_line(format!(
                    "Found {} importable server(s).",
                    report.new_servers.len()
                ));
                return Ok(());
            }

            println!();
            print_heading("Discovered");
            for server in &report.new_servers {
                println!(
                    "  {} {:<18} {}",
                    style("·").dim(),
                    style(&server.name).bold(),
                    style(format!("from {}", server.source)).dim()
                );
            }

            let selections = if yes {
                (0..report.new_servers.len()).collect::<Vec<_>>()
            } else {
                let labels: Vec<_> = report
                    .new_servers
                    .iter()
                    .map(|s| {
                        format!(
                            "{:<15} {}",
                            style(&s.name).bold(),
                            style(format!("(from {})", s.source)).dim()
                        )
                    })
                    .collect();
                MultiSelect::with_theme(&cli_prompt_theme())
                    .with_prompt("Select servers to import")
                    .items(&labels)
                    .defaults(&vec![true; labels.len()])
                    .interact()?
            };
            if selections.is_empty() {
                return Ok(());
            }

            let config_file = config_path
                .cloned()
                .unwrap_or_else(plug_core::config::default_config_path);
            let to_import: Vec<plug_core::import::DiscoveredServer> = selections
                .iter()
                .map(|&i| report.new_servers[i].clone())
                .collect();
            let existing_names: Vec<String> = existing.keys().cloned().collect();
            let toml = import::servers_to_toml(&to_import, &existing_names);

            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&config_file)?;
            file.write_all(toml.as_bytes())?;
            println!();
            print_success_line(format!("Imported {} server(s).", to_import.len()));
        }
    }
    Ok(())
}

pub(crate) async fn cmd_doctor(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let resolved = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);
    let config = plug_core::config::load_config(config_path)?;
    let mut report = plug_core::doctor::run_doctor(&config, &resolved).await;
    report
        .checks
        .extend(runtime_doctor_checks().await);
    report.exit_code = if report
        .checks
        .iter()
        .any(|c| matches!(c.status, plug_core::doctor::CheckStatus::Fail))
    {
        1
    } else if report
        .checks
        .iter()
        .any(|c| matches!(c.status, plug_core::doctor::CheckStatus::Warn))
    {
        2
    } else {
        0
    };
    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            print_banner("◆", "Doctor", "Diagnose problems with your plug setup");
            for c in &report.checks {
                let marker = match c.status {
                    plug_core::doctor::CheckStatus::Pass => style("●").green().bold(),
                    plug_core::doctor::CheckStatus::Warn => style("!").yellow().bold(),
                    plug_core::doctor::CheckStatus::Fail => style("×").red().bold(),
                };
                let prefix_text = format!("  {} {:<24} ", "•", c.name);
                let prefix_display =
                    format!("  {} {} ", marker, style(format!("{:<24}", c.name)).bold());
                crate::ui::print_wrapped_rows(
                    &prefix_text,
                    prefix_display,
                    &c.message,
                    crate::ui::terminal_width(),
                    |line| style(line),
                );
            }
        }
    }
    Ok(())
}

async fn runtime_doctor_checks() -> Vec<plug_core::doctor::CheckResult> {
    let mut checks = Vec::new();

    if let Ok(plug_core::ipc::IpcResponse::Status {
        servers,
        clients,
        uptime_secs,
    }) = crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::Status).await
    {
        let mut healthy = 0usize;
        let mut degraded = 0usize;
        let mut failed = 0usize;
        let mut auth_required = 0usize;

        for server in servers.iter().filter(|s| s.server_id != "__plug_internal__") {
            match server.health {
                plug_core::types::ServerHealth::Healthy => healthy += 1,
                plug_core::types::ServerHealth::Degraded => degraded += 1,
                plug_core::types::ServerHealth::Failed => failed += 1,
                plug_core::types::ServerHealth::AuthRequired => auth_required += 1,
            }
        }

        let runtime_status = if failed > 0 {
            plug_core::doctor::CheckStatus::Fail
        } else if degraded > 0 || auth_required > 0 {
            plug_core::doctor::CheckStatus::Warn
        } else {
            plug_core::doctor::CheckStatus::Pass
        };

        checks.push(plug_core::doctor::CheckResult {
            name: "runtime_health".to_string(),
            status: runtime_status,
            message: format!(
                "Daemon running: uptime={}s, clients={}, healthy={}, degraded={}, auth_required={}, failed={}",
                uptime_secs, clients, healthy, degraded, auth_required, failed
            ),
            fix_suggestion: if failed > 0 || auth_required > 0 {
                Some("Use `plug status` and `plug auth status` to inspect affected servers and next actions".to_string())
            } else {
                None
            },
        });
    }

    if let Ok(plug_core::ipc::IpcResponse::AuthStatus { servers }) =
        crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::AuthStatus).await
    {
        let mut reauth = Vec::new();
        let mut missing = Vec::new();
        let mut degraded = Vec::new();

        for server in &servers {
            match (server.authenticated, server.health) {
                (false, plug_core::types::ServerHealth::AuthRequired) => {
                    missing.push(server.name.clone())
                }
                (true, plug_core::types::ServerHealth::AuthRequired) => {
                    reauth.push(server.name.clone())
                }
                (_, plug_core::types::ServerHealth::Degraded) => {
                    degraded.push(server.name.clone())
                }
                _ => {}
            }
        }

        if !(reauth.is_empty() && missing.is_empty() && degraded.is_empty()) {
            checks.push(plug_core::doctor::CheckResult {
                name: "runtime_auth".to_string(),
                status: plug_core::doctor::CheckStatus::Warn,
                message: format!(
                    "{}{}{}",
                    if missing.is_empty() {
                        String::new()
                    } else {
                        format!("missing credentials: {}; ", missing.join(", "))
                    },
                    if reauth.is_empty() {
                        String::new()
                    } else {
                        format!("re-auth required: {}; ", reauth.join(", "))
                    },
                    if degraded.is_empty() {
                        String::new()
                    } else {
                        format!("degraded auth/runtime: {}", degraded.join(", "))
                    }
                )
                .trim_end_matches("; ")
                .to_string(),
                fix_suggestion: Some(
                    "Run `plug auth status` for token state and `plug auth login --server <name>` where re-auth is required".to_string(),
                ),
            });
        }
    }

    checks
}

pub(crate) async fn cmd_reload() -> anyhow::Result<()> {
    let auth = crate::daemon::read_auth_token()?;
    let req = plug_core::ipc::IpcRequest::Reload { auth_token: auth };
    match crate::daemon::ipc_request(&req).await? {
        plug_core::ipc::IpcResponse::Ok => {}
        plug_core::ipc::IpcResponse::Error { code, message } => {
            anyhow::bail!("{code}: {message}");
        }
        other => anyhow::bail!("unexpected daemon response: {other:?}"),
    }
    Ok(())
}

pub(crate) fn cmd_setup(config_path: Option<&std::path::PathBuf>, yes: bool) -> anyhow::Result<()> {
    use dialoguer::Confirm;

    print_banner(
        "◆",
        "Plug setup",
        "Discover servers, import config, and link your AI clients",
    );
    let existing = match plug_core::config::load_config(config_path) {
        Ok(cfg) => cfg.servers,
        Err(_) => std::collections::HashMap::new(),
    };
    let report = plug_core::import::import(&existing, plug_core::import::ClientSource::all());
    if !report.new_servers.is_empty() {
        print_heading("Discovered");
        print_success_line(format!("Found {} server(s).", report.new_servers.len()));
        for server in &report.new_servers {
            println!(
                "  {} {:<18} {}",
                style("·").dim(),
                style(&server.name).bold(),
                style(format!("from {}", server.source)).dim()
            );
        }
        println!();
        if yes
            || Confirm::with_theme(&cli_prompt_theme())
                .with_prompt("Import them?")
                .default(true)
                .interact()?
        {
            let path = config_path
                .cloned()
                .unwrap_or_else(plug_core::config::default_config_path);
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p)?;
            }
            let existing_names: Vec<String> = existing.keys().cloned().collect();
            let toml = plug_core::import::servers_to_toml(&report.new_servers, &existing_names);
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            file.write_all(toml.as_bytes())?;
        }
    }
    cmd_link(Vec::new(), false, yes)?;
    Ok(())
}

pub(crate) fn cmd_repair() -> anyhow::Result<()> {
    println!(
        "{} {}",
        style("◆").cyan().bold(),
        style("Repairing AI client configurations...").bold()
    );

    let all_clients = [
        "claude-desktop",
        "claude-code",
        "cursor",
        "vscode",
        "windsurf",
        "gemini-cli",
        "codex-cli",
        "opencode",
        "zed",
        "cline",
        "cline-cli",
        "roocode",
        "factory",
        "nanobot",
        "junie",
        "kilo",
        "antigravity",
        "goose",
    ];

    let mut repaired_count = 0;

    for target in all_clients {
        if let Some(transport) = crate::commands::clients::linked_client_transport(target, false) {
            print!("  {} Refreshing {}... ", style("›").cyan().bold(), target);
            if let Err(e) = execute_export(
                target,
                matches!(transport, plug_core::export::ExportTransport::Http),
                3282,
                true,
                false,
            ) {
                println!("{}", style(format!("failed: {e}")).red());
            } else {
                println!("{}", style("done").green());
                repaired_count += 1;
            }
        }
    }

    if repaired_count == 0 {
        println!(
            "\n{} No linked clients found to repair.",
            style("•").green().bold()
        );
    } else {
        println!(
            "\n{} Successfully repaired {} client configuration(s).",
            style("•").green().bold(),
            repaired_count
        );
    }

    Ok(())
}
