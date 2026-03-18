use dialoguer::console::style;

use crate::OutputFormat;
use crate::commands::clients::{cmd_link, execute_export};
use crate::ui::{
    cli_prompt_theme, print_banner, print_heading, print_info_line, print_next_step,
    print_success_line,
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
    report.checks.extend(runtime_doctor_checks().await);
    if let Some(interpreted) = synthesize_doctor_interpretation(&report.checks) {
        report.checks.push(interpreted);
    }
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
                    &doctor_check_details(c),
                    crate::ui::terminal_width(),
                    |line| style(line),
                );
            }
            let next_steps = doctor_next_steps(&report.checks);
            if !next_steps.is_empty() {
                println!();
                print_heading("Next");
                for (index, step) in next_steps.iter().enumerate() {
                    print_next_step(index + 1, step);
                }
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
        let mut failed_servers = Vec::new();
        let mut degraded_servers = Vec::new();

        for server in servers
            .iter()
            .filter(|s| s.server_id != "__plug_internal__")
        {
            match server.health {
                plug_core::types::ServerHealth::Healthy => healthy += 1,
                plug_core::types::ServerHealth::Degraded => {
                    degraded += 1;
                    degraded_servers.push(server.server_id.clone());
                }
                plug_core::types::ServerHealth::Failed => {
                    failed += 1;
                    failed_servers.push(server.server_id.clone());
                }
                plug_core::types::ServerHealth::AuthRequired => auth_required += 1,
            }
        }

        let runtime_status = if failed > 0 || degraded > 0 || auth_required > 0 {
            plug_core::doctor::CheckStatus::Warn
        } else {
            plug_core::doctor::CheckStatus::Pass
        };

        checks.push(plug_core::doctor::CheckResult {
            name: "runtime_health".to_string(),
            status: runtime_status,
            message: format!(
                "Daemon running: uptime={}s, daemon_proxy_clients={}, healthy={}, degraded={}, auth_required={}, failed={}",
                uptime_secs, clients, healthy, degraded, auth_required, failed
            ),
            fix_suggestion: if failed > 0 || degraded > 0 || auth_required > 0 {
                Some(
                    "Use `plug status` for affected servers, then `plug auth status` for auth recovery details".to_string(),
                )
            } else {
                None
            },
        });

        if !failed_servers.is_empty() {
            checks.push(plug_core::doctor::CheckResult {
                name: "runtime_failures".to_string(),
                status: plug_core::doctor::CheckStatus::Fail,
                message: format!("failing servers: {}", failed_servers.join(", ")),
                fix_suggestion: Some(
                    "Run `plug status` for the failing servers, then compare with `plug doctor` cold checks before restarting or editing config".to_string(),
                ),
            });
        }

        if !degraded_servers.is_empty() {
            checks.push(plug_core::doctor::CheckResult {
                name: "runtime_degraded".to_string(),
                status: plug_core::doctor::CheckStatus::Warn,
                message: format!("degraded servers: {}", degraded_servers.join(", ")),
                fix_suggestion: Some(
                    "Compare `plug status` and `plug doctor` to separate transient runtime degradation from cold connectivity or auth issues".to_string(),
                ),
            });
        }
    }

    if let Ok(plug_core::ipc::IpcResponse::AuthStatus { servers }) =
        crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::AuthStatus).await
    {
        checks.extend(runtime_auth_checks(&servers));
    }

    checks
}

#[cfg(test)]
fn runtime_health_checks_for_tests(
    servers: &[plug_core::types::ServerStatus],
    clients: usize,
    uptime_secs: u64,
) -> Vec<plug_core::doctor::CheckResult> {
    let mut healthy = 0usize;
    let mut degraded = 0usize;
    let mut failed = 0usize;
    let mut auth_required = 0usize;
    let mut failed_servers = Vec::new();
    let mut degraded_servers = Vec::new();

    for server in servers
        .iter()
        .filter(|s| s.server_id != "__plug_internal__")
    {
        match server.health {
            plug_core::types::ServerHealth::Healthy => healthy += 1,
            plug_core::types::ServerHealth::Degraded => {
                degraded += 1;
                degraded_servers.push(server.server_id.clone());
            }
            plug_core::types::ServerHealth::Failed => {
                failed += 1;
                failed_servers.push(server.server_id.clone());
            }
            plug_core::types::ServerHealth::AuthRequired => auth_required += 1,
        }
    }

    let mut checks = vec![plug_core::doctor::CheckResult {
        name: "runtime_health".to_string(),
        status: if failed > 0 || degraded > 0 || auth_required > 0 {
            plug_core::doctor::CheckStatus::Warn
        } else {
            plug_core::doctor::CheckStatus::Pass
        },
        message: format!(
            "Daemon running: uptime={}s, daemon_proxy_clients={}, healthy={}, degraded={}, auth_required={}, failed={}",
            uptime_secs, clients, healthy, degraded, auth_required, failed
        ),
        fix_suggestion: None,
    }];

    if !failed_servers.is_empty() {
        checks.push(plug_core::doctor::CheckResult {
            name: "runtime_failures".to_string(),
            status: plug_core::doctor::CheckStatus::Fail,
            message: format!("failing servers: {}", failed_servers.join(", ")),
            fix_suggestion: None,
        });
    }

    if !degraded_servers.is_empty() {
        checks.push(plug_core::doctor::CheckResult {
            name: "runtime_degraded".to_string(),
            status: plug_core::doctor::CheckStatus::Warn,
            message: format!("degraded servers: {}", degraded_servers.join(", ")),
            fix_suggestion: None,
        });
    }

    checks
}

fn synthesize_doctor_interpretation(
    checks: &[plug_core::doctor::CheckResult],
) -> Option<plug_core::doctor::CheckResult> {
    let connectivity = checks
        .iter()
        .find(|check| check.name == "server_connectivity")?;
    let runtime_health = checks.iter().find(|check| check.name == "runtime_health");
    let runtime_failures = checks.iter().find(|check| check.name == "runtime_failures");
    let runtime_auth_attention = checks.iter().any(|check| {
        matches!(check.status, plug_core::doctor::CheckStatus::Warn)
            && (check.name == "runtime_auth"
                || check.name == "runtime_auth_missing"
                || check.name == "runtime_auth_reauth"
                || check.name == "runtime_auth_degraded")
    });

    use plug_core::doctor::CheckStatus::{Fail, Pass, Warn};

    let (status, message, fix_suggestion) = match (
        &connectivity.status,
        runtime_failures.map(|check| &check.status),
        runtime_health.map(|check| &check.status),
        runtime_auth_attention,
    ) {
        (Warn, Some(Fail), _, _) | (Fail, Some(Fail), _, _) => (
            Fail,
            "The daemon is already failing one or more servers, and cold connectivity is also worse than the current live runtime.".to_string(),
            Some(
                "Use `plug status` to identify the failing servers, then fix the reported cold connectivity issue before restarting the daemon.".to_string(),
            ),
        ),
        (Warn, _, Some(Pass), _) | (Warn, _, Some(Warn), _) => (
            Warn,
            "Live daemon state is healthier than cold connectivity. Existing routed sessions are still running, but new connections after a restart may fail.".to_string(),
            Some(
                "Use `plug status` for live runtime truth, then fix the cold connectivity issue before restarting the daemon.".to_string(),
            ),
        ),
        (Pass, Some(Fail), _, _) => (
            Fail,
            "Basic reachability looks fine, but the running daemon is currently failing one or more servers.".to_string(),
            Some(
                "Use `plug status` for the failing servers, then compare with `plug doctor` to separate runtime failures from cold connectivity.".to_string(),
            ),
        ),
        (Pass, _, Some(Warn), true) | (Pass, _, Some(Warn), false) => (
            Warn,
            "Basic connectivity checks pass, but the running daemon still has degraded or auth-required servers.".to_string(),
            Some(
                "Use `plug auth status` and `plug status` to repair the affected runtime state before assuming the system is healthy.".to_string(),
            ),
        ),
        (Fail, _, Some(Pass), _) | (Fail, _, Some(Warn), _) => (
            Fail,
            "Cold connectivity is failing even though the daemon still has some live state. A restart would likely lose currently working routes.".to_string(),
            Some(
                "Fix the reported connectivity failures before restarting the daemon or repairing client/server config.".to_string(),
            ),
        ),
        (Pass, _, Some(Pass), true) => (
            Warn,
            "The runtime is broadly healthy, but some servers still need auth attention or re-authorization.".to_string(),
            Some(
                "Use `plug auth status` to see which servers need credentials or re-auth.".to_string(),
            ),
        ),
        _ => return None,
    };

    Some(plug_core::doctor::CheckResult {
        name: "doctor_interpretation".to_string(),
        status,
        message,
        fix_suggestion,
    })
}

fn runtime_auth_checks(
    servers: &[plug_core::ipc::IpcAuthServerInfo],
) -> Vec<plug_core::doctor::CheckResult> {
    let mut reauth = Vec::new();
    let mut missing = Vec::new();
    let mut degraded = Vec::new();

    for server in servers {
        match (server.authenticated, server.health) {
            (false, plug_core::types::ServerHealth::AuthRequired) => {
                missing.push(server.name.clone())
            }
            (true, plug_core::types::ServerHealth::AuthRequired) => {
                reauth.push(server.name.clone())
            }
            (_, plug_core::types::ServerHealth::Degraded) => degraded.push(server.name.clone()),
            _ => {}
        }
    }

    let mut checks = Vec::new();

    if !missing.is_empty() {
        checks.push(plug_core::doctor::CheckResult {
            name: "runtime_auth_missing".to_string(),
            status: plug_core::doctor::CheckStatus::Warn,
            message: format!("missing credentials: {}", missing.join(", ")),
            fix_suggestion: Some(
                "Run `plug auth login --server <name>` for each server missing credentials."
                    .to_string(),
            ),
        });
    }

    if !reauth.is_empty() {
        checks.push(plug_core::doctor::CheckResult {
            name: "runtime_auth_reauth".to_string(),
            status: plug_core::doctor::CheckStatus::Warn,
            message: format!("re-auth required: {}", reauth.join(", ")),
            fix_suggestion: Some(
                "Stored credentials exist but must be refreshed — run `plug auth login --server <name>`."
                    .to_string(),
            ),
        });
    }

    if !degraded.is_empty() {
        checks.push(plug_core::doctor::CheckResult {
            name: "runtime_auth_degraded".to_string(),
            status: plug_core::doctor::CheckStatus::Warn,
            message: format!("degraded auth/runtime: {}", degraded.join(", ")),
            fix_suggestion: Some(
                "Run `plug auth status` and compare with `plug status` to separate auth drift from broader runtime degradation."
                    .to_string(),
            ),
        });
    }

    checks
}

fn doctor_check_details(check: &plug_core::doctor::CheckResult) -> String {
    match check.fix_suggestion.as_deref() {
        Some(suggestion) if !suggestion.trim().is_empty() => {
            format!("{} Next: {}", check.message, suggestion.trim())
        }
        _ => check.message.clone(),
    }
}

fn doctor_next_steps(checks: &[plug_core::doctor::CheckResult]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut steps = Vec::new();

    for check in checks.iter().filter(|check| {
        matches!(
            check.status,
            plug_core::doctor::CheckStatus::Warn | plug_core::doctor::CheckStatus::Fail
        )
    }) {
        if let Some(suggestion) = check.fix_suggestion.as_deref() {
            let trimmed = suggestion.trim();
            if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
                steps.push(trimmed.to_string());
            }
        }
    }

    steps
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

pub(crate) fn cmd_setup(
    config_path: Option<&std::path::PathBuf>,
    yes: bool,
    transport: Option<plug_core::export::ExportTransport>,
) -> anyhow::Result<()> {
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
    cmd_link(config_path, Vec::new(), false, yes, transport)?;
    Ok(())
}

fn repair_export_endpoint(
    linked_endpoint: Option<&str>,
    config_path: Option<&std::path::PathBuf>,
) -> String {
    linked_endpoint.map(str::to_owned).unwrap_or_else(|| {
        crate::commands::clients::configured_http_export_url(config_path)
            .unwrap_or_else(|| "http://localhost:3282/mcp".to_string())
    })
}

fn repair_targets(requested: Vec<String>, all: bool) -> anyhow::Result<Vec<String>> {
    let known_targets = crate::commands::clients::all_client_targets()
        .iter()
        .map(|(_, target)| (*target).to_string())
        .collect::<std::collections::BTreeSet<_>>();

    if all {
        return Ok(known_targets.into_iter().collect());
    }

    if requested.is_empty() {
        return Ok(known_targets.into_iter().collect());
    }

    let mut selected = Vec::new();
    let mut unknown = Vec::new();
    for target in requested {
        if known_targets.contains(&target) {
            selected.push(target);
        } else {
            unknown.push(target);
        }
    }

    if !unknown.is_empty() {
        anyhow::bail!("unknown client target(s): {}", unknown.join(", "));
    }

    selected.sort_unstable();
    selected.dedup();
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::{
        doctor_check_details, doctor_next_steps, repair_export_endpoint, repair_targets,
        runtime_auth_checks, runtime_health_checks_for_tests, synthesize_doctor_interpretation,
    };
    use plug_core::doctor::{CheckResult, CheckStatus};
    use plug_core::ipc::IpcAuthServerInfo;
    use plug_core::types::{ServerHealth, ServerStatus};

    fn check(name: &str, status: CheckStatus, message: &str) -> CheckResult {
        CheckResult {
            name: name.to_string(),
            status,
            message: message.to_string(),
            fix_suggestion: None,
        }
    }

    #[test]
    fn interpretation_explains_cold_vs_live_difference() {
        let checks = vec![
            check(
                "server_connectivity",
                CheckStatus::Warn,
                "Cold connectivity issues: workspace: TCP connect failed",
            ),
            check(
                "runtime_health",
                CheckStatus::Pass,
                "Daemon running: uptime=10s, daemon_proxy_clients=1, healthy=2, degraded=0, auth_required=0, failed=0",
            ),
        ];
        let interpretation =
            synthesize_doctor_interpretation(&checks).expect("expected interpretation");
        assert_eq!(interpretation.status, CheckStatus::Warn);
        assert!(
            interpretation
                .message
                .contains("Live daemon state is healthier than cold connectivity")
        );
    }

    #[test]
    fn interpretation_explains_runtime_failure_despite_connectivity() {
        let checks = vec![
            check(
                "server_connectivity",
                CheckStatus::Pass,
                "All 3 servers are reachable",
            ),
            check(
                "runtime_health",
                CheckStatus::Warn,
                "Daemon running: uptime=20s, daemon_proxy_clients=2, healthy=1, degraded=0, auth_required=0, failed=2",
            ),
            check(
                "runtime_failures",
                CheckStatus::Fail,
                "failing servers: oura, notion",
            ),
        ];
        let interpretation =
            synthesize_doctor_interpretation(&checks).expect("expected interpretation");
        assert_eq!(interpretation.status, CheckStatus::Fail);
        assert!(
            interpretation
                .message
                .contains("running daemon is currently failing")
        );
    }

    #[test]
    fn interpretation_explains_when_cold_and_live_fail_together() {
        let checks = vec![
            check(
                "server_connectivity",
                CheckStatus::Fail,
                "Cold connectivity issues: oura: TCP connect failed",
            ),
            check(
                "runtime_health",
                CheckStatus::Warn,
                "Daemon running: uptime=20s, daemon_proxy_clients=2, healthy=1, degraded=0, auth_required=0, failed=1",
            ),
            check(
                "runtime_failures",
                CheckStatus::Fail,
                "failing servers: oura",
            ),
        ];
        let interpretation =
            synthesize_doctor_interpretation(&checks).expect("expected interpretation");
        assert_eq!(interpretation.status, CheckStatus::Fail);
        assert!(
            interpretation
                .message
                .contains("cold connectivity is also worse than the current live runtime")
        );
        assert!(
            interpretation
                .fix_suggestion
                .as_deref()
                .unwrap_or_default()
                .contains("fix the reported cold connectivity issue before restarting")
        );
    }

    #[test]
    fn interpretation_explains_auth_attention_when_runtime_is_healthy() {
        let checks = vec![
            check(
                "server_connectivity",
                CheckStatus::Pass,
                "All 2 servers are reachable",
            ),
            check(
                "runtime_health",
                CheckStatus::Pass,
                "Daemon running: uptime=30s, daemon_proxy_clients=3, healthy=2, degraded=0, auth_required=0, failed=0",
            ),
            check(
                "runtime_auth_reauth",
                CheckStatus::Warn,
                "re-auth required: notion",
            ),
        ];
        let interpretation =
            synthesize_doctor_interpretation(&checks).expect("expected interpretation");
        assert_eq!(interpretation.status, CheckStatus::Warn);
        assert!(interpretation.message.contains("need auth attention"));
    }

    #[test]
    fn runtime_checks_split_summary_from_named_failures() {
        let checks = runtime_health_checks_for_tests(
            &[
                ServerStatus {
                    server_id: "healthy".to_string(),
                    health: ServerHealth::Healthy,
                    tool_count: 1,
                    auth_status: "none".to_string(),
                    last_seen: None,
                },
                ServerStatus {
                    server_id: "oura".to_string(),
                    health: ServerHealth::Failed,
                    tool_count: 0,
                    auth_status: "none".to_string(),
                    last_seen: None,
                },
                ServerStatus {
                    server_id: "notion".to_string(),
                    health: ServerHealth::AuthRequired,
                    tool_count: 0,
                    auth_status: "oauth".to_string(),
                    last_seen: None,
                },
            ],
            4,
            120,
        );

        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "runtime_health");
        assert_eq!(checks[0].status, CheckStatus::Warn);
        assert!(checks[0].message.contains("healthy=1"));
        assert!(checks[0].message.contains("daemon_proxy_clients=4"));
        assert!(checks[0].message.contains("auth_required=1"));
        assert!(checks[0].message.contains("failed=1"));

        assert_eq!(checks[1].name, "runtime_failures");
        assert_eq!(checks[1].status, CheckStatus::Fail);
        assert_eq!(checks[1].message, "failing servers: oura");
    }

    #[test]
    fn runtime_checks_include_named_degraded_servers() {
        let checks = runtime_health_checks_for_tests(
            &[
                ServerStatus {
                    server_id: "healthy".to_string(),
                    health: ServerHealth::Healthy,
                    tool_count: 1,
                    auth_status: "none".to_string(),
                    last_seen: None,
                },
                ServerStatus {
                    server_id: "figma".to_string(),
                    health: ServerHealth::Degraded,
                    tool_count: 12,
                    auth_status: "oauth".to_string(),
                    last_seen: None,
                },
            ],
            2,
            45,
        );

        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "runtime_health");
        assert_eq!(checks[0].status, CheckStatus::Warn);
        assert!(checks[0].message.contains("degraded=1"));

        assert_eq!(checks[1].name, "runtime_degraded");
        assert_eq!(checks[1].status, CheckStatus::Warn);
        assert_eq!(checks[1].message, "degraded servers: figma");
    }

    #[test]
    fn runtime_auth_checks_split_missing_reauth_and_degraded_categories() {
        let checks = runtime_auth_checks(&[
            IpcAuthServerInfo {
                name: "notion".to_string(),
                url: Some("https://api.notion.com/mcp".to_string()),
                authenticated: false,
                health: ServerHealth::AuthRequired,
                scopes: None,
                token_expires_in_secs: None,
                warnings: vec![],
            },
            IpcAuthServerInfo {
                name: "supabase".to_string(),
                url: Some("https://mcp.supabase.com/mcp".to_string()),
                authenticated: true,
                health: ServerHealth::AuthRequired,
                scopes: None,
                token_expires_in_secs: Some(120),
                warnings: vec![],
            },
            IpcAuthServerInfo {
                name: "figma".to_string(),
                url: Some("https://api.figma.com/mcp".to_string()),
                authenticated: true,
                health: ServerHealth::Degraded,
                scopes: None,
                token_expires_in_secs: Some(300),
                warnings: vec![],
            },
        ]);

        assert_eq!(checks.len(), 3);
        assert_eq!(checks[0].name, "runtime_auth_missing");
        assert_eq!(checks[0].message, "missing credentials: notion");
        assert!(
            checks[0]
                .fix_suggestion
                .as_deref()
                .unwrap_or_default()
                .contains("plug auth login --server <name>")
        );

        assert_eq!(checks[1].name, "runtime_auth_reauth");
        assert_eq!(checks[1].message, "re-auth required: supabase");

        assert_eq!(checks[2].name, "runtime_auth_degraded");
        assert_eq!(checks[2].message, "degraded auth/runtime: figma");
    }

    #[test]
    fn doctor_check_details_appends_next_step_guidance() {
        let rendered = doctor_check_details(&CheckResult {
            name: "server_connectivity".to_string(),
            status: CheckStatus::Fail,
            message: "Cold connectivity issues: remote: TCP connect failed".to_string(),
            fix_suggestion: Some("Run `plug status` before editing config".to_string()),
        });

        assert!(rendered.contains("Cold connectivity issues"));
        assert!(rendered.contains("Next: Run `plug status` before editing config"));
    }

    #[test]
    fn doctor_check_details_leaves_plain_messages_unchanged() {
        let rendered = doctor_check_details(&CheckResult {
            name: "config_exists".to_string(),
            status: CheckStatus::Pass,
            message: "Config file valid".to_string(),
            fix_suggestion: None,
        });

        assert_eq!(rendered, "Config file valid");
    }

    #[test]
    fn doctor_next_steps_deduplicates_warning_guidance() {
        let steps = doctor_next_steps(&[
            CheckResult {
                name: "runtime_auth_reauth".to_string(),
                status: CheckStatus::Warn,
                message: "re-auth required: notion".to_string(),
                fix_suggestion: Some("Run `plug auth login --server <name>`.".to_string()),
            },
            CheckResult {
                name: "runtime_auth_missing".to_string(),
                status: CheckStatus::Warn,
                message: "missing credentials: supabase".to_string(),
                fix_suggestion: Some("Run `plug auth login --server <name>`.".to_string()),
            },
        ]);

        assert_eq!(
            steps,
            vec!["Run `plug auth login --server <name>`.".to_string()]
        );
    }

    #[test]
    fn repair_export_endpoint_prefers_linked_endpoint_when_present() {
        let endpoint = repair_export_endpoint(Some("https://plug.example.com/mcp"), None);
        assert_eq!(endpoint, "https://plug.example.com/mcp");
    }

    #[test]
    fn repair_targets_accepts_known_requested_targets() {
        let targets = repair_targets(vec!["cursor".to_string(), "codex-cli".to_string()], false)
            .expect("known targets should pass");
        assert_eq!(targets, vec!["codex-cli", "cursor"]);
    }

    #[test]
    fn repair_targets_rejects_unknown_targets() {
        let error = repair_targets(vec!["made-up-client".to_string()], false)
            .expect_err("unknown target should fail");
        assert!(error.to_string().contains("unknown client target"));
    }
}

pub(crate) fn cmd_repair(
    config_path: Option<&std::path::PathBuf>,
    targets: Vec<String>,
    all: bool,
) -> anyhow::Result<()> {
    println!(
        "{} {}",
        style("◆").cyan().bold(),
        style("Repairing AI client configurations...").bold()
    );

    let repair_targets = repair_targets(targets, all)?;

    let mut repaired_count = 0;

    for target in repair_targets {
        if let Some(linked) = crate::commands::clients::linked_client_config(&target, false) {
            print!("  {} Refreshing {}... ", style("›").cyan().bold(), target);
            let export_endpoint = repair_export_endpoint(linked.endpoint.as_deref(), config_path);
            if let Err(e) = execute_export(
                &target,
                matches!(linked.transport, plug_core::export::ExportTransport::Http),
                export_endpoint.as_str(),
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
