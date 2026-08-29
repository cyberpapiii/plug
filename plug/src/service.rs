//! Canonical launchd ownership for the shared daemon.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const LABEL: &str = "com.plug.daemon";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LaunchdJobRecord {
    pub label: String,
    pub program: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // Consumed by unified installation diagnosis in the next rollout task.
pub enum LaunchdProgramOwnership {
    CurrentApp,
    RecognizedLegacyPlug,
    Unknown,
}

/// CLI leftover-launchd sentence. The app leftover path still uses the generic
/// "Background running is off" / Turn On verdict, not this string.
#[cfg(any(target_os = "macos", test))]
pub const LEFTOVER_LAUNCHD_ADOPT_SENTENCE: &str =
    "Open Plug.app and tap Turn On to adopt the leftover launchd daemon.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceOwnership {
    Unmanaged,
    CliManaged,
    AppManaged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupAction {
    KickstartApp,
    OpenApp,
    RepairCli,
    InstallCli,
}

fn startup_action(app_installed: bool, ownership: ServiceOwnership) -> StartupAction {
    match (app_installed, ownership) {
        (_, ServiceOwnership::AppManaged) => StartupAction::KickstartApp,
        (true, ServiceOwnership::CliManaged | ServiceOwnership::Unmanaged) => {
            StartupAction::OpenApp
        }
        (false, ServiceOwnership::CliManaged) => StartupAction::RepairCli,
        (false, ServiceOwnership::Unmanaged) => StartupAction::InstallCli,
    }
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

fn paths_match(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Keep in lockstep with `LegacyPlugProgram.isRecognized` in Plug.app.
/// Both are pinned by `testdata/legacy_plug_programs.json`.
pub(crate) fn is_recognized_legacy_program(program: &Path) -> bool {
    if program.file_name().and_then(|name| name.to_str()) != Some("plug") {
        return false;
    }

    let is_cargo_binary =
        dirs::home_dir().is_some_and(|home| program == home.join(".cargo/bin/plug"));
    let is_local_bin = dirs::home_dir().is_some_and(|home| program == home.join(".local/bin/plug"));
    let is_formula_cellar_binary = [
        Path::new("/opt/homebrew/Cellar/plug"),
        Path::new("/usr/local/Cellar/plug"),
    ]
    .iter()
    .any(|prefix| {
        program.strip_prefix(prefix).is_ok_and(|relative| {
            let parts = relative.components().collect::<Vec<_>>();
            parts.len() == 3 && parts[1].as_os_str() == "bin" && parts[2].as_os_str() == "plug"
        })
    });

    is_cargo_binary
        || is_local_bin
        || program == Path::new("/opt/homebrew/bin/plug")
        || program == Path::new("/usr/local/bin/plug")
        || program == Path::new("/opt/homebrew/opt/plug/bin/plug")
        || program == Path::new("/usr/local/opt/plug/bin/plug")
        || is_formula_cellar_binary
}

pub fn classify_launchd_program(
    job: &LaunchdJobRecord,
    current_app_executable: Option<&Path>,
) -> LaunchdProgramOwnership {
    if current_app_executable.is_some_and(|current| paths_match(&job.program, current)) {
        LaunchdProgramOwnership::CurrentApp
    } else if is_recognized_legacy_program(&job.program) {
        LaunchdProgramOwnership::RecognizedLegacyPlug
    } else {
        LaunchdProgramOwnership::Unknown
    }
}

fn parse_launchd_labels(output: &str) -> Vec<String> {
    let mut in_services = false;
    let mut labels = Vec::new();
    for line in output.lines() {
        if !in_services {
            in_services = line.trim() == "services = {";
            continue;
        }
        if line == "\t}" || line == "}" {
            break;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() >= 3 && fields[0].parse::<u32>().is_ok() {
            labels.push(fields[fields.len() - 1].to_string());
        }
    }
    labels
}

fn parse_launchd_job(
    label: &str,
    output: &str,
    verified_app_program: Option<&Path>,
) -> Option<LaunchdJobRecord> {
    if let Some(program) = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("program = "))
    {
        let program = PathBuf::from(program.trim_matches('"'));
        let program = std::fs::canonicalize(&program).unwrap_or(program);
        return Some(LaunchdJobRecord {
            label: label.to_string(),
            program,
        });
    }

    let managed_by_service_management = output
        .lines()
        .any(|line| line.trim() == "managed_by = com.apple.xpc.ServiceManagement");
    let parent_is_plug = output
        .lines()
        .any(|line| line.trim() == "parent bundle identifier = com.cyberpapiii.plug");
    let program_identifier_is_plug = output.lines().any(|line| {
        line.trim()
            .strip_prefix("program identifier = ")
            .is_some_and(|value| {
                value == "Contents/Resources/plug" || value.starts_with("Contents/Resources/plug (")
            })
    });
    let arguments = parse_launchd_arguments(output);
    let arguments_identify_daemon = arguments.first().map(String::as_str)
        == Some("Contents/Resources/plug")
        && arguments.get(1).map(String::as_str) == Some("serve")
        && arguments.get(2).map(String::as_str) == Some("--daemon");

    if managed_by_service_management
        && parent_is_plug
        && program_identifier_is_plug
        && arguments_identify_daemon
    {
        let program = verified_app_program?;
        let program = std::fs::canonicalize(program).unwrap_or_else(|_| program.to_path_buf());
        return Some(LaunchdJobRecord {
            label: label.to_string(),
            program,
        });
    }

    None
}

fn parse_launchd_arguments(output: &str) -> Vec<String> {
    let mut in_arguments = false;
    let mut arguments = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if !in_arguments {
            in_arguments = line == "arguments = {";
            continue;
        }
        if line == "}" {
            break;
        }
        if !line.is_empty() {
            arguments.push(line.trim_matches('"').to_string());
        }
    }
    arguments
}

/// Enumerate launchd jobs for diagnosis only. Startup remains authoritative
/// only for the fixed `com.plug.daemon` label.
#[allow(dead_code)] // Consumed by unified installation diagnosis in the next rollout task.
pub fn discover_launchd_jobs() -> anyhow::Result<Vec<LaunchdJobRecord>> {
    let uid = user_id()?;
    let domain = format!("gui/{uid}");
    let output = Command::new("launchctl")
        .args(["print", &domain])
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "unable to inspect user launchd domain: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let labels = parse_launchd_labels(&String::from_utf8_lossy(&output.stdout));
    let verified_app_program = crate::install::resolve_verified_app()
        .ok()
        .flatten()
        .map(|app| app.executable_path);
    let mut jobs = Vec::new();
    for label in labels {
        let output = Command::new("launchctl")
            .args(["print", &format!("{domain}/{label}")])
            .output()?;
        if output.status.success()
            && let Some(job) = parse_launchd_job(
                &label,
                &String::from_utf8_lossy(&output.stdout),
                verified_app_program.as_deref(),
            )
        {
            jobs.push(job);
        }
    }
    Ok(jobs)
}

pub fn classify_launchctl_output(output: &str, cli_plist_exists: bool) -> ServiceOwnership {
    let managed_by_service_management = output
        .lines()
        .any(|line| line.trim() == "managed_by = com.apple.xpc.ServiceManagement");
    let parent_is_plug = output
        .lines()
        .any(|line| line.trim() == "parent bundle identifier = com.cyberpapiii.plug");
    let program_identifier_is_plug = output.lines().any(|line| {
        line.trim()
            .strip_prefix("program identifier = ")
            .is_some_and(|value| {
                value == "Contents/Resources/plug" || value.starts_with("Contents/Resources/plug (")
            })
    });
    let arguments = parse_launchd_arguments(output);
    let arguments_identify_daemon = arguments.first().map(String::as_str)
        == Some("Contents/Resources/plug")
        && arguments.get(1).map(String::as_str) == Some("serve")
        && arguments.get(2).map(String::as_str) == Some("--daemon");

    if managed_by_service_management
        && parent_is_plug
        && program_identifier_is_plug
        && arguments_identify_daemon
    {
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
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(ServiceState {
        ownership: classify_launchctl_output(&text, plist_path.exists()),
        loaded: output.status.success(),
        plist_path,
    })
}

fn map_service_ownership(ownership: ServiceOwnership) -> plug_core::ipc::DaemonOwnershipMode {
    match ownership {
        ServiceOwnership::AppManaged => plug_core::ipc::DaemonOwnershipMode::AppManaged,
        ServiceOwnership::CliManaged => plug_core::ipc::DaemonOwnershipMode::CliManaged,
        ServiceOwnership::Unmanaged => plug_core::ipc::DaemonOwnershipMode::Unmanaged,
    }
}

pub fn ipc_ownership() -> plug_core::ipc::DaemonOwnershipMode {
    match inspect() {
        Err(_) => plug_core::ipc::DaemonOwnershipMode::Unknown,
        Ok(state) => map_service_ownership(state.ownership),
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
    #[cfg(test)]
    if crate::daemon::test_runtime_paths_active() {
        anyhow::bail!("test daemon unavailable; refusing to mutate launchd");
    }
    let uid = user_id()?;
    let state = inspect()?;
    let verified_app = crate::install::resolve_verified_app()?;
    match startup_action(verified_app.is_some(), state.ownership) {
        StartupAction::KickstartApp => {
            launchctl(&[
                "kickstart".into(),
                "-k".into(),
                format!("gui/{uid}/{LABEL}"),
            ])?;
        }
        StartupAction::RepairCli => {
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
        StartupAction::InstallCli => {
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
        StartupAction::OpenApp => {
            let app = verified_app.expect("app-owned startup requires a verified app");
            let output = Command::new("/usr/bin/open")
                .args(["-gj"])
                .arg(&app.bundle_path)
                .output()?;
            if !output.status.success() {
                anyhow::bail!(
                    "Plug.app owns daemon startup but could not be opened: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
        }
    }
    for delay in [100, 200, 400, 800, 1_600, 2_500, 2_500] {
        if crate::daemon::connect_to_daemon().await.is_some() {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    anyhow::bail!(
        "daemon did not start; open Plug.app to finish setup, then view {}",
        crate::daemon::log_dir().display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_app_never_creates_or_repairs_cli_service() {
        assert_eq!(
            startup_action(true, ServiceOwnership::AppManaged),
            StartupAction::KickstartApp
        );
        assert_eq!(
            startup_action(true, ServiceOwnership::CliManaged),
            StartupAction::OpenApp
        );
        assert_eq!(
            startup_action(true, ServiceOwnership::Unmanaged),
            StartupAction::OpenApp
        );
        assert_eq!(
            startup_action(false, ServiceOwnership::CliManaged),
            StartupAction::RepairCli
        );
        assert_eq!(
            startup_action(false, ServiceOwnership::Unmanaged),
            StartupAction::InstallCli
        );
    }

    #[test]
    fn alternate_label_targeting_current_app_is_recognized() {
        let app_executable = Path::new("/Applications/Plug.app/Contents/Resources/plug");
        let job = LaunchdJobRecord {
            label: "local.claude-rc.plug".to_string(),
            program: app_executable.to_path_buf(),
        };

        assert_eq!(
            classify_launchd_program(&job, Some(app_executable)),
            LaunchdProgramOwnership::CurrentApp
        );
    }

    #[test]
    fn alternate_label_targeting_recognized_legacy_plug_is_recognized() {
        let cargo_program = dirs::home_dir().unwrap().join(".cargo/bin/plug");
        let job = LaunchdJobRecord {
            label: "local.claude-rc.plug".to_string(),
            program: cargo_program,
        };

        assert_eq!(
            classify_launchd_program(&job, None),
            LaunchdProgramOwnership::RecognizedLegacyPlug
        );
    }

    #[test]
    fn lookalike_legacy_paths_are_unknown() {
        for program in [
            "/tmp/attacker/.cargo/bin/plug",
            "/tmp/Plug.app/Contents/Resources/plug",
            "/tmp/project/target/release/plug",
        ] {
            let job = LaunchdJobRecord {
                label: "com.plug.daemon".to_string(),
                program: PathBuf::from(program),
            };
            assert_eq!(
                classify_launchd_program(&job, None),
                LaunchdProgramOwnership::Unknown,
                "lookalike path must not prove ownership: {program}"
            );
        }
    }

    #[test]
    fn resolved_homebrew_formula_program_is_recognized() {
        let job = LaunchdJobRecord {
            label: "legacy.plug".to_string(),
            program: PathBuf::from("/opt/homebrew/Cellar/plug/0.6.3/bin/plug"),
        };

        assert_eq!(
            classify_launchd_program(&job, None),
            LaunchdProgramOwnership::RecognizedLegacyPlug
        );
    }

    #[test]
    fn canonical_label_targeting_unrelated_software_is_unknown() {
        let job = LaunchdJobRecord {
            label: LABEL.to_string(),
            program: PathBuf::from("/Applications/Other.app/Contents/MacOS/other"),
        };

        assert_eq!(
            classify_launchd_program(&job, None),
            LaunchdProgramOwnership::Unknown
        );
    }

    #[test]
    fn opt_plug_program_is_recognized() {
        assert!(is_recognized_legacy_program(Path::new(
            "/opt/homebrew/opt/plug/bin/plug"
        )));
        assert!(is_recognized_legacy_program(Path::new(
            "/usr/local/opt/plug/bin/plug"
        )));
    }

    #[test]
    fn homebrew_bin_and_local_bin_programs_are_recognized() {
        assert!(is_recognized_legacy_program(Path::new(
            "/opt/homebrew/bin/plug"
        )));
        assert!(is_recognized_legacy_program(Path::new(
            "/usr/local/bin/plug"
        )));
        let local_bin = dirs::home_dir().unwrap().join(".local/bin/plug");
        let job = LaunchdJobRecord {
            label: "legacy.plug".to_string(),
            program: local_bin.clone(),
        };
        assert!(is_recognized_legacy_program(&local_bin));
        assert_eq!(
            classify_launchd_program(&job, None),
            LaunchdProgramOwnership::RecognizedLegacyPlug
        );
        let brew_bin = LaunchdJobRecord {
            label: "legacy.plug".to_string(),
            program: PathBuf::from("/opt/homebrew/bin/plug"),
        };
        assert_eq!(
            classify_launchd_program(&brew_bin, None),
            LaunchdProgramOwnership::RecognizedLegacyPlug
        );
    }

    #[test]
    fn inspect_error_maps_to_unknown_not_unmanaged() {
        let ownership = match Result::<ServiceState, anyhow::Error>::Err(anyhow::anyhow!(
            "launchctl unavailable"
        )) {
            Err(_) => plug_core::ipc::DaemonOwnershipMode::Unknown,
            Ok(state) => map_service_ownership(state.ownership),
        };
        assert_eq!(ownership, plug_core::ipc::DaemonOwnershipMode::Unknown);
        assert_ne!(ownership, plug_core::ipc::DaemonOwnershipMode::Unmanaged);
    }

    #[test]
    fn recognized_legacy_programs_match_shared_fixture() {
        #[derive(serde::Deserialize)]
        struct Fixture {
            recognized: Vec<String>,
            home_recognized_suffixes: Vec<String>,
            unrecognized: Vec<String>,
        }

        let fixture: Fixture =
            serde_json::from_str(include_str!("../../testdata/legacy_plug_programs.json"))
                .expect("shared leftover-path fixture");
        for path in fixture.recognized {
            assert!(
                is_recognized_legacy_program(Path::new(&path)),
                "expected recognized: {path}"
            );
        }
        let home = dirs::home_dir().expect("home directory");
        for suffix in fixture.home_recognized_suffixes {
            let path = home.join(&suffix);
            assert!(
                is_recognized_legacy_program(&path),
                "expected recognized: {}",
                path.display()
            );
        }
        for path in fixture.unrecognized {
            assert!(
                !is_recognized_legacy_program(Path::new(&path)),
                "expected unrecognized: {path}"
            );
        }
    }

    #[test]
    fn launchd_domain_parser_collects_labels_without_filtering_names() {
        let output = "\n\tservices = {\n\t\t  321  -  local.claude-rc.plug\n\t\t    0  0  com.plug.daemon\n\t\t  999  -  unrelated.job\n\t}\n";
        assert_eq!(
            parse_launchd_labels(output),
            vec![
                "local.claude-rc.plug".to_string(),
                "com.plug.daemon".to_string(),
                "unrelated.job".to_string(),
            ]
        );
    }

    #[test]
    fn launchd_job_parser_uses_program_evidence() {
        let output =
            "gui/501/local.claude-rc.plug = {\n\tprogram = /Users/example/.cargo/bin/plug\n}";
        assert_eq!(
            parse_launchd_job("local.claude-rc.plug", output, None),
            Some(LaunchdJobRecord {
                label: "local.claude-rc.plug".to_string(),
                program: PathBuf::from("/Users/example/.cargo/bin/plug"),
            })
        );
        assert_eq!(
            parse_launchd_job("unrelated.job", "state = running", None),
            None
        );
    }

    #[test]
    fn launchd_job_parser_resolves_verified_smappservice_program() {
        let output = r#"gui/501/com.plug.daemon = {
	active count = 1
	path = (submitted by smd.338)
	type = Submitted
	managed_by = com.apple.xpc.ServiceManagement
	state = running

	program identifier = Contents/Resources/plug (mode: 2)
	parent bundle identifier = com.cyberpapiii.plug
	parent bundle version = 12
	arguments = {
		Contents/Resources/plug
		serve
		--daemon
	}
}"#;
        let app_program = Path::new("/Applications/Plug.app/Contents/Resources/plug");

        assert_eq!(
            parse_launchd_job("com.plug.daemon", output, Some(app_program)),
            Some(LaunchdJobRecord {
                label: "com.plug.daemon".to_string(),
                program: app_program.to_path_buf(),
            })
        );
    }

    #[test]
    fn launchd_job_parser_rejects_unproven_relative_program() {
        let output = r#"managed_by = com.apple.xpc.ServiceManagement
program identifier = Contents/Resources/plug (mode: 2)
parent bundle identifier = com.example.other
arguments = {
	Contents/Resources/plug
	serve
	--daemon
}"#;
        let app_program = Path::new("/Applications/Plug.app/Contents/Resources/plug");

        assert_eq!(
            parse_launchd_job("com.plug.daemon", output, Some(app_program)),
            None
        );
    }

    #[test]
    fn embedded_cli_path_without_service_management_is_not_app_owned() {
        assert_eq!(
            classify_launchctl_output(
                "program = /Applications/Plug.app/Contents/Resources/plug",
                true
            ),
            ServiceOwnership::CliManaged
        );
    }

    #[test]
    fn app_service_is_recognized_from_real_launchctl_shape() {
        assert_eq!(
            classify_launchctl_output(
                "managed_by = com.apple.xpc.ServiceManagement\nparent bundle identifier = com.cyberpapiii.plug\nprogram identifier = Contents/Resources/plug (mode: 2)\narguments = {\nContents/Resources/plug\nserve\n--daemon\n}",
                false
            ),
            ServiceOwnership::AppManaged
        );
    }

    #[test]
    fn partial_service_management_metadata_is_not_app_owned() {
        assert_eq!(
            classify_launchctl_output(
                "managed_by = com.apple.xpc.ServiceManagement\nparent bundle identifier = com.cyberpapiii.plug\nprogram = /Applications/Plug.app/Contents/Resources/plug",
                false
            ),
            ServiceOwnership::CliManaged
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

    #[tokio::test]
    async fn redirected_test_runtime_never_mutates_launchd() {
        let _guard = crate::daemon::runtime_paths_test_lock().lock().await;
        let temp =
            std::env::temp_dir().join(format!("plug-service-launchd-guard-{}", std::process::id()));
        crate::daemon::set_test_runtime_paths(temp.join("runtime"), temp.join("state"));

        let result = ensure_started(None).await;

        crate::daemon::clear_test_runtime_paths();
        let error = result.expect_err("test startup must fail closed");
        assert!(
            error.to_string().contains("refusing to mutate launchd"),
            "unexpected error: {error:#}"
        );
    }
}
