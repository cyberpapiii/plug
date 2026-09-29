#![deny(unsafe_code)]
// RMCP 3.1 deprecates some APIs toward future SEP-2577. Plug intentionally
// retains them while MCP 2025-11-25 remains the default negotiated revision.
#![allow(deprecated)]
#![allow(
    clippy::items_after_test_module,
    clippy::large_enum_variant,
    clippy::manual_map,
    clippy::manual_unwrap_or,
    clippy::manual_unwrap_or_default,
    clippy::needless_borrow,
    clippy::suspicious_open_options,
    clippy::too_many_arguments,
    clippy::unnecessary_cast,
    clippy::unnecessary_min_or_max
)]

/// Load `.env` file vars into the process environment.
///
/// SAFETY: Called before tokio runtime starts (single-threaded at this point).
/// `set_var` is unsafe in Rust 2024 because it's not thread-safe, but we're
/// guaranteed single-threaded here since this runs before `#[tokio::main]`.
#[allow(unsafe_code)]
fn apply_dotenv() {
    for (key, value) in plug_core::dotenv::load_dotenv() {
        unsafe {
            std::env::set_var(&key, &value);
        }
    }
}

#[cfg(test)]
pub(crate) fn install_test_credential_environment() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        plug_core::oauth::install_test_credential_environment(
            std::env::temp_dir().join(format!("plug-oauth-binary-tests-{}", std::process::id())),
        );
    });
}

mod commands;
mod daemon;
// The unified installation model is fully exercised on macOS and in tests;
// Linux keeps only the small standalone-command subset at runtime.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod install;
mod ipc_proxy;
mod runtime;
mod service;
mod ui;
mod views;

use clap::{Parser, Subcommand};

const HELP_OVERVIEW: &str = "\
Workflow:
  Get started
    plug start              Start the shared background service
    plug setup              Discover servers and link clients
    plug clients            View and manage AI clients

  Inspect
    plug status             Show runtime health and next actions
    plug clients            Show linked, detected, and live clients
    plug servers            View and manage configured servers
    plug tools              View and manage available tools
    plug doctor             Diagnose setup problems

  Maintain
    plug repair             Refresh linked client configs
    plug config check       Validate config syntax and rules
    plug config --path      Print config file path
    plug link               Link plug to your AI clients
    plug unlink             Remove plug from your AI client configs

  Internal
    plug connect            stdio adapter invoked by AI clients
    plug serve              Run the shared service in the foreground
    plug serve --daemon     Run the shared background service (IPC + HTTP)
";

#[derive(Parser)]
#[command(
    name = "plug",
    version,
    about = "MCP multiplexer — one config, every client connected",
    after_help = HELP_OVERVIEW,
    styles = ui::cli_styles()
)]
struct Cli {
    /// Path to config file
    #[arg(long, global = true)]
    config: Option<std::path::PathBuf>,

    /// Increase verbosity (-v for debug, -vv for trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Output format
    #[arg(long, global = true, default_value = "text")]
    output: OutputFormat,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub(crate) enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum ClientLinkTransport {
    Stdio,
    Http,
}

#[derive(Subcommand)]
enum Commands {
    #[command(display_order = 1)]
    /// Start the shared background plug service (IPC + HTTP)
    Start,
    #[command(display_order = 2)]
    /// Discover servers, import config, and link your AI clients
    Setup {
        #[arg(long)]
        yes: bool,
        #[arg(long, value_enum)]
        transport: Option<ClientLinkTransport>,
    },
    #[command(display_order = 3)]
    /// Show runtime health and the next useful action
    Status {
        /// Reveal the HTTP auth token (hidden by default)
        #[arg(long)]
        show_token: bool,
    },
    #[command(display_order = 4)]
    /// Diagnose problems with your plug setup
    Doctor,
    #[command(display_order = 5)]
    /// Refresh linked AI client configuration files
    Repair {
        targets: Vec<String>,
        #[arg(long)]
        all: bool,
        /// Inspect repair needs without changing client configuration files
        #[arg(long)]
        dry_run: bool,
    },
    #[command(display_order = 6)]
    /// Internal: reload service config from disk
    Reload,
    #[command(display_order = 7)]
    /// View and manage linked, detected, and live AI clients
    Clients,
    #[command(display_order = 8)]
    /// View and manage configured servers
    Servers,
    #[command(display_order = 9)]
    /// View and manage available tools from your servers
    Tools {
        #[command(subcommand)]
        command: Option<ToolCommands>,
    },
    #[command(display_order = 10)]
    /// Link plug to your AI clients
    Link {
        targets: Vec<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long, value_enum)]
        transport: Option<ClientLinkTransport>,
    },
    #[command(display_order = 11)]
    /// Remove plug from your AI client configs
    Unlink {
        targets: Vec<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        yes: bool,
    },
    #[command(display_order = 12)]
    /// Manage configured servers
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },
    #[command(display_order = 13)]
    /// Internal: start the stdio adapter AI clients invoke
    Connect,
    #[command(display_order = 14)]
    /// Internal: run plug as the shared foreground or background service
    Serve {
        #[arg(long)]
        daemon: bool,
    },
    #[command(display_order = 15)]
    /// Internal: stop the background plug service
    Stop,
    #[command(display_order = 16)]
    /// Open the plug config file in your default editor
    Config {
        #[arg(long)]
        path: bool,
        #[command(subcommand)]
        command: Option<ConfigCommands>,
    },
    #[command(display_order = 17)]
    /// Advanced: import MCP servers from existing AI client configs
    Import {
        #[arg(long, value_delimiter = ',')]
        clients: Option<Vec<String>>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
    #[command(display_order = 18, hide = true)]
    /// Compatibility alias for `plug link`
    Export {
        targets: Vec<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long, value_enum)]
        transport: Option<ClientLinkTransport>,
    },
    #[command(display_order = 19)]
    /// Manage OAuth authentication for upstream servers
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },
    #[command(hide = true)]
    /// Internal: remove only installation artifacts proven to belong to Plug.app
    UninstallCleanup,
}

#[derive(Subcommand)]
pub(crate) enum ConfigCommands {
    Path,
    Check,
    /// Print the resolved downstream configuration without secrets
    Resolved,
}

#[derive(Subcommand)]
pub(crate) enum ServerCommands {
    Add {
        name: Option<String>,
        #[arg(long)]
        command: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long, value_delimiter = ',')]
        args: Vec<String>,
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long)]
        transport: Option<String>,
        #[arg(long)]
        auth: Option<String>,
        #[arg(long)]
        bearer_token: Option<String>,
        #[arg(long)]
        oauth_client_id: Option<String>,
        #[arg(long, value_delimiter = ',')]
        oauth_scopes: Option<Vec<String>>,
        #[arg(long)]
        disabled: bool,
    },
    Remove {
        name: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    Edit {
        name: Option<String>,
        #[arg(long)]
        command: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long, value_delimiter = ',')]
        args: Option<Vec<String>>,
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long = "unset-env", value_delimiter = ',')]
        unset_env: Vec<String>,
        #[arg(long)]
        transport: Option<String>,
        #[arg(long)]
        auth: Option<String>,
        #[arg(long)]
        bearer_token: Option<String>,
        #[arg(long)]
        oauth_client_id: Option<String>,
        #[arg(long, value_delimiter = ',')]
        oauth_scopes: Option<Vec<String>>,
    },
    Enable {
        name: Option<String>,
    },
    Disable {
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum ToolCommands {
    Disable {
        #[arg(long)]
        server: Option<String>,
        patterns: Vec<String>,
    },
    Enable {
        #[arg(long)]
        server: Option<String>,
        patterns: Vec<String>,
    },
    Disabled,
}

#[derive(Subcommand)]
pub(crate) enum AuthCommands {
    /// Authenticate with an OAuth-protected upstream server
    Login {
        /// Server name from config
        #[arg(long)]
        server: String,
        /// Print auth URL instead of opening browser
        #[arg(long)]
        no_browser: bool,
    },
    /// Inject pre-obtained OAuth tokens for a server
    Inject {
        /// Server name
        #[arg(long)]
        server: String,
        /// Access token value
        #[arg(long)]
        access_token: String,
        /// Refresh token value (enables auto-renewal)
        #[arg(long)]
        refresh_token: Option<String>,
        /// Token lifetime in seconds
        #[arg(long)]
        expires_in: Option<u64>,
    },
    /// Show OAuth authentication status for all servers
    Status,
    /// Complete an OAuth flow non-interactively with a pre-obtained code
    Complete {
        /// Server name from config
        #[arg(long)]
        server: String,
        /// Authorization code from the OAuth callback
        #[arg(long)]
        code: String,
        /// CSRF state parameter from the OAuth callback
        #[arg(long)]
        state: String,
        /// Optional RFC 9207 issuer parameter from the OAuth callback
        #[arg(long)]
        issuer: Option<String>,
    },
    /// Clear stored OAuth credentials for a server
    Logout {
        /// Server name
        #[arg(long)]
        server: String,
    },
    /// List or revoke downstream OAuth client registrations
    Clients {
        #[command(subcommand)]
        command: DownstreamOauthClientCommands,
    },
    /// Enroll or administer downstream OAuth owner passkeys
    Owner {
        #[command(subcommand)]
        command: OwnerCommands,
    },
}

#[derive(Subcommand)]
pub(crate) enum DownstreamOauthClientCommands {
    /// List registered downstream OAuth clients without exposing credentials
    List,
    /// Revoke a client and every code or token issued to it
    Revoke {
        /// Registered client ID
        client_id: String,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum OwnerCommands {
    /// Enroll a new owner passkey
    Enroll {
        /// Print enrollment URL instead of opening browser
        #[arg(long)]
        no_browser: bool,
    },
    /// List enrolled owner passkeys
    List,
    /// Remove an enrolled owner passkey
    Remove {
        /// Owner credential ID
        credential_id: String,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install::maybe_delegate_to_app()?;
    apply_dotenv();
    plug_core::tls::ensure_rustls_provider_installed();

    let cli = Cli::parse();

    let log_level = match cli.verbose {
        0 => default_log_level(cli.command.as_ref()),
        1 => "debug",
        _ => "trace",
    };

    let daemon_mode = matches!(&cli.command, Some(Commands::Serve { daemon: true, .. }));

    let _log_guard = if daemon_mode {
        Some(daemon::setup_file_logging(&daemon::log_dir())?)
    } else {
        init_stderr_tracing(log_level);
        None
    };

    match cli.command {
        None => views::overview::cmd_overview(cli.config.as_ref(), &cli.output).await?,
        Some(Commands::Start) => runtime::cmd_start(cli.config.as_ref(), &cli.output).await?,
        Some(Commands::Connect) => runtime::cmd_connect(cli.config.as_ref()).await?,
        Some(Commands::Serve { daemon }) => {
            if daemon {
                // Stderr is the only place this error would otherwise land, and
                // the app-owned LaunchAgent redirects stderr nowhere. Record the
                // reason in the daemon log so a restart loop can be diagnosed.
                runtime::cmd_daemon(cli.config.as_ref())
                    .await
                    .inspect_err(|error| {
                        tracing::error!(error = %error, "daemon exited with a fatal error");
                    })?;
            } else {
                runtime::cmd_serve(cli.config.as_ref()).await?;
            }
        }
        Some(Commands::Status { show_token }) => {
            views::overview::cmd_status(cli.config.as_ref(), &cli.output, show_token).await?
        }
        Some(Commands::Stop) => runtime::cmd_daemon_stop().await?,
        Some(Commands::Servers) => {
            views::servers::cmd_server_list(cli.config.as_ref(), &cli.output).await?
        }
        Some(Commands::Clients) => {
            views::clients::cmd_client_list(cli.config.as_ref(), &cli.output).await?
        }
        Some(Commands::Tools { command }) => {
            commands::tools::cmd_tool_command(
                cli.config.as_ref(),
                command,
                &cli.output,
                cli.verbose,
            )
            .await?
        }
        Some(Commands::Link {
            targets,
            all,
            yes,
            transport,
        }) => commands::clients::cmd_link(
            cli.config.as_ref(),
            targets,
            all,
            yes,
            transport.map(Into::into),
        )?,
        Some(Commands::Unlink { targets, all, yes }) => {
            commands::clients::cmd_unlink(targets, all, yes)?
        }
        Some(Commands::Server { command }) => {
            commands::servers::cmd_server_command(cli.config.as_ref(), command, &cli.output).await?
        }
        Some(Commands::Import {
            clients,
            all,
            dry_run,
            yes,
        }) => commands::misc::cmd_import(
            cli.config.as_ref(),
            clients,
            all,
            dry_run,
            yes,
            &cli.output,
        )?,
        Some(Commands::Doctor) => {
            let exit_code = commands::misc::cmd_doctor(cli.config.as_ref(), &cli.output).await?;
            if exit_code != 0 {
                // Flush before process::exit, which bypasses destructors.
                // Rust's stdout is line-buffered so the trailing-newline
                // output is already flushed, but make it explicit so a
                // future non-newline write can't be truncated when piped.
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
                std::process::exit(exit_code);
            }
        }
        Some(Commands::Repair {
            targets,
            all,
            dry_run,
        }) => commands::misc::cmd_repair(cli.config.as_ref(), targets, all, dry_run, &cli.output)?,
        Some(Commands::Setup { yes, transport }) => {
            commands::misc::cmd_setup(cli.config.as_ref(), yes, transport.map(Into::into))?
        }
        Some(Commands::Reload) => commands::misc::cmd_reload(&cli.output).await?,
        Some(Commands::Config { path, command }) => {
            commands::config::cmd_config(cli.config.as_ref(), path, command, &cli.output)?
        }
        Some(Commands::Export {
            targets,
            all,
            yes,
            transport,
        }) => commands::clients::cmd_link(
            cli.config.as_ref(),
            targets,
            all,
            yes,
            transport.map(Into::into),
        )?,
        Some(Commands::Auth { command }) => {
            commands::auth::cmd_auth(cli.config.as_ref(), command, &cli.output).await?
        }
        Some(Commands::UninstallCleanup) => commands::misc::cmd_uninstall_cleanup(&cli.output)?,
    }

    Ok(())
}

impl From<ClientLinkTransport> for plug_core::export::ExportTransport {
    fn from(value: ClientLinkTransport) -> Self {
        match value {
            ClientLinkTransport::Stdio => Self::Stdio,
            ClientLinkTransport::Http => Self::Http,
        }
    }
}

/// The stderr log level when `-v` is not given. The long-running processes
/// treat stderr as a log. For every other command stderr sits next to the
/// output the user is reading, so only errors belong there.
fn default_log_level(command: Option<&Commands>) -> &'static str {
    match command {
        Some(Commands::Serve { .. }) | Some(Commands::Connect) => "info",
        Some(Commands::Status { .. }) | Some(Commands::Servers) | Some(Commands::Tools { .. }) => {
            "none"
        }
        _ => "error",
    }
}

/// The filter used when `PLUG_LOG` is not set, for stderr and the daemon log
/// alike. Each directive quiets an RMCP module whose lines are not faults, or
/// restate a failure Plug already reports in its own words.
pub(crate) fn default_log_filter(level: &str) -> tracing_subscriber::EnvFilter {
    // Version negotiation: RMCP warns whenever a client asks for a protocol
    // version Plug does not speak and falls back, which is ordinary.
    // Transport worker: RMCP logs every failed upstream connection as a fatal
    // worker error, beside Plug's own line naming the server and the cause.
    ["rmcp::service::server=error", "rmcp::transport::worker=off"]
        .into_iter()
        .fold(
            tracing_subscriber::EnvFilter::new(level),
            |filter, directive| {
                filter.add_directive(directive.parse().expect("static tracing directive"))
            },
        )
}

fn init_stderr_tracing(level: &str) {
    if level == "none" {
        return;
    }

    let filter = tracing_subscriber::EnvFilter::try_from_env("PLUG_LOG")
        .unwrap_or_else(|_| default_log_filter(level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .compact()
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt as _;

    /// Records the level and target of every event that passes the filter.
    #[derive(Clone, Default)]
    struct Recorded(Arc<Mutex<Vec<(tracing::Level, String)>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Recorded {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let metadata = event.metadata();
            self.0
                .lock()
                .expect("recorded events")
                .push((*metadata.level(), metadata.target().to_string()));
        }
    }

    fn record_events(filter: tracing_subscriber::EnvFilter, emit: impl FnOnce()) -> Recorded {
        let recorded = Recorded::default();
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(recorded.clone());
        tracing::subscriber::with_default(subscriber, emit);
        recorded
    }

    #[test]
    fn default_filter_drops_rmcp_worker_errors_but_keeps_plug_errors() {
        let recorded = record_events(default_log_filter("info"), || {
            tracing::error!(target: "rmcp::transport::worker", "worker quit with fatal");
            tracing::warn!(target: "rmcp::service::server", "unsupported protocol version");
            tracing::error!(target: "plug_core::server", "failed to start server");
            tracing::info!(target: "plug_core::engine", "server reconnected");
        });
        let targets: Vec<String> = recorded
            .0
            .lock()
            .expect("recorded events")
            .iter()
            .map(|(_, target)| target.clone())
            .collect();
        assert_eq!(targets, ["plug_core::server", "plug_core::engine"]);
    }

    /// RMCP matches concurrent stdio requests to replies by id, so a stdio
    /// server with `max_concurrent > 1` is valid and must not warn. The warning
    /// used to fire on every config load, which the app does every poll.
    #[test]
    fn validating_a_concurrent_stdio_server_logs_nothing() {
        let server: plug_core::config::ServerConfig = serde_json::from_value(serde_json::json!({
            "command": "npx",
            "max_concurrent": 4,
        }))
        .expect("stdio server config");
        let mut config = plug_core::config::Config::default();
        config.servers.insert("slack".to_string(), server);

        let recorded = record_events(default_log_filter("info"), || {
            assert!(plug_core::config::validate_config(&config).is_empty());
        });
        assert!(
            recorded.0.lock().expect("recorded events").is_empty(),
            "validation logged: {:?}",
            recorded.0.lock().expect("recorded events")
        );
    }

    #[test]
    fn only_long_running_commands_log_below_error_by_default() {
        let level = |args: &[&str]| {
            let cli = Cli::try_parse_from(args).expect("command parses");
            default_log_level(cli.command.as_ref())
        };
        assert_eq!(level(&["plug", "serve"]), "info");
        assert_eq!(level(&["plug", "connect"]), "info");
        assert_eq!(level(&["plug", "status"]), "none");
        assert_eq!(level(&["plug", "doctor"]), "error");
        assert_eq!(level(&["plug", "clients"]), "error");
        assert_eq!(level(&["plug", "auth", "status"]), "error");
        assert_eq!(level(&["plug", "config", "check"]), "error");
        assert_eq!(level(&["plug"]), "error");
    }

    #[test]
    fn serve_command_rejects_stdio_flag() {
        let result = Cli::try_parse_from(["plug", "serve", "--stdio"]);
        assert!(result.is_err());
    }

    #[test]
    fn serve_command_accepts_daemon_flag() {
        let cli = Cli::try_parse_from(["plug", "serve", "--daemon"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Serve { daemon: true })
        ));
    }

    #[test]
    fn repair_command_accepts_dry_run_flag() {
        let cli = Cli::try_parse_from(["plug", "repair", "--all", "--dry-run"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Repair {
                all: true,
                dry_run: true,
                ..
            })
        ));
    }

    #[test]
    fn auth_owner_commands_parse() {
        let enroll = Cli::try_parse_from(["plug", "auth", "owner", "enroll", "--no-browser"])
            .expect("owner enroll should parse");
        assert!(matches!(
            enroll.command,
            Some(Commands::Auth {
                command: AuthCommands::Owner {
                    command: OwnerCommands::Enroll { no_browser: true }
                }
            })
        ));

        let list = Cli::try_parse_from(["plug", "--output", "json", "auth", "owner", "list"])
            .expect("owner list should parse");
        assert!(matches!(
            list.command,
            Some(Commands::Auth {
                command: AuthCommands::Owner {
                    command: OwnerCommands::List
                }
            })
        ));

        let remove =
            Cli::try_parse_from(["plug", "auth", "owner", "remove", "credential-1", "--yes"])
                .expect("owner remove should parse");
        assert!(matches!(
            remove.command,
            Some(Commands::Auth {
                command: AuthCommands::Owner {
                    command: OwnerCommands::Remove {
                        credential_id,
                        yes: true
                    }
                }
            }) if credential_id == "credential-1"
        ));
    }
}
