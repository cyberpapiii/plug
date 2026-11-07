//! `plug import`: bring servers over from other clients' settings files.

use dialoguer::console::style;
use plug_core::clients::ClientTarget;
use plug_core::import::{DiscoveredServer, ImportReport};

use crate::OutputFormat;
use crate::ui::{
    cli_prompt_theme, print_banner, print_heading, print_info_line, print_success_line,
};

/// Add `servers` to the config at `config_path` through the mutation every
/// config edit takes. A config that does not load is refused rather than
/// written over. The daemon keeps the keys the servers came with as it does
/// for any server added; a key still written in the file is named so the
/// person can move it.
pub(crate) async fn add_servers(
    config_path: Option<&std::path::PathBuf>,
    servers: &[DiscoveredServer],
) -> anyhow::Result<()> {
    let result = crate::commands::servers::apply_server_mutation(
        config_path,
        plug_core::operator::OperatorMutation::ImportServers {
            servers: servers.iter().map(Into::into).collect(),
        },
    )
    .await?;
    let (_, config) = crate::commands::config::load_editable_config(config_path)?;
    let stores = plug_core::secrets::Stores::from_config(&config.secrets);
    let typed: Vec<String> = result
        .imported
        .unwrap_or_default()
        .into_iter()
        .filter_map(|summary| config.servers.get(&summary.name).map(|s| (summary.name, s)))
        .flat_map(|(name, server)| {
            stores
                .plaintext(server)
                .into_iter()
                .map(move |field| format!("{name}.{field}"))
        })
        .collect();
    if !typed.is_empty() {
        crate::ui::print_warning_line(format!(
            "Keys written in the config file: {}. Run `plug secret move` to keep them in the Keychain.",
            typed.join(", ")
        ));
    }
    Ok(())
}

/// Scan `sources` for servers the config does not have yet.
pub(crate) fn scan(
    config_path: Option<&std::path::PathBuf>,
    sources: &[ClientTarget],
) -> ImportReport {
    // A config that does not load still lets the scan say what is out there.
    let config = plug_core::config::load_config(config_path).unwrap_or_default();
    plug_core::import::import(&config, sources)
}

pub(crate) fn print_discovered(servers: &[DiscoveredServer]) {
    print_heading("Discovered");
    for server in servers {
        println!(
            "  {} {:<18} {}",
            style("·").dim(),
            style(&server.name).bold(),
            style(format!("from {}", server.source)).dim()
        );
    }
}

pub(crate) async fn cmd_import(
    config_path: Option<&std::path::PathBuf>,
    clients: Option<Vec<String>>,
    dry_run: bool,
    yes: bool,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    use dialoguer::MultiSelect;

    let sources: Vec<ClientTarget> = match clients {
        Some(names) => names
            .iter()
            .map(|name| name.parse().map_err(|error: String| anyhow::anyhow!(error)))
            .collect::<anyhow::Result<_>>()?,
        None => ClientTarget::all().collect(),
    };

    if matches!(output, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::to_string_pretty(&scan(config_path, &sources))?
        );
        return Ok(());
    }

    print_banner(
        "◆",
        "Import",
        "Scan existing AI client configs for MCP servers",
    );
    print_info_line(style("Scanning for MCP servers...").bold());
    let report = scan(config_path, &sources);
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
    print_discovered(&report.new_servers);
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

    let to_import: Vec<DiscoveredServer> = selections
        .iter()
        .map(|&i| report.new_servers[i].clone())
        .collect();
    add_servers(config_path, &to_import).await?;
    println!();
    print_success_line(format!("Imported {} server(s).", to_import.len()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With `--config`, an import lands in that file through the operator
    /// mutation, a taken name renamed and nothing written over.
    #[tokio::test]
    async fn an_offline_import_goes_through_the_mutation() {
        let dir =
            std::env::temp_dir().join(format!("plug-import-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("create dir");
        let path = dir.join("config.toml");
        let mut config = plug_core::config::Config::default();
        let github = plug_core::config::ServerConfig {
            command: Some("gh-mcp".to_string()),
            ..Default::default()
        };
        config.servers.insert("github".to_string(), github);
        plug_core::operator::persist_config_atomic(&path, &config).expect("write config");

        let source = ClientTarget::Cursor;
        let found = DiscoveredServer {
            name: "github".to_string(),
            config: plug_core::config::ServerConfig {
                command: Some("npx".to_string()),
                ..Default::default()
            },
            source,
            source_target: source.slug(),
            source_name: source.display_name(),
        };
        add_servers(Some(&path), &[found]).await.expect("import");

        let (_, written) =
            crate::commands::config::load_editable_config(Some(&path)).expect("reload");
        assert_eq!(written.servers["github"].command.as_deref(), Some("gh-mcp"));
        assert_eq!(
            written.servers["github-cursor"].command.as_deref(),
            Some("npx")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
