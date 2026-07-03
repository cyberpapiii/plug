use dialoguer::console::style;
use figment::Figment;
use figment::providers::{Format, Serialized, Toml};

use crate::ui::{
    print_banner, print_heading, print_label_value, print_success_line, print_warning_line,
};
use crate::{ConfigCommands, OutputFormat};

pub(crate) fn load_editable_config(
    config_path: Option<&std::path::PathBuf>,
) -> anyhow::Result<(std::path::PathBuf, plug_core::config::Config)> {
    let path = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);

    let config = if path.exists() {
        Figment::new()
            .merge(Serialized::defaults(plug_core::config::Config::default()))
            .merge(Toml::file(&path))
            .extract()?
    } else {
        plug_core::config::Config::default()
    };

    Ok((path, config))
}

pub(crate) fn save_config(
    path: &std::path::Path,
    config: &plug_core::config::Config,
) -> anyhow::Result<()> {
    let errors = plug_core::config::validate_config(config);
    if !errors.is_empty() {
        anyhow::bail!(errors.join("; "));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string_pretty(config)?;

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::fs::PermissionsExt;
        // Try exclusive create first (new file starts life at 0600 regardless
        // of umask), falling back to truncate-and-tighten if it already
        // exists so a previously world-readable config gets locked down on
        // the next save too. config.toml can hold plaintext secrets
        // (`http.oauth_client_secret`, per-server `auth_token`), matching the
        // 0600 convention used by auth.rs/oauth.rs/downstream_oauth.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path);
        let mut file = match file {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(path)?;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                file
            }
            Err(e) => return Err(e.into()),
        };
        file.write_all(contents.as_bytes())?;
    }

    #[cfg(not(unix))]
    std::fs::write(path, contents)?;

    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::save_config;
    use std::os::unix::fs::PermissionsExt;

    fn unique_temp_path(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plug-config-test-{}-{}-{}",
            label,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir.join("config.toml")
    }

    fn mode_of(path: &std::path::Path) -> u32 {
        std::fs::metadata(path)
            .expect("read metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn save_config_writes_new_file_owner_only() {
        let path = unique_temp_path("new");
        let config = plug_core::config::Config::default();
        save_config(&path, &config).expect("save_config");
        assert_eq!(mode_of(&path), 0o600);
    }

    #[test]
    fn save_config_tightens_permissions_on_existing_world_readable_file() {
        let path = unique_temp_path("existing");
        // Pre-create the file world-readable (0644), simulating a config
        // written before this fix.
        std::fs::write(&path, "").expect("pre-create file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("set initial permissions");
        assert_eq!(mode_of(&path), 0o644);

        let config = plug_core::config::Config::default();
        save_config(&path, &config).expect("save_config");
        assert_eq!(mode_of(&path), 0o600);
    }
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
