use dialoguer::console::style;

use crate::ui::{
    print_banner, print_heading, print_label_value, print_success_line, print_warning_line,
};
use crate::{ConfigCommands, OutputFormat};

/// Resolve the config path and load it the one way every editor does:
/// defaults under the file, no environment overlay.
pub(crate) fn load_editable_config(
    config_path: Option<&std::path::PathBuf>,
) -> anyhow::Result<(std::path::PathBuf, plug_core::config::Config)> {
    let path = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);
    let config = plug_core::operator::load_editable_config(&path)?;
    Ok((path, config))
}

pub(crate) fn cmd_config(
    config_path: Option<&std::path::PathBuf>,
    path_only: bool,
    command: Option<ConfigCommands>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let path = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);
    if path_only {
        println!("{}", path.display());
        return Ok(());
    }

    match command {
        Some(ConfigCommands::Path) => {
            println!("{}", path.display());
        }
        Some(ConfigCommands::Check) => {
            let exists = path.exists();
            let result = if exists {
                match plug_core::config::load_config(Some(&path)) {
                    Ok(config) => {
                        let errors = plug_core::config::validate_config(&config);
                        serde_json::json!({
                            "path": path,
                            "exists": true,
                            "valid": errors.is_empty(),
                            "errors": errors
                        })
                    }
                    Err(error) => serde_json::json!({
                        "path": path,
                        "exists": true,
                        "valid": false,
                        "errors": [error.to_string()]
                    }),
                }
            } else {
                serde_json::json!({
                    "path": path,
                    "exists": false,
                    "valid": false,
                    "errors": ["config file not found"]
                })
            };

            if matches!(output, OutputFormat::Json) {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                print_banner("◆", "Config check", "Validate config syntax and core rules");
                print_label_value("Path", style(path.display()).dim());
                if !exists {
                    print_warning_line("Config file not found.");
                } else if let Some(errors) = result.get("errors").and_then(|v| v.as_array()) {
                    if errors.is_empty() {
                        print_success_line("Config is valid.");
                    } else {
                        println!();
                        print_heading("Issues");
                        for error in errors {
                            if let Some(error) = error.as_str() {
                                println!("  {} {}", style("×").red().bold(), error);
                            }
                        }
                    }
                }
            }
        }
        Some(ConfigCommands::Resolved) => {
            let config = plug_core::config::load_config(Some(&path))?;
            let result = serde_json::json!({
                "path": path,
                "downstream_auth_mode": config.http.auth_mode.label(),
                "public_base_url": config.http.public_base_url,
                "binary_version": env!("CARGO_PKG_VERSION"),
            });
            if matches!(output, OutputFormat::Json) {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                print_banner("◆", "Resolved config", "Effective downstream settings");
                print_label_value("Path", style(path.display()).dim());
                print_label_value("Downstream Auth", config.http.auth_mode.label());
                if let Some(public_base_url) = config.http.public_base_url {
                    print_label_value("Public URL", public_base_url);
                }
                print_label_value("Binary Version", env!("CARGO_PKG_VERSION"));
            }
        }
        None => {
            if path.exists() {
                open::that(&path)?;
            } else {
                println!("Config missing at {}. Run setup.", path.display());
            }
        }
    }
    Ok(())
}
