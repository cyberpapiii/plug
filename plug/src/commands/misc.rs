use dialoguer::console::style;

use crate::OutputFormat;
use crate::commands::clients::{
    ClientRepairItem, ClientRepairReport, PlugLinkDisposition, cmd_link, repair_client_content,
};
use crate::ui::{
    cli_prompt_theme, print_banner, print_heading, print_info_line, print_next_step,
    print_success_line, print_warning_line,
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

/// Runs diagnostics and returns the computed process exit code
/// (`0` = all pass, `1` = at least one failure, `2` = warnings only).
/// The caller is responsible for exiting the process with this code.
pub(crate) async fn cmd_doctor(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<i32> {
    let resolved = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);
    let config = plug_core::config::load_config(config_path)?;
    let mut report = plug_core::doctor::run_doctor(&config, &resolved).await;
    report.checks.extend(runtime_doctor_checks().await);
    #[cfg(target_os = "macos")]
    report.checks.push(unified_install_check().await);
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
    Ok(report.exit_code)
}

fn unified_install_check_from_snapshot(
    snapshot: &crate::install::UnifiedInstallSnapshot,
) -> plug_core::doctor::CheckResult {
    use plug_core::doctor::{CheckResult, CheckStatus};

    let Some(app) = snapshot.app.as_ref() else {
        return CheckResult {
            name: "unified_install".to_string(),
            status: CheckStatus::Fail,
            message: "No verified Plug.app installation was found.".to_string(),
            fix_suggestion: Some("Open Plug.app and retry reconciliation.".to_string()),
        };
    };

    let mut drift = Vec::new();
    if !snapshot
        .shell_resolution
        .as_ref()
        .is_some_and(|path| install_paths_match(path, &app.executable_path))
    {
        drift.push(format!(
            "shell resolves {} instead of {}",
            snapshot.shell_resolution.as_ref().map_or_else(
                || "no plug command".to_string(),
                |path| path.display().to_string()
            ),
            app.executable_path.display()
        ));
    }
    if snapshot.daemon_version.as_deref() != Some(app.version.as_str()) {
        drift.push(format!(
            "daemon version is {} instead of {}",
            snapshot.daemon_version.as_deref().unwrap_or("unavailable"),
            app.version
        ));
    }
    if !snapshot
        .daemon_executable
        .as_ref()
        .is_some_and(|path| install_paths_match(path, &app.executable_path))
    {
        drift.push(format!(
            "daemon executable is {} instead of {}",
            snapshot.daemon_executable.as_ref().map_or_else(
                || "unavailable".to_string(),
                |path| path.display().to_string()
            ),
            app.executable_path.display()
        ));
    }
    if snapshot.ownership != plug_core::ipc::DaemonOwnershipMode::AppManaged {
        drift.push(format!("daemon ownership is {:?}", snapshot.ownership));
    }
    let stale_clients = snapshot
        .linked_clients
        .iter()
        .filter(|item| {
            !matches!(
                item.disposition,
                PlugLinkDisposition::Canonical
                    | PlugLinkDisposition::Http
                    | PlugLinkDisposition::Missing
            )
        })
        .map(|item| item.target.as_str())
        .collect::<Vec<_>>();
    if !stale_clients.is_empty() {
        drift.push(format!(
            "client links need repair: {}",
            stale_clients.join(", ")
        ));
    }
    let incompatible_adapters = snapshot
        .adapters
        .iter()
        .filter(|state| matches!(state, crate::install::AdapterVersionState::Incompatible))
        .count();
    if incompatible_adapters > 0 {
        drift.push(format!(
            "{incompatible_adapters} live adapter(s) are incompatible"
        ));
    }
    if !snapshot.shadows.is_empty() {
        let paths = snapshot
            .shadows
            .iter()
            .map(|shadow| {
                if shadow.verified_plug_owned {
                    shadow.path.display().to_string()
                } else {
                    format!("{} (unknown file, left untouched)", shadow.path.display())
                }
            })
            .collect::<Vec<_>>();
        drift.push(format!("shadow installations: {}", paths.join(", ")));
    }
    let unexpected_jobs = snapshot
        .launchd_jobs
        .iter()
        .filter(|job| {
            !install_paths_match(&job.program, &app.executable_path)
                || job.label != "com.plug.daemon"
        })
        .map(|job| format!("{} ({})", job.label, job.program.display()))
        .collect::<Vec<_>>();
    if !unexpected_jobs.is_empty() {
        drift.push(format!(
            "unknown or legacy launchd jobs left untouched: {}",
            unexpected_jobs.join(", ")
        ));
    }

    if drift.is_empty() {
        CheckResult {
            name: "unified_install".to_string(),
            status: CheckStatus::Pass,
            message: "Plug.app owns the app, command line, daemon, and client links.".to_string(),
            fix_suggestion: None,
        }
    } else {
        CheckResult {
            name: "unified_install".to_string(),
            status: CheckStatus::Warn,
            message: drift.join("; "),
            fix_suggestion: Some("Open Plug.app and retry reconciliation.".to_string()),
        }
    }
}

fn install_paths_match(left: &std::path::Path, right: &std::path::Path) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(target_os = "macos")]
async fn unified_install_check() -> plug_core::doctor::CheckResult {
    use plug_core::ipc::{
        DaemonOwnershipMode, IpcRequest, IpcResponse, OPERATOR_IPC_MAX, OPERATOR_IPC_MIN,
    };

    let app = crate::install::resolve_verified_app().ok().flatten();
    let shell_resolution = crate::install::resolve_login_shell_command().ok().flatten();
    let handshake = crate::daemon::ipc_request(&IpcRequest::OperatorHandshake {
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        ipc_min: OPERATOR_IPC_MIN,
        ipc_max: OPERATOR_IPC_MAX,
    })
    .await
    .ok();
    let (daemon_version, daemon_executable, ownership) = match handshake {
        Some(IpcResponse::OperatorHandshake { handshake }) => (
            Some(handshake.daemon_version),
            handshake.daemon_executable,
            handshake.ownership,
        ),
        _ => (None, None, DaemonOwnershipMode::Unmanaged),
    };
    let live_sessions = match crate::daemon::ipc_request(&IpcRequest::ListLiveSessions).await {
        Ok(IpcResponse::LiveSessions { sessions, .. }) => sessions,
        _ => Vec::new(),
    };
    let current_version = app
        .as_ref()
        .map_or(env!("CARGO_PKG_VERSION"), |app| app.version.as_str());
    let adapters = live_sessions
        .iter()
        .map(|session| {
            crate::install::classify_adapter_version(
                session.adapter_version.as_deref(),
                current_version,
            )
        })
        .collect();
    let linked_clients = inspect_linked_clients(app.as_ref());
    let launchd_jobs = crate::service::discover_launchd_jobs()
        .unwrap_or_default()
        .into_iter()
        .filter(crate::install::is_plug_launchd_candidate)
        .collect();
    let shadows = crate::install::discover_shadow_installations(app.as_ref());
    unified_install_check_from_snapshot(&crate::install::UnifiedInstallSnapshot {
        app,
        shell_resolution,
        daemon_version,
        daemon_executable,
        ownership,
        linked_clients,
        adapters,
        shadows,
        launchd_jobs,
    })
}

#[cfg(target_os = "macos")]
fn inspect_linked_clients(
    app: Option<&crate::install::VerifiedAppInstallation>,
) -> Vec<ClientRepairItem> {
    let Some(app) = app else {
        return Vec::new();
    };
    let http_url = crate::commands::clients::configured_http_export_url(None)
        .unwrap_or_else(|| "http://localhost:3282/mcp".to_string());
    crate::commands::clients::all_client_targets()
        .iter()
        .filter_map(|(_, target)| {
            let target_enum: plug_core::export::ExportTarget = target.parse().ok()?;
            let path = plug_core::export::default_config_path(target_enum, false)?;
            if !path.exists() {
                return None;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(error) => {
                    return Some(ClientRepairItem {
                        target: (*target).to_string(),
                        path,
                        disposition: PlugLinkDisposition::UnknownCommand,
                        changed: false,
                        message: format!("Could not read client config; left untouched: {error}"),
                    });
                }
            };
            match repair_client_content(
                target_enum,
                &path,
                &content,
                &app.executable_path,
                &http_url,
            ) {
                Ok(repair) => Some(ClientRepairItem {
                    target: (*target).to_string(),
                    path,
                    disposition: repair.disposition,
                    changed: false,
                    message: repair.message,
                }),
                Err(error) => Some(ClientRepairItem {
                    target: (*target).to_string(),
                    path,
                    disposition: PlugLinkDisposition::UnknownCommand,
                    changed: false,
                    message: format!("Could not inspect client config; left untouched: {error}"),
                }),
            }
        })
        .collect()
}

async fn runtime_doctor_checks() -> Vec<plug_core::doctor::CheckResult> {
    let mut checks = Vec::new();
    let daemon_reachable = crate::runtime::daemon_running().await;
    let status_response = crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::Status).await;
    let auth_response = crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::AuthStatus).await;

    if daemon_reachable
        && !matches!(
            status_response,
            Ok(plug_core::ipc::IpcResponse::Status { .. })
        )
    {
        checks.push(plug_core::doctor::CheckResult {
            name: "runtime_availability".to_string(),
            status: plug_core::doctor::CheckStatus::Warn,
            message: "Daemon socket is reachable, but runtime status inspection failed".to_string(),
            fix_suggestion: Some(
                "Run `plug status` and inspect daemon logs to diagnose IPC/runtime availability problems.".to_string(),
            ),
        });
    }
    if daemon_reachable
        && !matches!(
            auth_response,
            Ok(plug_core::ipc::IpcResponse::AuthStatus { .. })
        )
    {
        checks.push(plug_core::doctor::CheckResult {
            name: "auth_availability".to_string(),
            status: plug_core::doctor::CheckStatus::Warn,
            message: "Daemon socket is reachable, but auth status inspection failed".to_string(),
            fix_suggestion: Some(
                "Run `plug auth status` and inspect daemon logs to diagnose auth/runtime availability problems.".to_string(),
            ),
        });
    }

    if let Ok(plug_core::ipc::IpcResponse::Status {
        servers,
        clients,
        uptime_secs,
        ..
    }) = status_response
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

    if let Ok(plug_core::ipc::IpcResponse::AuthStatus { servers }) = auth_response {
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
        (Pass, _, Some(Warn), _) => (
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

pub(crate) async fn cmd_reload(output: &OutputFormat) -> anyhow::Result<()> {
    let auth = crate::daemon::read_auth_token()?;
    let req = plug_core::ipc::IpcRequest::Reload { auth_token: auth };
    match crate::daemon::ipc_request(&req).await? {
        plug_core::ipc::IpcResponse::Reloaded { report } => {
            if matches!(output, OutputFormat::Json) {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else if report.restart_required.is_empty() {
                print_success_line("Config reloaded.");
            } else {
                print_success_line("Config reloaded with restart-required changes.");
                for warning in report.restart_required {
                    print_info_line(format!("Restart required: {warning}"));
                }
                print_info_line(
                    "Run `plug stop` then `plug start` before relying on those changes in live sessions.",
                );
            }
        }
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

#[cfg(test)]
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

fn repair_attention_messages(report: &ClientRepairReport) -> Vec<String> {
    report
        .items
        .iter()
        .filter(|item| {
            !item.changed
                && !matches!(
                    item.disposition,
                    PlugLinkDisposition::Canonical
                        | PlugLinkDisposition::Http
                        | PlugLinkDisposition::Missing
                )
        })
        .map(|item| format!("{}: {}", item.target, item.message))
        .collect()
}

fn repair_text_summary(report: &ClientRepairReport, repaired_count: usize) -> String {
    let needing_attention = repair_attention_messages(report);
    if !needing_attention.is_empty() {
        format!(
            "Repair finished with {} client configuration(s) needing attention.",
            needing_attention.len()
        )
    } else if repaired_count > 0 {
        format!("Successfully repaired {repaired_count} client configuration(s).")
    } else if report
        .items
        .iter()
        .any(|item| !matches!(item.disposition, PlugLinkDisposition::Missing))
    {
        "Linked client configurations already use the canonical command.".to_string()
    } else {
        "No linked clients found to repair.".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClientRepairItem, ClientRepairReport, PlugLinkDisposition, doctor_check_details,
        doctor_next_steps, repair_attention_messages, repair_client_content,
        repair_export_endpoint, repair_targets, repair_text_summary, runtime_auth_checks,
        runtime_health_checks_for_tests, synthesize_doctor_interpretation,
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

    fn unified_snapshot() -> crate::install::UnifiedInstallSnapshot {
        let executable = std::path::PathBuf::from("/Applications/Plug.app/Contents/Resources/plug");
        crate::install::UnifiedInstallSnapshot {
            app: Some(crate::install::VerifiedAppInstallation {
                bundle_path: std::path::PathBuf::from("/Applications/Plug.app"),
                executable_path: executable.clone(),
                version: "0.6.4".to_string(),
            }),
            shell_resolution: Some(executable.clone()),
            daemon_version: Some("0.6.4".to_string()),
            daemon_executable: Some(executable),
            ownership: plug_core::ipc::DaemonOwnershipMode::AppManaged,
            linked_clients: vec![ClientRepairItem {
                target: "cursor".to_string(),
                path: std::path::PathBuf::from("/tmp/cursor.json"),
                disposition: PlugLinkDisposition::Canonical,
                changed: false,
                message: "Already canonical.".to_string(),
            }],
            adapters: vec![
                crate::install::AdapterVersionState::Current,
                crate::install::AdapterVersionState::CompatibleOlder,
                crate::install::AdapterVersionState::Missing,
            ],
            shadows: Vec::new(),
            launchd_jobs: vec![crate::service::LaunchdJobRecord {
                label: "com.plug.daemon".to_string(),
                program: std::path::PathBuf::from("/Applications/Plug.app/Contents/Resources/plug"),
            }],
        }
    }

    #[test]
    fn unified_install_healthy_matrix_has_one_canonical_message() {
        let snapshot = unified_snapshot();
        let result = super::unified_install_check_from_snapshot(&snapshot);
        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(
            result.message,
            "Plug.app owns the app, command line, daemon, and client links."
        );
        assert_eq!(result.fix_suggestion, None);
        let json = serde_json::to_value(snapshot).unwrap();
        for key in [
            "app",
            "shell_resolution",
            "daemon_version",
            "daemon_executable",
            "ownership",
            "linked_clients",
            "adapters",
            "shadows",
            "launchd_jobs",
        ] {
            assert!(json.get(key).is_some(), "missing stable JSON field {key}");
        }
    }

    #[test]
    fn unified_install_matrix_reports_every_repairable_drift_with_one_action() {
        let cases = [
            {
                let mut snapshot = unified_snapshot();
                snapshot.shell_resolution =
                    Some(std::path::PathBuf::from("/opt/homebrew/bin/plug"));
                snapshot
            },
            {
                let mut snapshot = unified_snapshot();
                snapshot.daemon_version = Some("0.6.3".to_string());
                snapshot
            },
            {
                let mut snapshot = unified_snapshot();
                snapshot.daemon_executable =
                    Some(std::path::PathBuf::from("/Users/me/.cargo/bin/plug"));
                snapshot
            },
            {
                let mut snapshot = unified_snapshot();
                snapshot.ownership = plug_core::ipc::DaemonOwnershipMode::CliManaged;
                snapshot
            },
            {
                let mut snapshot = unified_snapshot();
                snapshot.linked_clients[0].disposition = PlugLinkDisposition::RecognizedLegacy;
                snapshot
            },
            {
                let mut snapshot = unified_snapshot();
                snapshot.shadows.push(crate::install::ShadowInstallation {
                    kind: "homebrew_formula".to_string(),
                    path: std::path::PathBuf::from("/opt/homebrew/Cellar/plug/0.6.3/bin/plug"),
                    verified_plug_owned: true,
                });
                snapshot
            },
            {
                let mut snapshot = unified_snapshot();
                snapshot
                    .adapters
                    .push(crate::install::AdapterVersionState::Incompatible);
                snapshot
            },
            {
                let mut snapshot = unified_snapshot();
                snapshot
                    .launchd_jobs
                    .push(crate::service::LaunchdJobRecord {
                        label: "local.plug.lookalike".to_string(),
                        program: std::path::PathBuf::from("/Applications/Other.app/other"),
                    });
                snapshot
            },
        ];

        for snapshot in cases {
            let result = super::unified_install_check_from_snapshot(&snapshot);
            assert_eq!(result.status, CheckStatus::Warn, "{}", result.message);
            assert_eq!(
                result.fix_suggestion.as_deref(),
                Some("Open Plug.app and retry reconciliation.")
            );
        }
    }

    #[test]
    fn unified_install_absent_app_is_a_failure() {
        let mut snapshot = unified_snapshot();
        snapshot.app = None;
        let result = super::unified_install_check_from_snapshot(&snapshot);
        assert_eq!(result.status, CheckStatus::Fail);
        assert_eq!(
            result.fix_suggestion.as_deref(),
            Some("Open Plug.app and retry reconciliation.")
        );
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
                    upstream: None,
                    metrics: None,
                    availability: Default::default(),
                    selected_protocol_era: None,
                    selected_protocol_version: None,
                    last_seen: None,
                },
                ServerStatus {
                    server_id: "oura".to_string(),
                    health: ServerHealth::Failed,
                    tool_count: 0,
                    auth_status: "none".to_string(),
                    upstream: None,
                    metrics: None,
                    availability: Default::default(),
                    selected_protocol_era: None,
                    selected_protocol_version: None,
                    last_seen: None,
                },
                ServerStatus {
                    server_id: "notion".to_string(),
                    health: ServerHealth::AuthRequired,
                    tool_count: 0,
                    auth_status: "oauth".to_string(),
                    upstream: None,
                    metrics: None,
                    availability: Default::default(),
                    selected_protocol_era: None,
                    selected_protocol_version: None,
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
                    upstream: None,
                    metrics: None,
                    availability: Default::default(),
                    selected_protocol_era: None,
                    selected_protocol_version: None,
                    last_seen: None,
                },
                ServerStatus {
                    server_id: "figma".to_string(),
                    health: ServerHealth::Degraded,
                    tool_count: 12,
                    auth_status: "oauth".to_string(),
                    upstream: None,
                    metrics: None,
                    availability: Default::default(),
                    selected_protocol_era: None,
                    selected_protocol_version: None,
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

    #[test]
    fn repair_content_preserves_canonical_and_unknown_json_entries() {
        let canonical = std::path::Path::new("/Applications/Plug.app/Contents/Resources/plug");
        let canonical_content = format!(
            r#"{{"mcpServers":{{"plug":{{"command":"{}","args":["connect"],"custom":true}},"other":{{"command":"other"}}}},"unknown":{{"preserved":true}}}}"#,
            canonical.display()
        );
        let canonical_repair = repair_client_content(
            plug_core::export::ExportTarget::Cursor,
            std::path::Path::new("config.json"),
            &canonical_content,
            canonical,
            "http://localhost:3282/mcp",
        )
        .expect("canonical config should parse");
        assert_eq!(canonical_repair.disposition, PlugLinkDisposition::Canonical);
        assert_eq!(canonical_repair.updated, None);

        let unknown_content = r#"{"mcpServers":{"plug":{"command":"/tmp/other","args":["connect"]},"other":{"command":"other"}},"unknown":{"preserved":true}}"#;
        let unknown_repair = repair_client_content(
            plug_core::export::ExportTarget::Cursor,
            std::path::Path::new("config.json"),
            unknown_content,
            canonical,
            "http://localhost:3282/mcp",
        )
        .expect("unknown config should parse");
        assert_eq!(
            unknown_repair.disposition,
            PlugLinkDisposition::UnknownCommand
        );
        assert_eq!(unknown_repair.updated, None);
    }

    #[test]
    fn repair_content_updates_legacy_json_toml_and_yaml_without_losing_neighbors() {
        let canonical = std::path::Path::new("/Applications/Plug.app/Contents/Resources/plug");
        let cargo_command = dirs::home_dir()
            .expect("test home directory")
            .join(".cargo/bin/plug")
            .to_string_lossy()
            .into_owned();
        let fixtures = vec![
            (
                plug_core::export::ExportTarget::Cursor,
                std::path::Path::new("config.json"),
                format!(r#"{{"mcpServers":{{"plug":{{"command":"{cargo_command}","args":["connect"]}},"other":{{"command":"other"}}}},"unknown":{{"preserved":true}}}}"#),
            ),
            (
                plug_core::export::ExportTarget::CodexCli,
                std::path::Path::new("config.toml"),
                "[mcp_servers.plug]\ncommand = \"/opt/homebrew/bin/plug\"\nargs = [\"connect\"]\nunknown = \"keep\"\n\n[mcp_servers.other]\ncommand = \"other\"\n".to_string(),
            ),
            (
                plug_core::export::ExportTarget::Goose,
                std::path::Path::new("config.yaml"),
                "extensions:\n  plug:\n    type: stdio\n    command: /Users/rob/src/plug/target/release/plug\n    args: [connect]\n    unknown: keep\n  other:\n    type: stdio\n    command: other\nunknown: preserved\n".to_string(),
            ),
        ];

        for (target, path, content) in &fixtures {
            let repair = repair_client_content(
                *target,
                path,
                content,
                canonical,
                "http://localhost:3282/mcp",
            )
            .expect("legacy config should parse");
            assert_eq!(repair.disposition, PlugLinkDisposition::RecognizedLegacy);
            let updated = repair.updated.expect("legacy entry should update");
            assert!(updated.contains(canonical.to_str().unwrap()));
            assert!(updated.contains("other"));
            assert!(updated.contains("unknown"));
        }
    }

    #[test]
    fn repair_rejects_invalid_args_and_updates_the_same_json_container_it_parsed() {
        let canonical = std::path::Path::new("/Applications/Plug.app/Contents/Resources/plug");
        let cargo_command = dirs::home_dir()
            .expect("test home directory")
            .join(".cargo/bin/plug")
            .to_string_lossy()
            .into_owned();
        let invalid_args = format!(
            r#"{{"mcpServers":{{"plug":{{"command":"{cargo_command}","args":["connect",7]}}}}}}"#
        );
        let valid_args = format!(
            r#"{{"mcpServers":{{"plug":{{"command":"{cargo_command}","args":["connect"]}}}}}}"#
        );
        let recognized = repair_client_content(
            plug_core::export::ExportTarget::Cursor,
            std::path::Path::new("config.json"),
            &valid_args,
            canonical,
            "http://localhost:3282/mcp",
        )
        .expect("recognized Cargo path should be repairable with valid args");
        assert_eq!(
            recognized.disposition,
            PlugLinkDisposition::RecognizedLegacy
        );
        let invalid = repair_client_content(
            plug_core::export::ExportTarget::Cursor,
            std::path::Path::new("config.json"),
            &invalid_args,
            canonical,
            "http://localhost:3282/mcp",
        )
        .expect("invalid args are reported, not repaired");
        assert_eq!(invalid.disposition, PlugLinkDisposition::UnknownCommand);
        assert_eq!(invalid.updated, None);

        let dual_container = format!(
            r#"{{"mcpServers":{{"other":{{"command":"other"}}}},"context_servers":{{"plug":{{"command":"{cargo_command}","args":["connect"]}}}}}}"#
        );
        let repaired = repair_client_content(
            plug_core::export::ExportTarget::Zed,
            std::path::Path::new("config.json"),
            &dual_container,
            canonical,
            "http://localhost:3282/mcp",
        )
        .expect("dual container config should repair");
        let updated = repaired
            .updated
            .expect("context_servers Plug entry should update");
        let json: serde_json::Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(
            json["context_servers"]["plug"]["command"].as_str(),
            canonical.to_str()
        );
        assert_eq!(
            json["mcpServers"]["other"]["command"].as_str(),
            Some("other")
        );
    }

    #[test]
    fn repair_preserves_toml_and_yaml_comments_outside_repaired_fields() {
        let canonical = std::path::Path::new("/Applications/Plug.app/Contents/Resources/plug");
        let cargo_command = dirs::home_dir()
            .expect("test home directory")
            .join(".cargo/bin/plug")
            .to_string_lossy()
            .into_owned();
        let fixtures = vec![
            (
                plug_core::export::ExportTarget::CodexCli,
                std::path::Path::new("config.toml"),
                "# keep this document comment\n[mcp_servers.plug]\n# keep this entry comment\ncommand = \"/opt/homebrew/bin/plug\" # command note\nargs = [\"connect\"] # args note\nunknown = \"keep\"\n\n# keep neighboring comment\n[mcp_servers.other]\ncommand = \"other\"\n".to_string(),
                "# keep neighboring comment",
            ),
            (
                plug_core::export::ExportTarget::Goose,
                std::path::Path::new("config.yaml"),
                format!("# keep this document comment\nextensions:\n  plug:\n    # keep this entry comment\n    command: {cargo_command} # command note\n    args: [connect] # args note\n    unknown: keep\n  # keep neighboring comment\n  other:\n    command: other\n"),
                "# keep neighboring comment",
            ),
        ];
        for (target, path, content, retained_comment) in fixtures {
            let repair =
                repair_client_content(target, path, &content, canonical, "unused").unwrap();
            let updated = repair.updated.expect("legacy entry should update");
            assert!(updated.contains("# keep this document comment"));
            assert!(updated.contains(retained_comment));
            assert!(updated.contains("# command note"));
            assert!(updated.contains("# args note"));
            assert!(updated.contains("unknown = \"keep\"") || updated.contains("unknown: keep"));
        }
    }

    #[test]
    fn repair_replaces_complete_multiline_toml_and_yaml_args_values() {
        let canonical = std::path::Path::new("/Applications/Plug.app/Contents/Resources/plug");
        let cargo_command = dirs::home_dir()
            .expect("test home directory")
            .join(".cargo/bin/plug")
            .to_string_lossy()
            .into_owned();
        let fixtures = vec![
            (
                plug_core::export::ExportTarget::CodexCli,
                std::path::Path::new("config.toml"),
                "[mcp_servers.plug]\ncommand = \"/opt/homebrew/bin/plug\"\nargs = [\n  \"connect\",\n]\n\n# retain this unrelated comment\n[mcp_servers.other]\ncommand = \"other\"\n".to_string(),
            ),
            (
                plug_core::export::ExportTarget::Goose,
                std::path::Path::new("config.yaml"),
                format!("extensions:\n  plug:\n    command: {cargo_command}\n    args:\n\n      - connect\n\n  # retain this unrelated comment\n  other:\n    command: other\n"),
            ),
        ];
        for (target, path, content) in fixtures {
            let repair =
                repair_client_content(target, path, &content, canonical, "unused").unwrap();
            let updated = repair.updated.expect("legacy entry should update");
            assert!(updated.contains("# retain this unrelated comment"));
            assert!(
                updated.contains("[mcp_servers.other]\ncommand = \"other\"")
                    || updated.contains("other:\n    command: other")
            );
            match target {
                plug_core::export::ExportTarget::CodexCli => {
                    assert!(toml::from_str::<toml::Value>(&updated).is_ok());
                }
                plug_core::export::ExportTarget::Goose => {
                    assert!(serde_norway::from_str::<serde_norway::Value>(&updated).is_ok());
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn repair_text_reports_attention_without_a_success_summary() {
        let report = ClientRepairReport {
            canonical_command: std::path::PathBuf::from(
                "/Applications/Plug.app/Contents/Resources/plug",
            ),
            items: vec![ClientRepairItem {
                target: "cursor".to_string(),
                path: std::path::PathBuf::from("/tmp/cursor.json"),
                disposition: PlugLinkDisposition::UnknownCommand,
                changed: false,
                message: "Plug command is not recognized; left unchanged.".to_string(),
            }],
        };
        assert_eq!(
            repair_attention_messages(&report),
            vec!["cursor: Plug command is not recognized; left unchanged.".to_string()]
        );
        assert!(repair_text_summary(&report, 0).contains("needing attention"));
        assert!(!repair_text_summary(&report, 0).contains("Successfully"));
    }
}

pub(crate) fn cmd_repair(
    config_path: Option<&std::path::PathBuf>,
    targets: Vec<String>,
    all: bool,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let repair_targets = repair_targets(targets, all)?;
    let canonical_command = crate::install::canonical_client_command()?;
    let mut report = ClientRepairReport {
        canonical_command: canonical_command.clone(),
        items: Vec::new(),
    };
    let http_url = crate::commands::clients::configured_http_export_url(config_path)
        .unwrap_or_else(|| "http://localhost:3282/mcp".to_string());

    if matches!(output, OutputFormat::Text) {
        println!(
            "{} {}",
            style("◆").cyan().bold(),
            style("Repairing AI client configurations...").bold()
        );
    }

    let mut repaired_count = 0;

    for target in repair_targets {
        let target_enum: plug_core::export::ExportTarget = target
            .parse()
            .map_err(|error: String| anyhow::anyhow!(error))?;
        let Some(path) = plug_core::export::default_config_path(target_enum, false) else {
            report.items.push(ClientRepairItem {
                target,
                path: std::path::PathBuf::new(),
                disposition: PlugLinkDisposition::Missing,
                changed: false,
                message: "This client has no supported config path on this platform.".to_string(),
            });
            continue;
        };
        if !path.exists() {
            report.items.push(ClientRepairItem {
                target,
                path,
                disposition: PlugLinkDisposition::Missing,
                changed: false,
                message: "Client config file does not exist.".to_string(),
            });
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                report.items.push(ClientRepairItem {
                    target,
                    path,
                    disposition: PlugLinkDisposition::UnknownCommand,
                    changed: false,
                    message: format!("Could not read client config; left unchanged: {error}"),
                });
                continue;
            }
        };
        let repair = match repair_client_content(
            target_enum,
            &path,
            &content,
            &canonical_command,
            &http_url,
        ) {
            Ok(repair) => repair,
            Err(error) => {
                report.items.push(ClientRepairItem {
                    target,
                    path,
                    disposition: PlugLinkDisposition::UnknownCommand,
                    changed: false,
                    message: format!("Could not inspect client config; left unchanged: {error}"),
                });
                continue;
            }
        };

        let mut item = ClientRepairItem {
            target: target.clone(),
            path: path.clone(),
            disposition: repair.disposition,
            changed: false,
            message: repair.message,
        };
        if let Some(updated) = repair.updated {
            if let Err(error) = std::fs::write(&path, updated) {
                item.message = format!("Could not repair recognized Plug command: {error}");
            } else {
                item.changed = true;
                repaired_count += 1;
                if matches!(output, OutputFormat::Text) {
                    println!(
                        "  {} Refreshing {}... {}",
                        style("›").cyan().bold(),
                        target,
                        style("done").green()
                    );
                }
            }
        }
        report.items.push(item);
    }

    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            let needing_attention = repair_attention_messages(&report);
            for message in &needing_attention {
                print_warning_line(message);
            }
            println!("\n{}", repair_text_summary(&report, repaired_count));
        }
    }

    Ok(())
}

pub(crate) fn cmd_uninstall_cleanup(output: &OutputFormat) -> anyhow::Result<()> {
    let report = crate::install::uninstall_cleanup()?;
    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            for item in report.items {
                if item.changed {
                    print_success_line(item.message);
                } else {
                    print_info_line(item.message);
                }
            }
        }
    }
    Ok(())
}
