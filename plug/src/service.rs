//! Canonical launchd ownership for the shared daemon.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const LABEL: &str = "com.plug.daemon";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceOwnership {
    Unmanaged,
    CliManaged,
    AppManaged,
}

#[derive(Debug, Clone)]
pub struct ServiceState {
    pub ownership: ServiceOwnership,
    pub loaded: bool,
    pub plist_path: PathBuf,
}

fn user_id() -> anyhow::Result<String> {
    let output = Command::new("id").arg("-u").output()?;
    if !output.status.success() {
        anyhow::bail!("unable to determine login user id");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn cli_plist_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

pub fn classify_launchctl_output(output: &str, cli_plist_exists: bool) -> ServiceOwnership {
    if output.contains("Plug.app/Contents/") || output.contains("BundleProgram") {
        ServiceOwnership::AppManaged
    } else if !output.trim().is_empty() || cli_plist_exists {
        ServiceOwnership::CliManaged
    } else {
        ServiceOwnership::Unmanaged
    }
}

pub fn inspect() -> anyhow::Result<ServiceState> {
    let uid = user_id()?;
    let plist_path = cli_plist_path();
    let output = Command::new("launchctl")
        .args(["print", &format!("gui/{uid}/{LABEL}")])
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(ServiceState {
        ownership: classify_launchctl_output(&text, plist_path.exists()),
        loaded: output.status.success(),
        plist_path,
    })
}

pub fn ipc_ownership() -> plug_core::ipc::DaemonOwnershipMode {
    match inspect().map(|state| state.ownership) {
        Ok(ServiceOwnership::AppManaged) => plug_core::ipc::DaemonOwnershipMode::AppManaged,
        Ok(ServiceOwnership::CliManaged) => plug_core::ipc::DaemonOwnershipMode::CliManaged,
        Ok(ServiceOwnership::Unmanaged) | Err(_) => plug_core::ipc::DaemonOwnershipMode::Unmanaged,
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn render_cli_plist(executable: &Path, config_path: Option<&Path>) -> String {
    let mut arguments = vec![
        executable.display().to_string(),
        "serve".to_string(),
        "--daemon".to_string(),
    ];
    if let Some(path) = config_path {
        arguments.push("--config".to_string());
        arguments.push(path.display().to_string());
    }
    let arguments = arguments
        .iter()
        .map(|arg| format!("    <string>{}</string>", xml_escape(arg)))
        .collect::<Vec<_>>()
        .join("\n");
    let log = crate::daemon::log_dir().join("launchd.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array>
{arguments}
  </array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict></plist>
"#,
        log = xml_escape(&log.display().to_string())
    )
}

fn install_cli_plist(path: &Path, config_path: Option<&Path>) -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid LaunchAgent path"))?;
    std::fs::create_dir_all(parent)?;
    std::fs::create_dir_all(crate::daemon::log_dir())?;
    let contents = render_cli_plist(&executable, config_path);
    let temp = path.with_extension("plist.tmp");
    std::fs::write(&temp, contents)?;
    std::fs::rename(temp, path)?;
    Ok(())
}

fn launchctl(args: &[String]) -> anyhow::Result<()> {
    let output = Command::new("launchctl").args(args).output()?;
    if !output.status.success() {
        anyhow::bail!(
            "launchctl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

pub async fn ensure_started(config_path: Option<&Path>) -> anyhow::Result<bool> {
    if crate::daemon::connect_to_daemon().await.is_some() {
        return Ok(false);
    }
    let uid = user_id()?;
    let state = inspect()?;
    match state.ownership {
        ServiceOwnership::AppManaged => {
            launchctl(&[
                "kickstart".into(),
                "-k".into(),
                format!("gui/{uid}/{LABEL}"),
            ])?;
        }
        ServiceOwnership::CliManaged => {
            if state.loaded {
                launchctl(&["bootout".into(), format!("gui/{uid}/{LABEL}")])?;
            }
            install_cli_plist(&state.plist_path, config_path)?;
            launchctl(&[
                "bootstrap".into(),
                format!("gui/{uid}"),
                state.plist_path.display().to_string(),
            ])?;
            launchctl(&[
                "kickstart".into(),
                "-k".into(),
                format!("gui/{uid}/{LABEL}"),
            ])?;
        }
        ServiceOwnership::Unmanaged => {
            install_cli_plist(&state.plist_path, config_path)?;
            launchctl(&[
                "bootstrap".into(),
                format!("gui/{uid}"),
                state.plist_path.display().to_string(),
            ])?;
            launchctl(&[
                "kickstart".into(),
                "-k".into(),
                format!("gui/{uid}/{LABEL}"),
            ])?;
        }
    }
    for delay in [100, 200, 400, 800, 1_600] {
        if crate::daemon::connect_to_daemon().await.is_some() {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    anyhow::bail!(
        "daemon did not start; view {}",
        crate::daemon::log_dir().display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_service_wins_over_stale_cli_plist() {
        assert_eq!(
            classify_launchctl_output(
                "program = /Applications/Plug.app/Contents/Resources/plug",
                true
            ),
            ServiceOwnership::AppManaged
        );
    }

    #[test]
    fn rendered_cli_plist_has_one_daemon_program_and_escapes_paths() {
        let plist = render_cli_plist(
            Path::new("/tmp/Plug & Me/plug"),
            Some(Path::new("/tmp/a<b.toml")),
        );
        assert!(plist.contains("/tmp/Plug &amp; Me/plug"));
        assert!(plist.contains("/tmp/a&lt;b.toml"));
        assert_eq!(plist.matches("--daemon").count(), 1);
    }
}
