use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde::Serialize;

use crate::config::{Config, TransportType};

/// Every check's identifier. The CLI adds the checks that need a running
/// Plug, so it names them from here, where their titles are.
pub mod check {
    pub const CONFIG_EXISTS: &str = "config_exists";
    pub const CONFIG_PERMISSIONS: &str = "config_permissions";
    pub const KEYS_IN_CONFIG: &str = "keys_in_config";
    pub const PORT_AVAILABLE: &str = "port_available";
    pub const ENV_VARS: &str = "env_vars";
    pub const SERVER_BINARIES: &str = "server_binaries";
    pub const CLIENT_LIMITS: &str = "client_limits";
    pub const PID_STALENESS: &str = "pid_staleness";
    pub const RUNTIME_HEALTH: &str = "runtime_health";
    pub const CLIENT_CONFIGS: &str = "client_configs";
    pub const SERVER_CONNECTIVITY: &str = "server_connectivity";
    pub const HTTP_AUTH: &str = "http_auth";
    pub const DOWNSTREAM_OAUTH_OWNER: &str = "downstream_oauth_owner";
    pub const DOWNSTREAM_OAUTH_CLIENTS: &str = "downstream_oauth_clients";
    pub const OAUTH_CONFIG: &str = "oauth_config";
    pub const OAUTH_TOKENS: &str = "oauth_tokens";
    pub const AUTH_AVAILABILITY: &str = "auth_availability";
    pub const RUNTIME_AUTH_MISSING: &str = "runtime_auth_missing";
    pub const RUNTIME_AUTH_REAUTH: &str = "runtime_auth_reauth";
    pub const RUNTIME_AUTH_DEGRADED: &str = "runtime_auth_degraded";
    pub const RUNTIME_DEGRADED: &str = "runtime_degraded";
    pub const RUNTIME_FAILURES: &str = "runtime_failures";
    pub const RUNTIME_AVAILABILITY: &str = "runtime_availability";
    pub const CODESIGN_IDENTITY: &str = "codesign_identity";
    pub const UNIFIED_INSTALL: &str = "unified_install";
    pub const DOCTOR_INTERPRETATION: &str = "doctor_interpretation";
}

/// Status of a single diagnostic check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

/// Result of a single diagnostic check.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    pub fix_suggestion: Option<String>,
}

impl CheckResult {
    /// What the check is called where a person reads it. `name` is an
    /// identifier (`config_permissions`); the CLI and the app both show this.
    pub fn title(&self) -> String {
        let title = match self.name.as_str() {
            check::CONFIG_EXISTS => "Settings file",
            check::CONFIG_PERMISSIONS => "Settings file is private",
            check::KEYS_IN_CONFIG => "Keys kept in the Keychain",
            check::PORT_AVAILABLE => "Network port",
            check::ENV_VARS => "Server keys",
            check::SERVER_BINARIES => "Server programs",
            check::CLIENT_LIMITS => "Tools a client can take",
            check::PID_STALENESS | check::RUNTIME_HEALTH => "Background service",
            check::CLIENT_CONFIGS => "Client setup",
            check::SERVER_CONNECTIVITY => "Servers responding",
            check::HTTP_AUTH | check::DOWNSTREAM_OAUTH_OWNER => "Remote access",
            check::DOWNSTREAM_OAUTH_CLIENTS => "Remote clients",
            check::OAUTH_CONFIG | check::OAUTH_TOKENS | check::AUTH_AVAILABILITY => {
                "Stored sign-ins"
            }
            check::RUNTIME_AUTH_MISSING
            | check::RUNTIME_AUTH_REAUTH
            | check::RUNTIME_AUTH_DEGRADED => "Server sign-ins",
            check::RUNTIME_DEGRADED | check::RUNTIME_FAILURES | check::RUNTIME_AVAILABILITY => {
                "Servers"
            }
            check::CODESIGN_IDENTITY => "App signature",
            check::UNIFIED_INSTALL => "Installation",
            check::DOCTOR_INTERPRETATION => "Summary",
            other => return other.replace('_', " "),
        };
        title.to_string()
    }
}

impl Serialize for CheckResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut check = serializer.serialize_struct("CheckResult", 5)?;
        check.serialize_field("name", &self.name)?;
        check.serialize_field("title", &self.title())?;
        check.serialize_field("status", &self.status)?;
        check.serialize_field("message", &self.message)?;
        check.serialize_field("fix_suggestion", &self.fix_suggestion)?;
        check.end()
    }
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
    let (
        config_exists,
        config_perms,
        port,
        env_vars,
        binaries,
        connectivity,
        pid,
        clients,
        http_auth,
        downstream_oauth_owner,
        oauth_config,
        oauth_tokens,
        codesign,
    ) = tokio::join!(
        check_config_exists(config_path),
        check_config_permissions(config_path),
        check_port_available(config),
        check_env_vars(config, config_path),
        check_server_binaries(config),
        check_server_connectivity(config),
        check_pid_staleness(),
        check_client_configs(&config.http),
        check_http_auth(config),
        check_downstream_oauth_owner(config),
        check_oauth_config(config),
        check_oauth_tokens(config),
        check_codesign_identity(config),
    );

    let checks = vec![
        config_exists,
        config_perms,
        check_keys_in_config(config_path),
        port,
        env_vars,
        binaries,
        check_client_limits(&config.http),
        pid,
        clients,
        connectivity,
        http_auth,
        downstream_oauth_owner,
        oauth_config,
        oauth_tokens,
        codesign,
    ];

    DoctorReport::from_checks(checks)
}

async fn check_downstream_oauth_owner(config: &Config) -> CheckResult {
    let state_dir = crate::paths::downstream_oauth_dir();
    check_downstream_oauth_owner_with_state_dir(config, &state_dir).await
}

async fn check_downstream_oauth_owner_with_state_dir(
    config: &Config,
    state_dir: &Path,
) -> CheckResult {
    let name = check::DOWNSTREAM_OAUTH_OWNER.to_string();
    let Some(oauth) =
        crate::downstream_oauth::DownstreamOauthConfig::from_http_config(&config.http)
    else {
        return CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "Downstream OAuth owner approval is not required".to_string(),
            fix_suggestion: None,
        };
    };

    match crate::downstream_oauth::inspect_owner_enrollment_in_dir(&oauth, state_dir) {
        crate::downstream_oauth::OwnerEnrollmentStatus::NotEnrolled => CheckResult {
            name,
            status: CheckStatus::Fail,
            message:
                "Downstream OAuth owner credential missing. Run `plug auth owner enroll`."
                    .to_string(),
            fix_suggestion: None,
        },
        crate::downstream_oauth::OwnerEnrollmentStatus::Enrolled { credential_count } => {
            CheckResult {
                name,
                status: CheckStatus::Pass,
                message: format!(
                    "Downstream OAuth owner approval is ready ({credential_count} enrolled credentials)"
                ),
                fix_suggestion: None,
            }
        }
        crate::downstream_oauth::OwnerEnrollmentStatus::UnsafePermissions => {
            let path = crate::downstream_oauth::owner_enrollment_state_path_in_dir(&oauth, state_dir);
            CheckResult {
                name,
                status: CheckStatus::Fail,
                message: "Downstream OAuth owner state is visible to other local users. Plug will not treat owner approval as ready."
                    .to_string(),
                fix_suggestion: Some(format!("Run: chmod 600 {}", path.display())),
            }
        }
        crate::downstream_oauth::OwnerEnrollmentStatus::StateUnavailable => CheckResult {
            name,
            status: CheckStatus::Fail,
            message: "Downstream OAuth owner state could not be read safely. No new access can be approved."
                .to_string(),
            fix_suggestion: Some(
                "Run `plug auth owner list`. If it fails, stop Plug and restore the downstream OAuth state from a trusted backup."
                    .to_string(),
            ),
        },
    }
}

/// Whether the config file exists and is valid TOML.
async fn check_config_exists(config_path: &Path) -> CheckResult {
    let name = check::CONFIG_EXISTS.to_string();

    if !config_path.exists() {
        return CheckResult {
            name,
            status: CheckStatus::Warn,
            message: format!("Config file not found: {}", config_path.display()),
            fix_suggestion: Some("Run `plug setup` to create and link a config".to_string()),
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
    toml::from_str::<toml::Table>(contents)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Keys written in the config file, as `<server>.<field>`. Reads the file as
/// written: after `$VAR` expansion every value looks typed.
fn keys_written(config_path: &Path) -> Vec<String> {
    let mut found: Vec<String> = crate::config::load_editable_config(config_path)
        .map(|config| {
            let stores = crate::secrets::Stores::from_config(&config.secrets);
            config
                .servers
                .iter()
                .flat_map(|(server, config)| {
                    stores
                        .plaintext(config)
                        .into_iter()
                        .map(move |field| format!("{server}.{field}"))
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// Keys written in the config file, which the Keychain would keep better.
fn check_keys_in_config(config_path: &Path) -> CheckResult {
    let name = check::KEYS_IN_CONFIG.to_string();
    let found = keys_written(config_path);
    if found.is_empty() {
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "No keys are written in the config file".to_string(),
            fix_suggestion: None,
        }
    } else {
        CheckResult {
            name,
            status: CheckStatus::Warn,
            message: format!("Keys written in the config file: {}", found.join(", ")),
            fix_suggestion: Some("Run `plug secret move` to keep them in the Keychain".to_string()),
        }
    }
}

/// Whether a config file that holds keys is private.
async fn check_config_permissions(config_path: &Path) -> CheckResult {
    let name = check::CONFIG_PERMISSIONS.to_string();

    if !config_path.exists() {
        return CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "No config file to check permissions on".to_string(),
            fix_suggestion: None,
        };
    }

    {
        match std::fs::metadata(config_path) {
            Ok(meta) => {
                let mode = meta.permissions().mode() & 0o777;
                let has_secrets = !keys_written(config_path).is_empty();

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
}

/// Whether the configured HTTP port is free, or held by Plug itself.
async fn check_port_available(config: &Config) -> CheckResult {
    let name = check::PORT_AVAILABLE.to_string();
    let addr = format!("{}:{}", config.http.bind_address, config.http.port);

    match tokio::net::TcpListener::bind(&addr).await {
        Ok(_listener) => CheckResult {
            name,
            status: CheckStatus::Pass,
            message: format!("Port {} is available", config.http.port),
            fix_suggestion: None,
        },
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
                        "Stop the other program using port {} or change http.port in config",
                        config.http.port
                    )),
                }
            }
        }
    }
}

/// Whether every `$NAME` the config uses is set. One in Plug's own settings
/// fails, since Plug would not start with it; one in a server only warns.
async fn check_env_vars(config: &Config, config_path: &Path) -> CheckResult {
    let name = check::ENV_VARS.to_string();
    let mut core_missing: Vec<String> = Vec::new();
    let mut third_party_missing: Vec<String> = Vec::new();
    let vars = crate::config::env::env_source(config_path);

    crate::config::for_each_expandable(&mut config.clone(), |server_name, field, value| {
        for var in crate::config::env::env_refs(value) {
            if vars.contains_key(&var) {
                continue;
            }
            match server_name {
                Some(server_name) => {
                    third_party_missing.push(format!("{server_name}.{field} references ${var}"))
                }
                None => core_missing.push(format!("config.{field} references ${var}")),
            }
        }
    });

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

/// Whether each stdio server's command is found in PATH.
async fn check_server_binaries(config: &Config) -> CheckResult {
    let login_path = crate::server::path::stdio_login_path().await;
    server_binaries_result(
        config,
        |command| {
            let path = crate::server::path::resolve_stdio_command(command, login_path);
            path.is_file().then_some(path)
        },
        crate::server::path::resolve_without_login_shell,
    )
}

/// `resolve` finds a server's whole `command` the way the daemon does.
/// `resolve_fallback` finds a bare name the way the daemon does when its
/// login-shell PATH probe fails, which happens after a reboot on a slow login.
/// A name only the first finds is a server that will not start after some
/// reboots, so it earns a warning.
fn server_binaries_result(
    config: &Config,
    resolve: impl Fn(&str) -> Option<std::path::PathBuf>,
    resolve_fallback: impl Fn(&str) -> Option<std::path::PathBuf>,
) -> CheckResult {
    let name = check::SERVER_BINARIES.to_string();
    let mut missing: Vec<String> = Vec::new();
    let mut login_shell_only: Vec<String> = Vec::new();
    let check_fallback = cfg!(target_os = "macos");

    let mut servers: Vec<_> = config.servers.iter().collect();
    servers.sort_by(|a, b| a.0.cmp(b.0));
    for (server_name, server) in servers {
        if !server.enabled {
            continue;
        }
        if !matches!(server.transport, TransportType::Stdio) {
            continue;
        }
        let Some(command) = server.command.as_deref() else {
            continue;
        };
        // An unset `$NAME` is the env var check's to report.
        if command.starts_with('$') {
            continue;
        }
        match resolve(command) {
            None => missing.push(format!("{server_name}: {command}")),
            Some(found)
                if check_fallback
                    && crate::server::path::is_bare_name(command)
                    && resolve_fallback(command).is_none() =>
            {
                login_shell_only.push(format!("{server_name}: {command} ({})", found.display()));
            }
            Some(_) => {}
        }
    }

    if !missing.is_empty() {
        CheckResult {
            name,
            status: CheckStatus::Fail,
            message: format!("Missing binaries: {}", missing.join(", ")),
            fix_suggestion: Some(
                "Install the missing binaries or fix the command path".to_string(),
            ),
        }
    } else if !login_shell_only.is_empty() {
        CheckResult {
            name,
            status: CheckStatus::Warn,
            message: format!(
                "Found only through your login shell PATH: {}. If the daemon cannot read that PATH at boot, these servers fail to start",
                login_shell_only.join(", ")
            ),
            fix_suggestion: Some("Use the absolute path shown as the server's command".to_string()),
        }
    } else {
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "All server binaries found".to_string(),
            fix_suggestion: None,
        }
    }
}

/// Each settings file of `target` that exists, with its scope and contents.
fn client_files(
    target: crate::clients::ClientTarget,
) -> Vec<(crate::clients::Scope, std::path::PathBuf, String)> {
    [crate::clients::Scope::User, crate::clients::Scope::Project]
        .into_iter()
        .filter_map(|scope| Some((scope, target.config_path(scope)?)))
        .filter_map(|(scope, path)| {
            let content = std::fs::read_to_string(&path).ok()?;
            Some((scope, path, content))
        })
        .collect()
}

/// Whether `target`'s own config points at plug, globally or per project.
fn client_is_linked_to_plug(
    target: crate::clients::ClientTarget,
    plug: &crate::config::HttpConfig,
) -> bool {
    client_files(target)
        .iter()
        .any(|(_, _, content)| target.layout().plug_entry_count(content, plug) > 0)
}

/// Published tool ceilings of the clients actually using plug.
///
/// Doctor never starts a server, so it cannot know how many tools plug merges.
/// This check used to multiply the enabled-server count by an assumed ten tools
/// each and warn when that product cleared a ceiling, which is wrong in both
/// directions: thirteen small servers invented 130 tools and warned, while four
/// large ones hid a real 600. The one thing doctor can read from disk is which
/// clients are pointed at plug, so that is what it reports. The real total is
/// `plug status`, which counts tools the servers actually returned.
fn check_client_limits(plug: &crate::config::HttpConfig) -> CheckResult {
    let linked = crate::clients::TOOL_LIMITS
        .iter()
        .filter(|entry| {
            entry
                .linked_by
                .is_some_and(|target| client_is_linked_to_plug(target, plug))
        })
        .collect();
    client_limits_result(check::CLIENT_LIMITS.to_string(), linked)
}

fn client_limits_result(
    name: String,
    linked: Vec<&crate::clients::ClientToolLimit>,
) -> CheckResult {
    if linked.is_empty() {
        return CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "No linked client publishes a tool-count ceiling".to_string(),
            fix_suggestion: None,
        };
    }

    let ceilings: Vec<String> = linked
        .iter()
        .map(|entry| {
            // Printing the source alongside the number is the whole
            // anti-rot mechanism: a reader who doubts the figure can check it
            // without going and finding this table first.
            format!(
                "{} caps at {} tools (verified {}, {})",
                entry.client, entry.limit, entry.verified, entry.source
            )
        })
        .collect();

    CheckResult {
        name,
        status: CheckStatus::Warn,
        message: format!("Linked clients with a tool ceiling: {}", ceilings.join("; ")),
        fix_suggestion: Some(
            "Compare against the tool total from `plug status`, and use tool_filter_enabled and priority_tools if a linked client needs fewer".to_string(),
        ),
    }
}

/// Whether the process a PID file names is running.
async fn check_pid_staleness() -> CheckResult {
    let name = check::PID_STALENESS.to_string();

    let pid_path = crate::paths::pid_path();

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

fn is_process_running(pid: u32) -> bool {
    let result = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    matches!(result, Ok(status) if status.success())
}

fn running_daemon_pid() -> Option<u32> {
    let pid_path = crate::paths::pid_path();
    let pid_str = std::fs::read_to_string(pid_path).ok()?;
    let pid = pid_str.trim().parse::<u32>().ok()?;
    is_process_running(pid).then_some(pid)
}

/// Whether each enabled remote server's URL accepts a connection.
async fn check_server_connectivity(config: &Config) -> CheckResult {
    use futures::future::join_all;

    let name = check::SERVER_CONNECTIVITY.to_string();
    // Stdio servers are covered by `server_binaries`; reporting a missing
    // binary here too made one problem show up as two.
    let remote_servers: Vec<(&str, &str)> = config
        .servers
        .iter()
        .filter(|(_, s)| {
            s.enabled && matches!(s.transport, TransportType::Http | TransportType::Sse)
        })
        .filter_map(|(name, s)| Some((name.as_str(), s.url.as_deref()?)))
        .collect();

    if remote_servers.is_empty() {
        return CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "No remote servers configured to check".to_string(),
            fix_suggestion: None,
        };
    }

    let connectivity_checks = remote_servers.iter().map(|(server_name, url)| async move {
        check_http_reachable(url)
            .await
            .err()
            .map(|e| format!("{server_name}: {e}"))
    });
    let unreachable = join_all(connectivity_checks)
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    if unreachable.is_empty() {
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message: format!("All {} remote servers are reachable", remote_servers.len()),
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

/// Duplicate or unreadable plug entries in client configs.
async fn check_client_configs(plug: &crate::config::HttpConfig) -> CheckResult {
    let mut issues = Vec::new();
    for target in crate::clients::ClientTarget::all() {
        for (scope, path, content) in client_files(target) {
            issues.extend(client_file_issues(target, scope, &path, &content, plug));
        }
    }
    client_configs_result(&issues)
}

/// One thing wrong with a client's settings file.
#[derive(Debug, PartialEq, Eq)]
enum ClientFileIssue {
    /// More than one Plug entry, which `plug link` leaves as one.
    Duplicates(String),
    /// A file that does not parse, which only a person can fix.
    Unreadable(String),
}

impl ClientFileIssue {
    fn text(&self) -> &str {
        match self {
            Self::Duplicates(text) | Self::Unreadable(text) => text,
        }
    }
}

fn client_configs_result(issues: &[ClientFileIssue]) -> CheckResult {
    let name = check::CLIENT_CONFIGS.to_string();
    if issues.is_empty() {
        return CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "All detected client configurations are healthy".to_string(),
            fix_suggestion: None,
        };
    }
    let texts: Vec<&str> = issues.iter().map(ClientFileIssue::text).collect();
    let fix = if issues
        .iter()
        .any(|issue| matches!(issue, ClientFileIssue::Unreadable(_)))
    {
        "Fix a file that does not parse, then run `plug link <client>` again to leave one Plug entry"
    } else {
        "Run `plug link <client>` again to leave one Plug entry"
    };
    CheckResult {
        name,
        status: CheckStatus::Warn,
        message: format!("Issues found in client configs: {}", texts.join(", ")),
        fix_suggestion: Some(fix.to_string()),
    }
}

/// What is wrong with one of `target`'s settings files: more than one Plug
/// entry in the user file, the only one `plug link` rewrites, or a file that
/// does not parse.
fn client_file_issues(
    target: crate::clients::ClientTarget,
    scope: crate::clients::Scope,
    path: &std::path::Path,
    content: &str,
    plug: &crate::config::HttpConfig,
) -> Vec<ClientFileIssue> {
    let layout = target.layout();
    let mut issues = Vec::new();
    let duplicates =
        scope == crate::clients::Scope::User && layout.plug_entry_count(content, plug) > 1;
    if duplicates {
        issues.push(ClientFileIssue::Duplicates(format!(
            "{} (duplicate entries in {})",
            target.slug(),
            path.display()
        )));
    }
    // Two of Plug's entries make a TOML or YAML file unparseable. Where
    // `plug link` clears them, the rest of the file is what is checked.
    let checked = if duplicates {
        layout.without_plug_entries(content)
    } else {
        content.into()
    };
    if let Err(error) = layout.check_syntax(&checked) {
        issues.push(ClientFileIssue::Unreadable(format!(
            "{} ({error} in {})",
            target.slug(),
            path.display()
        )));
    }
    issues
}

/// Downstream HTTP auth configuration and token state.
async fn check_http_auth(config: &Config) -> CheckResult {
    let name = check::HTTP_AUTH.to_string();

    match config.http.auth_mode {
        crate::config::DownstreamAuthMode::None => CheckResult {
            name,
            status: CheckStatus::Pass,
            message: if crate::config::is_loopback_host(&config.http.bind_address) {
                "HTTP auth disabled explicitly (loopback/local-only deployment)".to_string()
            } else {
                "HTTP auth disabled explicitly".to_string()
            },
            fix_suggestion: None,
        },
        crate::config::DownstreamAuthMode::Oauth => {
            let Some(base_url) = config.http.public_base_url.as_deref() else {
                return CheckResult {
                    name,
                    status: CheckStatus::Warn,
                    message: "HTTP auth mode is oauth but http.public_base_url is not set, so remote clients cannot discover the OAuth endpoints".to_string(),
                    fix_suggestion: Some(
                        "Set http.public_base_url to the HTTPS address remote clients use".to_string(),
                    ),
                };
            };
            match check_oauth_metadata_reachable(base_url).await {
                Ok(()) => CheckResult {
                    name,
                    status: CheckStatus::Pass,
                    message: format!("HTTP auth mode is oauth; {base_url} serves OAuth metadata"),
                    fix_suggestion: None,
                },
                Err(error) => CheckResult {
                    name,
                    status: CheckStatus::Warn,
                    message: format!(
                        "HTTP auth mode is oauth, but {base_url} did not serve OAuth metadata: {error}"
                    ),
                    fix_suggestion: Some(format!(
                        "Check that the tunnel or proxy in front of {base_url} is running and forwards to port {}",
                        config.http.port
                    )),
                },
            }
        }
        crate::config::DownstreamAuthMode::Auto | crate::config::DownstreamAuthMode::Bearer => {
            if !config.http.requires_bearer() {
                return CheckResult {
                    name,
                    status: CheckStatus::Pass,
                    message: "HTTP auth in auto mode resolves to no auth on loopback".to_string(),
                    fix_suggestion: None,
                };
            }
            let mode = if config.http.auth_mode == crate::config::DownstreamAuthMode::Bearer {
                "HTTP auth mode is bearer".to_string()
            } else {
                "HTTP auth in auto mode resolves to bearer because the server is reachable off this Mac"
                    .to_string()
            };

            let token_path = crate::auth::http_auth_token_path(config.http.port);

            if !token_path.exists() {
                return CheckResult {
                    name,
                    status: CheckStatus::Warn,
                    message: format!(
                        "{mode}, but the auth token has not been generated yet ({})",
                        token_path.display()
                    ),
                    fix_suggestion: Some(
                        "Start Plug (open Plug.app, or run `plug start`) and it generates the token, or change http.auth_mode for your deployment".to_string(),
                    ),
                };
            }

            {
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
                            fix_suggestion: Some(format!(
                                "Run: chmod 600 {}",
                                token_path.display()
                            )),
                        };
                    }
                }
            }

            CheckResult {
                name,
                status: CheckStatus::Pass,
                message: format!("{mode}; the auth token is in {}", token_path.display()),
                fix_suggestion: None,
            }
        }
    }
}

/// Fetch the public OAuth authorization-server metadata the way a remote
/// client discovers it, so a stopped tunnel or a wrong public URL shows up
/// here instead of as a failed sign-in in some other app.
async fn check_oauth_metadata_reachable(base_url: &str) -> Result<(), String> {
    let url = format!(
        "{}/.well-known/oauth-authorization-server",
        base_url.trim_end_matches('/')
    );
    let client = crate::tls::http_client_builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client.get(&url).send().await.map_err(|error| {
        if error.is_timeout() {
            "request timed out (5s)".to_string()
        } else {
            // reqwest's Display repeats the URL, which the message already names.
            error.without_url().to_string()
        }
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    let metadata = response
        .json::<serde_json::Value>()
        .await
        .map_err(|_| "the response was not OAuth metadata JSON".to_string())?;
    if metadata
        .get("issuer")
        .and_then(|issuer| issuer.as_str())
        .is_none()
    {
        return Err("the response has no issuer".to_string());
    }
    Ok(())
}

/// OAuth servers that ask for no scopes. The rules a config must keep to
/// load are `validate_config`'s, and doctor only sees a config that loaded.
async fn check_oauth_config(config: &Config) -> CheckResult {
    let name = check::OAUTH_CONFIG.to_string();
    let mut unscoped: Vec<String> = config
        .servers
        .iter()
        .filter(|(_, server)| {
            server.is_oauth() && server.oauth_scopes.as_ref().is_none_or(Vec::is_empty)
        })
        .map(|(server_name, _)| {
            format!("server '{server_name}': auth = \"oauth\" but no scopes configured")
        })
        .collect();
    unscoped.sort();

    if unscoped.is_empty() {
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "OAuth config fields are coherent".to_string(),
            fix_suggestion: None,
        }
    } else {
        CheckResult {
            name,
            status: CheckStatus::Warn,
            message: unscoped.join("; "),
            fix_suggestion: Some("Check OAuth settings in your config.toml".to_string()),
        }
    }
}

/// Check token status for OAuth-configured servers.
async fn check_oauth_tokens(config: &Config) -> CheckResult {
    let name = check::OAUTH_TOKENS.to_string();

    let oauth_servers: Vec<_> = config
        .servers
        .iter()
        .filter(|(_, sc)| sc.is_oauth() && sc.enabled)
        .collect();

    if oauth_servers.is_empty() {
        return CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "No OAuth-configured servers".to_string(),
            fix_suggestion: None,
        };
    }

    // Each signed-in server keeps a token file next to its Keychain entry. The
    // file is the runtime's required mirror, so its presence is expected and
    // not worth a warning; the only problem worth flagging is one other users
    // can read.
    let tokens_dir = crate::oauth::tokens_dir();
    let mut signed_in = 0usize;
    let mut exposed = Vec::new();
    for (server_name, _) in &oauth_servers {
        let Ok(token_file) = crate::oauth::token_file_path(server_name) else {
            continue;
        };
        let Ok(metadata) = std::fs::metadata(&token_file) else {
            continue;
        };
        signed_in += 1;
        if metadata.permissions().mode() & 0o077 != 0 {
            exposed.push(format!(
                "server '{server_name}': token file {} is readable by other users",
                token_file.display()
            ));
        }
    }

    if exposed.is_empty() {
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message: format!(
                "{signed_in} of {} OAuth servers have stored sign-ins (doctor does not probe keychain-backed credentials)",
                oauth_servers.len()
            ),
            fix_suggestion: None,
        }
    } else {
        CheckResult {
            name,
            status: CheckStatus::Warn,
            message: exposed.join("; "),
            fix_suggestion: Some(format!(
                "Restrict the token files to your user: chmod 600 {}/*.json",
                tokens_dir.display()
            )),
        }
    }
}

/// Source-development repair sequence for the macOS code-signing warning.
#[cfg(any(target_os = "macos", test))]
fn codesign_fix_suggestion() -> &'static str {
    "Install or open signed Plug.app for production. For source development, run `./scripts/dev-install.sh`, which builds a Developer ID signed Plug.app from the working tree and installs it; never re-sign Plug.app or a release executable by hand."
}

/// macOS only: warn when the running binary is ad-hoc signed *and* keychain-backed
/// OAuth upstreams are configured, because the macOS Keychain "Always Allow" ACL
/// binds to the binary's signature and an ad-hoc signature changes on every
/// rebuild — so macOS re-prompts for Keychain access constantly. Production
/// installs should use Plug.app's Developer ID signature; isolated source builds
/// use the setup sequence in `codesign_fix_suggestion`.
async fn check_codesign_identity(config: &Config) -> CheckResult {
    let name = check::CODESIGN_IDENTITY.to_string();

    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
        CheckResult {
            name,
            status: CheckStatus::Pass,
            message: "Not macOS — code-signing does not affect Keychain prompts here".to_string(),
            fix_suggestion: None,
        }
    }

    #[cfg(target_os = "macos")]
    {
        let oauth_count = config
            .servers
            .iter()
            .filter(|(_, sc)| sc.is_oauth() && sc.enabled)
            .count();

        if oauth_count == 0 {
            return CheckResult {
                name,
                status: CheckStatus::Pass,
                message: "No keychain-backed OAuth upstreams configured".to_string(),
                fix_suggestion: None,
            };
        }

        match self_is_adhoc_or_unsigned() {
            Some(true) => CheckResult {
                name,
                status: CheckStatus::Warn,
                message: format!(
                    "plug is ad-hoc signed, so macOS re-prompts for Keychain access on every rebuild ({oauth_count} OAuth upstream(s) affected)"
                ),
                fix_suggestion: Some(codesign_fix_suggestion().to_string()),
            },
            Some(false) => CheckResult {
                name,
                status: CheckStatus::Pass,
                message: "plug has a stable code-signing identity; Keychain approvals persist"
                    .to_string(),
                fix_suggestion: None,
            },
            None => CheckResult {
                name,
                status: CheckStatus::Pass,
                message: "Code-signing status could not be determined".to_string(),
                fix_suggestion: None,
            },
        }
    }
}

/// Returns `Some(true)` if this binary is ad-hoc signed or unsigned, `Some(false)`
/// if it carries a real signing authority, or `None` if codesign output was
/// unrecognized. macOS only.
#[cfg(target_os = "macos")]
fn self_is_adhoc_or_unsigned() -> Option<bool> {
    let exe = std::env::current_exe().ok()?;
    // codesign writes the display info to stderr.
    let out = std::process::Command::new("codesign")
        .arg("-dvv")
        .arg(&exe)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    if text.contains("adhoc") || text.contains("not signed") {
        Some(true)
    } else if text.contains("Authority=") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_a_duplicate_plug_link_can_clear_is_reported() {
        use crate::clients::{ClientTarget, Scope};
        let plug = crate::config::HttpConfig::default();
        let twice = r#"{"servers":{"plug":{"command":"plug"},"Plug":{"command":"plug"}}}"#;
        let issues = |scope| {
            super::client_file_issues(
                ClientTarget::VSCodeCopilot,
                scope,
                std::path::Path::new("mcp.json"),
                twice,
                &plug,
            )
        };
        assert_eq!(
            issues(Scope::User),
            [super::ClientFileIssue::Duplicates(
                "vscode (duplicate entries in mcp.json)".to_string()
            )]
        );
        assert!(issues(Scope::Project).is_empty());
    }

    /// A TOML or YAML file linked twice does not parse, but `plug link`
    /// fixes it, so doctor says so once and does not call it unreadable.
    #[test]
    fn a_file_linked_twice_is_reported_once_with_plug_link_as_the_fix() {
        use super::ClientFileIssue::{Duplicates, Unreadable};
        use crate::clients::{ClientTarget, Scope};
        let plug = crate::config::HttpConfig::default();
        for (target, twice, broken) in [
            (
                ClientTarget::CodexCli,
                "[mcp_servers.plug]\ncommand = \"a\"\n\n[mcp_servers.other]\ncommand = \"x\"\n\n[mcp_servers.plug]\ncommand = \"b\"\n",
                "[mcp_servers.plug]\ncommand = \"a\"\n\n[mcp_servers.plug]\ncommand = \"b\"\n\n[broken\n",
            ),
            (
                ClientTarget::Hermes,
                "mcp_servers:\n  plug:\n    command: a\n  other:\n    command: x\n  plug:\n    command: b\n",
                "mcp_servers:\n  plug:\n    command: a\n  plug:\n    command: b\nother: [\n",
            ),
        ] {
            let path = std::path::Path::new("settings");
            let issues =
                |scope, content| super::client_file_issues(target, scope, path, content, &plug);
            let user = issues(Scope::User, twice);
            assert!(
                matches!(user.as_slice(), [Duplicates(_)]),
                "{target:?}: {user:?}"
            );
            assert_eq!(
                super::client_configs_result(&user)
                    .fix_suggestion
                    .as_deref(),
                Some("Run `plug link <client>` again to leave one Plug entry")
            );
            // `plug link` does not write a project file, so a person fixes it.
            let project = issues(Scope::Project, twice);
            assert!(
                matches!(project.as_slice(), [Unreadable(_)]),
                "{target:?}: {project:?}"
            );
            // Broken apart from Plug's entries, the file is still unreadable.
            let both = issues(Scope::User, broken);
            assert!(
                matches!(both.as_slice(), [Duplicates(_), Unreadable(_)]),
                "{target:?}: {both:?}"
            );
            assert!(
                super::client_configs_result(&both)
                    .fix_suggestion
                    .unwrap()
                    .starts_with("Fix a file that does not parse")
            );
        }
    }

    use std::collections::HashMap;

    use super::*;
    use crate::config::{Config, ServerConfig, TransportType};

    fn test_config() -> Config {
        Config::default()
    }

    /// Plug's folder in a fresh temporary folder, so a check reads no real
    /// token, PID file or daemon.
    struct TempPlug {
        _paths: crate::paths::test::Isolated,
        _dir: tempfile::TempDir,
    }

    impl TempPlug {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            Self {
                _paths: crate::paths::test::isolate(dir.path()).await,
                _dir: dir,
            }
        }
    }

    #[test]
    fn a_check_carries_the_title_a_person_reads() {
        let check = |name: &str| CheckResult {
            name: name.to_string(),
            status: CheckStatus::Pass,
            message: String::new(),
            fix_suggestion: None,
        };
        assert_eq!(
            check("config_permissions").title(),
            "Settings file is private"
        );
        assert_eq!(check("brand_new_check").title(), "brand new check");

        let json = serde_json::to_value(check("client_configs")).unwrap();
        assert_eq!(json["name"], "client_configs");
        assert_eq!(json["title"], "Client setup");
    }

    fn stdio_server(cmd: &str) -> ServerConfig {
        ServerConfig {
            command: Some(cmd.to_string()),
            ..Default::default()
        }
    }

    fn oauth_http_server(url: &str) -> ServerConfig {
        ServerConfig {
            transport: TransportType::Http,
            protocol_mode: Default::default(),
            url: Some(url.to_string()),
            auth: Some("oauth".to_string()),
            oauth_scopes: Some(vec!["read".to_string()]),
            ..stdio_server("echo")
        }
    }

    // -- check_config_exists --

    #[test]
    fn keys_in_the_config_file_are_named_and_references_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[servers.a]
command = "a"
[servers.a.env]
API_KEY = "typed"
MODE = "fast"
OTHER_TOKEN = "$FROM_ENV"

[servers.b]
url = "https://example.com/mcp"
transport = "http"
auth_token = "keychain:b.token"
"#,
        )
        .unwrap();
        let result = check_keys_in_config(&path);
        assert_eq!(result.status, CheckStatus::Warn);
        assert_eq!(result.message, "Keys written in the config file: a.API_KEY");

        std::fs::write(&path, "[servers.b]\ncommand = \"b\"\n").unwrap();
        assert_eq!(check_keys_in_config(&path).status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn config_exists_missing_file() {
        let result = check_config_exists(Path::new("/nonexistent/path/config.toml")).await;
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("not found"));
        assert_eq!(
            result.fix_suggestion.as_deref(),
            Some("Run `plug setup` to create and link a config")
        );
    }

    #[tokio::test]
    async fn config_exists_valid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[http]\nport = 3282\n").unwrap();

        let result = check_config_exists(&path).await;
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn config_exists_invalid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[invalid toml ===").unwrap();

        let result = check_config_exists(&path).await;
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(result.message.contains("invalid TOML"));
    }

    #[tokio::test]
    async fn config_permissions_no_file() {
        let result = check_config_permissions(Path::new("/nonexistent/config.toml")).await;
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn config_permissions_warns_on_world_readable_with_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[servers.test]\ncommand = \"echo\"\nenv = { LOG_LEVEL = \"KEY_TOKEN_SECRET\" }\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let result = check_config_permissions(&path).await;
        assert_eq!(result.status, CheckStatus::Pass, "a setting is not a key");

        std::fs::write(
            &path,
            "[servers.test]\ncommand = \"echo\"\nauth_token = \"secret\"\n",
        )
        .unwrap();
        let result = check_config_permissions(&path).await;
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("world/group-readable"));
    }

    // -- check_port_available --

    #[tokio::test]
    async fn a_port_another_program_holds_fails() {
        let _plug = TempPlug::new().await;
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = test_config();
        config.http.bind_address = "127.0.0.1".to_string();
        config.http.port = held.local_addr().unwrap().port();
        let result = check_port_available(&config).await;
        assert_eq!(result.status, CheckStatus::Fail, "{}", result.message);
    }

    // -- check_env_vars --

    #[tokio::test]
    async fn env_vars_all_set() {
        let config = test_config();
        let result = check_env_vars(&config, Path::new("/nonexistent/plug/config.toml")).await;
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
        let result = check_env_vars(&config, Path::new("/nonexistent/plug/config.toml")).await;
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("PLUG_NONEXISTENT_VAR_XYZ"));
    }

    #[tokio::test]
    async fn env_vars_missing_core_fails() {
        let mut config = test_config();
        config.http.public_base_url = Some("$PLUG_NONEXISTENT_VAR_XYZ".to_string());
        let result = check_env_vars(&config, Path::new("/nonexistent/plug/config.toml")).await;
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(result.message.contains("PLUG_NONEXISTENT_VAR_XYZ"));
    }

    #[tokio::test]
    async fn a_server_named_like_plug_only_warns() {
        let mut config = test_config();
        config.servers.insert(
            "plugin-x".to_string(),
            ServerConfig {
                env: HashMap::from([(
                    "API_KEY".to_string(),
                    "$PLUG_NONEXISTENT_VAR_XYZ".to_string(),
                )]),
                ..stdio_server("echo")
            },
        );
        let result = check_env_vars(&config, Path::new("/nonexistent/plug/config.toml")).await;
        assert_eq!(result.status, CheckStatus::Warn);
    }

    // -- check_http_auth --

    #[tokio::test]
    async fn http_auth_auto_loopback_passes_without_token() {
        let _plug = TempPlug::new().await;
        let mut config = test_config();
        config.http.auth_mode = crate::config::DownstreamAuthMode::Auto;
        config.http.bind_address = "127.0.0.1".to_string();
        config.http.port = 62001;

        let result = check_http_auth(&config).await;
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.message.contains("auto mode"));
    }

    #[tokio::test]
    async fn http_auth_auto_behind_a_tunnel_needs_the_token() {
        let _plug = TempPlug::new().await;
        let mut config = test_config();
        config.http.auth_mode = crate::config::DownstreamAuthMode::Auto;
        config.http.bind_address = "127.0.0.1".to_string();
        config.http.public_base_url = Some("https://plug.example.com".to_string());
        config.http.port = 62003;

        let result = check_http_auth(&config).await;
        assert!(
            result.message.contains("resolves to bearer"),
            "{}",
            result.message
        );
    }

    #[tokio::test]
    async fn http_auth_bearer_on_loopback_warns_when_token_missing() {
        let _plug = TempPlug::new().await;
        let mut config = test_config();
        config.http.auth_mode = crate::config::DownstreamAuthMode::Bearer;
        config.http.bind_address = "127.0.0.1".to_string();
        config.http.port = 62002;

        let result = check_http_auth(&config).await;
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("auth mode is bearer"));
    }

    async fn serve_oauth_metadata(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind metadata listener");
        let address = listener.local_addr().expect("metadata listener address");
        let router = axum::Router::new().route(
            "/.well-known/oauth-authorization-server",
            axum::routing::get(move || async move {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
            }),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        format!("http://{address}")
    }

    fn oauth_config(public_base_url: Option<String>) -> Config {
        let mut config = test_config();
        config.http.auth_mode = crate::config::DownstreamAuthMode::Oauth;
        config.http.public_base_url = public_base_url;
        config.http.oauth_scopes = Some(vec!["tools:read".to_string()]);
        config.http.port = 62003;
        config
    }

    #[tokio::test]
    async fn http_auth_oauth_passes_when_the_public_url_serves_metadata() {
        let base_url = serve_oauth_metadata(r#"{"issuer":"https://plug.example.com"}"#).await;
        let result = check_http_auth(&oauth_config(Some(base_url))).await;
        assert_eq!(result.status, CheckStatus::Pass, "{}", result.message);
        assert!(result.fix_suggestion.is_none());
    }

    #[tokio::test]
    async fn http_auth_oauth_warns_when_the_public_url_serves_something_else() {
        let base_url = serve_oauth_metadata(r#"{"hello":"world"}"#).await;
        let result = check_http_auth(&oauth_config(Some(base_url))).await;
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("no issuer"), "{}", result.message);
    }

    #[tokio::test]
    async fn http_auth_oauth_warns_when_the_public_url_is_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve port");
        let address = listener.local_addr().expect("reserved address");
        drop(listener);
        let result = check_http_auth(&oauth_config(Some(format!("http://{address}")))).await;
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(
            result.message.contains("did not serve OAuth metadata"),
            "{}",
            result.message
        );
        assert!(result.fix_suggestion.is_some());
    }

    #[tokio::test]
    async fn http_auth_oauth_warns_without_a_public_url() {
        let result = check_http_auth(&oauth_config(None)).await;
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("public_base_url"));
    }

    #[tokio::test]
    async fn oauth_without_owner_credential_is_blocking() {
        let state_dir = std::env::temp_dir().join(format!(
            "plug-doctor-owner-missing-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&state_dir).expect("create state directory");
        let mut config = test_config();
        config.http.auth_mode = crate::config::DownstreamAuthMode::Oauth;
        config.http.public_base_url = Some("https://plug.example.com".to_string());
        config.http.oauth_scopes = Some(vec!["tools:read".to_string()]);

        let result = check_downstream_oauth_owner_with_state_dir(&config, &state_dir).await;

        assert_eq!(result.status, CheckStatus::Fail);
        assert_eq!(
            result.message,
            "Downstream OAuth owner credential missing. Run `plug auth owner enroll`."
        );
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[tokio::test]
    async fn owner_credential_is_not_required_when_downstream_oauth_is_disabled() {
        let state_dir = std::env::temp_dir().join(format!(
            "plug-doctor-owner-disabled-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&state_dir).expect("create state directory");
        let config = test_config();

        let result = check_downstream_oauth_owner_with_state_dir(&config, &state_dir).await;

        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(
            result.message,
            "Downstream OAuth owner approval is not required"
        );
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[tokio::test]
    async fn corrupt_owner_state_fails_without_disclosing_state_contents() {
        let state_dir = std::env::temp_dir().join(format!(
            "plug-doctor-owner-corrupt-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&state_dir).expect("create state directory");
        let mut config = test_config();
        config.http.auth_mode = crate::config::DownstreamAuthMode::Oauth;
        config.http.public_base_url = Some("https://plug.example.com".to_string());
        config.http.oauth_scopes = Some(vec!["tools:read".to_string()]);
        let oauth = crate::downstream_oauth::DownstreamOauthConfig::from_http_config(&config.http)
            .expect("OAuth config");
        let path = crate::downstream_oauth::owner_enrollment_state_path_in_dir(&oauth, &state_dir);
        let secret = "refresh-token-that-must-not-appear";
        std::fs::write(
            &path,
            format!(r#"{{"version":3,"owner_credentials":{secret}}}"#),
        )
        .expect("write corrupt state");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("set owner-only permissions");
        }

        let result = check_downstream_oauth_owner_with_state_dir(&config, &state_dir).await;

        assert_eq!(result.status, CheckStatus::Fail);
        assert_eq!(
            result.message,
            "Downstream OAuth owner state could not be read safely. No new access can be approved."
        );
        assert!(!result.message.contains(secret));
        assert!(
            result
                .fix_suggestion
                .as_deref()
                .is_some_and(|suggestion| suggestion.contains("plug auth owner list"))
        );
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn owner_state_permissions_have_an_exact_safe_repair() {
        use std::os::unix::fs::PermissionsExt as _;

        let state_dir = std::env::temp_dir().join(format!(
            "plug-doctor-owner-permissions-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&state_dir).expect("create state directory");
        let mut config = test_config();
        config.http.auth_mode = crate::config::DownstreamAuthMode::Oauth;
        config.http.public_base_url = Some("https://plug.example.com".to_string());
        config.http.oauth_scopes = Some(vec!["tools:read".to_string()]);
        let oauth = crate::downstream_oauth::DownstreamOauthConfig::from_http_config(&config.http)
            .expect("OAuth config");
        let path = crate::downstream_oauth::owner_enrollment_state_path_in_dir(&oauth, &state_dir);
        std::fs::write(&path, r#"{"version":3,"owner_credentials":{}}"#).expect("write state");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("weaken permissions");

        let result = check_downstream_oauth_owner_with_state_dir(&config, &state_dir).await;

        assert_eq!(result.status, CheckStatus::Fail);
        assert_eq!(
            result.fix_suggestion,
            Some(format!("Run: chmod 600 {}", path.display()))
        );
        let _ = std::fs::remove_dir_all(state_dir);
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

    /// The token file is the runtime's required mirror of every sign-in, so
    /// a private one is the normal state, not a warning.
    #[tokio::test]
    async fn oauth_tokens_pass_when_a_private_token_file_exists() {
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let result = check_oauth_tokens(&config).await;
        let _ = std::fs::remove_file(&token_file);

        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.message.contains("1 of 1"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn oauth_tokens_warn_when_token_file_is_readable_by_others() {
        use std::os::unix::fs::PermissionsExt as _;

        let server_name = format!("oauth-doctor-exposed-{}", std::process::id());
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
        std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o644)).unwrap();

        let result = check_oauth_tokens(&config).await;
        let _ = std::fs::remove_file(&token_file);

        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains(&server_name));
        assert!(result.message.contains("readable by other users"));
        assert!(
            result
                .fix_suggestion
                .as_deref()
                .unwrap_or_default()
                .contains("chmod 600")
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

    /// The daemon runs the whole `command`, so a path with a space in it is
    /// found, and a bare name is not cut at its first word.
    #[tokio::test]
    async fn server_binaries_check_the_whole_command() {
        let dir = tempfile::tempdir().unwrap();
        let spaced = dir.path().join("My Tools").join("server");
        std::fs::create_dir_all(spaced.parent().unwrap()).unwrap();
        std::fs::write(&spaced, "").unwrap();
        let mut config = test_config();
        config
            .servers
            .insert("spaced".to_string(), stdio_server(spaced.to_str().unwrap()));
        assert_eq!(
            check_server_binaries(&config).await.status,
            CheckStatus::Pass
        );

        config
            .servers
            .insert("split".to_string(), stdio_server("echo hello"));
        let result = check_server_binaries(&config).await;
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(
            result.message.contains("split: echo hello"),
            "{}",
            result.message
        );
    }

    /// `node` from a version manager resolves in a shell but not on the PATH
    /// the daemon falls back to when its login-shell probe fails. Doctor used
    /// to pass that; the server then failed after a slow reboot.
    #[test]
    fn server_binaries_warn_when_only_the_login_shell_finds_one() {
        let mut config = test_config();
        config
            .servers
            .insert("oura".to_string(), stdio_server("node"));
        config.servers.insert("sys".to_string(), stdio_server("sh"));
        let result = server_binaries_result(
            &config,
            |binary| {
                Some(std::path::PathBuf::from(format!(
                    "/Users/me/.nvm/bin/{binary}"
                )))
            },
            |binary| (binary == "sh").then(|| std::path::PathBuf::from("/bin/sh")),
        );
        if cfg!(target_os = "macos") {
            assert_eq!(result.status, CheckStatus::Warn);
            assert!(
                result
                    .message
                    .contains("oura: node (/Users/me/.nvm/bin/node)"),
                "{}",
                result.message
            );
            assert!(!result.message.contains("sys"), "{}", result.message);
        } else {
            assert_eq!(result.status, CheckStatus::Pass);
        }
    }

    // -- check_client_limits --

    #[test]
    fn client_limits_pass_when_no_capped_client_is_linked() {
        let result = client_limits_result(check::CLIENT_LIMITS.to_string(), Vec::new());
        assert_eq!(result.status, CheckStatus::Pass, "{}", result.message);
    }

    #[test]
    fn a_linked_capped_client_warns_with_its_source() {
        let result = client_limits_result(
            check::CLIENT_LIMITS.to_string(),
            crate::clients::TOOL_LIMITS.iter().collect(),
        );
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(
            result
                .message
                .contains(crate::clients::TOOL_LIMITS[0].source)
        );
    }

    // -- check_pid_staleness --

    #[tokio::test]
    async fn pid_staleness_reads_the_pid_file() {
        let _plug = TempPlug::new().await;
        assert_eq!(check_pid_staleness().await.status, CheckStatus::Pass);

        let pid_path = crate::paths::pid_path();
        std::fs::create_dir_all(pid_path.parent().unwrap()).unwrap();
        std::fs::write(&pid_path, std::process::id().to_string()).unwrap();
        let running = check_pid_staleness().await;
        assert_eq!(running.status, CheckStatus::Pass);
        assert!(
            running.message.contains("is running"),
            "{}",
            running.message
        );

        // macOS gives out PIDs below 100000.
        std::fs::write(&pid_path, "99999999").unwrap();
        let stale = check_pid_staleness().await;
        assert_eq!(stale.status, CheckStatus::Warn);
        assert!(stale.message.contains("Stale"), "{}", stale.message);

        std::fs::write(&pid_path, "not a pid").unwrap();
        assert_eq!(check_pid_staleness().await.status, CheckStatus::Warn);
    }

    // -- check_server_connectivity --

    #[tokio::test]
    async fn connectivity_no_servers() {
        let config = test_config();
        let result = check_server_connectivity(&config).await;
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn connectivity_leaves_stdio_servers_to_server_binaries() {
        let mut config = test_config();
        config.servers.insert(
            "missing".to_string(),
            stdio_server("/nonexistent/plug-doctor-test-binary"),
        );
        let result = check_server_connectivity(&config).await;
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(!result.message.contains("missing"));
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
    async fn connectivity_http_server_unreachable_reports_server_name() {
        let _plug = TempPlug::new().await;
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
    async fn connectivity_mixed_servers_report_remote_when_unreachable() {
        let _plug = TempPlug::new().await;
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

    // -- DoctorReport --

    #[test]
    fn codesign_fix_suggestion_names_the_dev_install_loop() {
        let suggestion = codesign_fix_suggestion();
        assert!(suggestion.contains("./scripts/dev-install.sh"));
        assert!(suggestion.contains("never re-sign Plug.app"));
    }

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
        assert_eq!(report.checks.len(), 15);
    }
}
