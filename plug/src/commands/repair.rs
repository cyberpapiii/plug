//! `plug repair`: point client links at the Plug.app command again.

use dialoguer::console::style;
use plug_core::clients::ClientTarget;

use crate::OutputFormat;
use crate::commands::doctor::{
    ClientRepairItem, ClientRepairReport, PlugLinkDisposition, inspect_link,
};
use crate::ui::print_warning_line;

/// The named clients, or every listed one when none is named.
fn repair_targets(requested: Vec<String>, all: bool) -> anyhow::Result<Vec<ClientTarget>> {
    if all || requested.is_empty() {
        return Ok(ClientTarget::listed().collect());
    }
    let (known, unknown): (Vec<_>, Vec<_>) = requested
        .iter()
        .map(|name| (name, ClientTarget::from_slug(name)))
        .partition(|(_, target)| target.is_some());
    if !unknown.is_empty() {
        let names: Vec<&str> = unknown.iter().map(|(name, _)| name.as_str()).collect();
        anyhow::bail!("unknown client target(s): {}", names.join(", "));
    }
    let mut targets: Vec<ClientTarget> = known.into_iter().filter_map(|(_, t)| t).collect();
    targets.sort_by_key(|target| target.slug());
    targets.dedup();
    Ok(targets)
}

fn repair_attention_messages(report: &ClientRepairReport) -> Vec<String> {
    report
        .items
        .iter()
        .filter(|item| !item.changed && item.disposition.needs_repair())
        .map(|item| format!("{}: {}", item.target, item.message))
        .collect()
}

fn repair_text_summary(report: &ClientRepairReport) -> String {
    let needing_attention = repair_attention_messages(report);
    let repaired = report.items.iter().filter(|item| item.changed).count();
    if !needing_attention.is_empty() {
        format!(
            "Repair finished with {} client configuration(s) needing attention.",
            needing_attention.len()
        )
    } else if repaired > 0 {
        format!("Successfully repaired {repaired} client configuration(s).")
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

pub(crate) fn cmd_repair(
    targets: Vec<String>,
    all: bool,
    dry_run: bool,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let targets = repair_targets(targets, all)?;
    // A dry run inspects against this binary, so it needs no Plug.app.
    let canonical_command = if dry_run {
        std::env::current_exe()?
    } else {
        crate::install::canonical_client_command()?
    };

    if matches!(output, OutputFormat::Text) {
        println!(
            "{} {}",
            style("◆").cyan().bold(),
            style(if dry_run {
                "Inspecting AI client configurations..."
            } else {
                "Repairing AI client configurations..."
            })
            .bold()
        );
    }

    let items = targets
        .into_iter()
        .map(|target| {
            let item = inspect_link(target, &canonical_command, !dry_run).unwrap_or_else(|| {
                ClientRepairItem {
                    target: target.slug().to_string(),
                    path: target
                        .config_path(plug_core::clients::Scope::User)
                        .unwrap_or_default(),
                    disposition: PlugLinkDisposition::Missing,
                    changed: false,
                    message: "Client config file does not exist.".to_string(),
                }
            });
            if item.changed && matches!(output, OutputFormat::Text) {
                println!(
                    "  {} Refreshing {}... {}",
                    style("›").cyan().bold(),
                    item.target,
                    style("done").green()
                );
            }
            item
        })
        .collect();
    let report = ClientRepairReport {
        canonical_command,
        items,
    };

    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            for message in repair_attention_messages(&report) {
                print_warning_line(message);
            }
            println!("\n{}", repair_text_summary(&report));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_targets_accepts_every_name_link_accepts() {
        let targets = repair_targets(
            vec![
                "cursor".to_string(),
                "codex".to_string(),
                "codex-cli".to_string(),
            ],
            false,
        )
        .expect("known targets should pass");
        assert_eq!(targets, [ClientTarget::CodexCli, ClientTarget::Cursor]);
    }

    #[test]
    fn repair_targets_rejects_unknown_targets() {
        let error = repair_targets(vec!["made-up-client".to_string()], false)
            .expect_err("unknown target should fail");
        assert!(error.to_string().contains("unknown client target"));
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
        assert!(repair_text_summary(&report).contains("needing attention"));
        assert!(!repair_text_summary(&report).contains("Successfully"));
    }
}
