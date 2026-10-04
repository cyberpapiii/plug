use std::path::PathBuf;

use dialoguer::console::style;
use plug_core::events::{EventStatus, WatchConfig, WatchHealth};

use crate::OutputFormat;
use crate::ui::{format_duration, print_heading, print_info_line, print_success_line};

#[derive(clap::Subcommand)]
pub(crate) enum EventCommands {
    /// Watch a tool and send an event when its result changes
    Watch {
        /// The server, by its name in `plug servers`
        server: String,
        /// The tool's own name on that server, as `plug tools <server>` lists it
        tool: String,
        /// What to call the event; it becomes `<server>.<name>` (default: the tool name)
        #[arg(long)]
        name: Option<String>,
        /// Seconds between checks (minimum 30)
        #[arg(long, default_value_t = 300)]
        every: u64,
        /// An argument for the tool, as key=value; the value may be JSON
        #[arg(long = "arg", value_name = "KEY=VALUE")]
        args: Vec<String>,
        /// Watch a tool its server does not mark read-only
        #[arg(long)]
        allow_writes: bool,
    },
    /// Stop watching, by event name
    Unwatch {
        /// The event, as `plug events` lists it
        event: String,
    },
}

/// Turn a tool name into something an event may be called.
fn event_name_from(tool: &str) -> String {
    let name: String = tool
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    name.trim_matches('_').to_string()
}

/// `key=value` pairs as tool arguments. A value that parses as JSON is JSON;
/// anything else is text.
fn parse_arguments(args: &[String]) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    let mut arguments = serde_json::Map::new();
    for arg in args {
        let Some((key, value)) = arg.split_once('=') else {
            anyhow::bail!("`{arg}` is not key=value");
        };
        if key.is_empty() {
            anyhow::bail!("`{arg}` has no key");
        }
        let value = serde_json::from_str(value)
            .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
        arguments.insert(key.to_string(), value);
    }
    Ok(arguments)
}

pub(crate) async fn cmd_event_watch(
    config_path: Option<&PathBuf>,
    server: String,
    tool: String,
    name: Option<String>,
    every: u64,
    args: Vec<String>,
    allow_writes: bool,
) -> anyhow::Result<()> {
    let watch = WatchConfig {
        name: name.unwrap_or_else(|| event_name_from(&tool)),
        server,
        tool,
        arguments: parse_arguments(&args)?,
        every_secs: every,
        allow_writes,
    };
    let event = watch.event_name();
    let every = watch.every_secs;
    crate::commands::servers::apply_server_mutation(
        config_path,
        plug_core::operator::OperatorMutation::AddWatch { watch },
    )
    .await?;
    print_success_line(format!(
        "Watching. `{event}` is sent when the result changes, checked every {}.",
        format_duration(every)
    ));
    print_info_line(style("The first result is the starting point and sends nothing.").dim());
    Ok(())
}

pub(crate) async fn cmd_event_unwatch(
    config_path: Option<&PathBuf>,
    event: String,
) -> anyhow::Result<()> {
    crate::commands::servers::apply_server_mutation(
        config_path,
        plug_core::operator::OperatorMutation::RemoveWatch {
            event: event.clone(),
        },
    )
    .await?;
    print_success_line(format!("Stopped watching `{event}`."));
    Ok(())
}

/// The watches and how they are doing: from the daemon when it runs and no
/// other config was named, else from the file alone.
async fn event_statuses(config_path: Option<&PathBuf>) -> anyhow::Result<Vec<EventStatus>> {
    if config_path.is_none()
        && crate::runtime::daemon_running().await
        && let Ok(auth_token) = crate::daemon::read_auth_token()
        && let Ok(plug_core::ipc::IpcResponse::OperatorSnapshot { snapshot }) =
            crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::OperatorSnapshot { auth_token })
                .await
    {
        return Ok(snapshot.events);
    }
    let config = plug_core::config::load_config(config_path)?;
    let mut statuses: Vec<EventStatus> = config
        .events
        .watch
        .iter()
        .map(EventStatus::waiting)
        .collect();
    statuses.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(statuses)
}

fn ago(timestamp: Option<u64>) -> String {
    match timestamp {
        Some(at) => format!(
            "{} ago",
            format_duration(plug_core::slack_events::now().saturating_sub(at))
        ),
        None => "never".to_string(),
    }
}

/// One plain sentence for how a watch is doing.
pub(crate) fn health_line(status: &EventStatus) -> String {
    match status.state {
        WatchHealth::Waiting => "Waiting for the first check.".to_string(),
        WatchHealth::Watching => format!(
            "Checked {}; last change {}.",
            ago(status.last_checked),
            ago(status.last_changed)
        ),
        WatchHealth::ToolMissing => {
            "The tool is not there right now. Is the server running?".to_string()
        }
        WatchHealth::NotReadOnly => "Not watched: the tool is not marked read-only.".to_string(),
        WatchHealth::CallFailed => format!(
            "The last check failed ({}). Plug keeps trying.",
            ago(status.last_checked)
        ),
        WatchHealth::TooLarge => "The result is too large to send.".to_string(),
    }
}

pub(crate) async fn cmd_event_list(
    config_path: Option<&PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let statuses = event_statuses(config_path).await?;
    if matches!(output, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "events": statuses }))?
        );
        return Ok(());
    }
    print_heading("Events");
    if statuses.is_empty() {
        print_info_line("Nothing is watched.");
        print_info_line(style("Start with: plug events watch <server> <tool>").dim());
        return Ok(());
    }
    for status in &statuses {
        println!("  {}", style(&status.name).bold());
        let tool = status.tool.as_deref().unwrap_or("-");
        let every = status.every_secs.map(format_duration).unwrap_or_default();
        println!(
            "    {}",
            style(format!(
                "{tool} on {}, every {every}; {} listening",
                status.server,
                match status.subscribers {
                    0 => "no client".to_string(),
                    1 => "1 client".to_string(),
                    n => format!("{n} clients"),
                }
            ))
            .dim()
        );
        println!("    {}", health_line(status));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_take_json_values_and_fall_back_to_text() {
        let arguments = parse_arguments(&[
            "query=is:unread".to_string(),
            "limit=5".to_string(),
            "labels=[\"a\"]".to_string(),
        ])
        .unwrap();
        assert_eq!(arguments["query"], "is:unread");
        assert_eq!(arguments["limit"], 5);
        assert_eq!(arguments["labels"], serde_json::json!(["a"]));
        assert!(parse_arguments(&["novalue".to_string()]).is_err());
    }

    #[test]
    fn an_event_is_named_after_its_tool_unless_told_otherwise() {
        assert_eq!(event_name_from("Search-Messages"), "search_messages");
        assert_eq!(event_name_from("__list__"), "list");
    }

    #[test]
    fn a_watch_says_how_it_is_doing_in_plain_words() {
        let mut status = EventStatus::waiting(&WatchConfig {
            name: "inbox".into(),
            server: "mail".into(),
            tool: "unread".into(),
            arguments: Default::default(),
            every_secs: 60,
            allow_writes: false,
        });
        assert_eq!(health_line(&status), "Waiting for the first check.");
        status.state = WatchHealth::Watching;
        status.last_checked = Some(plug_core::slack_events::now());
        assert!(health_line(&status).ends_with("last change never."));
    }
}
