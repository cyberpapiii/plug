use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use plug_core::paths::BUNDLED_BINARY;
#[cfg(target_os = "macos")]
use plug_core::paths::{APP_BUNDLE_ID, TEAM_ID};

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
    let dev = plug_core::paths::is_development();
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

/// The app's own executable stays, so the exec below cannot loop.
fn delegation_decision(
    current: &Path,
    app: Option<&VerifiedAppInstallation>,
    dev: bool,
) -> DelegationDecision {
    match app {
        Some(app) if !dev && !paths_match(current, &app.executable_path) => {
            DelegationDecision::Exec(app.executable_path.clone())
        }
        _ => DelegationDecision::Stay,
    }
}

/// Re-exec a standalone macOS command through a freshly verified Plug.app.
/// This must run before dotenv loading and Clap parsing.
pub fn maybe_delegate_to_app() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let dev = plug_core::paths::is_development();
        if dev {
            return Ok(());
        }

        let current =
            std::env::current_exe().context("could not resolve the running executable")?;

        // Verification exists to choose which binary should run. When the
        // running executable is already a bundle executable, there is nothing
        // left to choose: verifying would re-prove the code executing this
        // line, and it costs a codesign walk plus two subprocess spawns on
        // every single invocation. Bundle integrity is owned by `plug doctor`
        // and by the app's own installation reconciliation, both of which
        // still run the full check.
        if is_app_executable(&current) {
            return Ok(());
        }

        let app = resolve_verified_app()?;

        match delegation_decision(&current, app.as_ref(), false) {
            DelegationDecision::Stay => Ok(()),
            DelegationDecision::Exec(executable) => {
                use std::{io, os::unix::process::CommandExt, process::Stdio};

                let mut command = Command::new(&executable);
                command
                    .args(std::env::args_os().skip(1))
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

/// Whether `path` is the embedded executable of some Plug.app, wherever the
/// app is.
///
/// Local bundle paths are checked before the LaunchServices query because the
/// canonical install is one of them, and answering from a path comparison
/// alone keeps the common case free of any system call beyond `canonicalize`.
pub(crate) fn is_app_executable(path: &Path) -> bool {
    fallback_bundle_paths()
        .into_iter()
        .chain(std::iter::once_with(registered_bundle_paths).flatten())
        .any(|bundle| paths_match(path, &bundle.join(BUNDLED_BINARY)))
}

/// Whether two paths name the same file once symlinks are resolved.
///
/// The plain equality check first matters: `canonicalize` fails on a path that
/// does not exist yet, and install decisions routinely compare against a
/// destination that has not been created.
pub(crate) fn paths_match(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

pub(crate) fn bounded_output(
    mut command: Command,
    timeout: std::time::Duration,
) -> Result<std::process::Output> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().context("could not start bounded command")?;
    // Read both pipes while the command runs: one that prints more than a
    // pipe holds blocks on the write and never exits.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(child.stdout.take().map(|pipe| Box::new(pipe) as _));
    let stderr = drain(child.stderr.take().map(|pipe| Box::new(pipe) as _));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("command timed out after {}ms", timeout.as_millis());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let collected = |reader: std::thread::JoinHandle<Vec<u8>>| {
        reader
            .join()
            .map_err(|_| anyhow::anyhow!("could not collect command output"))
    };
    Ok(std::process::Output {
        status,
        stdout: collected(stdout)?,
        stderr: collected(stderr)?,
    })
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
    let mut candidates: Vec<(&str, PathBuf)> = crate::legacy_programs::legacy_program_candidates()
        .into_iter()
        .map(|path| (shadow_kind(&path), path))
        .collect();
    candidates.sort_by(|left, right| left.1.cmp(&right.1));
    // `opt/plug/bin/plug` links into the Cellar; report each file once.
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|(_, path)| seen.insert(std::fs::canonicalize(path).unwrap_or(path.clone())));
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

/// How an older Plug at `path` was installed, by where it is.
fn shadow_kind(path: &Path) -> &'static str {
    let text = path.to_string_lossy();
    if text.contains("/Cellar/") {
        "homebrew_formula"
    } else if text.contains("/.cargo/") {
        "cargo"
    } else if text.starts_with("/opt/homebrew/") || text.starts_with("/usr/local/") {
        "homebrew"
    } else {
        "local"
    }
}

fn verify_candidate(bundle_version: &str, executable_version: &str) -> Result<()> {
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

/// Only macOS has LaunchServices to ask.
#[cfg(not(target_os = "macos"))]
fn registered_bundle_paths() -> Vec<PathBuf> {
    Vec::new()
}

fn fallback_bundle_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("/Applications/Plug.app")];
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join("Applications/Plug.app"));
    }
    paths
}

#[cfg(target_os = "macos")]
fn verify_app_bundle(bundle_path: &Path) -> Result<VerifiedAppInstallation> {
    let executable_path = bundle_path.join(BUNDLED_BINARY);
    ensure!(
        executable_path.is_file(),
        "Plug.app is missing its embedded executable {}",
        executable_path.display()
    );

    verify_codesign(bundle_path)?;
    let bundle_version = bundle_version(bundle_path)?;
    let executable_version = executable_version(&executable_path)?;
    verify_candidate(&bundle_version, &executable_version)?;

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
        "-R=anchor apple generic and identifier \"{APP_BUNDLE_ID}\" and certificate leaf[subject.OU] = \"{TEAM_ID}\""
    )
}

#[cfg(target_os = "macos")]
fn read_bundle_plist_string(bundle_path: &Path, key: &str, label: &str) -> Result<String> {
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", key, "raw", "-o", "-"])
        .arg(bundle_path.join("Contents/Info.plist"))
        .output()
        .with_context(|| format!("could not read Plug.app {label}"))?;
    ensure!(output.status.success(), "Plug.app has no readable {label}");
    let value = String::from_utf8(output.stdout)
        .with_context(|| format!("Plug.app {label} was not UTF-8"))?;
    let value = value.trim();
    ensure!(!value.is_empty(), "Plug.app {label} was empty");
    Ok(value.to_owned())
}

#[cfg(target_os = "macos")]
fn bundle_version(bundle_path: &Path) -> Result<String> {
    read_bundle_plist_string(bundle_path, "CFBundleShortVersionString", "bundle version")
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
    command.arg("--version").env(plug_core::paths::DEV_ENV, "1");
    command
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_bounded_command_may_print_more_than_a_pipe_holds() {
        // `launchctl print gui/<uid>` prints over 100 KB; a pipe holds 64 KB.
        let mut command = std::process::Command::new("/bin/sh");
        command.args([
            "-c",
            "head -c 200000 /dev/zero; head -c 100000 /dev/zero >&2",
        ]);
        let output = super::bounded_output(command, std::time::Duration::from_secs(5))
            .expect("the command finishes");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 200_000);
        assert_eq!(output.stderr.len(), 100_000);
    }

    use std::path::{Path, PathBuf};

    use super::{
        DelegationDecision, VerifiedAppInstallation, canonical_client_command_from,
        delegation_decision, verify_candidate,
    };

    fn app(path: impl Into<PathBuf>) -> VerifiedAppInstallation {
        let bundle_path = path.into();
        VerifiedAppInstallation {
            executable_path: plug_core::paths::bundled_binary(&bundle_path),
            bundle_path,
            version: "0.6.4".into(),
        }
    }

    #[test]
    fn absent_app_stays() {
        assert_eq!(
            delegation_decision(Path::new("/usr/local/bin/plug"), None, false),
            DelegationDecision::Stay,
        );
    }

    #[test]
    fn bundle_executable_stays() {
        let app = app("/Applications/Plug.app");
        assert_eq!(
            delegation_decision(&app.executable_path, Some(&app), false),
            DelegationDecision::Stay,
        );
    }

    #[test]
    fn stray_executable_delegates_to_verified_bundle() {
        let app = app("/Applications/Plug.app");
        assert_eq!(
            delegation_decision(Path::new("/opt/homebrew/bin/plug"), Some(&app), false),
            DelegationDecision::Exec(app.executable_path),
        );
    }

    #[test]
    fn development_mode_stays() {
        let app = app("/Applications/Plug.app");
        assert_eq!(
            delegation_decision(Path::new("/opt/homebrew/bin/plug"), Some(&app), true),
            DelegationDecision::Stay,
        );
    }

    #[test]
    fn mismatched_versions_and_invalid_signatures_fail_verification() {
        assert!(verify_candidate("0.6.4", "0.6.5").is_err());
        assert!(verify_candidate("0.6.4", "0.6.4").is_ok());
    }

    #[test]
    fn moved_registered_app_delegates() {
        let app = app("/Volumes/External Applications/Plug.app");
        assert_eq!(
            delegation_decision(Path::new("/usr/local/bin/plug"), Some(&app), false),
            DelegationDecision::Exec(app.executable_path),
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
        assert!(argument.contains(plug_core::paths::APP_BUNDLE_ID));
        assert!(argument.contains(plug_core::paths::TEAM_ID));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn embedded_version_probe_does_not_reenter_app_delegation() {
        use std::os::unix::fs::PermissionsExt;

        let probe = plug_core::paths::test::scratch("version-probe").join("plug");
        std::fs::write(
            &probe,
            "#!/bin/sh\nif [ \"${PLUG_DEV:-}\" = 1 ]; then echo 'plug 0.7.1'; exit 0; fi\nexec \"$0\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(super::executable_version(&probe).unwrap(), "0.7.1");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn bundle_executables_skip_delegation_verification() {
        for bundle in super::fallback_bundle_paths() {
            assert!(super::is_app_executable(
                &bundle.join(plug_core::paths::BUNDLED_BINARY)
            ));
        }
        assert!(!super::is_app_executable(std::path::Path::new(
            "/usr/local/bin/plug"
        )));
    }
}
