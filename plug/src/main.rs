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

mod client_host;
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
    plug events             Watch a tool and tell clients when it changes
    plug doctor             Diagnose setup problems

  Maintain
    plug repair             Refresh linked client configs
    plug config check       Validate config syntax and rules
    plug config path        Print config file path
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
    about = "MCP gateway — one config, every client connected",
    after_help = HELP_OVERVIEW,
    styles = ui::cli_styles()
)]
struct Cli {
    /// Path to config file
    #[arg(long, global = true)]
    config: Option<std::path::PathBuf>,

    /// Increase verbosity (-v for debug, -vv for trace; `clients` and `tools` spend the first -v on listing every row)
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
        /// Import every server found and accept the defaults without asking
        #[arg(long)]
        yes: bool,
        /// How linked clients reach plug: stdio (`plug connect`) or http
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
        /// Client targets to repair, such as `cursor` or `codex-cli` (default: all)
        targets: Vec<String>,
        /// Repair every client plug knows about
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
    /// View and manage linked, detected, and live AI clients (-v lists each session)
    Clients {
        #[command(subcommand)]
        command: Option<commands::clients::ClientCommands>,
    },
    #[command(display_order = 8)]
    /// View and manage configured servers
    Servers,
    #[command(display_order = 9)]
    /// Tool counts per server; `plug tools <server>` lists that server's tools, -v lists all
    Tools {
        #[command(subcommand)]
        command: Option<ToolCommands>,
        /// Server name or tool group whose tools to list
        server: Option<String>,
    },
    #[command(display_order = 9)]
    /// Watch a tool and tell clients when its result changes
    Events {
        #[command(subcommand)]
        command: Option<commands::events::EventCommands>,
    },
    #[command(display_order = 10)]
    /// Link plug to your AI clients
    Link {
        /// Client targets to link, such as `cursor` or `claude-code`
        targets: Vec<String>,
        /// Link every detected client
        #[arg(long)]
        all: bool,
        /// Accept the defaults (stdio transport) without asking
        #[arg(long)]
        yes: bool,
        /// How linked clients reach plug: stdio (`plug connect`) or http
        #[arg(long, value_enum)]
        transport: Option<ClientLinkTransport>,
    },
    #[command(display_order = 11)]
    /// Remove plug from your AI client configs
    Unlink {
        /// Client targets to unlink, such as `cursor` or `claude-code`
        targets: Vec<String>,
        /// Unlink every linked client
        #[arg(long)]
        all: bool,
        /// Unlink every linked client without asking
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
    Connect {
        /// The target `plug link` installed this command for
        #[arg(long, value_name = "TARGET")]
        client: Option<String>,
    },
    #[command(display_order = 14)]
    /// Internal: run plug as the shared foreground or background service
    Serve {
        /// Run as the shared background service (IPC + HTTP) that launchd manages
        #[arg(long)]
        daemon: bool,
    },
    #[command(display_order = 15)]
    /// Internal: stop the background plug service
    Stop,
    #[command(display_order = 16)]
    /// Open the plug config file in your default editor
    Config {
        /// Same as `plug config path`
        #[arg(long, hide = true)]
        path: bool,
        #[command(subcommand)]
        command: Option<ConfigCommands>,
    },
    #[command(display_order = 17)]
    /// Advanced: import MCP servers from existing AI client configs
    Import {
        /// Comma-separated clients to scan, such as `cursor,claude-code` (default: all)
        #[arg(long, value_delimiter = ',')]
        clients: Option<Vec<String>>,
        /// Scan every supported client (the default)
        #[arg(long)]
        all: bool,
        /// Show what would be imported without changing the config
        #[arg(long)]
        dry_run: bool,
        /// Import every server found without asking
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
    #[command(display_order = 20)]
    /// Keep a server's key in the Keychain instead of in the config file
    Secret {
        #[command(subcommand)]
        command: SecretCommands,
    },
    #[command(hide = true)]
    /// Internal: remove only installation artifacts proven to belong to Plug.app
    UninstallCleanup,
}

#[derive(Subcommand)]
pub(crate) enum SecretCommands {
    /// Store a value under a name; the value is asked for, never an argument
    Set {
        /// The name a server refers to it by, as `keychain:<name>`
        name: String,
    },
    /// Remove a stored value
    Rm {
        /// The name it was stored under
        name: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConfigCommands {
    /// Print the config file path
    Path,
    /// Validate config syntax and rules
    Check,
    /// Print the resolved downstream configuration without secrets
    Resolved,
}

#[derive(Subcommand)]
pub(crate) enum ServerCommands {
    /// Add a server (prompts for anything not given)
    Add {
        /// Server name
        name: Option<String>,
        /// Command that starts a stdio server
        #[arg(long)]
        command: Option<String>,
        /// URL of an HTTP or SSE server
        #[arg(long)]
        url: Option<String>,
        /// Comma-separated arguments for the command
        #[arg(long, value_delimiter = ',')]
        args: Vec<String>,
        /// Environment variable as KEY=VALUE; repeat for more
        #[arg(long = "env")]
        env: Vec<String>,
        /// Upstream transport: stdio, http, or sse
        #[arg(long)]
        transport: Option<String>,
        /// Upstream auth for HTTP or SSE servers: none, bearer, or oauth
        #[arg(long)]
        auth: Option<String>,
        /// Token sent as a bearer token (with --auth bearer)
        #[arg(long)]
        bearer_token: Option<String>,
        /// Pre-registered OAuth client ID (with --auth oauth)
        #[arg(long)]
        oauth_client_id: Option<String>,
        /// Comma-separated OAuth scopes to request
        #[arg(long, value_delimiter = ',')]
        oauth_scopes: Option<Vec<String>>,
        /// Add the server switched off
        #[arg(long)]
        disabled: bool,
        /// Turn an HTTP API into a server: the URL or file of its OpenAPI
        /// document. --url overrides where the API lives
        #[arg(long, value_name = "URL_OR_FILE")]
        openapi: Option<String>,
        /// Comma-separated operations of the API to expose (with --openapi);
        /// `*` is a wildcard
        #[arg(long, value_delimiter = ',')]
        operations: Vec<String>,
        /// Where the API wants the token from --bearer-token (with
        /// --openapi): `bearer`, `header:<name>`, or `query:<name>`. Left
        /// out, the OpenAPI document decides
        #[arg(long, value_name = "PLACE")]
        token_in: Option<String>,
    },
    /// Remove a server from the config
    Remove {
        /// Server name
        name: Option<String>,
        /// Remove without asking
        #[arg(long)]
        yes: bool,
    },
    /// Add a server again for a second account, as <server>-<account>
    AddAccount {
        /// The server that is already configured
        server: String,
        /// A short name for the account: lowercase letters and digits
        account: String,
    },
    /// Change a server's command, URL, env, or auth
    Edit {
        /// Server name
        name: Option<String>,
        /// Command that starts a stdio server
        #[arg(long)]
        command: Option<String>,
        /// URL of an HTTP or SSE server
        #[arg(long)]
        url: Option<String>,
        /// Comma-separated arguments for the command
        #[arg(long, value_delimiter = ',')]
        args: Option<Vec<String>>,
        /// Environment variable as KEY=VALUE; repeat for more
        #[arg(long = "env")]
        env: Vec<String>,
        /// Comma-separated environment variable names to remove
        #[arg(long = "unset-env", value_delimiter = ',')]
        unset_env: Vec<String>,
        /// Upstream transport: stdio, http, or sse
        #[arg(long)]
        transport: Option<String>,
        /// Upstream auth for HTTP or SSE servers: none, bearer, or oauth
        #[arg(long)]
        auth: Option<String>,
        /// Token sent as a bearer token (with --auth bearer)
        #[arg(long)]
        bearer_token: Option<String>,
        /// Pre-registered OAuth client ID (with --auth oauth)
        #[arg(long)]
        oauth_client_id: Option<String>,
        /// Comma-separated OAuth scopes to request
        #[arg(long, value_delimiter = ',')]
        oauth_scopes: Option<Vec<String>>,
    },
    /// Turn a disabled server back on
    Enable { name: Option<String> },
    /// Keep a server in the config but stop starting it
    Disable { name: Option<String> },
}

#[derive(Subcommand)]
pub(crate) enum ToolCommands {
    /// Hide tools from every client
    Disable {
        /// Hide every tool from this server
        #[arg(long)]
        server: Option<String>,
        /// Tool names or glob patterns, such as `github__delete_*`
        patterns: Vec<String>,
    },
    /// Re-enable tools you disabled
    Enable {
        /// Show every tool from this server again
        #[arg(long)]
        server: Option<String>,
        /// Tool names or glob patterns, such as `github__delete_*`
        patterns: Vec<String>,
    },
    /// List disabled tool patterns
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
    /// Store or remove the Slack Events signing secret in the OS Keychain
    SlackEvents {
        #[command(subcommand)]
        command: SlackEventCredentialCommands,
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
pub(crate) enum SlackEventCredentialCommands {
    /// Enter the source app signing secret interactively without echo
    Set {
        #[arg(long)]
        team_id: String,
        #[arg(long)]
        app_id: String,
    },
    /// Remove the stored signing secret; first disable events and restart through Plug.app
    Remove {
        #[arg(long)]
        team_id: String,
        #[arg(long)]
        app_id: String,
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

    // `plug clients -v` and `plug tools -v` spend the first -v on listing
    // more rows, so their debug logs start at -vv.
    let log_verbosity = match &cli.command {
        Some(Commands::Clients { .. }) | Some(Commands::Tools { .. }) => {
            cli.verbose.saturating_sub(1)
        }
        _ => cli.verbose,
    };
    let log_level = match log_verbosity {
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
        Some(Commands::Connect { client }) => {
            runtime::cmd_connect(cli.config.as_ref(), client).await?
        }
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
        Some(Commands::Clients { command: None }) => {
            views::clients::cmd_client_list(cli.config.as_ref(), &cli.output, cli.verbose > 0)
                .await?
        }
        Some(Commands::Clients {
            command: Some(commands::clients::ClientCommands::Rename { client, name }),
        }) => commands::clients::cmd_client_rename(cli.config.as_ref(), client, name).await?,
        Some(Commands::Clients {
            command:
                Some(commands::clients::ClientCommands::Block {
                    client,
                    servers,
                    tools,
                }),
        }) => {
            commands::clients::cmd_client_block(cli.config.as_ref(), client, servers, tools, true)
                .await?
        }
        Some(Commands::Clients {
            command:
                Some(commands::clients::ClientCommands::Unblock {
                    client,
                    servers,
                    tools,
                }),
        }) => {
            commands::clients::cmd_client_block(cli.config.as_ref(), client, servers, tools, false)
                .await?
        }
        Some(Commands::Events { command: None }) => {
            commands::events::cmd_event_list(cli.config.as_ref(), &cli.output).await?
        }
        Some(Commands::Events {
            command:
                Some(commands::events::EventCommands::Watch {
                    server,
                    tool,
                    name,
                    every,
                    args,
                    allow_writes,
                }),
        }) => {
            commands::events::cmd_event_watch(
                cli.config.as_ref(),
                server,
                tool,
                name,
                every,
                args,
                allow_writes,
            )
            .await?
        }
        Some(Commands::Events {
            command: Some(commands::events::EventCommands::Unwatch { event }),
        }) => commands::events::cmd_event_unwatch(cli.config.as_ref(), event).await?,
        Some(Commands::Tools { command, server }) => {
            commands::tools::cmd_tool_command(
                cli.config.as_ref(),
                command,
                server,
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
        Some(Commands::Secret { command }) => commands::secrets::cmd_secret(command)?,
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
        Some(Commands::Serve { .. }) | Some(Commands::Connect { .. }) => "info",
        Some(Commands::Status { .. })
        | Some(Commands::Servers)
        | Some(Commands::Clients { .. })
        | Some(Commands::Tools { .. }) => "none",
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
        assert_eq!(level(&["plug", "clients"]), "none");
        assert_eq!(level(&["plug", "auth", "status"]), "error");
        assert_eq!(level(&["plug", "config", "check"]), "error");
        assert_eq!(level(&["plug"]), "error");
    }

    #[test]
    fn tools_command_takes_a_server_or_a_subcommand() {
        let cli = Cli::try_parse_from(["plug", "tools", "workspace"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Tools { command: None, server: Some(ref server) }) if server == "workspace"
        ));
        let cli = Cli::try_parse_from(["plug", "tools", "disabled"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Tools {
                command: Some(ToolCommands::Disabled),
                server: None
            })
        ));
    }

    #[test]
    fn config_path_flag_still_works() {
        let cli = Cli::try_parse_from(["plug", "config", "--path"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Config { path: true, .. })
        ));
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
    fn slack_events_credential_cli_has_no_secret_argument_or_pipe_mode() {
        let args = [
            "plug",
            "auth",
            "slack-events",
            "set",
            "--team-id",
            "T123",
            "--app-id",
            "A123",
        ];
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Auth {
                command: AuthCommands::SlackEvents {
                    command: SlackEventCredentialCommands::Set { .. }
                }
            })
        ));
        for extra in ["--secret", "--signing-secret", "--stdin"] {
            let mut argv = args.to_vec();
            argv.push(extra);
            assert!(Cli::try_parse_from(argv).is_err());
        }
        assert!(
            Cli::try_parse_from([
                "plug",
                "auth",
                "slack-events",
                "remove",
                "--team-id",
                "T123",
                "--app-id",
                "A123"
            ])
            .is_ok()
        );
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
