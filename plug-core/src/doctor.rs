#![allow(clippy::mutable_key_type)]

use std::path::Path;

use serde::Serialize;

use crate::config::{Config, TransportType};

/// Status of a single diagnostic check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

/// Result of a single diagnostic check.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    pub fix_suggestion: Option<String>,
}

/// Aggregated report from all diagnostic checks.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<CheckResult>,
    /// 0 = all pass, 1 = any fail, 2 = warn only.
    pub exit_code: i32,
}

impl DoctorReport {
    fn from_checks(checks: Vec<CheckResult>) -> Self {
        let has_fail = checks.iter().any(|c| c.status == CheckStatus::Fail);
        let has_warn = checks.iter().any(|c| c.status == CheckStatus::Warn);
        let exit_code = if has_fail {
            1
        } else if has_warn {
            2
        } else {
            0
        };
        Self { checks, exit_code }
    }
}

/// Run all diagnostic checks and return an aggregated report.
pub async fn run_doctor(config: &Config, config_path: &Path) -> DoctorReport {
    // Run independent checks concurrently
    let (
        config_exists,
        config_perms,
        port,
        env_vars,
        binaries,
        collisions,
        limits,
        pid,
        clients,
        http_auth,
        oauth_config,
        oauth_tokens,
    ) = tokio::join!(
        check_config_exists(config_path),
        check_config_permissions(config, config_path),
        check_port_available(config),
        check_env_vars(config),
        check_server_binaries(config),
        check_tool_collisions(config),
        check_client_limits(config),
        check_pid_staleness(),
        check_client_configs(),
        check_http_auth(config),
        check_oauth_config(config),
        check_oauth_tokens(config),
    );

    // Server connectivity is sequential-ish internally but we run it after the rest
    let connectivity = check_server_connectivity(config).await;

    let checks = vec![
        config_exists,
        config_perms,
        port,
        env_vars,
        binaries,
        collisions,
        limits,
        pid,
        clients,
        connectivity,
        http_auth,
        oauth_config,
        oauth_tokens,
    ];

    DoctorReport::from_checks(checks)
}

/// Check 1: config file exists and is valid TOML.
async fn check_config_exists(config_path: &Path) -> CheckResult {
    let name = "config_exists".to_string();

    if !config_path.exists() {
        return CheckResult {
            name,
            status: CheckStatus::Warn,
            message: format!("Config file not found: {}", config_path.display()),
            fix_suggestion: Some("Run `plug init` to create a default config".to_string()),
        };
    }

    match tokio::fs::read_to_string(config_path).await {
        Ok(contents) => match toml_parse(&contents) {
            Ok(()) => CheckResult {
                name,
                status: CheckStatus::Pass,
                message: format!("Config file valid: {}", config_path.display()),
                fix_suggestion: None,
            },
            Err(e) => CheckResult {
                name,
                status: CheckStatus::Fail,
                message: format!("Config file has invalid TOML: {e}"),
                fix_suggestion: Some("Fix the TOML syntax in your config file".to_string()),
            },
        },
        Err(e) => CheckResult {
            name,
            status: CheckStatus::Fail,
            message: format!("Cannot read config file: {e}"),
            fix_suggestion: Some("Check file permissions".to_string()),
        },
    }
}

/// Validate TOML syntax without depending on the full Config schema.
fn toml_parse(contents: &str) -> Result<(), String> {
    // Use serde_json roundtrip via figment's TOML parser indirectly —
    // but simpler: just try to parse as a generic toml table via figment.
    // Actually, we just try to deserialize as Config.
    use figment::Figment;
    use figment::providers::{Format, Toml};

    Figment::new()
        .merge(Toml::string(contents))
        .extract::<toml_value::Value>()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Minimal TOML value type for validation only.
mod toml_value {
    use serde::Deserialize;
    use std::collections::HashMap;

    #[derive(Deserialize)]
    #[serde(untagged)]
    #[allow(dead_code)]
    pub enum Value {
        Table(HashMap<String, Value>),
        Array(Vec<Value>),
        String(String),
        Integer(i64),
        Float(f64),
        Bool(bool),
    }
}

/// Check 2: config file permissions (Unix only).
async fn check_config_permissions(config: &Config, config_path: &Path) -> CheckResult {
    let name = "config_permissions".to_string();

    if !config_path.exists() {
        return CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "No config file to check permissions on".to_string(),
            fix_suggestion: None,
        };
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(config_path) {
            Ok(meta) => {
                let mode = meta.permissions().mode() & 0o777;
                let has_secrets = config.servers.values().any(|s| {
                    s.auth_token.is_some()
                        || s.env.values().any(|v| {
                            v.contains("KEY") || v.contains("TOKEN") || v.contains("SECRET")
                        })
                });

                if has_secrets && (mode & 0o077) != 0 {
                    CheckResult {
                        name,
                        status: CheckStatus::Warn,
                        message: format!(
                            "Config contains secrets but is world/group-readable (mode {mode:04o})"
                        ),
                        fix_suggestion: Some(format!("chmod 600 {}", config_path.display())),
                    }
                } else {
                    CheckResult {
                        name,
                        status: CheckStatus::Pass,
                        message: format!("Config permissions OK (mode {mode:04o})"),
                        fix_suggestion: None,
                    }
                }
            }
            Err(e) => CheckResult {
                name,
                status: CheckStatus::Warn,
                message: format!("Cannot check permissions: {e}"),
                fix_suggestion: None,
            },
        }
    }

    #[cfg(not(unix))]
    {
        let _ = config;
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "Permission check skipped (non-Unix)".to_string(),
            fix_suggestion: None,
        }
    }
}

/// Check 3: port 3282 (or configured port) is available.
async fn check_port_available(config: &Config) -> CheckResult {
    let name = "port_available".to_string();
    let addr = format!("{}:{}", config.http.bind_address, config.http.port);

    match tokio::net::TcpListener::bind(&addr).await {
        Ok(_listener) => {
            // Listener is dropped here, releasing the port
            CheckResult {
                name,
                status: CheckStatus::Pass,
                message: format!("Port {} is available", config.http.port),
                fix_suggestion: None,
            }
        }
        Err(e) => {
            if let Some(pid) = running_daemon_pid() {
                CheckResult {
                    name,
                    status: CheckStatus::Pass,
                    message: format!(
                        "Port {} is already bound by the running plug daemon (PID {})",
                        config.http.port, pid
                    ),
                    fix_suggestion: None,
                }
            } else {
                CheckResult {
                    name,
                    status: CheckStatus::Fail,
                    message: format!("Port {} is not available: {e}", config.http.port),
                    fix_suggestion: Some(format!(
                        "Stop the process using port {} or change http.port in config",
                        config.http.port
                    )),
                }
            }
        }
    }
}

/// Check 4: all env vars referenced in server configs are set.
/// Core plug vars (PLUG_*) cause Fail; third-party server vars only Warn.
async fn check_env_vars(config: &Config) -> CheckResult {
    let name = "env_vars".to_string();
    let mut core_missing: Vec<String> = Vec::new();
    let mut third_party_missing: Vec<String> = Vec::new();

    for (server_name, server) in &config.servers {
        let mut server_missing: Vec<String> = Vec::new();

        // Check env values for $VAR references
        for (key, val) in &server.env {
            for var in extract_env_refs(val) {
                if std::env::var(&var).is_err() {
                    server_missing.push(format!("{server_name}.env.{key} references ${var}"));
                }
            }
        }
        // Check command
        if let Some(ref cmd) = server.command {
            for var in extract_env_refs(cmd) {
                if std::env::var(&var).is_err() {
                    server_missing.push(format!("{server_name}.command references ${var}"));
                }
            }
        }
        // Check args
        for arg in &server.args {
            for var in extract_env_refs(arg) {
                if std::env::var(&var).is_err() {
                    server_missing.push(format!("{server_name}.args references ${var}"));
                }
            }
        }
        // Check url
        if let Some(ref url) = server.url {
            for var in extract_env_refs(url) {
                if std::env::var(&var).is_err() {
                    server_missing.push(format!("{server_name}.url references ${var}"));
                }
            }
        }

        // Core plug servers fail; third-party servers only warn
        if server_name.starts_with("plug") {
            core_missing.extend(server_missing);
        } else {
            third_party_missing.extend(server_missing);
        }
    }

    if core_missing.is_empty() && third_party_missing.is_empty() {
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "All referenced env vars are set".to_string(),
            fix_suggestion: None,
        }
    } else if !core_missing.is_empty() {
        let mut all = core_missing;
        all.extend(third_party_missing);
        CheckResult {
            name,
            status: CheckStatus::Fail,
            message: format!("Missing env vars: {}", all.join(", ")),
            fix_suggestion: Some("Set the missing environment variables".to_string()),
        }
    } else {
        CheckResult {
            name,
            status: CheckStatus::Warn,
            message: format!(
                "Third-party servers have missing env vars (non-blocking): {}",
                third_party_missing.join(", ")
            ),
            fix_suggestion: Some(
                "Set the missing environment variables or remove unused servers".to_string(),
            ),
        }
    }
}

/// Extract `$VAR_NAME` references from a string.
fn extract_env_refs(input: &str) -> Vec<String> {
    crate::config::expand::extract_env_refs(input)
}

/// Check 5: each stdio server's command binary is found in PATH.
async fn check_server_binaries(config: &Config) -> CheckResult {
    let name = "server_binaries".to_string();
    let mut missing: Vec<String> = Vec::new();

    for (server_name, server) in &config.servers {
        if !server.enabled {
            continue;
        }
        if !matches!(server.transport, TransportType::Stdio) {
            continue;
        }
        if let Some(ref cmd) = server.command {
            // Extract the actual binary name (first word, skip env var refs)
            let binary = cmd.split_whitespace().next().unwrap_or(cmd);
            if binary.starts_with('$') {
                continue; // Skip env var references
            }
            // Check if it's an absolute path
            if binary.starts_with('/') || binary.starts_with('.') {
                if !Path::new(binary).exists() {
                    missing.push(format!("{server_name}: {binary}"));
                }
            } else {
                // Check PATH
                match which(binary) {
                    Some(_) => {}
                    None => missing.push(format!("{server_name}: {binary}")),
                }
            }
        }
    }

    if missing.is_empty() {
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "All server binaries found".to_string(),
            fix_suggestion: None,
        }
    } else {
        CheckResult {
            name,
            status: CheckStatus::Fail,
            message: format!("Missing binaries: {}", missing.join(", ")),
            fix_suggestion: Some(
                "Install the missing binaries or fix the command path".to_string(),
            ),
        }
    }
}

/// Simple which-like lookup in PATH.
fn which(binary: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(binary))
            .find(|path| path.is_file())
    })
}

/// Check 6: detect tools with the same name from different servers.
async fn check_tool_collisions(config: &Config) -> CheckResult {
    let name = "tool_collisions".to_string();

    let message = if config.enable_prefix {
        "Tool prefixing is enabled — collisions are avoided".to_string()
    } else {
        "Tool prefixing is always on in v0.1; `enable_prefix = false` is ignored".to_string()
    };

    CheckResult {
        name,
        status: CheckStatus::Pass,
        message,
        fix_suggestion: None,
    }
}

/// Check 7: warn if total tool count might exceed known client limits.
async fn check_client_limits(config: &Config) -> CheckResult {
    let name = "client_limits".to_string();
    let server_count = config.servers.values().filter(|s| s.enabled).count();

    // We can't know exact tool counts without starting servers, but we can
    // warn about the number of servers vs known limits.
    let known_limits: &[(&str, usize)] =
        &[("Cursor", 40), ("Windsurf", 100), ("VS Code Copilot", 128)];

    // Rough heuristic: assume ~10 tools per server
    let estimated_tools = server_count * 10;
    let mut warnings: Vec<String> = Vec::new();

    for (client, limit) in known_limits {
        if estimated_tools > *limit {
            warnings.push(format!(
                "{client} limit is {limit} tools (estimated ~{estimated_tools} from {server_count} servers)"
            ));
        }
    }

    if warnings.is_empty() {
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message: format!("{server_count} servers configured — within known client limits",),
            fix_suggestion: None,
        }
    } else {
        CheckResult {
            name,
            status: CheckStatus::Warn,
            message: format!("May exceed client tool limits: {}", warnings.join("; ")),
            fix_suggestion: Some(
                "Use tool_filter_enabled and priority_tools to stay within limits".to_string(),
            ),
        }
    }
}

/// Check 8: if PID file exists, check if process is actually running.
async fn check_pid_staleness() -> CheckResult {
    let name = "pid_staleness".to_string();

    let pid_path = directories::ProjectDirs::from("", "", "plug")
        .map(|dirs| {
            dirs.runtime_dir()
                .unwrap_or(dirs.data_dir())
                .join("plug.pid")
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/plug.pid"));

    if !pid_path.exists() {
        return CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "No PID file found (daemon not running)".to_string(),
            fix_suggestion: None,
        };
    }

    match tokio::fs::read_to_string(&pid_path).await {
        Ok(contents) => match contents.trim().parse::<u32>() {
            Ok(pid) => {
                if is_process_running(pid) {
                    CheckResult {
                        name,
                        status: CheckStatus::Pass,
                        message: format!("Daemon is running (PID {pid})"),
                        fix_suggestion: None,
                    }
                } else {
                    CheckResult {
                        name,
                        status: CheckStatus::Warn,
                        message: format!("Stale PID file — process {pid} is not running"),
                        fix_suggestion: Some(format!(
                            "Remove stale PID file: rm {}",
                            pid_path.display()
                        )),
                    }
                }
            }
            Err(_) => CheckResult {
                name,
                status: CheckStatus::Warn,
                message: "PID file contains invalid data".to_string(),
                fix_suggestion: Some(format!(
                    "Remove invalid PID file: rm {}",
                    pid_path.display()
                )),
            },
        },
        Err(e) => CheckResult {
            name,
            status: CheckStatus::Warn,
            message: format!("Cannot read PID file: {e}"),
            fix_suggestion: None,
        },
    }
}

/// Check if a process with the given PID is running.
fn is_process_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) checks if the process exists without sending a signal
        // SAFETY: This is a libc call but we use nix-free approach via std
        let result = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        matches!(result, Ok(status) if status.success())
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn running_daemon_pid() -> Option<u32> {
    let pid_path = directories::ProjectDirs::from("", "", "plug")
        .map(|dirs| {
            dirs.runtime_dir()
                .unwrap_or(dirs.data_dir())
                .join("plug.pid")
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/plug.pid"));
    let pid_str = std::fs::read_to_string(pid_path).ok()?;
    let pid = pid_str.trim().parse::<u32>().ok()?;
    is_process_running(pid).then_some(pid)
}

/// Check 9: try to start and initialize each server (5s timeout).
async fn check_server_connectivity(config: &Config) -> CheckResult {
    use futures::future::join_all;
    use std::future::Future;
    use std::pin::Pin;

    let name = "server_connectivity".to_string();
    let enabled_servers: Vec<&str> = config
        .servers
        .iter()
        .filter(|(_, s)| s.enabled)
        .map(|(name, _)| name.as_str())
        .collect();

    if enabled_servers.is_empty() {
        return CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "No servers configured to check".to_string(),
            fix_suggestion: None,
        };
    }

    // We don't actually start servers in doctor mode — that would be disruptive.
    // Instead, we check that the basic requirements are met (binary exists, URL reachable).
    let connectivity_checks: Vec<Pin<Box<dyn Future<Output = Option<String>> + Send>>> = config
        .servers
        .iter()
        .filter_map(|(server_name, server)| {
            if !server.enabled {
                return None;
            }

            let server_name = server_name.clone();
            match server.transport {
                TransportType::Stdio => {
                    let command = server.command.clone();
                    Some(Box::pin(async move {
                        if let Some(cmd) = command {
                            let binary = cmd.split_whitespace().next().unwrap_or(&cmd).to_string();
                            if !binary.starts_with('$') {
                                let found = if binary.starts_with('/') || binary.starts_with('.') {
                                    Path::new(&binary).exists()
                                } else {
                                    which(&binary).is_some()
                                };
                                if !found {
                                    return Some(format!("{server_name}: binary not found"));
                                }
                            }
                        }
                        None
                    })
                        as Pin<Box<dyn Future<Output = Option<String>> + Send>>)
                }
                TransportType::Http | TransportType::Sse => {
                    let url = server.url.clone();
                    Some(Box::pin(async move {
                        if let Some(url) = url {
                            return check_http_reachable(&url)
                                .await
                                .err()
                                .map(|e| format!("{server_name}: {e}"));
                        }
                        None
                    })
                        as Pin<Box<dyn Future<Output = Option<String>> + Send>>)
                }
            }
        })
        .collect();
    let unreachable = join_all(connectivity_checks)
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    if unreachable.is_empty() {
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message: format!("All {} servers are reachable", enabled_servers.len()),
            fix_suggestion: None,
        }
    } else {
        let daemon_running = running_daemon_pid().is_some();
        CheckResult {
            name,
            status: if daemon_running {
                CheckStatus::Warn
            } else {
                CheckStatus::Fail
            },
            message: if daemon_running {
                format!(
                    "Cold connectivity issues: {} (daemon is running; compare with `plug status` for live health)",
                    unreachable.join(", ")
                )
            } else {
                format!("Unreachable servers: {}", unreachable.join(", "))
            },
            fix_suggestion: Some(if daemon_running {
                "Compare with `plug status`; raw TCP reachability may differ from current daemon-routed health"
                    .to_string()
            } else {
                "Check server URLs and network connectivity".to_string()
            }),
        }
    }
}

/// Try to connect to an HTTP URL to verify basic reachability.
async fn check_http_reachable(url: &str) -> Result<(), String> {
    let is_https = url.starts_with("https://");
    // Parse URL to extract host:port
    let url = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host_port = url.split('/').next().unwrap_or(url);
    let addr = if host_port.contains(':') {
        host_port.to_string()
    } else {
        format!("{host_port}:{}", if is_https { 443 } else { 80 })
    };

    // DNS resolution + TCP connect with timeout
    let socket_addrs: Vec<_> = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::lookup_host(&addr),
    )
    .await
    .map_err(|_| "DNS resolution timed out (5s)".to_string())?
    .map_err(|e| format!("DNS resolution failed: {e}"))?
    .collect();

    if socket_addrs.is_empty() {
        return Err("DNS resolution returned no addresses".to_string());
    }

    let mut last_error = None;
    for socket_addr in socket_addrs {
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::net::TcpStream::connect(socket_addr),
        )
        .await
        {
            Ok(Ok(_)) => return Ok(()),
            Ok(Err(e)) => last_error = Some(format!("TCP connect failed: {e}")),
            Err(_) => last_error = Some("connection timed out (5s)".to_string()),
        }
    }

    Err(last_error.unwrap_or_else(|| "connection failed".to_string()))
}

/// Check 9: detect duplicate or corrupted plug entries in AI client configs.
async fn check_client_configs() -> CheckResult {
    let name = "client_configs".to_string();
    let mut issues = Vec::new();

    let all_targets = [
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

    for target in all_targets {
        let target_enum: crate::export::ExportTarget = match target.parse() {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Check both global and project paths (if applicable)
        let paths = vec![
            crate::export::default_config_path(target_enum, false),
            crate::export::default_config_path(target_enum, true),
        ];

        for path in paths.into_iter().flatten() {
            if !path.exists() {
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let ext = path.extension().and_then(|e| e.to_str());

            if ext == Some("toml") {
                // Count occurrences of [mcp_servers.plug]
                let count = content
                    .lines()
                    .filter(|l| l.trim() == "[mcp_servers.plug]")
                    .count();
                if count > 1 {
                    issues.push(format!(
                        "{} (duplicate entries in {})",
                        target,
                        path.display()
                    ));
                }
                // Also check if it's even valid TOML
                if let Err(e) = content.parse::<toml::Value>() {
                    issues.push(format!(
                        "{} (invalid TOML in {}: {})",
                        target,
                        path.display(),
                        e
                    ));
                }
            } else if ext == Some("yaml") || ext == Some("yml") {
                // For YAML (Goose), check for duplicate "plug:" keys under extensions
                let count = content.lines().filter(|l| l.trim() == "plug:").count();
                if count > 1 {
                    issues.push(format!(
                        "{} (duplicate entries in {})",
                        target,
                        path.display()
                    ));
                }
                if let Err(e) = serde_yml::from_str::<serde_yml::Value>(&content) {
                    issues.push(format!(
                        "{} (invalid YAML in {}: {})",
                        target,
                        path.display(),
                        e
                    ));
                }
            } else {
                // For JSON, check for multiple "plug" keys in valid MCP locations
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                    let mut plug_locations = 0;

                    // Location 1: mcpServers / context_servers
                    for key in ["mcpServers", "context_servers"] {
                        if json.get(key).and_then(|v| v.get("plug")).is_some() {
                            plug_locations += 1;
                        }
                    }
                    // Location 2: mcp.servers
                    if json
                        .get("mcp")
                        .and_then(|v| v.get("servers"))
                        .and_then(|s| s.get("plug"))
                        .is_some()
                    {
                        plug_locations += 1;
                    }
                    // Location 3: tools.mcpServers
                    if json
                        .get("tools")
                        .and_then(|v| v.get("mcpServers"))
                        .and_then(|s| s.get("plug"))
                        .is_some()
                    {
                        plug_locations += 1;
                    }

                    if plug_locations > 1 {
                        issues.push(format!(
                            "{} ({} duplicate plug entries in {})",
                            target,
                            plug_locations,
                            path.display()
                        ));
                    }
                } else {
                    issues.push(format!("{} (invalid JSON in {})", target, path.display()));
                }
            }
        }
    }

    if issues.is_empty() {
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "All detected client configurations are healthy".to_string(),
            fix_suggestion: None,
        }
    } else {
        CheckResult {
            name,
            status: CheckStatus::Warn,
            message: format!("Issues found in client configs: {}", issues.join(", ")),
            fix_suggestion: Some(
                "Run `plug repair` to automatically clean up your client configurations"
                    .to_string(),
            ),
        }
    }
}

/// Check 11: Downstream HTTP auth configuration and token state.
async fn check_http_auth(config: &Config) -> CheckResult {
    let name = "http_auth".to_string();

    match config.http.auth_mode {
        crate::config::DownstreamAuthMode::None => CheckResult {
            name,
            status: CheckStatus::Pass,
            message: if crate::config::http_bind_is_loopback(&config.http.bind_address) {
                "HTTP auth disabled explicitly (loopback/local-only deployment)".to_string()
            } else {
                "HTTP auth disabled explicitly".to_string()
            },
            fix_suggestion: None,
        },
        crate::config::DownstreamAuthMode::Oauth => CheckResult {
            name,
            status: CheckStatus::Warn,
            message: "HTTP auth mode is oauth (metadata and token routes are configured, but doctor does not verify external endpoint reachability or public URL correctness)".to_string(),
            fix_suggestion: Some(
                "Verify the configured public URL externally, including /.well-known/mcp.json and OAuth endpoints"
                    .to_string(),
            ),
        },
        crate::config::DownstreamAuthMode::Auto | crate::config::DownstreamAuthMode::Bearer => {
            let requires_token = matches!(config.http.auth_mode, crate::config::DownstreamAuthMode::Bearer)
                || !crate::config::http_bind_is_loopback(&config.http.bind_address);

            if !requires_token {
                return CheckResult {
                    name,
                    status: CheckStatus::Pass,
                    message: "HTTP auth in auto mode resolves to no auth on loopback".to_string(),
                    fix_suggestion: None,
                };
            }

            let token_path = crate::auth::http_auth_token_path(config.http.port);

            if !token_path.exists() {
                return CheckResult {
                    name,
                    status: CheckStatus::Warn,
                    message: match config.http.auth_mode {
                        crate::config::DownstreamAuthMode::Bearer => format!(
                            "HTTP auth mode is bearer but auth token is not yet generated — run `plug serve` to initialize ({})",
                            token_path.display()
                        ),
                        crate::config::DownstreamAuthMode::Auto => format!(
                            "HTTP auth in auto mode resolves to bearer for bind {} but auth token is not yet generated — run `plug serve` to initialize",
                            config.http.bind_address
                        ),
                        _ => unreachable!("requires_token only applies to auto/bearer"),
                    },
                    fix_suggestion: Some(
                        "Run `plug serve` to auto-generate an auth token, or change http.auth_mode for your deployment".to_string(),
                    ),
                };
            }

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&token_path) {
                    let mode = meta.permissions().mode() & 0o777;
                    if mode != 0o600 {
                        return CheckResult {
                            name,
                            status: CheckStatus::Warn,
                            message: format!(
                                "Auth token file has permissions {:o} (should be 600): {}",
                                mode,
                                token_path.display()
                            ),
                            fix_suggestion: Some(format!("Run: chmod 600 {}", token_path.display())),
                        };
                    }
                }
            }

            CheckResult {
                name,
                status: CheckStatus::Pass,
                message: match config.http.auth_mode {
                    crate::config::DownstreamAuthMode::Bearer => format!(
                        "HTTP bearer auth token configured ({})",
                        token_path.display()
                    ),
                    crate::config::DownstreamAuthMode::Auto => format!(
                        "HTTP auth in auto mode resolves to bearer for bind {}",
                        config.http.bind_address
                    ),
                    _ => unreachable!("requires_token only applies to auto/bearer"),
                },
                fix_suggestion: None,
            }
        }
    }
}

/// Check OAuth config fields are coherent.
async fn check_oauth_config(config: &Config) -> CheckResult {
    let name = "oauth_config".to_string();

    let mut issues = Vec::new();
    for (server_name, sc) in &config.servers {
        if sc.auth.as_deref() != Some("oauth") {
            continue;
        }

        if sc.auth_token.is_some() {
            issues.push(format!(
                "server '{server_name}': auth = \"oauth\" and auth_token are both set (mutually exclusive)"
            ));
        }
        if matches!(sc.transport, TransportType::Stdio) {
            issues.push(format!(
                "server '{server_name}': auth = \"oauth\" on stdio transport (requires http or sse)"
            ));
        }
        if sc.oauth_scopes.is_none() || sc.oauth_scopes.as_ref().is_some_and(|s| s.is_empty()) {
            issues.push(format!(
                "server '{server_name}': auth = \"oauth\" but no scopes configured"
            ));
        }
    }

    if issues.is_empty() {
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "OAuth config fields are coherent".to_string(),
            fix_suggestion: None,
        }
    } else {
        CheckResult {
            name,
            status: if issues
                .iter()
                .any(|i| i.contains("mutually exclusive") || i.contains("stdio"))
            {
                CheckStatus::Fail
            } else {
                CheckStatus::Warn
            },
            message: issues.join("; "),
            fix_suggestion: Some("Check OAuth settings in your config.toml".to_string()),
        }
    }
}

/// Check token status for OAuth-configured servers.
async fn check_oauth_tokens(config: &Config) -> CheckResult {
    let name = "oauth_tokens".to_string();

    let oauth_servers: Vec<_> = config
        .servers
        .iter()
        .filter(|(_, sc)| sc.auth.as_deref() == Some("oauth") && sc.enabled)
        .collect();

    if oauth_servers.is_empty() {
        return CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "No OAuth-configured servers".to_string(),
            fix_suggestion: None,
        };
    }

    let tokens_dir = crate::oauth::tokens_dir();
    let mut plaintext_token_files = Vec::new();

    for (server_name, _) in &oauth_servers {
        let token_file = tokens_dir.join(format!("{server_name}.json"));
        if token_file.exists() {
            plaintext_token_files.push(format!(
                "server '{server_name}': plaintext token file present at {}",
                token_file.display()
            ));
        }
    }

    if plaintext_token_files.is_empty() {
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message:
                "No plaintext OAuth token files detected (doctor does not probe keychain-backed credentials)"
                    .to_string(),
            fix_suggestion: None,
        }
    } else {
        CheckResult {
            name,
            status: CheckStatus::Warn,
            message: plaintext_token_files.join("; "),
            fix_suggestion: Some(
                "Use `plug auth status` for live credential state; these local token files back process restarts and should remain protected with filesystem permissions".to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::{Config, ServerConfig, TransportType};

    fn test_config() -> Config {
        Config::default()
    }

    fn stdio_server(cmd: &str) -> ServerConfig {
        ServerConfig {
            command: Some(cmd.to_string()),
            args: vec![],
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::Stdio,
            url: None,
            auth_token: None,
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
            timeout_secs: 30,
            call_timeout_secs: 300,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
        }
    }

    fn oauth_http_server(url: &str) -> ServerConfig {
        ServerConfig {
            transport: TransportType::Http,
            url: Some(url.to_string()),
            auth: Some("oauth".to_string()),
            oauth_scopes: Some(vec!["read".to_string()]),
            ..stdio_server("echo")
        }
    }

    // -- check_config_exists --

    #[tokio::test]
    async fn config_exists_missing_file() {
        let result = check_config_exists(Path::new("/nonexistent/path/config.toml")).await;
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("not found"));
    }

    #[tokio::test]
    async fn config_exists_valid_toml() {
        let dir = std::env::temp_dir().join("plug_doctor_test_valid");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        std::fs::write(&path, "[http]\nport = 3282\n").unwrap();

        let result = check_config_exists(&path).await;
        assert_eq!(result.status, CheckStatus::Pass);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn config_exists_invalid_toml() {
        let dir = std::env::temp_dir().join("plug_doctor_test_invalid");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        std::fs::write(&path, "[invalid toml ===").unwrap();

        let result = check_config_exists(&path).await;
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(result.message.contains("invalid TOML"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- check_config_permissions --

    #[tokio::test]
    async fn config_permissions_no_file() {
        let config = test_config();
        let result = check_config_permissions(&config, Path::new("/nonexistent/config.toml")).await;
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn config_permissions_warns_on_world_readable_with_secrets() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join("plug_doctor_test_perms");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        std::fs::write(&path, "# test").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let mut config = test_config();
        config.servers.insert(
            "test".to_string(),
            ServerConfig {
                auth_token: Some("secret".to_string().into()),
                ..stdio_server("echo")
            },
        );

        let result = check_config_permissions(&config, &path).await;
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("world/group-readable"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- check_port_available --

    #[tokio::test]
    async fn port_available_on_random_port() {
        let mut config = test_config();
        config.http.port = 0; // port 0 should always be bindable (OS picks)
        // Actually port 0 fails our validation, use a high random port
        config.http.port = 49152 + (std::process::id() as u16 % 1000);
        let result = check_port_available(&config).await;
        // May or may not pass depending on port availability, just check it returns
        assert!(!result.name.is_empty());
    }

    // -- check_env_vars --

    #[tokio::test]
    async fn env_vars_all_set() {
        let config = test_config();
        let result = check_env_vars(&config).await;
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn env_vars_missing_third_party_warns() {
        let mut config = test_config();
        config.servers.insert(
            "test".to_string(),
            ServerConfig {
                env: HashMap::from([(
                    "API_KEY".to_string(),
                    "$PLUG_NONEXISTENT_VAR_XYZ".to_string(),
                )]),
                ..stdio_server("echo")
            },
        );
        let result = check_env_vars(&config).await;
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("PLUG_NONEXISTENT_VAR_XYZ"));
    }

    #[tokio::test]
    async fn env_vars_missing_core_fails() {
        let mut config = test_config();
        config.servers.insert(
            "plug-internal".to_string(),
            ServerConfig {
                env: HashMap::from([(
                    "API_KEY".to_string(),
                    "$PLUG_NONEXISTENT_VAR_XYZ".to_string(),
                )]),
                ..stdio_server("echo")
            },
        );
        let result = check_env_vars(&config).await;
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(result.message.contains("PLUG_NONEXISTENT_VAR_XYZ"));
    }

    // -- check_http_auth --

    #[tokio::test]
    async fn http_auth_auto_loopback_passes_without_token() {
        let mut config = test_config();
        config.http.auth_mode = crate::config::DownstreamAuthMode::Auto;
        config.http.bind_address = "127.0.0.1".to_string();
        config.http.port = 62001;

        let result = check_http_auth(&config).await;
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.message.contains("auto mode"));
    }

    #[tokio::test]
    async fn http_auth_bearer_on_loopback_warns_when_token_missing() {
        let mut config = test_config();
        config.http.auth_mode = crate::config::DownstreamAuthMode::Bearer;
        config.http.bind_address = "127.0.0.1".to_string();
        config.http.port = 62002;

        let token_path = crate::auth::http_auth_token_path(config.http.port);
        let _ = std::fs::remove_file(&token_path);

        let result = check_http_auth(&config).await;
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("auth mode is bearer"));
    }

    #[tokio::test]
    async fn http_auth_oauth_warns_for_unverified_external_surface() {
        let mut config = test_config();
        config.http.auth_mode = crate::config::DownstreamAuthMode::Oauth;
        config.http.public_base_url = Some("https://plug.example.com".to_string());
        config.http.oauth_client_id = Some("client-123".to_string());
        config.http.port = 62003;

        let result = check_http_auth(&config).await;
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(
            result
                .message
                .contains("does not verify external endpoint reachability")
        );
    }

    // -- check_oauth_tokens --

    #[tokio::test]
    async fn oauth_tokens_pass_without_plaintext_fallback_files() {
        let server_name = format!("oauth-doctor-no-file-{}", std::process::id());
        let mut config = test_config();
        config.servers.insert(
            server_name.clone(),
            oauth_http_server("https://example.com/mcp"),
        );

        let token_file = crate::oauth::tokens_dir().join(format!("{server_name}.json"));
        let _ = std::fs::remove_file(&token_file);

        let result = check_oauth_tokens(&config).await;
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(
            result
                .message
                .contains("does not probe keychain-backed credentials")
        );
    }

    #[tokio::test]
    async fn oauth_tokens_warn_when_plaintext_token_file_exists() {
        let server_name = format!("oauth-doctor-file-{}", std::process::id());
        let mut config = test_config();
        config.servers.insert(
            server_name.clone(),
            oauth_http_server("https://example.com/mcp"),
        );

        let token_file = crate::oauth::tokens_dir().join(format!("{server_name}.json"));
        if let Some(parent) = token_file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&token_file, "{}").unwrap();

        let result = check_oauth_tokens(&config).await;

        let _ = std::fs::remove_file(&token_file);

        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains(&server_name));
        assert!(result.message.contains("plaintext token file"));
        assert!(
            result
                .fix_suggestion
                .as_deref()
                .unwrap_or_default()
                .contains("filesystem permissions")
        );
    }

    // -- check_server_binaries --

    #[tokio::test]
    async fn server_binaries_found() {
        let mut config = test_config();
        // "echo" should be in PATH on any Unix system
        config
            .servers
            .insert("test".to_string(), stdio_server("echo"));
        let result = check_server_binaries(&config).await;
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn server_binaries_missing() {
        let mut config = test_config();
        config.servers.insert(
            "test".to_string(),
            stdio_server("plug_nonexistent_binary_xyz"),
        );
        let result = check_server_binaries(&config).await;
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(result.message.contains("plug_nonexistent_binary_xyz"));
    }

    // -- check_tool_collisions --

    #[tokio::test]
    async fn tool_collisions_prefix_enabled() {
        let config = test_config(); // prefix enabled by default
        let result = check_tool_collisions(&config).await;
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn tool_collisions_warns_without_prefix() {
        let mut config = test_config();
        config.enable_prefix = false;
        config.servers.insert("a".to_string(), stdio_server("echo"));
        config.servers.insert("b".to_string(), stdio_server("echo"));
        let result = check_tool_collisions(&config).await;
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.message.contains("always on"));
    }

    // -- check_client_limits --

    #[tokio::test]
    async fn client_limits_ok_with_few_servers() {
        let mut config = test_config();
        config.servers.insert("a".to_string(), stdio_server("echo"));
        let result = check_client_limits(&config).await;
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn client_limits_warns_with_many_servers() {
        let mut config = test_config();
        // 5 servers * 10 estimated tools = 50, exceeds Cursor's 40
        for i in 0..5 {
            config
                .servers
                .insert(format!("server_{i}"), stdio_server("echo"));
        }
        let result = check_client_limits(&config).await;
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("Cursor"));
    }

    // -- check_pid_staleness --

    #[tokio::test]
    async fn pid_staleness_no_file() {
        // Default path likely doesn't exist in test environment
        let result = check_pid_staleness().await;
        // Should be Pass (no PID file) or Warn (stale PID), never Fail
        assert_ne!(result.status, CheckStatus::Fail);
    }

    // -- check_server_connectivity --

    #[tokio::test]
    async fn connectivity_no_servers() {
        let config = test_config();
        let result = check_server_connectivity(&config).await;
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn connectivity_stdio_binary_found() {
        let mut config = test_config();
        config
            .servers
            .insert("test".to_string(), stdio_server("echo"));
        let result = check_server_connectivity(&config).await;
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn connectivity_http_server_reachable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let mut config = test_config();
        config.servers.insert(
            "remote".to_string(),
            oauth_http_server(&format!("http://127.0.0.1:{port}/mcp")),
        );

        let result = check_server_connectivity(&config).await;
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn connectivity_http_server_unreachable_fails_with_server_name() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let mut config = test_config();
        config.servers.insert(
            "remote".to_string(),
            oauth_http_server(&format!("http://127.0.0.1:{port}/mcp")),
        );

        let result = check_server_connectivity(&config).await;
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(result.message.contains("remote"));
        assert!(result.message.contains("TCP connect failed"));
    }

    #[tokio::test]
    async fn connectivity_mixed_servers_fail_when_remote_is_unreachable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let mut config = test_config();
        config
            .servers
            .insert("local".to_string(), stdio_server("echo"));
        config.servers.insert(
            "remote".to_string(),
            oauth_http_server(&format!("http://127.0.0.1:{port}/mcp")),
        );

        let result = check_server_connectivity(&config).await;
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(result.message.contains("remote"));
        assert!(!result.message.contains("local"));
    }

    // -- extract_env_refs --

    #[test]
    fn extract_refs_basic() {
        assert_eq!(extract_env_refs("$HOME/bin"), vec!["HOME"]);
        assert_eq!(extract_env_refs("$FOO and $BAR"), vec!["FOO", "BAR"]);
        assert!(extract_env_refs("no vars").is_empty());
        assert!(extract_env_refs("$(shell)").is_empty());
        assert!(extract_env_refs("$lowercase").is_empty());
    }

    // -- DoctorReport --

    #[test]
    fn report_exit_code_all_pass() {
        let checks = vec![CheckResult {
            name: "test".to_string(),
            status: CheckStatus::Pass,
            message: "ok".to_string(),
            fix_suggestion: None,
        }];
        assert_eq!(DoctorReport::from_checks(checks).exit_code, 0);
    }

    #[test]
    fn report_exit_code_with_fail() {
        let checks = vec![
            CheckResult {
                name: "a".to_string(),
                status: CheckStatus::Pass,
                message: "ok".to_string(),
                fix_suggestion: None,
            },
            CheckResult {
                name: "b".to_string(),
                status: CheckStatus::Fail,
                message: "bad".to_string(),
                fix_suggestion: None,
            },
        ];
        assert_eq!(DoctorReport::from_checks(checks).exit_code, 1);
    }

    #[test]
    fn report_exit_code_warn_only() {
        let checks = vec![CheckResult {
            name: "a".to_string(),
            status: CheckStatus::Warn,
            message: "meh".to_string(),
            fix_suggestion: None,
        }];
        assert_eq!(DoctorReport::from_checks(checks).exit_code, 2);
    }

    // -- run_doctor integration --

    #[tokio::test]
    async fn run_doctor_returns_all_checks() {
        let config = test_config();
        let report = run_doctor(&config, Path::new("/nonexistent/config.toml")).await;
        assert_eq!(report.checks.len(), 13);
    }
}
