use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};

pub const APP_BUNDLE_ID: &str = "com.cyberpapiii.plug";
pub const DEVELOPER_TEAM_ID: &str = "HJF7LN64XX";

const LOOP_MARKER: &str = "PLUG_APP_EXEC";
const BUNDLE_EXECUTABLE: &str = "Contents/Resources/plug";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedAppInstallation {
    pub bundle_path: PathBuf,
    pub executable_path: PathBuf,
    pub version: String,
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
/// preserve the current executable for development and Linux installs.
#[allow(dead_code)] // Consumed by the following client-repair task.
pub fn canonical_client_command() -> Result<PathBuf> {
    Ok(match resolve_verified_app()? {
        Some(app) => app.executable_path,
        None => std::env::current_exe().context("could not resolve the running executable")?,
    })
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
    let requirement = format!(
        "anchor apple generic and identifier \"{APP_BUNDLE_ID}\" and certificate leaf[subject.OU] = \"{DEVELOPER_TEAM_ID}\""
    );
    let status = Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict", "-R", &requirement])
        .arg(bundle_path)
        .status()
        .context("could not run codesign to verify Plug.app")?;
    ensure!(
        status.success(),
        "Plug.app at {} is unsigned or does not satisfy the Plug Developer ID requirement",
        bundle_path.display()
    );
    Ok(())
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
    let output = Command::new(executable_path)
        .arg("--version")
        .output()
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        DelegationDecision, VerifiedAppInstallation, delegation_decision, verify_candidate,
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
}
