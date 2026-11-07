//! `plug setup`: import servers from other clients, then link clients.

use plug_core::clients::{ClientTarget, LinkTransport};

use crate::OutputFormat;
use crate::commands::import::{add_servers, print_discovered, scan};
use crate::ui::{cli_prompt_theme, print_banner, print_success_line};

pub(crate) async fn cmd_setup(
    config_path: Option<&std::path::PathBuf>,
    yes: bool,
    transport: Option<LinkTransport>,
) -> anyhow::Result<()> {
    use dialoguer::Confirm;

    print_banner(
        "◆",
        "Plug setup",
        "Discover servers, import config, and link your AI clients",
    );
    let sources: Vec<ClientTarget> = ClientTarget::all().collect();
    let report = scan(config_path, &sources);
    if !report.new_servers.is_empty() {
        print_discovered(&report.new_servers);
        print_success_line(format!("Found {} server(s).", report.new_servers.len()));
        println!();
        if yes
            || Confirm::with_theme(&cli_prompt_theme())
                .with_prompt("Import them?")
                .default(true)
                .interact()?
        {
            add_servers(config_path, &report.new_servers).await?;
        }
    }
    crate::commands::clients::cmd_link(
        config_path,
        Vec::new(),
        false,
        yes,
        transport,
        &OutputFormat::Text,
    )
}
