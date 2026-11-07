//! `plug doctor`: the cold checks from plug-core, what the running daemon
//! says, and whether Plug.app owns the install and every client link.

use std::path::{Path, PathBuf};

use dialoguer::console::style;
use plug_core::clients::{ClientTarget, Scope};
use plug_core::doctor::{CheckResult, CheckStatus, check};

use crate::OutputFormat;
use crate::ui::{print_banner, print_heading, print_next_step};

/// What a client's Plug entry runs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlugLinkDisposition {
    Canonical,
    RecognizedLegacy,
    UnknownCommand,
    Http,
    Missing,
}

impl PlugLinkDisposition {
    /// A stdio link that does not run the canonical command.
    pub fn needs_repair(&self) -> bool {
        matches!(self, Self::RecognizedLegacy | Self::UnknownCommand)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientRepairItem {
    pub target: String,
    pub path: PathBuf,
    pub disposition: PlugLinkDisposition,
    pub changed: bool,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientRepairReport {
    pub canonical_command: PathBuf,
    pub items: Vec<ClientRepairItem>,
    /// A link still needs repair: one that does not run the canonical
    /// command and was not changed. The app reads this rather than judging
    /// the dispositions itself.
    pub repair_needed: bool,
}

impl ClientRepairReport {
    pub fn new(canonical_command: PathBuf, items: Vec<ClientRepairItem>) -> Self {
        let repair_needed = items
            .iter()
            .any(|item| !item.changed && item.disposition.needs_repair());
        Self {
            canonical_command,
            items,
            repair_needed,
        }
    }
}

/// Everything doctor learned about who owns the install. The app reads it
/// from `plug doctor --output json` as `unified_install`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct UnifiedInstallSnapshot {
    pub app: Option<crate::install::VerifiedAppInstallation>,
    pub shell_resolution: Option<PathBuf>,
    pub daemon_version: Option<String>,
    pub daemon_executable: Option<PathBuf>,
    pub ownership: plug_core::ipc::DaemonOwnershipMode,
    pub linked_clients: Vec<ClientRepairItem>,
    pub adapters: Vec<crate::install::AdapterVersionState>,
    pub shadows: Vec<crate::install::ShadowInstallation>,
    pub launchd_jobs: Vec<crate::service::LaunchdJobRecord>,
    pub daemon_inspection_complete: bool,
    pub adapter_inspection_complete: bool,
    pub launchd_inspection_complete: bool,
    pub app_service_daemon_invocation_verified: bool,
    pub inspection_errors: Vec<String>,
    pub client_repair_needed: bool,
}

/// Classify a stdio `plug` entry without executing its configured command.
///
/// Repair is intentionally limited to paths that can only reasonably be a
/// Plug installation. A bare `plug` command or an arbitrary executable might
/// be another program, so it remains untouched.
pub fn classify_plug_client_command(
    command: &str,
    args: &[String],
    canonical: &Path,
) -> PlugLinkDisposition {
    if !plug_core::clients::is_connect_args(args) {
        return PlugLinkDisposition::UnknownCommand;
    }

    let command_path = Path::new(command);
    if crate::install::paths_match(command_path, canonical) {
        return PlugLinkDisposition::Canonical;
    }

    let identified_path = command_path
        .canonicalize()
        .unwrap_or_else(|_| command_path.into());
    if is_recognized_legacy_plug_path(&identified_path) {
        PlugLinkDisposition::RecognizedLegacy
    } else {
        PlugLinkDisposition::UnknownCommand
    }
}

fn is_recognized_legacy_plug_path(path: &Path) -> bool {
    if crate::service::is_recognized_legacy_program(path) {
        return true;
    }

    let path_components = path.components().collect::<Vec<_>>();
    let has_suffix = |suffix: &[&str]| {
        path_components.len() >= suffix.len()
            && path_components[path_components.len() - suffix.len()..]
                .iter()
                .map(|component| component.as_os_str())
                .eq(suffix.iter().map(std::ffi::OsStr::new))
    };

    // A client may also run the app's binary or a development build directly.
    crate::install::is_app_executable(path)
        || has_suffix(&["target", "debug", "plug"])
        || has_suffix(&["target", "release", "plug"])
}

/// What Plug's entry in `content` runs, and `content` pointed at `canonical`
/// when the entry is a recognized older Plug.
fn repair_content(
    target: ClientTarget,
    content: &str,
    canonical: &Path,
) -> anyhow::Result<(PlugLinkDisposition, Option<String>, &'static str)> {
    let layout = target.layout();
    let Some(plug) = layout.read_plug(content) else {
        return Ok((
            PlugLinkDisposition::Missing,
            None,
            "No Plug entry found; left unchanged.",
        ));
    };
    if plug.url.is_some() {
        return Ok((
            PlugLinkDisposition::Http,
            None,
            "HTTP Plug entry does not need a command repair.",
        ));
    }
    let disposition = match (plug.command.as_deref(), plug.args.as_deref()) {
        (Some(command), Some(args)) => classify_plug_client_command(command, args, canonical),
        _ => PlugLinkDisposition::UnknownCommand,
    };
    Ok(match disposition {
        PlugLinkDisposition::RecognizedLegacy => (
            disposition,
            Some(layout.set_plug_command(
                content,
                &canonical.to_string_lossy(),
                &target.connect_args(),
            )?),
            "Repaired a recognized legacy Plug command.",
        ),
        PlugLinkDisposition::Canonical => (
            disposition,
            None,
            "Already uses the canonical Plug command.",
        ),
        _ => (
            disposition,
            None,
            "Plug command is not recognized; left unchanged.",
        ),
    })
}

/// Inspect `target`'s link against `canonical`, and repair it when `write`.
/// `None` when the client has no settings file.
pub(crate) fn inspect_link(
    target: ClientTarget,
    canonical: &Path,
    write: bool,
) -> Option<ClientRepairItem> {
    let path = target.config_path(Scope::User)?;
    let item = |disposition, message: String| ClientRepairItem {
        target: target.slug().to_string(),
        path: path.clone(),
        disposition,
        changed: false,
        message,
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            return Some(item(
                PlugLinkDisposition::UnknownCommand,
                format!("Could not read client config; left unchanged: {error}"),
            ));
        }
    };
    let (disposition, updated, message) = match repair_content(target, &content, canonical) {
        Ok(repair) => repair,
        Err(error) => {
            return Some(item(
                PlugLinkDisposition::UnknownCommand,
                format!("Could not inspect client config; left unchanged: {error}"),
            ));
        }
    };
    let mut result = item(disposition, message.to_string());
    match updated {
        Some(_) if !write => {
            result.message = "Recognized legacy Plug command needs repair.".to_string();
        }
        Some(updated) => {
            match plug_core::fs_perm::write_atomic_keeping_mode(&path, updated.as_bytes()) {
                Ok(()) => result.changed = true,
                Err(error) => {
                    result.message = format!("Could not repair recognized Plug command: {error}");
                }
            }
        }
        None => {}
    }
    Some(result)
}

/// Runs diagnostics and returns the process exit code: `0` all pass, `1` at
/// least one failure, `2` warnings only.
pub(crate) async fn cmd_doctor(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<i32> {
    let resolved = config_path
        .cloned()
        .unwrap_or_else(plug_core::paths::default_config_path);
    let config = plug_core::config::load_config(config_path)?;
    let mut report = plug_core::doctor::run_doctor(&config, &resolved).await;
    report.checks.extend(runtime_doctor_checks(&config).await);
    fold_daemon_liveness_checks(&mut report.checks);
    let unified_snapshot = unified_install_snapshot().await;
    report
        .checks
        .push(unified_install_check_from_snapshot(&unified_snapshot));
    if let Some(interpreted) = synthesize_doctor_interpretation(&report.checks) {
        report.checks.push(interpreted);
    }
    report.exit_code = if report
        .checks
        .iter()
        .any(|c| matches!(c.status, CheckStatus::Fail))
    {
        1
    } else if report
        .checks
        .iter()
        .any(|c| matches!(c.status, CheckStatus::Warn))
    {
        2
    } else {
        0
    };
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&doctor_json_value(&report, &unified_snapshot))?
        ),
        OutputFormat::Text => {
            print_banner("◆", "Checkup", "The whole setup, and what to fix");
            for c in &report.checks {
                let marker = match c.status {
                    CheckStatus::Pass => style("●").green().bold(),
                    CheckStatus::Warn => style("!").yellow().bold(),
                    CheckStatus::Fail => style("×").red().bold(),
                };
                let title = c.title();
                let prefix_text = format!("  {} {:<26} ", "•", title);
                let prefix_display =
                    format!("  {} {} ", marker, style(format!("{:<26}", title)).bold());
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

pub(crate) fn doctor_json_value(
    report: &plug_core::doctor::DoctorReport,
    snapshot: &UnifiedInstallSnapshot,
) -> serde_json::Value {
    serde_json::json!({
        "checks": report.checks,
        "exit_code": report.exit_code,
        "unified_install": snapshot,
    })
}

fn unified_install_check_from_snapshot(snapshot: &UnifiedInstallSnapshot) -> CheckResult {
    let Some(app) = snapshot.app.as_ref() else {
        return CheckResult {
            name: check::UNIFIED_INSTALL.to_string(),
            status: CheckStatus::Fail,
            message: "No verified Plug.app installation was found.".to_string(),
            fix_suggestion: Some("Open Plug.app and retry reconciliation.".to_string()),
        };
    };

    if !snapshot.inspection_errors.is_empty()
        || !snapshot.daemon_inspection_complete
        || !snapshot.adapter_inspection_complete
        || !snapshot.launchd_inspection_complete
    {
        let mut failures = snapshot.inspection_errors.clone();
        if !snapshot.daemon_inspection_complete
            && !failures.iter().any(|failure| failure.contains("daemon"))
        {
            failures.push("daemon inspection was incomplete".to_string());
        }
        if !snapshot.adapter_inspection_complete
            && !failures.iter().any(|failure| failure.contains("adapter"))
        {
            failures.push("adapter inspection was incomplete".to_string());
        }
        if !snapshot.launchd_inspection_complete
            && !failures.iter().any(|failure| failure.contains("launchd"))
        {
            failures.push("launchd inspection was incomplete".to_string());
        }
        return CheckResult {
            name: check::UNIFIED_INSTALL.to_string(),
            status: CheckStatus::Fail,
            message: failures.join("; "),
            fix_suggestion: Some("Open Plug.app and retry reconciliation.".to_string()),
        };
    }

    let mut drift = Vec::new();
    if !snapshot
        .shell_resolution
        .as_ref()
        .is_some_and(|path| crate::install::paths_match(path, &app.executable_path))
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
        .is_some_and(|path| crate::install::paths_match(path, &app.executable_path))
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
    let expected_service_present = snapshot.launchd_jobs.iter().any(|job| {
        job.label == plug_core::paths::DAEMON_LABEL
            && crate::install::paths_match(&job.program, &app.executable_path)
    });
    if !expected_service_present {
        drift.push("expected app service is absent".to_string());
    } else if !snapshot.app_service_daemon_invocation_verified {
        drift.push("expected app service daemon invocation is not proven".to_string());
    }
    let stale_clients = snapshot
        .linked_clients
        .iter()
        .filter(|item| item.disposition.needs_repair())
        .map(|item| item.target.as_str())
        .collect::<Vec<_>>();
    if snapshot.client_repair_needed || !stale_clients.is_empty() {
        drift.push(format!(
            "client links need repair: {}",
            if stale_clients.is_empty() {
                "inspection incomplete".to_string()
            } else {
                stale_clients.join(", ")
            }
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
    let app_executable = app.executable_path.as_path();
    let mut leftover_legacy_jobs = Vec::new();
    let mut unknown_launchd_jobs = Vec::new();
    for job in &snapshot.launchd_jobs {
        let is_expected_current = job.label == plug_core::paths::DAEMON_LABEL
            && crate::install::paths_match(&job.program, app_executable);
        if is_expected_current {
            continue;
        }
        let formatted = format!("{} ({})", job.label, job.program.display());
        match crate::service::classify_launchd_program(job, Some(app_executable)) {
            crate::service::LaunchdProgramOwnership::RecognizedLegacyPlug => {
                leftover_legacy_jobs.push(formatted);
            }
            crate::service::LaunchdProgramOwnership::CurrentApp
            | crate::service::LaunchdProgramOwnership::Unknown => {
                unknown_launchd_jobs.push(formatted);
            }
        }
    }
    if !leftover_legacy_jobs.is_empty() {
        drift.push(format!(
            "leftover launchd jobs require adoption: {}",
            leftover_legacy_jobs.join(", ")
        ));
    }
    if !unknown_launchd_jobs.is_empty() {
        drift.push(format!(
            "unknown launchd jobs left untouched: {}",
            unknown_launchd_jobs.join(", ")
        ));
    }

    if drift.is_empty() {
        CheckResult {
            name: check::UNIFIED_INSTALL.to_string(),
            status: CheckStatus::Pass,
            message: "Plug.app owns the app, command line, daemon, and client links.".to_string(),
            fix_suggestion: None,
        }
    } else {
        CheckResult {
            name: check::UNIFIED_INSTALL.to_string(),
            status: CheckStatus::Warn,
            message: drift.join("; "),
            fix_suggestion: Some(if leftover_legacy_jobs.is_empty() {
                "Open Plug.app and retry reconciliation.".to_string()
            } else {
                crate::service::LEFTOVER_LAUNCHD_ADOPT_SENTENCE.to_string()
            }),
        }
    }
}

fn client_repair_needed(linked_clients: &[ClientRepairItem]) -> bool {
    linked_clients
        .iter()
        .any(|item| item.disposition.needs_repair())
}

async fn bounded_ipc_probe<F>(
    future: F,
    timeout: std::time::Duration,
    name: &str,
) -> anyhow::Result<plug_core::ipc::IpcResponse>
where
    F: std::future::Future<
            Output = Result<plug_core::ipc::IpcResponse, crate::ipc_client::IpcClientError>,
        >,
{
    Ok(tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| anyhow::anyhow!("{name} timed out after {}ms", timeout.as_millis()))??)
}

async fn unified_install_snapshot() -> UnifiedInstallSnapshot {
    use plug_core::ipc::{DaemonOwnershipMode, IpcRequest, IpcResponse};

    let mut inspection_errors = Vec::new();
    let app = match crate::install::resolve_verified_app() {
        Ok(app) => app,
        Err(error) => {
            inspection_errors.push(format!("app inspection failed: {error}"));
            None
        }
    };
    let shell_resolution = match crate::install::resolve_login_shell_command() {
        Ok(path) => path,
        Err(error) => {
            inspection_errors.push(format!("shell inspection failed: {error}"));
            None
        }
    };
    let handshake = bounded_ipc_probe(
        crate::ipc_client::request(IpcRequest::OperatorHandshake {
            client_version: env!("CARGO_PKG_VERSION").to_string(),
        }),
        std::time::Duration::from_secs(2),
        "daemon operator handshake",
    )
    .await;
    let (daemon_version, daemon_executable, ownership, daemon_inspection_complete) = match handshake
    {
        Ok(IpcResponse::OperatorHandshake { handshake }) => (
            Some(handshake.daemon_version),
            handshake.daemon_executable,
            handshake.ownership,
            true,
        ),
        Ok(_) => {
            inspection_errors
                .push("daemon operator handshake returned an unexpected response".to_string());
            (None, None, DaemonOwnershipMode::Unknown, false)
        }
        Err(error) => {
            inspection_errors.push(format!("daemon inspection failed: {error}"));
            (None, None, DaemonOwnershipMode::Unknown, false)
        }
    };
    let live_sessions = bounded_ipc_probe(
        crate::ipc_client::request(IpcRequest::ListLiveSessions),
        std::time::Duration::from_secs(2),
        "adapter session inspection",
    )
    .await;
    let (live_sessions, adapter_inspection_complete) = match live_sessions {
        Ok(IpcResponse::LiveSessions { sessions, .. }) => (sessions, true),
        Ok(_) => {
            inspection_errors
                .push("adapter session inspection returned an unexpected response".to_string());
            (Vec::new(), false)
        }
        Err(error) => {
            inspection_errors.push(format!("adapter inspection failed: {error}"));
            (Vec::new(), false)
        }
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
    let app_program = app.as_ref().map(|app| app.executable_path.clone());
    let launchd = tokio::task::spawn_blocking(move || {
        crate::service::discover_launchd_jobs(
            app_program.as_deref(),
            std::time::Duration::from_secs(3),
        )
    })
    .await;
    let (launchd_jobs, launchd_inspection_complete) = match launchd {
        Ok(Ok(jobs)) => (
            jobs.into_iter()
                .filter(crate::service::is_plug_launchd_candidate)
                .collect(),
            true,
        ),
        Ok(Err(error)) => {
            inspection_errors.push(format!("launchd inspection failed: {error}"));
            (Vec::new(), false)
        }
        Err(error) => {
            inspection_errors.push(format!("launchd inspection worker failed: {error}"));
            (Vec::new(), false)
        }
    };
    let mut launchd_inspection_complete = launchd_inspection_complete;
    let app_service_daemon_invocation_verified = match app.as_ref() {
        Some(app)
            if launchd_jobs.iter().any(|job| {
                job.label == plug_core::paths::DAEMON_LABEL
                    && crate::install::paths_match(&job.program, &app.executable_path)
            }) =>
        {
            match crate::service::inspect() {
                Ok(state) => state.ownership == crate::service::ServiceOwnership::AppManaged,
                Err(error) => {
                    launchd_inspection_complete = false;
                    inspection_errors.push(format!(
                        "launchd app service invocation inspection failed: {error}"
                    ));
                    false
                }
            }
        }
        _ => false,
    };
    let shadows = crate::install::discover_shadow_installations(app.as_ref());
    let client_repair_needed = client_repair_needed(&linked_clients);
    UnifiedInstallSnapshot {
        app,
        shell_resolution,
        daemon_version,
        daemon_executable,
        ownership,
        linked_clients,
        adapters,
        shadows,
        launchd_jobs,
        daemon_inspection_complete,
        adapter_inspection_complete,
        launchd_inspection_complete,
        app_service_daemon_invocation_verified,
        inspection_errors,
        client_repair_needed,
    }
}

fn inspect_linked_clients(
    app: Option<&crate::install::VerifiedAppInstallation>,
) -> Vec<ClientRepairItem> {
    let Some(app) = app else {
        return Vec::new();
    };
    ClientTarget::listed()
        .filter_map(|target| inspect_link(target, &app.executable_path, false))
        .collect()
}

async fn runtime_doctor_checks(config: &plug_core::config::Config) -> Vec<CheckResult> {
    let mut checks = Vec::new();
    let daemon_reachable = crate::ipc_client::daemon_running().await;
    let status_response = crate::ipc_client::request(plug_core::ipc::IpcRequest::Status).await;
    let auth_response = crate::ipc_client::request(plug_core::ipc::IpcRequest::AuthStatus).await;

    if daemon_reachable
        && !matches!(
            status_response,
            Ok(plug_core::ipc::IpcResponse::Status { .. })
        )
    {
        checks.push(CheckResult {
            name: check::RUNTIME_AVAILABILITY.to_string(),
            status: CheckStatus::Warn,
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
        checks.push(CheckResult {
            name: check::AUTH_AVAILABILITY.to_string(),
            status: CheckStatus::Warn,
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
        checks.extend(runtime_health_checks(&servers, clients, uptime_secs));
    }

    if let Ok(plug_core::ipc::IpcResponse::AuthStatus { servers }) = auth_response {
        checks.extend(runtime_auth_checks(&servers));
    }

    if daemon_reachable {
        let snapshot = crate::ipc_client::operator(plug_core::ipc::OperatorRequest::Snapshot).await;
        if let Ok(plug_core::ipc::IpcResponse::OperatorSnapshot { snapshot }) = snapshot {
            // A registration the owner named is one they know about.
            let named = config
                .clients
                .iter()
                .filter(|(_, settings)| settings.name.is_some())
                .filter_map(|(key, _)| {
                    plug_core::downstream_identity::grant_of_client_key(key).map(str::to_owned)
                })
                .collect();
            checks.extend(duplicate_downstream_client_check(
                &snapshot.downstream_clients,
                &named,
            ));
        }
    }

    checks
}

/// Remote apps that sign in through dynamic registration make a new client
/// each time, and every old one keeps its grants until its registration
/// runs out. Several registrations under one name are usually leftovers
/// worth revoking. One the owner gave a name of its own is not a leftover,
/// and one whose sign-in never finished goes by itself.
fn duplicate_downstream_client_check(
    clients: &[plug_core::downstream_oauth::RegisteredClientSummary],
    named: &std::collections::BTreeSet<String>,
) -> Option<CheckResult> {
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for client in clients {
        if client.is_unactivated() || named.contains(&client.client_id) {
            continue;
        }
        *counts.entry(client.client_name.as_str()).or_default() += 1;
    }
    let duplicates = counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, count)| format!("{name} ×{count}"))
        .collect::<Vec<_>>();
    if duplicates.is_empty() {
        return None;
    }
    Some(CheckResult {
        name: check::DOWNSTREAM_OAUTH_CLIENTS.to_string(),
        status: CheckStatus::Warn,
        message: format!(
            "Clients signed in more than once: {}. Each old sign-in keeps its access until {} days after its last use",
            duplicates.join(", "),
            plug_core::downstream_oauth::REGISTRATION_LIFETIME_SECS / (24 * 3600)
        ),
        fix_suggestion: Some(
            "Open Clients in Plug and turn off the ones you no longer use, or give each one you keep a name of its own. From a terminal: `plug auth clients list`, then `plug auth clients revoke <client_id>`".to_string(),
        ),
    })
}

/// With the daemon answering, `runtime_health` already says it is running.
/// The cold port and PID checks only repeat that when they pass, so they are
/// dropped then and kept whenever they found something wrong.
fn fold_daemon_liveness_checks(checks: &mut Vec<CheckResult>) {
    if !checks
        .iter()
        .any(|check| check.name == check::RUNTIME_HEALTH)
    {
        return;
    }
    checks.retain(|check| {
        !(matches!(
            check.name.as_str(),
            check::PORT_AVAILABLE | check::PID_STALENESS
        ) && matches!(check.status, CheckStatus::Pass))
    });
}

struct RuntimeServerCounts {
    healthy: usize,
    degraded: usize,
    auth_required: usize,
    failed: usize,
}

/// One readable line for the live daemon: how long it has run, who is
/// connected, and how many servers are healthy, naming only the problems.
fn runtime_health_message(uptime_secs: u64, clients: usize, counts: RuntimeServerCounts) -> String {
    let total = counts.healthy + counts.degraded + counts.auth_required + counts.failed;
    let clients = match clients {
        1 => "1 local client".to_string(),
        n => format!("{n} local clients"),
    };
    let servers = if total == counts.healthy {
        match total {
            0 => "no servers configured".to_string(),
            1 => "its server is healthy".to_string(),
            n => format!("all {n} servers healthy"),
        }
    } else {
        let problems = [
            (counts.degraded, "degraded"),
            (counts.auth_required, "need sign-in"),
            (counts.failed, "failed"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect::<Vec<_>>()
        .join(", ");
        format!("{} of {total} servers healthy ({problems})", counts.healthy)
    };
    format!(
        "Daemon running for {}, {clients}, {servers}",
        crate::ui::format_duration(uptime_secs)
    )
}

/// `name (why it failed)`, or just the name when the daemon gave no reason.
fn server_with_reason(server: &plug_core::types::ServerStatus) -> String {
    match &server.error {
        Some(error) => format!("{} ({error})", server.server_id),
        None => server.server_id.clone(),
    }
}

/// The live daemon's server health: one summary, then the failing and
/// degraded servers by name.
fn runtime_health_checks(
    servers: &[plug_core::types::ServerStatus],
    clients: usize,
    uptime_secs: u64,
) -> Vec<CheckResult> {
    let mut counts = RuntimeServerCounts {
        healthy: 0,
        degraded: 0,
        auth_required: 0,
        failed: 0,
    };
    let mut failed_servers = Vec::new();
    let mut degraded_servers = Vec::new();
    for server in servers
        .iter()
        .filter(|s| s.server_id != plug_core::tool_naming::INTERNAL_SERVER_ID)
    {
        match server.health {
            plug_core::types::ServerHealth::Healthy => counts.healthy += 1,
            plug_core::types::ServerHealth::Degraded => {
                counts.degraded += 1;
                degraded_servers.push(server_with_reason(server));
            }
            plug_core::types::ServerHealth::Failed => {
                counts.failed += 1;
                failed_servers.push(server_with_reason(server));
            }
            plug_core::types::ServerHealth::AuthRequired => counts.auth_required += 1,
        }
    }
    let troubled = counts.failed > 0 || counts.degraded > 0 || counts.auth_required > 0;

    let mut checks = vec![CheckResult {
        name: check::RUNTIME_HEALTH.to_string(),
        status: if troubled {
            CheckStatus::Warn
        } else {
            CheckStatus::Pass
        },
        message: runtime_health_message(uptime_secs, clients, counts),
        fix_suggestion: troubled.then(|| {
            "Use `plug status` for affected servers, then `plug auth status` for auth recovery details".to_string()
        }),
    }];
    if !failed_servers.is_empty() {
        checks.push(CheckResult {
            name: check::RUNTIME_FAILURES.to_string(),
            status: CheckStatus::Fail,
            message: format!("failing servers: {}", failed_servers.join("; ")),
            fix_suggestion: Some(
                "Fix the cause named for each server. Plug keeps retrying and picks the server back up without a restart".to_string(),
            ),
        });
    }
    if !degraded_servers.is_empty() {
        checks.push(CheckResult {
            name: check::RUNTIME_DEGRADED.to_string(),
            status: CheckStatus::Warn,
            message: format!("degraded servers: {}", degraded_servers.join("; ")),
            fix_suggestion: Some(
                "Compare `plug status` and `plug doctor` to separate transient runtime degradation from cold connectivity or auth issues".to_string(),
            ),
        });
    }
    checks
}

fn synthesize_doctor_interpretation(checks: &[CheckResult]) -> Option<CheckResult> {
    let connectivity = checks
        .iter()
        .find(|check| check.name == check::SERVER_CONNECTIVITY)?;
    let runtime_health = checks
        .iter()
        .find(|check| check.name == check::RUNTIME_HEALTH);
    let runtime_failures = checks
        .iter()
        .find(|check| check.name == check::RUNTIME_FAILURES);
    let runtime_auth_attention = checks.iter().any(|check| {
        matches!(check.status, CheckStatus::Warn)
            && (check.name == check::RUNTIME_AUTH_MISSING
                || check.name == check::RUNTIME_AUTH_REAUTH
                || check.name == check::RUNTIME_AUTH_DEGRADED)
    });

    use CheckStatus::{Fail, Pass, Warn};

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
                "Fix the cause named for each server under runtime_failures. Plug keeps retrying and picks the server back up without a restart.".to_string(),
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

    Some(CheckResult {
        name: check::DOCTOR_INTERPRETATION.to_string(),
        status,
        message,
        fix_suggestion,
    })
}

fn runtime_auth_checks(servers: &[plug_core::ipc::IpcAuthServerInfo]) -> Vec<CheckResult> {
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
        checks.push(CheckResult {
            name: check::RUNTIME_AUTH_MISSING.to_string(),
            status: CheckStatus::Warn,
            message: format!("missing credentials: {}", missing.join(", ")),
            fix_suggestion: Some(
                "Run `plug auth login --server <name>` for each server missing credentials."
                    .to_string(),
            ),
        });
    }

    if !reauth.is_empty() {
        checks.push(CheckResult {
            name: check::RUNTIME_AUTH_REAUTH.to_string(),
            status: CheckStatus::Warn,
            message: format!("re-auth required: {}", reauth.join(", ")),
            fix_suggestion: Some(
                "Stored credentials exist but must be refreshed — run `plug auth login --server <name>`."
                    .to_string(),
            ),
        });
    }

    if !degraded.is_empty() {
        checks.push(CheckResult {
            name: check::RUNTIME_AUTH_DEGRADED.to_string(),
            status: CheckStatus::Warn,
            message: format!("degraded auth/runtime: {}", degraded.join(", ")),
            fix_suggestion: Some(
                "Run `plug auth status` and compare with `plug status` to separate auth drift from broader runtime degradation."
                    .to_string(),
            ),
        });
    }

    checks
}

fn doctor_check_details(check: &CheckResult) -> String {
    match check.fix_suggestion.as_deref() {
        Some(suggestion) if !suggestion.trim().is_empty() => {
            format!("{} Next: {}", check.message, suggestion.trim())
        }
        _ => check.message.clone(),
    }
}

fn doctor_next_steps(checks: &[CheckResult]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut steps = Vec::new();

    for check in checks
        .iter()
        .filter(|check| matches!(check.status, CheckStatus::Warn | CheckStatus::Fail))
    {
        if let Some(suggestion) = check.fix_suggestion.as_deref() {
            let trimmed = suggestion.trim();
            if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
                steps.push(trimmed.to_string());
            }
        }
    }

    steps
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn unified_snapshot() -> UnifiedInstallSnapshot {
        let executable = std::path::PathBuf::from("/Applications/Plug.app/Contents/Resources/plug");
        UnifiedInstallSnapshot {
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
            daemon_inspection_complete: true,
            adapter_inspection_complete: true,
            launchd_inspection_complete: true,
            app_service_daemon_invocation_verified: true,
            inspection_errors: Vec::new(),
            client_repair_needed: false,
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
            "daemon_inspection_complete",
            "adapter_inspection_complete",
            "launchd_inspection_complete",
            "app_service_daemon_invocation_verified",
            "inspection_errors",
            "client_repair_needed",
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
    fn unified_install_leftover_legacy_launchd_uses_adopt_sentence() {
        let mut snapshot = unified_snapshot();
        snapshot.launchd_jobs = vec![
            crate::service::LaunchdJobRecord {
                label: "com.plug.daemon".to_string(),
                program: std::path::PathBuf::from("/Applications/Plug.app/Contents/Resources/plug"),
            },
            crate::service::LaunchdJobRecord {
                label: "com.plug.daemon".to_string(),
                program: std::path::PathBuf::from("/opt/homebrew/Cellar/plug/0.6.3/bin/plug"),
            },
        ];

        let result = super::unified_install_check_from_snapshot(&snapshot);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(
            result
                .message
                .contains("leftover launchd jobs require adoption")
        );
        assert!(result.message.contains("com.plug.daemon"));
        assert!(
            result
                .message
                .contains("/opt/homebrew/Cellar/plug/0.6.3/bin/plug")
        );
        assert!(!result.message.contains("unknown or legacy"));
        assert_eq!(
            result.fix_suggestion.as_deref(),
            Some(crate::service::LEFTOVER_LAUNCHD_ADOPT_SENTENCE)
        );
        assert!(
            result
                .fix_suggestion
                .as_deref()
                .is_some_and(|sentence| sentence.contains("Turn On") && sentence.contains("adopt"))
        );
    }

    #[test]
    fn unified_install_unknown_launchd_stays_distinct_from_leftover_legacy() {
        let mut snapshot = unified_snapshot();
        snapshot
            .launchd_jobs
            .push(crate::service::LaunchdJobRecord {
                label: "local.plug.lookalike".to_string(),
                program: std::path::PathBuf::from("/Applications/Other.app/other"),
            });

        let result = super::unified_install_check_from_snapshot(&snapshot);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(
            result
                .message
                .contains("unknown launchd jobs left untouched")
        );
        assert!(
            !result
                .message
                .contains("leftover launchd jobs require adoption")
        );
        assert_eq!(
            result.fix_suggestion.as_deref(),
            Some("Open Plug.app and retry reconciliation.")
        );
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
    fn unified_install_absent_expected_service_is_not_healthy() {
        let mut snapshot = unified_snapshot();
        snapshot.launchd_jobs.clear();
        let result = super::unified_install_check_from_snapshot(&snapshot);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("expected app service is absent"));
    }

    #[test]
    fn unified_install_unproven_daemon_invocation_is_not_healthy() {
        let mut snapshot = unified_snapshot();
        snapshot.app_service_daemon_invocation_verified = false;
        let result = super::unified_install_check_from_snapshot(&snapshot);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("daemon invocation is not proven"));
    }

    #[test]
    fn unified_install_inspection_failure_fails_closed() {
        let mut snapshot = unified_snapshot();
        snapshot.launchd_inspection_complete = false;
        snapshot
            .inspection_errors
            .push("launchd inspection timed out".to_string());
        let result = super::unified_install_check_from_snapshot(&snapshot);
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(result.message.contains("launchd inspection timed out"));
    }

    #[test]
    fn unified_install_client_repair_needed_is_conservative_and_serialized() {
        for (disposition, expected) in [
            (PlugLinkDisposition::Canonical, false),
            (PlugLinkDisposition::Http, false),
            (PlugLinkDisposition::Missing, false),
            (PlugLinkDisposition::RecognizedLegacy, true),
            (PlugLinkDisposition::UnknownCommand, true),
        ] {
            let mut snapshot = unified_snapshot();
            snapshot.linked_clients[0].disposition = disposition;
            snapshot.client_repair_needed = super::client_repair_needed(&snapshot.linked_clients);
            let report = plug_core::doctor::DoctorReport {
                checks: vec![super::unified_install_check_from_snapshot(&snapshot)],
                exit_code: 0,
            };
            let value = super::doctor_json_value(&report, &snapshot);
            assert_eq!(
                value["unified_install"]["client_repair_needed"],
                serde_json::json!(expected)
            );
            assert_eq!(
                value["unified_install"]["linked_clients"][0]["disposition"],
                serde_json::to_value(&snapshot.linked_clients[0].disposition).unwrap()
            );
        }
    }

    #[test]
    fn unified_install_incomplete_inspection_does_not_false_pass_client_state() {
        let mut snapshot = unified_snapshot();
        snapshot.adapter_inspection_complete = false;
        snapshot
            .inspection_errors
            .push("adapter inspection failed".to_string());
        snapshot.client_repair_needed = false;

        let result = super::unified_install_check_from_snapshot(&snapshot);
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(result.message.contains("adapter inspection failed"));
        assert!(!snapshot.client_repair_needed);
    }

    #[test]
    fn unified_install_cli_json_contract_exposes_snapshot() {
        let snapshot = unified_snapshot();
        let report = plug_core::doctor::DoctorReport {
            checks: vec![super::unified_install_check_from_snapshot(&snapshot)],
            exit_code: 0,
        };
        let value = super::doctor_json_value(&report, &snapshot);
        assert_eq!(
            value["unified_install"]["app"]["version"],
            serde_json::json!("0.6.4")
        );
        assert_eq!(
            value["unified_install"]["ownership"],
            serde_json::json!("app_managed")
        );
        assert!(value["checks"].is_array());
    }

    #[test]
    fn app_coordinator_release_contract_is_stable_and_preserves_unknown_state() {
        let app = crate::install::VerifiedAppInstallation {
            bundle_path: std::path::PathBuf::from("/Applications/Plug.app"),
            executable_path: std::path::PathBuf::from(
                "/Applications/Plug.app/Contents/Resources/plug",
            ),
            version: "0.6.4".to_string(),
        };
        let canonical_command = app.executable_path.to_string_lossy().into_owned();

        // Canonical stdio export points clients at the app-owned executable.
        let exported = ClientTarget::Cursor.plug_entry(
            plug_core::clients::LinkTransport::Stdio,
            &canonical_command,
            "",
        );
        assert_eq!(exported["command"], canonical_command);
        assert_eq!(
            exported["args"],
            serde_json::json!(["connect", "--client", "cursor"])
        );

        // A recognized legacy client entry migrates to the canonical command.
        let legacy = r#"{"mcpServers":{"plug":{"command":"/opt/homebrew/bin/plug","args":["connect"]},"unrelated":{"command":"other","custom":true}},"unknown":{"preserved":true}}"#;
        let migrated = repair_content(ClientTarget::Cursor, legacy, &app.executable_path)
            .unwrap()
            .1
            .expect("recognized legacy entry must migrate");
        let migrated: serde_json::Value = serde_json::from_str(&migrated).unwrap();
        assert_eq!(
            migrated["mcpServers"]["plug"]["command"],
            serde_json::json!(app.executable_path)
        );
        assert_eq!(migrated["mcpServers"]["unrelated"]["custom"], true);
        assert_eq!(migrated["unknown"]["preserved"], true);

        // Unknown commands are represented exactly and never receive a rewrite.
        let unknown_source = r#"{"mcpServers":{"plug":{"command":"/tmp/not-plug","args":["connect"]}},"unknown":{"preserved":true}}"#;
        let (disposition, updated, _) =
            repair_content(ClientTarget::Cursor, unknown_source, &app.executable_path).unwrap();
        assert_eq!(updated, None);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(unknown_source).unwrap()["unknown"],
            serde_json::json!({"preserved": true})
        );

        let repair = ClientRepairReport::new(
            app.executable_path.clone(),
            vec![ClientRepairItem {
                target: "cursor".to_string(),
                path: std::path::PathBuf::from("/tmp/unknown.json"),
                disposition,
                changed: false,
                message: "Unknown command was left untouched.".to_string(),
            }],
        );
        assert_eq!(
            serde_json::to_value(&repair).unwrap(),
            serde_json::json!({
                "canonical_command": "/Applications/Plug.app/Contents/Resources/plug",
                "items": [{
                    "target": "cursor",
                    "path": "/tmp/unknown.json",
                    "disposition": "unknown_command",
                    "changed": false,
                    "message": "Unknown command was left untouched."
                }],
                "repair_needed": true
            })
        );

        let snapshot = unified_snapshot();
        let report = plug_core::doctor::DoctorReport {
            checks: vec![super::unified_install_check_from_snapshot(&snapshot)],
            exit_code: 0,
        };
        let doctor = super::doctor_json_value(&report, &snapshot);
        assert_eq!(doctor["exit_code"], 0);
        assert_eq!(doctor["checks"][0]["status"], "Pass");
        assert_eq!(
            doctor["checks"][0]["message"],
            "Plug.app owns the app, command line, daemon, and client links."
        );
        assert_eq!(
            doctor["unified_install"]["daemon_executable"],
            "/Applications/Plug.app/Contents/Resources/plug"
        );
        assert_eq!(doctor["unified_install"]["adapters"][0], "current");
    }

    #[tokio::test(start_paused = true)]
    async fn unified_install_ipc_probe_timeout_is_bounded() {
        let result = super::bounded_ipc_probe(
            std::future::pending::<
                Result<plug_core::ipc::IpcResponse, crate::ipc_client::IpcClientError>,
            >(),
            std::time::Duration::from_secs(2),
            "operator handshake",
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("timed out"));
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
    fn runtime_health_reads_as_a_sentence_when_everything_is_healthy() {
        assert_eq!(
            runtime_health_message(
                2117,
                18,
                RuntimeServerCounts {
                    healthy: 14,
                    degraded: 0,
                    auth_required: 0,
                    failed: 0,
                },
            ),
            "Daemon running for 35m, 18 local clients, all 14 servers healthy"
        );
    }

    #[test]
    fn passing_port_and_pid_checks_fold_into_runtime_health() {
        let mut checks = vec![
            check(
                "port_available",
                CheckStatus::Pass,
                "Port 3282 is bound by the daemon",
            ),
            check(
                "pid_staleness",
                CheckStatus::Pass,
                "Daemon is running (PID 1)",
            ),
            check("runtime_health", CheckStatus::Pass, "Daemon running for 1m"),
        ];
        fold_daemon_liveness_checks(&mut checks);
        let names = checks.iter().map(|c| c.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, ["runtime_health"]);
    }

    #[test]
    fn failing_port_or_pid_checks_survive_the_fold() {
        let mut checks = vec![
            check(
                "port_available",
                CheckStatus::Fail,
                "Port 3282 is not available",
            ),
            check("pid_staleness", CheckStatus::Warn, "Stale PID file"),
            check("runtime_health", CheckStatus::Pass, "Daemon running for 1m"),
        ];
        fold_daemon_liveness_checks(&mut checks);
        assert_eq!(checks.len(), 3);
    }

    #[test]
    fn liveness_checks_stay_when_the_daemon_did_not_answer() {
        let mut checks = vec![
            check(
                "port_available",
                CheckStatus::Pass,
                "Port 3282 is available",
            ),
            check("pid_staleness", CheckStatus::Pass, "No PID file found"),
        ];
        fold_daemon_liveness_checks(&mut checks);
        assert_eq!(checks.len(), 2);
    }

    fn registered(name: &str, id: &str) -> plug_core::downstream_oauth::RegisteredClientSummary {
        plug_core::downstream_oauth::RegisteredClientSummary {
            client_id: id.to_string(),
            client_name: name.to_string(),
            redirect_uris: vec!["http://127.0.0.1/callback".to_string()],
            source: plug_core::downstream_oauth::ClientSource::DynamicRegistration,
            created_at: 1,
            last_used_at: Some(1),
            expires_at: 2,
            signed_in: true,
            client_key: format!("oauth:{id}"),
            target: None,
        }
    }

    #[test]
    fn duplicate_client_registrations_warn_by_name() {
        let check = duplicate_downstream_client_check(
            &[
                registered("Claude", "a"),
                registered("Cursor", "b"),
                registered("Cursor", "c"),
                registered("Devin", "d"),
                registered("Devin", "e"),
                registered("Devin", "f"),
            ],
            &Default::default(),
        )
        .expect("duplicates warn");
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.message.contains("Cursor ×2, Devin ×3"),
            "{}",
            check.message
        );
        assert!(!check.message.contains("Claude"));
        assert!(
            check
                .fix_suggestion
                .as_deref()
                .is_some_and(|fix| fix.contains("plug auth clients revoke"))
        );
    }

    #[test]
    fn unique_client_registrations_add_no_check() {
        assert!(
            duplicate_downstream_client_check(
                &[registered("Claude", "a"), registered("Cursor", "b")],
                &Default::default()
            )
            .is_none()
        );
    }

    #[test]
    fn named_and_unfinished_registrations_are_not_duplicates() {
        let mut unfinished = registered("Cursor", "c");
        unfinished.signed_in = false;
        unfinished.last_used_at = None;
        let named = ["b".to_string()].into_iter().collect();
        assert!(
            duplicate_downstream_client_check(
                &[
                    registered("Cursor", "a"),
                    registered("Cursor", "b"),
                    unfinished
                ],
                &named
            )
            .is_none()
        );
    }

    #[test]
    fn runtime_checks_split_summary_from_named_failures() {
        let checks = runtime_health_checks(
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
                    error: None,
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
                    error: Some("failed to spawn `node`: not found on PATH".to_string()),
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
                    error: None,
                    last_seen: None,
                },
            ],
            4,
            120,
        );

        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "runtime_health");
        assert_eq!(checks[0].status, CheckStatus::Warn);
        assert_eq!(
            checks[0].message,
            "Daemon running for 2m, 4 local clients, 1 of 3 servers healthy (1 need sign-in, 1 failed)"
        );

        assert_eq!(checks[1].name, "runtime_failures");
        assert_eq!(checks[1].status, CheckStatus::Fail);
        assert_eq!(
            checks[1].message,
            "failing servers: oura (failed to spawn `node`: not found on PATH)"
        );
    }

    #[test]
    fn runtime_checks_include_named_degraded_servers() {
        let checks = runtime_health_checks(
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
                    error: None,
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
                    error: None,
                    last_seen: None,
                },
            ],
            2,
            45,
        );

        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "runtime_health");
        assert_eq!(checks[0].status, CheckStatus::Warn);
        assert_eq!(
            checks[0].message,
            "Daemon running for 45s, 2 local clients, 1 of 2 servers healthy (1 degraded)"
        );

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
    fn repair_content_preserves_canonical_and_unknown_json_entries() {
        let canonical = std::path::Path::new("/Applications/Plug.app/Contents/Resources/plug");
        let canonical_content = format!(
            r#"{{"mcpServers":{{"plug":{{"command":"{}","args":["connect"],"custom":true}},"other":{{"command":"other"}}}},"unknown":{{"preserved":true}}}}"#,
            canonical.display()
        );
        let canonical_repair = repair_content(ClientTarget::Cursor, &canonical_content, canonical)
            .expect("canonical config should parse");
        assert_eq!(canonical_repair.0, PlugLinkDisposition::Canonical);
        assert_eq!(canonical_repair.1, None);

        let unknown_content = r#"{"mcpServers":{"plug":{"command":"/tmp/other","args":["connect"]},"other":{"command":"other"}},"unknown":{"preserved":true}}"#;
        let unknown_repair = repair_content(ClientTarget::Cursor, unknown_content, canonical)
            .expect("unknown config should parse");
        assert_eq!(unknown_repair.0, PlugLinkDisposition::UnknownCommand);
        assert_eq!(unknown_repair.1, None);
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
                ClientTarget::Cursor,
                format!(r#"{{"mcpServers":{{"plug":{{"command":"{cargo_command}","args":["connect"]}},"other":{{"command":"other"}}}},"unknown":{{"preserved":true}}}}"#),
            ),
            (
                ClientTarget::CodexCli,
                "[mcp_servers.plug]\ncommand = \"/opt/homebrew/bin/plug\"\nargs = [\"connect\"]\nunknown = \"keep\"\n\n[mcp_servers.other]\ncommand = \"other\"\n".to_string(),
            ),
            (
                ClientTarget::Goose,
                "extensions:\n  plug:\n    type: stdio\n    command: /Users/rob/src/plug/target/release/plug\n    args: [connect]\n    unknown: keep\n  other:\n    type: stdio\n    command: other\nunknown: preserved\n".to_string(),
            ),
        ];

        for (target, content) in &fixtures {
            let repair =
                repair_content(*target, content, canonical).expect("legacy config should parse");
            assert_eq!(repair.0, PlugLinkDisposition::RecognizedLegacy);
            let updated = repair.1.expect("legacy entry should update");
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
        let recognized = repair_content(ClientTarget::Cursor, &valid_args, canonical)
            .expect("recognized Cargo path should be repairable with valid args");
        assert_eq!(recognized.0, PlugLinkDisposition::RecognizedLegacy);
        let invalid = repair_content(ClientTarget::Cursor, &invalid_args, canonical)
            .expect("invalid args are reported, not repaired");
        assert_eq!(invalid.0, PlugLinkDisposition::UnknownCommand);
        assert_eq!(invalid.1, None);

        let dual_container = format!(
            r#"{{"mcpServers":{{"other":{{"command":"other"}}}},"context_servers":{{"plug":{{"command":"{cargo_command}","args":["connect"]}}}}}}"#
        );
        let repaired = repair_content(ClientTarget::Zed, &dual_container, canonical)
            .expect("dual container config should repair");
        let updated = repaired
            .1
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
                ClientTarget::CodexCli,
                "# keep this document comment\n[mcp_servers.plug]\n# keep this entry comment\ncommand = \"/opt/homebrew/bin/plug\" # command note\nargs = [\"connect\"] # args note\nunknown = \"keep\"\n\n# keep neighboring comment\n[mcp_servers.other]\ncommand = \"other\"\n".to_string(),
                "# keep neighboring comment",
            ),
            (
                ClientTarget::Goose,
                format!("# keep this document comment\nextensions:\n  plug:\n    # keep this entry comment\n    command: {cargo_command} # command note\n    args: [connect] # args note\n    unknown: keep\n  # keep neighboring comment\n  other:\n    command: other\n"),
                "# keep neighboring comment",
            ),
        ];
        for (target, content, retained_comment) in fixtures {
            let repair = repair_content(target, &content, canonical).unwrap();
            let updated = repair.1.expect("legacy entry should update");
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
                ClientTarget::CodexCli,
                "[mcp_servers.plug]\ncommand = \"/opt/homebrew/bin/plug\"\nargs = [\n  \"connect\",\n]\n\n# retain this unrelated comment\n[mcp_servers.other]\ncommand = \"other\"\n".to_string(),
            ),
            (
                ClientTarget::Goose,
                format!("extensions:\n  plug:\n    command: {cargo_command}\n    args:\n\n      - connect\n\n  # retain this unrelated comment\n  other:\n    command: other\n"),
            ),
        ];
        for (target, content) in fixtures {
            let repair = repair_content(target, &content, canonical).unwrap();
            let updated = repair.1.expect("legacy entry should update");
            assert!(updated.contains("# retain this unrelated comment"));
            assert!(
                updated.contains("[mcp_servers.other]\ncommand = \"other\"")
                    || updated.contains("other:\n    command: other")
            );
            match target {
                ClientTarget::CodexCli => {
                    assert!(toml::from_str::<toml::Value>(&updated).is_ok());
                }
                ClientTarget::Goose => {
                    assert!(serde_norway::from_str::<serde_norway::Value>(&updated).is_ok());
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn classifies_only_canonical_or_recognized_legacy_connect_commands() {
        let canonical = std::path::Path::new("/Applications/Plug.app/Contents/Resources/plug");
        let connect = vec!["connect".to_string()];
        let home = dirs::home_dir().expect("test home directory");

        assert_eq!(
            classify_plug_client_command(canonical.to_str().unwrap(), &connect, canonical),
            PlugLinkDisposition::Canonical
        );
        for legacy in [
            home.join(".cargo/bin/plug").to_string_lossy().into_owned(),
            "/opt/homebrew/bin/plug".to_string(),
            home.join("Applications/Plug.app/Contents/Resources/plug")
                .to_string_lossy()
                .into_owned(),
            "/Users/rob/src/plug/target/release/plug".to_string(),
        ] {
            assert_eq!(
                classify_plug_client_command(&legacy, &connect, canonical),
                PlugLinkDisposition::RecognizedLegacy,
                "{legacy} should be a recognized legacy Plug command"
            );
        }
        assert_eq!(
            classify_plug_client_command("/usr/local/bin/not-plug", &connect, canonical),
            PlugLinkDisposition::UnknownCommand
        );
        assert_eq!(
            classify_plug_client_command(
                "/Users/rob/project/target/release/plug",
                &["serve".to_string()],
                canonical,
            ),
            PlugLinkDisposition::UnknownCommand
        );

        // A link that says which client it serves is the same link.
        let named = ["connect", "--client", "cursor"].map(str::to_string);
        assert_eq!(
            classify_plug_client_command(canonical.to_str().unwrap(), &named, canonical),
            PlugLinkDisposition::Canonical
        );
        assert_eq!(
            classify_plug_client_command("/opt/homebrew/bin/plug", &named, canonical),
            PlugLinkDisposition::RecognizedLegacy
        );
        for other in [
            vec!["connect", "--client"],
            vec!["connect", "--config", "/tmp/other.toml"],
            vec!["connect", "--client", "cursor", "--verbose"],
        ] {
            let other: Vec<String> = other.into_iter().map(str::to_string).collect();
            assert_eq!(
                classify_plug_client_command(canonical.to_str().unwrap(), &other, canonical),
                PlugLinkDisposition::UnknownCommand,
                "{other:?} is not an argument list plug link writes"
            );
        }
    }

    #[test]
    fn rejects_broad_legacy_path_shapes() {
        let canonical = std::path::Path::new("/Applications/Plug.app/Contents/Resources/plug");
        let connect = vec!["connect".to_string()];
        for command in [
            "/tmp/homebrew/bin/plug",
            "/tmp/opt/homebrew/bin/plug",
            "/tmp/usr/local/bin/plug",
            "/Applications/Other Plug.app/Contents/Resources/plug",
            "/Users/rob/project/target/custom/plug",
            "/Users/rob/target/release/plug/not-a-plug/plug",
        ] {
            assert_eq!(
                classify_plug_client_command(command, &connect, canonical),
                PlugLinkDisposition::UnknownCommand,
                "{command} must not be adopted"
            );
        }
    }
}
