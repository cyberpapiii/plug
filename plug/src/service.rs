//! Canonical launchd ownership for the shared daemon.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::legacy_programs::is_recognized_legacy_program;
use plug_core::paths::{
    APP_BUNDLE_ID, BUNDLED_BINARY, DAEMON_ARGS, DAEMON_LABEL, SERVICE_DISABLED_MARKER,
    STARTUP_BUDGET,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LaunchdJobRecord {
    pub label: String,
    pub program: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchdProgramOwnership {
    CurrentApp,
    RecognizedLegacyPlug,
    Unknown,
}

/// CLI leftover-launchd sentence. The app leftover path still uses the generic
/// "Background running is off" / Turn On verdict, not this string.
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
    InstallCli,
}

fn startup_action(app_installed: bool, ownership: ServiceOwnership) -> StartupAction {
    match (app_installed, ownership) {
        (_, ServiceOwnership::AppManaged) => StartupAction::KickstartApp,
        (true, ServiceOwnership::CliManaged | ServiceOwnership::Unmanaged) => {
            StartupAction::OpenApp
        }
        (false, ServiceOwnership::CliManaged | ServiceOwnership::Unmanaged) => {
            StartupAction::InstallCli
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServiceState {
    pub ownership: ServiceOwnership,
    pub loaded: bool,
    pub plist_path: PathBuf,
}

/// How long one `launchctl` or `id` call may take. A wedged launchd answers
/// nothing at all, and every caller here would otherwise hang with it.
const LAUNCHCTL_TIMEOUT: Duration = Duration::from_secs(5);

fn login_user_id() -> anyhow::Result<String> {
    let mut command = Command::new("/usr/bin/id");
    command.arg("-u");
    let output = crate::install::bounded_output(command, LAUNCHCTL_TIMEOUT)?;
    if !output.status.success() {
        anyhow::bail!("unable to determine login user id");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn launchctl_output(args: &[&str], timeout: Duration) -> anyhow::Result<std::process::Output> {
    let mut command = Command::new("/bin/launchctl");
    command.args(args);
    crate::install::bounded_output(command, timeout)
}

fn launchctl(args: &[&str]) -> anyhow::Result<()> {
    let output = launchctl_output(args, LAUNCHCTL_TIMEOUT)?;
    if !output.status.success() {
        anyhow::bail!(
            "launchctl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Unregister one job from the login session's launchd domain.
pub fn bootout(label: &str) -> anyhow::Result<()> {
    launchctl(&["bootout", &format!("gui/{}/{label}", login_user_id()?)])
}

fn cli_plist_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/LaunchAgents")
        .join(format!("{DAEMON_LABEL}.plist"))
}

pub fn classify_launchd_program(
    job: &LaunchdJobRecord,
    current_app_executable: Option<&Path>,
) -> LaunchdProgramOwnership {
    if current_app_executable
        .is_some_and(|current| crate::install::paths_match(&job.program, current))
    {
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

/// Whether `launchctl print` output describes the daemon job Plug.app owns.
///
/// All four conditions must hold together. `managed_by` and the parent bundle
/// identifier prove SMAppService registered the job on behalf of the app, and
/// the program identifier and argument vector prove the job is the daemon
/// rather than some other executable inside the bundle. A leftover Homebrew or
/// cargo job satisfies none of them, which is the distinction the adopt path
/// depends on.
///
/// This is the single definition on purpose. It used to be copied into
/// `parse_launchd_job` and `classify_launchctl_output`, and two copies of the
/// ownership test that can drift is exactly how the daemon and the app end up
/// disagreeing about who owns `com.plug.daemon`.
fn app_managed_launchd_evidence(output: &str) -> bool {
    let managed_by_service_management = output
        .lines()
        .any(|line| line.trim() == "managed_by = com.apple.xpc.ServiceManagement");
    let parent_is_plug = output
        .lines()
        .any(|line| line.trim().strip_prefix("parent bundle identifier = ") == Some(APP_BUNDLE_ID));
    let program_identifier_is_plug = output.lines().any(|line| {
        line.trim()
            .strip_prefix("program identifier = ")
            .is_some_and(|value| {
                value == BUNDLED_BINARY
                    || value
                        .strip_prefix(BUNDLED_BINARY)
                        .is_some_and(|rest| rest.starts_with(" ("))
            })
    });
    let arguments = parse_launchd_arguments(output);
    let arguments_identify_daemon = arguments.first().map(String::as_str) == Some(BUNDLED_BINARY)
        && arguments[1..] == DAEMON_ARGS;

    managed_by_service_management
        && parent_is_plug
        && program_identifier_is_plug
        && arguments_identify_daemon
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

    if app_managed_launchd_evidence(output) {
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

/// Enumerate launchd jobs for diagnosis and uninstall, within one time budget
/// for the whole walk. Startup remains authoritative only for the fixed
/// `com.plug.daemon` label.
pub fn discover_launchd_jobs(
    verified_app_program: Option<&Path>,
    budget: Duration,
) -> anyhow::Result<Vec<LaunchdJobRecord>> {
    let deadline = std::time::Instant::now() + budget;
    let remaining = || {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            anyhow::bail!(
                "launchd inspection timed out after {}ms",
                budget.as_millis()
            );
        }
        Ok(left)
    };
    let domain = format!("gui/{}", login_user_id()?);
    let output = launchctl_output(&["print", &domain], remaining()?)?;
    if !output.status.success() {
        anyhow::bail!(
            "unable to inspect user launchd domain: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let labels = parse_launchd_labels(&String::from_utf8_lossy(&output.stdout));
    let mut jobs = Vec::new();
    for label in labels {
        let output = launchctl_output(&["print", &format!("{domain}/{label}")], remaining()?)?;
        if output.status.success()
            && let Some(job) = parse_launchd_job(
                &label,
                &String::from_utf8_lossy(&output.stdout),
                verified_app_program,
            )
        {
            jobs.push(job);
        }
    }
    Ok(jobs)
}

/// Whether a launchd job is one plug could own.
///
/// Matching a label that merely *ends* in `.plug` sweeps in jobs that only
/// mention plug: `local.claude-rc.plug` runs `claude remote-control` with this
/// repo as its working directory, and `plug doctor` reported it as an unknown
/// job on every run. A job plug owns either sits in plug's own label namespace
/// or runs a binary called `plug`; the historical labels (`com.plug.daemon`,
/// `com.plug.serve`) are both inside that namespace.
pub fn is_plug_launchd_candidate(job: &LaunchdJobRecord) -> bool {
    job.label.to_ascii_lowercase().starts_with("com.plug.")
        || job.program.file_name().and_then(|name| name.to_str()) == Some("plug")
}

pub fn classify_launchctl_output(output: &str, cli_plist_exists: bool) -> ServiceOwnership {
    if app_managed_launchd_evidence(output) {
        ServiceOwnership::AppManaged
    } else if !output.trim().is_empty() || cli_plist_exists {
        ServiceOwnership::CliManaged
    } else {
        ServiceOwnership::Unmanaged
    }
}

pub fn inspect() -> anyhow::Result<ServiceState> {
    let uid = login_user_id()?;
    let plist_path = cli_plist_path();
    let output = launchctl_output(
        &["print", &format!("gui/{uid}/{DAEMON_LABEL}")],
        LAUNCHCTL_TIMEOUT,
    )?;
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
        Err(error) => {
            tracing::warn!(%error, "could not read who runs the daemon");
            plug_core::ipc::DaemonOwnershipMode::Unknown
        }
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
    let mut arguments = vec![executable.display().to_string()];
    arguments.extend(DAEMON_ARGS.map(str::to_string));
    if let Some(path) = config_path {
        arguments.push("--config".to_string());
        arguments.push(path.display().to_string());
    }
    let arguments = arguments
        .iter()
        .map(|arg| format!("    <string>{}</string>", xml_escape(arg)))
        .collect::<Vec<_>>()
        .join("\n");
    let log = plug_core::paths::log_dir().join("launchd.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{DAEMON_LABEL}</string>
  <key>ProgramArguments</key><array>
{arguments}
  </array>
  <key>RunAtLoad</key><true/>
  <!-- Restart only on a non-zero exit. `plug stop` exits 0 and stays
       stopped; a panic or a kill self-heals. -->
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
  <!-- Shutdown first lets running tool calls finish, and launchd's own
       default kills an agent about five seconds after SIGTERM. -->
  <key>ExitTimeOut</key><integer>20</integer>
  <!-- Every server, client and stream holds a file handle, and the default
       of 256 runs out on a busy day, after which nothing new connects. -->
  <key>SoftResourceLimits</key><dict><key>NumberOfFiles</key><integer>4096</integer></dict>
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
    std::fs::create_dir_all(plug_core::paths::log_dir())?;
    let contents = render_cli_plist(&executable, config_path);
    let temp = path.with_extension("plist.tmp");
    std::fs::write(&temp, contents)?;
    std::fs::rename(temp, path)?;
    Ok(())
}

/// Wait for the daemon's IPC socket, distinguishing a slow start from a dead one.
///
/// `already_starting` says the caller has already seen the runtime lock held, so
/// a subsequent disappearance of that lock is conclusive rather than the gap
/// before a freshly kickstarted daemon claims it.
async fn wait_for_daemon_socket(started: bool, already_starting: bool) -> anyhow::Result<bool> {
    let deadline = std::time::Instant::now() + STARTUP_BUDGET;
    let mut saw_lock = already_starting;
    let mut delay = Duration::from_millis(100);
    loop {
        if crate::ipc_client::daemon_running().await {
            return Ok(started);
        }
        if crate::ipc_client::daemon_starting() {
            saw_lock = true;
        } else if saw_lock {
            // The lock was held and is now gone with no socket to show for it.
            // The daemon exited during startup, so waiting out the rest of the
            // budget would only delay the report.
            anyhow::bail!(
                "daemon exited during startup; see {}",
                plug_core::paths::log_dir().display()
            );
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_millis(500));
    }
    anyhow::bail!(
        "daemon did not start within {}s; open Plug.app to finish setup, then view {}",
        STARTUP_BUDGET.as_secs(),
        plug_core::paths::log_dir().display()
    )
}

pub async fn ensure_started(config_path: Option<&Path>) -> anyhow::Result<bool> {
    ensure_service_enabled(&plug_core::paths::marker(SERVICE_DISABLED_MARKER))?;
    if crate::ipc_client::daemon_running().await {
        return Ok(false);
    }
    if cfg!(test) {
        anyhow::bail!("test daemon unavailable; refusing to mutate launchd");
    }
    // A held runtime lock with no socket means a daemon claimed the singleton
    // and has not finished binding yet. Kickstarting now would SIGKILL that
    // healthy cold start and begin it again, and a second client arriving would
    // do it once more. Plug.app stopped force-kickstarting inside its
    // verification loop for this reason; the CLI drives the same daemon and
    // needs the same restraint.
    if crate::ipc_client::daemon_starting() {
        return wait_for_daemon_socket(false, true).await;
    }
    let uid = login_user_id()?;
    let job = format!("gui/{uid}/{DAEMON_LABEL}");
    let state = inspect()?;
    let verified_app = crate::install::resolve_verified_app()?;
    match startup_action(verified_app.is_some(), state.ownership) {
        StartupAction::KickstartApp => {
            // No `-k`: a daemon that started a moment ago and has not taken
            // the runtime lock yet would be killed in the middle of starting.
            // Without it launchd starts the job only if it is not running.
            launchctl(&["kickstart", &job])?;
        }
        StartupAction::InstallCli => {
            if state.loaded {
                launchctl(&["bootout", &job])?;
            }
            install_cli_plist(&state.plist_path, config_path)?;
            launchctl(&[
                "bootstrap",
                &format!("gui/{uid}"),
                &state.plist_path.display().to_string(),
            ])?;
            launchctl(&["kickstart", "-k", &job])?;
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
    wait_for_daemon_socket(true, false).await
}

fn ensure_service_enabled(marker: &Path) -> anyhow::Result<()> {
    if marker.try_exists()? {
        anyhow::bail!("Plug is off. Turn it on in Plug.app.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy_programs::{
        LEGACY_CELLAR_ROOTS, LEGACY_EXACT_PROGRAMS, LEGACY_HOME_RELATIVE_PROGRAMS,
    };

    #[test]
    fn intentional_off_blocks_connector_start_until_marker_is_removed() {
        let marker = plug_core::paths::test::scratch("service-off").join(SERVICE_DISABLED_MARKER);
        assert!(ensure_service_enabled(&marker).is_ok());
        std::fs::write(&marker, "").unwrap();
        assert_eq!(
            ensure_service_enabled(&marker).unwrap_err().to_string(),
            "Plug is off. Turn it on in Plug.app."
        );
        std::fs::remove_file(&marker).unwrap();
        assert!(ensure_service_enabled(&marker).is_ok());
    }

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
            StartupAction::InstallCli
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
            label: DAEMON_LABEL.to_string(),
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
    fn recognized_legacy_programs_match_shared_fixture() {
        #[derive(serde::Deserialize)]
        struct Fixture {
            rules: Rules,
            recognized: Vec<String>,
            home_recognized_suffixes: Vec<String>,
            unrecognized: Vec<String>,
        }

        #[derive(serde::Deserialize)]
        struct Rules {
            exact: Vec<String>,
            home_relative: Vec<String>,
            cellar_roots: Vec<String>,
        }

        let fixture: Fixture =
            serde_json::from_str(include_str!("../../testdata/legacy_plug_programs.json"))
                .expect("shared leftover-path fixture");

        // The table, not only the cases: a shape added here but not to the
        // fixture never reaches Plug.app's copy of the same rules.
        assert_eq!(
            fixture.rules.exact,
            LEGACY_EXACT_PROGRAMS.map(str::to_string)
        );
        assert_eq!(
            fixture.rules.home_relative,
            LEGACY_HOME_RELATIVE_PROGRAMS.map(str::to_string)
        );
        assert_eq!(
            fixture.rules.cellar_roots,
            LEGACY_CELLAR_ROOTS.map(str::to_string)
        );
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
    fn plug_launchd_candidates_ignore_unrelated_jobs() {
        let unrelated = LaunchdJobRecord {
            label: "com.apple.XprotectFramework.PluginService".to_string(),
            program: PathBuf::from("/System/Library/XProtectPluginService"),
        };
        // Named after the working directory it runs in, not after plug: the
        // program is `claude`, so this job is nothing plug should report on.
        let claude_remote_control = LaunchdJobRecord {
            label: "local.claude-rc.plug".to_string(),
            program: PathBuf::from("/Users/example/.local/bin/claude"),
        };
        let legacy_serve = LaunchdJobRecord {
            label: "com.plug.serve".to_string(),
            program: PathBuf::from("/Users/example/.cargo/bin/plug"),
        };
        let relabelled = LaunchdJobRecord {
            label: "local.something.else".to_string(),
            program: PathBuf::from("/Users/example/.local/bin/plug"),
        };
        assert!(!is_plug_launchd_candidate(&unrelated));
        assert!(!is_plug_launchd_candidate(&claude_remote_control));
        assert!(is_plug_launchd_candidate(&legacy_serve));
        assert!(is_plug_launchd_candidate(&relabelled));
    }

    #[test]
    fn launchd_discovery_out_of_budget_fails_closed() {
        let error = discover_launchd_jobs(None, Duration::ZERO).unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error}");
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

    /// The samples in `testdata/launchd`, which the app's
    /// `LaunchdJobInspectorTests` read too, so the CLI and the app take the
    /// same job for the same owner.
    #[test]
    fn launchd_samples_read_the_way_the_app_reads_them() {
        let domain = include_str!("../../testdata/launchd/gui_domain.txt");
        assert_eq!(
            parse_launchd_labels(domain),
            [
                "local.claude-rc.plug",
                "com.plug.daemon",
                "com.apple.Finder",
                "homebrew.mxcl.plug"
            ]
        );

        let app_program = Path::new("/Applications/Plug.app/Contents/Resources/plug");
        let app_job = include_str!("../../testdata/launchd/app_service_job.txt");
        assert!(app_managed_launchd_evidence(app_job));
        assert_eq!(
            parse_launchd_job(DAEMON_LABEL, app_job, Some(app_program)).map(|job| job.program),
            Some(app_program.to_path_buf())
        );

        let unmanaged = include_str!("../../testdata/launchd/app_job_not_service_managed.txt");
        assert!(!app_managed_launchd_evidence(unmanaged));
        assert_eq!(
            parse_launchd_job(DAEMON_LABEL, unmanaged, Some(app_program)),
            None
        );

        let legacy = include_str!("../../testdata/launchd/legacy_job.txt");
        let job = parse_launchd_job("homebrew.mxcl.plug", legacy, None).expect("legacy job");
        // Resolved through links when the path is on this Mac, so only its
        // kind is checked.
        assert!(is_recognized_legacy_program(&job.program));
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

    /// The CLI plist and the app plist describe the same launchd job. They are
    /// written in two languages, in two directories, by two different
    /// installers, which is exactly the shape of thing that drifts. Restart
    /// policy is the field that matters most: one half restarting a crashed
    /// daemon while the other leaves it dead is a difference an operator would
    /// experience as "sometimes it comes back".
    #[test]
    fn cli_and_app_plists_agree_on_crash_only_restart() {
        const CRASH_ONLY: &str =
            "<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>";
        // launchd's default of 256 file handles runs out with many servers.
        const FILE_LIMIT: &str = "<key>SoftResourceLimits</key><dict><key>NumberOfFiles</key><integer>4096</integer></dict>";

        let cli = render_cli_plist(Path::new("/tmp/plug"), None);
        assert!(
            cli.contains(CRASH_ONLY),
            "CLI plist lost its crash-only restart policy"
        );
        assert!(cli.contains(FILE_LIMIT), "CLI plist lost its file limit");

        let app_plist = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../PlugApp/PlugApp/Resources/com.plug.daemon.plist");
        let app = std::fs::read_to_string(&app_plist)
            .unwrap_or_else(|error| panic!("read {}: {error}", app_plist.display()));
        assert!(
            app.contains(CRASH_ONLY),
            "app plist lost its crash-only restart policy"
        );
        assert!(app.contains(FILE_LIMIT), "app plist lost its file limit");

        // launchd kills the daemon `ExitTimeOut` seconds after SIGTERM, so
        // the call drain and the HTTP grace that follows it must fit inside.
        let exit_timeout = |plist: &str| -> Duration {
            let value = plist
                .split("<key>ExitTimeOut</key><integer>")
                .nth(1)
                .and_then(|rest| rest.split('<').next())
                .and_then(|seconds| seconds.parse().ok())
                .expect("plist sets ExitTimeOut");
            Duration::from_secs(value)
        };
        assert_eq!(exit_timeout(&cli), exit_timeout(&app));
        let shutdown = crate::daemon::CALL_DRAIN_TIMEOUT + crate::runtime::HTTP_SHUTDOWN_GRACE;
        assert!(
            shutdown < exit_timeout(&app),
            "shutdown takes up to {shutdown:?}, past launchd's ExitTimeOut"
        );
    }

    /// The app registers the job that `app_managed_launchd_evidence` must
    /// recognize as its own; a label, path or argument that differs makes the
    /// app's daemon read as one the CLI started.
    #[test]
    fn app_plist_runs_the_job_service_recognizes() {
        let app_plist = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../PlugApp/PlugApp/Resources/com.plug.daemon.plist");
        let app = std::fs::read_to_string(&app_plist)
            .unwrap_or_else(|error| panic!("read {}: {error}", app_plist.display()));
        let arguments: String = std::iter::once(BUNDLED_BINARY)
            .chain(DAEMON_ARGS)
            .map(|argument| format!("<string>{argument}</string>"))
            .collect();
        for expected in [
            format!("<key>Label</key><string>{DAEMON_LABEL}</string>"),
            format!("<key>BundleProgram</key><string>{BUNDLED_BINARY}</string>"),
            arguments,
        ] {
            assert!(app.contains(&expected), "app plist lacks {expected}");
        }
    }

    /// A daemon that claimed the runtime lock and then exited without binding a
    /// socket has failed, and the caller should hear that immediately rather
    /// than after the full cold-start budget.
    #[tokio::test]
    async fn wait_reports_a_daemon_that_died_during_startup() {
        let started = std::time::Instant::now();
        let error = wait_for_daemon_socket(false, true)
            .await
            .expect_err("a vanished startup lock must not be reported as success");

        assert!(
            error.to_string().contains("exited during startup"),
            "unexpected error: {error}"
        );
        assert!(
            started.elapsed() < STARTUP_BUDGET,
            "a dead startup should not consume the whole cold-start budget"
        );
    }

    /// The socket appearing is the only success signal, and it is what a slow
    /// cold start eventually produces.
    #[tokio::test]
    async fn wait_succeeds_once_the_socket_appears() {
        let socket = plug_core::paths::socket_path();
        std::fs::create_dir_all(socket.parent().expect("socket has a parent"))
            .expect("create runtime dir");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");

        let started = wait_for_daemon_socket(true, false).await;

        drop(listener);

        assert!(started.expect("socket present means started"));
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
    async fn a_test_never_mutates_launchd() {
        let result = ensure_started(None).await;
        let error = result.expect_err("test startup must fail closed");
        assert!(
            error.to_string().contains("refusing to mutate launchd"),
            "unexpected error: {error:#}"
        );
    }
}
