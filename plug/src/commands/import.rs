//! `plug import`: bring servers over from other clients' settings files.

use dialoguer::console::style;
use plug_core::clients::ClientTarget;
use plug_core::import::{DiscoveredServer, ImportReport};

use crate::OutputFormat;
use crate::ui::{
    cli_prompt_theme, print_banner, print_heading, print_info_line, print_success_line,
};

/// Add `servers` to the config at `config_path`. A config that does not load
/// is refused rather than written over.
pub(crate) fn add_servers(
    config_path: Option<&std::path::PathBuf>,
    servers: &[DiscoveredServer],
) -> anyhow::Result<()> {
    let (path, mut config) = crate::commands::config::load_editable_config(config_path)?;
    plug_core::import::add_to_config(&mut config, servers);
    plug_core::operator::persist_config_atomic(&path, &config)
}

/// Scan `sources` for servers the config does not have yet.
pub(crate) fn scan(
    config_path: Option<&std::path::PathBuf>,
    sources: &[ClientTarget],
) -> ImportReport {
    // A config that does not load still lets the scan say what is out there.
    let existing = plug_core::config::load_config(config_path)
        .map(|config| config.servers)
        .unwrap_or_default();
    plug_core::import::import(&existing, sources)
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

pub(crate) fn cmd_import(
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
    add_servers(config_path, &to_import)?;
    println!();
    print_success_line(format!("Imported {} server(s).", to_import.len()));
    Ok(())
}
