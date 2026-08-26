use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use serde::Serialize;

pub const APP_BUNDLE_ID: &str = "com.cyberpapiii.plug";
pub const DEVELOPER_TEAM_ID: &str = "HJF7LN64XX";

const LOOP_MARKER: &str = "PLUG_APP_EXEC";
const BUNDLE_EXECUTABLE: &str = "Contents/Resources/plug";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VerifiedAppInstallation {
    pub bundle_path: PathBuf,
    pub executable_path: PathBuf,
    pub version: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterVersionState {
    Current,
    CompatibleOlder,
    Missing,
    Incompatible,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ShadowInstallation {
    pub kind: String,
    pub path: PathBuf,
    pub verified_plug_owned: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct UnifiedInstallSnapshot {
    pub app: Option<VerifiedAppInstallation>,
    pub shell_resolution: Option<PathBuf>,
    pub daemon_version: Option<String>,
    pub daemon_executable: Option<PathBuf>,
    pub ownership: plug_core::ipc::DaemonOwnershipMode,
    pub linked_clients: Vec<crate::commands::clients::ClientRepairItem>,
    pub adapters: Vec<AdapterVersionState>,
    pub shadows: Vec<ShadowInstallation>,
    pub launchd_jobs: Vec<crate::service::LaunchdJobRecord>,
    pub daemon_inspection_complete: bool,
    pub adapter_inspection_complete: bool,
    pub launchd_inspection_complete: bool,
    pub app_service_daemon_invocation_verified: bool,
    pub inspection_errors: Vec<String>,
    pub client_repair_needed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UninstallCleanupItem {
    pub kind: String,
    pub path: Option<PathBuf>,
    pub label: Option<String>,
    pub changed: bool,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UninstallCleanupReport {
    pub items: Vec<UninstallCleanupItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CleanupLaunchdPlan {
    remove_labels: Vec<String>,
    preserved: Vec<UninstallCleanupItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DelegationDecision {
    Stay,
    Exec(PathBuf),
}

/// Resolve the signed Plug.app that owns the public macOS command line.
///
/// Non-macOS builds intentionally have no app-owned installation.
pub fn resolve_verified_app() -> Result<Option<VerifiedAppInstallation>> {
    #[cfg(target_os = "macos")]
    {
        let mut candidates = registered_bundle_paths();
        for fallback in fallback_bundle_paths() {
            if !candidates.contains(&fallback) {
                candidates.push(fallback);
            }
        }

        for bundle_path in candidates {
            if bundle_path.exists() {
                return verify_app_bundle(&bundle_path).map(Some);
            }
        }
        Ok(None)
    }

    #[cfg(not(target_os = "macos"))]
    Ok(None)
}

/// Return the app-owned client command when Plug.app is available, otherwise
/// preserve the current executable only for development and Linux installs.
pub fn canonical_client_command() -> Result<PathBuf> {
    let app = resolve_verified_app()?;
    let current = std::env::current_exe().context("could not resolve the running executable")?;
    #[cfg(target_os = "macos")]
    let production_macos = true;
    #[cfg(not(target_os = "macos"))]
    let production_macos = false;
    let dev = std::env::var_os("PLUG_DEV").as_deref() == Some(OsStr::new("1"));
    canonical_client_command_from(app.as_ref(), &current, dev, production_macos)
}

fn canonical_client_command_from(
    app: Option<&VerifiedAppInstallation>,
    current: &Path,
    dev: bool,
    production_macos: bool,
) -> Result<PathBuf> {
    if let Some(app) = app {
        return Ok(app.executable_path.clone());
    }
    if !production_macos || dev {
        return Ok(current.to_path_buf());
    }
    anyhow::bail!(
        "no verified Plug.app is available; install or open Plug.app, or set PLUG_DEV=1 for source development"
    )
}

#[allow(dead_code)] // Exposed for focused decision tests and diagnostic callers.
pub fn delegation_decision(
    current: &Path,
    app: Option<&VerifiedAppInstallation>,
    dev: bool,
) -> Result<DelegationDecision> {
    delegation_decision_with_loop(current, app, dev, false)
}

fn delegation_decision_with_loop(
    current: &Path,
    app: Option<&VerifiedAppInstallation>,
    dev: bool,
    loop_marker: bool,
) -> Result<DelegationDecision> {
    let Some(app) = app else {
        return Ok(DelegationDecision::Stay);
    };
    if dev {
        return Ok(DelegationDecision::Stay);
    }

    // The marker is intentionally not an externally usable bypass: a forged
    // marker on a standalone executable still delegates. The canonical path
    // comparison is the durable loop guard after exec.
    if loop_marker && paths_match(current, &app.executable_path) {
        return Ok(DelegationDecision::Stay);
    }

    if paths_match(current, &app.executable_path) {
        return Ok(DelegationDecision::Stay);
    }

    Ok(DelegationDecision::Exec(app.executable_path.clone()))
}

/// Re-exec a standalone macOS command through a freshly verified Plug.app.
/// This must run before dotenv loading and Clap parsing.
pub fn maybe_delegate_to_app() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let dev = std::env::var_os("PLUG_DEV").as_deref() == Some(OsStr::new("1"));
        if dev {
            return Ok(());
        }

        let app = resolve_verified_app()?;
        let current =
            std::env::current_exe().context("could not resolve the running executable")?;
        let loop_marker = std::env::var_os(LOOP_MARKER).is_some();

        match delegation_decision_with_loop(&current, app.as_ref(), false, loop_marker)? {
            DelegationDecision::Stay => Ok(()),
            DelegationDecision::Exec(executable) => {
                use std::{io, os::unix::process::CommandExt, process::Stdio};

                let mut command = Command::new(&executable);
                command
                    .args(std::env::args_os().skip(1))
                    .env(LOOP_MARKER, "1")
                    .stdin(Stdio::inherit())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit());
                if let Ok(cwd) = std::env::current_dir() {
                    command.current_dir(cwd);
                }

                let error: io::Error = command.exec();
                Err(error).with_context(|| {
                    format!(
                        "could not delegate to the verified Plug.app executable {}",
                        executable.display()
                    )
                })
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    Ok(())
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

fn bounded_output(
    mut command: Command,
    timeout: std::time::Duration,
) -> Result<std::process::Output> {
    use std::process::Stdio;
    use std::time::Instant;

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().context("could not start bounded command")?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return child
                .wait_with_output()
                .context("could not collect command output");
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("command timed out after {}ms", timeout.as_millis());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Resolve the command a normal interactive login shell will execute.
pub fn resolve_login_shell_command() -> Result<Option<PathBuf>> {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| OsStr::new("/bin/zsh").to_owned());
    let mut command = Command::new(shell);
    command.args(["-lic", "command -v plug"]);
    let output = bounded_output(command, std::time::Duration::from_secs(2))?;
    if !output.status.success() {
        return Ok(None);
    }
    let output =
        String::from_utf8(output.stdout).context("shell command resolution was not UTF-8")?;
    let path = output
        .lines()
        .last()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    Ok(path.map(PathBuf::from))
}

pub fn classify_adapter_version(version: Option<&str>, current: &str) -> AdapterVersionState {
    let Some(version) = version else {
        return AdapterVersionState::Missing;
    };
    if version == current {
        return AdapterVersionState::Current;
    }
    match (numeric_version(version), numeric_version(current)) {
        (Some(found), Some(expected)) if found < expected => AdapterVersionState::CompatibleOlder,
        _ => AdapterVersionState::Incompatible,
    }
}

fn numeric_version(version: &str) -> Option<Vec<u64>> {
    version
        .split('.')
        .map(|part| {
            part.split_once('-')
                .map_or(part, |(number, _)| number)
                .parse()
                .ok()
        })
        .collect()
}

fn verify_shadow_identity(path: &Path) -> bool {
    let mut command = Command::new(path);
    command.arg("--version");
    bounded_output(command, std::time::Duration::from_secs(2))
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|output| {
            output
                .trim()
                .strip_prefix("plug ")
                .is_some_and(|version| numeric_version(version).is_some())
        })
}

/// Inspect only recognized legacy installation locations. Unknown contents are
/// reported with `verified_plug_owned = false` and never mutated here.
pub fn discover_shadow_installations(
    app: Option<&VerifiedAppInstallation>,
) -> Vec<ShadowInstallation> {
    let mut candidates = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(("cargo", home.join(".cargo/bin/plug")));
    }
    candidates.push(("homebrew", PathBuf::from("/opt/homebrew/bin/plug")));
    candidates.push(("homebrew", PathBuf::from("/usr/local/bin/plug")));
    for cellar in ["/opt/homebrew/Cellar/plug", "/usr/local/Cellar/plug"] {
        if let Ok(versions) = std::fs::read_dir(cellar) {
            for version in versions.flatten() {
                candidates.push(("homebrew_formula", version.path().join("bin/plug")));
            }
        }
    }

    candidates.sort_by(|left, right| left.1.cmp(&right.1));
    candidates.dedup_by(|left, right| left.1 == right.1);
    candidates
        .into_iter()
        .filter(|(_, path)| path.exists())
        .filter(|(_, path)| !app.is_some_and(|app| paths_match(path, &app.executable_path)))
        .map(|(kind, path)| ShadowInstallation {
            verified_plug_owned: verify_shadow_identity(&path),
            kind: kind.to_string(),
            path,
        })
        .collect()
}

fn cleanup_command_link(
    path: &Path,
    app: Option<&VerifiedAppInstallation>,
) -> Result<UninstallCleanupItem> {
    let base = UninstallCleanupItem {
        kind: "command_link".to_string(),
        path: Some(path.to_path_buf()),
        label: None,
        changed: false,
        message: String::new(),
    };
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(UninstallCleanupItem {
            message: "Command link was absent; nothing changed.".to_string(),
            ..base
        });
    };
    if !metadata.file_type().is_symlink() {
        return Ok(UninstallCleanupItem {
            message: "Unknown command file was left untouched.".to_string(),
            ..base
        });
    }
    let Some(app) = app else {
        return Ok(UninstallCleanupItem {
            message: "Command link ownership could not be proven; left untouched.".to_string(),
            ..base
        });
    };
    let target = std::fs::read_link(path)?;
    let resolved = if target.is_absolute() {
        target
    } else {
        path.parent().unwrap_or_else(|| Path::new(".")).join(target)
    };
    if !paths_match(&resolved, &app.executable_path) {
        return Ok(UninstallCleanupItem {
            message: "Command link does not target the verified Plug.app; left untouched."
                .to_string(),
            ..base
        });
    }
    std::fs::remove_file(path)?;
    Ok(UninstallCleanupItem {
        changed: true,
        message: "Removed the verified Plug.app command link.".to_string(),
        ..base
    })
}

fn cleanup_launchd_plan(
    jobs: &[crate::service::LaunchdJobRecord],
    app: Option<&VerifiedAppInstallation>,
    proven_daemon_invocations: &std::collections::BTreeSet<String>,
) -> CleanupLaunchdPlan {
    let mut remove_labels = Vec::new();
    let mut preserved = Vec::new();
    for job in jobs {
        if job.label == "com.plug.daemon"
            && proven_daemon_invocations.contains(&job.label)
            && matches!(
                crate::service::classify_launchd_program(
                    job,
                    app.map(|app| app.executable_path.as_path())
                ),
                crate::service::LaunchdProgramOwnership::CurrentApp
            )
        {
            remove_labels.push(job.label.clone());
        } else {
            preserved.push(UninstallCleanupItem {
                kind: "launchd_job".to_string(),
                path: Some(job.program.clone()),
                label: Some(job.label.clone()),
                changed: false,
                message: "Launchd job ownership was not proven app-owned; left untouched."
                    .to_string(),
            });
        }
    }
    CleanupLaunchdPlan {
        remove_labels,
        preserved,
    }
}

pub fn is_plug_launchd_candidate(job: &crate::service::LaunchdJobRecord) -> bool {
    job.label == "com.plug.daemon"
        || job.label.to_ascii_lowercase().ends_with(".plug")
        || job.program.file_name().and_then(|name| name.to_str()) == Some("plug")
}

fn receive_launchd_discovery(
    receiver: std::sync::mpsc::Receiver<anyhow::Result<Vec<crate::service::LaunchdJobRecord>>>,
    timeout: std::time::Duration,
) -> Result<Vec<crate::service::LaunchdJobRecord>> {
    receiver
        .recv_timeout(timeout)
        .map_err(|error| match error {
            std::sync::mpsc::RecvTimeoutError::Timeout => {
                anyhow::anyhow!(
                    "launchd inspection timed out after {}ms",
                    timeout.as_millis()
                )
            }
            std::sync::mpsc::RecvTimeoutError::Disconnected => {
                anyhow::anyhow!("launchd inspection worker stopped unexpectedly")
            }
        })?
}

pub fn discover_launchd_jobs_bounded(
    timeout: std::time::Duration,
) -> Result<Vec<crate::service::LaunchdJobRecord>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(crate::service::discover_launchd_jobs());
    });
    receive_launchd_discovery(receiver, timeout)
}

fn login_user_id() -> Result<String> {
    let uid = bounded_output(
        {
            let mut command = Command::new("/usr/bin/id");
            command.arg("-u");
            command
        },
        std::time::Duration::from_secs(2),
    )?;
    ensure!(uid.status.success(), "could not determine login user id");
    Ok(String::from_utf8(uid.stdout)?.trim().to_string())
}

pub fn prove_app_daemon_invocation(label: &str, app: &VerifiedAppInstallation) -> Result<bool> {
    if label != "com.plug.daemon" {
        return Ok(false);
    }
    let uid = login_user_id()?;
    let mut command = Command::new("/bin/launchctl");
    command.args(["print", &format!("gui/{uid}/{label}")]);
    let output = bounded_output(command, std::time::Duration::from_secs(2))?;
    if !output.status.success() {
        return Ok(false);
    }
    let output = String::from_utf8(output.stdout)?;
    let has_daemon_arguments = output
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .windows(2)
        .any(|lines| lines == ["serve", "--daemon"]);
    let identifies_app = output.contains("parent bundle identifier = com.cyberpapiii.plug")
        || output.contains(&format!("program = {}", app.executable_path.display()));
    Ok(has_daemon_arguments && identifies_app)
}

/// Remove only installation artifacts positively proven to belong to the
/// currently verified Plug.app. Configuration and credentials are untouched.
pub fn uninstall_cleanup() -> Result<UninstallCleanupReport> {
    let mut items = Vec::new();
    let app = match resolve_verified_app() {
        Ok(app) => app,
        Err(error) => {
            items.push(UninstallCleanupItem {
                kind: "app_verification".to_string(),
                path: None,
                label: None,
                changed: false,
                message: format!("Plug.app verification failed; all unknown files and jobs were left untouched: {error}"),
            });
            None
        }
    };
    let command_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/bin/plug");
    items.push(cleanup_command_link(&command_path, app.as_ref())?);

    let jobs = match discover_launchd_jobs_bounded(std::time::Duration::from_secs(4)) {
        Ok(jobs) => jobs.into_iter().filter(is_plug_launchd_candidate).collect(),
        Err(error) => {
            items.push(UninstallCleanupItem {
                kind: "launchd_discovery".to_string(),
                path: None,
                label: None,
                changed: false,
                message: format!("Launchd jobs could not be proven; left untouched: {error}"),
            });
            Vec::new()
        }
    };
    let mut proven_daemon_invocations = std::collections::BTreeSet::new();
    if let Some(app) = app.as_ref() {
        for job in &jobs {
            if prove_app_daemon_invocation(&job.label, app).unwrap_or(false) {
                proven_daemon_invocations.insert(job.label.clone());
            }
        }
    }
    let plan = cleanup_launchd_plan(&jobs, app.as_ref(), &proven_daemon_invocations);
    items.extend(plan.preserved);
    if !plan.remove_labels.is_empty() {
        let uid = login_user_id()?;
        for label in plan.remove_labels {
            let mut command = Command::new("/bin/launchctl");
            command.args(["bootout", &format!("gui/{uid}/{label}")]);
            let output = bounded_output(command, std::time::Duration::from_secs(5));
            match output {
                Ok(output) if output.status.success() => items.push(UninstallCleanupItem {
                    kind: "launchd_job".to_string(),
                    path: app.as_ref().map(|app| app.executable_path.clone()),
                    label: Some(label),
                    changed: true,
                    message: "Unregistered the verified Plug.app launchd service.".to_string(),
                }),
                Ok(output) => items.push(UninstallCleanupItem {
                    kind: "launchd_job".to_string(),
                    path: app.as_ref().map(|app| app.executable_path.clone()),
                    label: Some(label),
                    changed: false,
                    message: format!(
                        "Verified app-owned launchd service could not be unregistered; left registered: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                }),
                Err(error) => items.push(UninstallCleanupItem {
                    kind: "launchd_job".to_string(),
                    path: app.as_ref().map(|app| app.executable_path.clone()),
                    label: Some(label),
                    changed: false,
                    message: format!("Verified app-owned launchd service could not be unregistered; left registered: {error}"),
                }),
            }
        }
    }
    Ok(UninstallCleanupReport { items })
}

fn verify_candidate(
    bundle_version: &str,
    executable_version: &str,
    signature_valid: bool,
) -> Result<()> {
    ensure!(signature_valid, "Plug.app signature verification failed");
    ensure!(
        bundle_version == executable_version,
        "Plug.app version {bundle_version} does not match embedded plug version {executable_version}"
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn registered_bundle_paths() -> Vec<PathBuf> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::NSString;

    let identifier = NSString::from_str(APP_BUNDLE_ID);
    let workspace = NSWorkspace::sharedWorkspace();
    workspace
        .URLsForApplicationsWithBundleIdentifier(&identifier)
        .to_vec()
        .into_iter()
        .filter_map(|url| url.path())
        .map(|path| PathBuf::from(path.to_string()))
        .collect()
}

#[cfg(target_os = "macos")]
fn fallback_bundle_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("/Applications/Plug.app")];
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join("Applications/Plug.app"));
    }
    paths
}

#[cfg(target_os = "macos")]
fn verify_app_bundle(bundle_path: &Path) -> Result<VerifiedAppInstallation> {
    let executable_path = bundle_path.join(BUNDLE_EXECUTABLE);
    ensure!(
        executable_path.is_file(),
        "Plug.app is missing its embedded executable {}",
        executable_path.display()
    );

    verify_codesign(bundle_path)?;
    let bundle_version = bundle_version(bundle_path)?;
    let executable_version = executable_version(&executable_path)?;
    verify_candidate(&bundle_version, &executable_version, true)?;

    Ok(VerifiedAppInstallation {
        bundle_path: bundle_path.to_path_buf(),
        executable_path,
        version: bundle_version,
    })
}

#[cfg(target_os = "macos")]
fn verify_codesign(bundle_path: &Path) -> Result<()> {
    let requirement = codesign_requirement_argument();
    let status = bounded_output(
        {
            let mut command = Command::new("/usr/bin/codesign");
            command
                .args(["--verify", "--strict"])
                .arg(&requirement)
                .arg(bundle_path);
            command
        },
        std::time::Duration::from_secs(3),
    )
    .context("could not run codesign to verify Plug.app")?
    .status;
    ensure!(
        status.success(),
        "Plug.app at {} is unsigned or does not satisfy the Plug Developer ID requirement",
        bundle_path.display()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn codesign_requirement_argument() -> String {
    // codesign requires the short requirement option and its value in one
    // argv element. Passing "-R" and the expression separately makes codesign
    // interpret the expression as a file path.
    format!(
        "-R=anchor apple generic and identifier \"{APP_BUNDLE_ID}\" and certificate leaf[subject.OU] = \"{DEVELOPER_TEAM_ID}\""
    )
}

#[cfg(target_os = "macos")]
fn bundle_version(bundle_path: &Path) -> Result<String> {
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"])
        .arg(bundle_path.join("Contents/Info.plist"))
        .output()
        .context("could not read Plug.app bundle version")?;
    ensure!(
        output.status.success(),
        "Plug.app has no readable bundle version"
    );
    let version =
        String::from_utf8(output.stdout).context("Plug.app bundle version was not UTF-8")?;
    let version = version.trim();
    ensure!(!version.is_empty(), "Plug.app bundle version was empty");
    Ok(version.to_owned())
}

#[cfg(target_os = "macos")]
fn executable_version(executable_path: &Path) -> Result<String> {
    let output = bounded_output(
        version_probe_command(executable_path),
        std::time::Duration::from_secs(2),
    )
    .with_context(|| format!("could not run {} --version", executable_path.display()))?;
    ensure!(output.status.success(), "embedded plug --version failed");
    let output = String::from_utf8(output.stdout).context("embedded plug version was not UTF-8")?;
    let version = output
        .trim()
        .strip_prefix("plug ")
        .context("embedded executable did not report a Plug version")?;
    ensure!(!version.is_empty(), "embedded Plug version was empty");
    Ok(version.to_owned())
}

#[cfg(target_os = "macos")]
fn version_probe_command(executable_path: &Path) -> Command {
    let mut command = Command::new(executable_path);
    // A production executable normally resolves and verifies Plug.app before
    // parsing arguments. The verifier is already checking that same signed
    // bundle, so a nested --version probe must not enter delegation again.
    command.arg("--version").env("PLUG_DEV", "1");
    command
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        DelegationDecision, VerifiedAppInstallation, canonical_client_command_from,
        delegation_decision, verify_candidate,
    };

    fn app(path: impl Into<PathBuf>) -> VerifiedAppInstallation {
        let bundle_path = path.into();
        VerifiedAppInstallation {
            executable_path: bundle_path.join("Contents/Resources/plug"),
            bundle_path,
            version: "0.6.4".into(),
        }
    }

    #[test]
    fn absent_app_stays() {
        assert_eq!(
            delegation_decision(Path::new("/usr/local/bin/plug"), None, false).unwrap(),
            DelegationDecision::Stay,
        );
    }

    #[test]
    fn bundle_executable_stays() {
        let app = app("/Applications/Plug.app");
        assert_eq!(
            delegation_decision(&app.executable_path, Some(&app), false).unwrap(),
            DelegationDecision::Stay,
        );
    }

    #[test]
    fn stray_executable_delegates_to_verified_bundle() {
        let app = app("/Applications/Plug.app");
        assert_eq!(
            delegation_decision(Path::new("/opt/homebrew/bin/plug"), Some(&app), false).unwrap(),
            DelegationDecision::Exec(app.executable_path),
        );
    }

    #[test]
    fn development_mode_stays() {
        let app = app("/Applications/Plug.app");
        assert_eq!(
            delegation_decision(Path::new("/opt/homebrew/bin/plug"), Some(&app), true).unwrap(),
            DelegationDecision::Stay,
        );
    }

    #[test]
    fn mismatched_versions_and_invalid_signatures_fail_verification() {
        assert!(verify_candidate("0.6.4", "0.6.5", true).is_err());
        assert!(verify_candidate("0.6.4", "0.6.4", false).is_err());
    }

    #[test]
    fn moved_registered_app_delegates() {
        let app = app("/Volumes/External Applications/Plug.app");
        assert_eq!(
            delegation_decision(Path::new("/usr/local/bin/plug"), Some(&app), false).unwrap(),
            DelegationDecision::Exec(app.executable_path),
        );
    }

    #[test]
    fn loop_marker_prevents_a_second_delegation() {
        let app = app("/Applications/Plug.app");
        assert_eq!(
            super::delegation_decision_with_loop(&app.executable_path, Some(&app), false, true,)
                .unwrap(),
            DelegationDecision::Stay,
        );
    }

    #[test]
    fn production_macos_requires_a_verified_app_for_client_exports() {
        let current = Path::new("/opt/homebrew/bin/plug");
        assert!(canonical_client_command_from(None, current, false, true).is_err());
        assert_eq!(
            canonical_client_command_from(None, current, true, true).unwrap(),
            current
        );
        assert_eq!(
            canonical_client_command_from(None, current, false, false).unwrap(),
            current
        );
    }

    #[test]
    fn adapter_versions_distinguish_current_older_missing_and_incompatible() {
        assert_eq!(
            super::classify_adapter_version(Some("0.6.4"), "0.6.4"),
            super::AdapterVersionState::Current
        );
        assert_eq!(
            super::classify_adapter_version(Some("0.6.3"), "0.6.4"),
            super::AdapterVersionState::CompatibleOlder
        );
        assert_eq!(
            super::classify_adapter_version(None, "0.6.4"),
            super::AdapterVersionState::Missing
        );
        assert_eq!(
            super::classify_adapter_version(Some("0.7.0"), "0.6.4"),
            super::AdapterVersionState::Incompatible
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn codesign_requirement_is_one_option_argument() {
        let argument = super::codesign_requirement_argument();
        assert!(argument.starts_with("-R=anchor apple generic"));
        assert!(argument.contains(super::APP_BUNDLE_ID));
        assert!(argument.contains(super::DEVELOPER_TEAM_ID));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn embedded_version_probe_does_not_reenter_app_delegation() {
        use std::os::unix::fs::PermissionsExt;

        let probe = std::env::temp_dir().join(format!(
            "plug-version-probe-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(
            &probe,
            "#!/bin/sh\nif [ \"${PLUG_DEV:-}\" = 1 ]; then echo 'plug 0.7.1'; exit 0; fi\nexec \"$0\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(super::executable_version(&probe).unwrap(), "0.7.1");
        std::fs::remove_file(probe).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_cleanup_removes_only_symlink_to_verified_app() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "plug-uninstall-cleanup-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let app = app(root.join("Plug.app"));
        std::fs::create_dir_all(app.executable_path.parent().unwrap()).unwrap();
        std::fs::write(&app.executable_path, "plug").unwrap();
        let link = root.join("bin/plug");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&app.executable_path, &link).unwrap();

        let outcome = super::cleanup_command_link(&link, Some(&app)).unwrap();
        assert!(outcome.changed);
        assert!(!link.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn uninstall_cleanup_reports_unknown_file_and_leaves_it_untouched() {
        let root =
            std::env::temp_dir().join(format!("plug-uninstall-unknown-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("plug");
        std::fs::write(&path, "unrelated").unwrap();

        let outcome = super::cleanup_command_link(&path, None).unwrap();
        assert!(!outcome.changed);
        assert!(outcome.message.contains("left untouched"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "unrelated");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn uninstall_cleanup_plans_only_proven_app_owned_launchd_jobs() {
        let app = app("/Applications/Plug.app");
        let jobs = vec![
            crate::service::LaunchdJobRecord {
                label: "com.plug.daemon".to_string(),
                program: app.executable_path.clone(),
            },
            crate::service::LaunchdJobRecord {
                label: "local.plug.unknown".to_string(),
                program: PathBuf::from("/Applications/Other.app/other"),
            },
        ];

        let proven = std::collections::BTreeSet::from(["com.plug.daemon".to_string()]);
        let plan = super::cleanup_launchd_plan(&jobs, Some(&app), &proven);
        assert_eq!(plan.remove_labels, vec!["com.plug.daemon"]);
        assert_eq!(plan.preserved.len(), 1);
        assert!(plan.preserved[0].message.contains("left untouched"));
    }

    #[test]
    fn uninstall_cleanup_preserves_unknown_label_targeting_current_app() {
        let app = app("/Applications/Plug.app");
        let jobs = vec![crate::service::LaunchdJobRecord {
            label: "unknown.helper".to_string(),
            program: app.executable_path.clone(),
        }];
        let proven = std::collections::BTreeSet::from(["unknown.helper".to_string()]);
        let plan = super::cleanup_launchd_plan(&jobs, Some(&app), &proven);
        assert!(plan.remove_labels.is_empty());
        assert_eq!(plan.preserved.len(), 1);
        assert!(plan.preserved[0].message.contains("left untouched"));
    }

    #[test]
    fn uninstall_cleanup_preserves_canonical_label_without_daemon_invocation_proof() {
        let app = app("/Applications/Plug.app");
        let jobs = vec![crate::service::LaunchdJobRecord {
            label: "com.plug.daemon".to_string(),
            program: app.executable_path.clone(),
        }];
        let plan =
            super::cleanup_launchd_plan(&jobs, Some(&app), &std::collections::BTreeSet::new());
        assert!(plan.remove_labels.is_empty());
        assert_eq!(plan.preserved.len(), 1);
    }

    #[test]
    fn launchd_discovery_timeout_fails_closed() {
        let (_sender, receiver) = std::sync::mpsc::channel();
        let result =
            super::receive_launchd_discovery(receiver, std::time::Duration::from_millis(1));
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[test]
    fn uninstall_cleanup_candidate_filter_ignores_unrelated_plugin_jobs() {
        let unrelated = crate::service::LaunchdJobRecord {
            label: "com.apple.XprotectFramework.PluginService".to_string(),
            program: PathBuf::from("/System/Library/XProtectPluginService"),
        };
        let unknown_plug = crate::service::LaunchdJobRecord {
            label: "local.claude-rc.plug".to_string(),
            program: PathBuf::from("/Users/example/.local/bin/claude"),
        };
        assert!(!super::is_plug_launchd_candidate(&unrelated));
        assert!(super::is_plug_launchd_candidate(&unknown_plug));
    }
}
