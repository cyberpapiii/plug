//! `plug uninstall-cleanup`: remove what the app put outside its bundle.

use std::path::{Path, PathBuf};
use std::time::Duration;

use plug_core::paths::DAEMON_LABEL;
use serde::Serialize;

use crate::OutputFormat;
use crate::install::{VerifiedAppInstallation, paths_match};
use crate::service::{LaunchdJobRecord, LaunchdProgramOwnership, ServiceOwnership};
use crate::ui::{print_info_line, print_success_line};

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

pub(crate) fn cmd_uninstall_cleanup(output: &OutputFormat) -> anyhow::Result<()> {
    let report = uninstall_cleanup()?;
    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            for item in report.items {
                if item.changed {
                    print_success_line(item.message);
                } else {
                    print_info_line(item.message);
                }
            }
        }
    }
    Ok(())
}

/// Remove only installation artifacts positively proven to belong to the
/// currently verified Plug.app. Configuration and credentials are untouched.
fn uninstall_cleanup() -> anyhow::Result<UninstallCleanupReport> {
    let mut items = Vec::new();
    let app = match crate::install::resolve_verified_app() {
        Ok(app) => app,
        Err(error) => {
            items.push(UninstallCleanupItem {
                kind: "app_verification".to_string(),
                path: None,
                label: None,
                changed: false,
                message: format!(
                    "Plug.app verification failed; all unknown files and jobs were left untouched: {error}"
                ),
            });
            None
        }
    };
    let command_path = plug_core::paths::command_link();
    items.push(cleanup_command_link(&command_path, app.as_ref())?);

    let jobs = match crate::service::discover_launchd_jobs(
        app.as_ref().map(|app| app.executable_path.as_path()),
        Duration::from_secs(4),
    ) {
        Ok(jobs) => jobs
            .into_iter()
            .filter(crate::service::is_plug_launchd_candidate)
            .collect(),
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
    let daemon_job_app_owned = app.is_some()
        && jobs.iter().any(|job| job.label == DAEMON_LABEL)
        && crate::service::inspect()
            .is_ok_and(|state| state.ownership == ServiceOwnership::AppManaged);
    let plan = cleanup_launchd_plan(&jobs, app.as_ref(), daemon_job_app_owned);
    items.extend(plan.preserved);
    for label in plan.remove_labels {
        let path = app.as_ref().map(|app| app.executable_path.clone());
        items.push(match crate::service::bootout(&label) {
            Ok(()) => UninstallCleanupItem {
                kind: "launchd_job".to_string(),
                path,
                label: Some(label),
                changed: true,
                message: "Unregistered the verified Plug.app launchd service.".to_string(),
            },
            Err(error) => UninstallCleanupItem {
                kind: "launchd_job".to_string(),
                path,
                label: Some(label),
                changed: false,
                message: format!(
                    "Verified app-owned launchd service could not be unregistered; left registered: {error}"
                ),
            },
        });
    }
    Ok(UninstallCleanupReport { items })
}

fn cleanup_command_link(
    path: &Path,
    app: Option<&VerifiedAppInstallation>,
) -> anyhow::Result<UninstallCleanupItem> {
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

/// Only `com.plug.daemon`, registered by the verified app through
/// SMAppService, is removed. Every other job is reported and left alone.
fn cleanup_launchd_plan(
    jobs: &[LaunchdJobRecord],
    app: Option<&VerifiedAppInstallation>,
    daemon_job_app_owned: bool,
) -> CleanupLaunchdPlan {
    let mut remove_labels = Vec::new();
    let mut preserved = Vec::new();
    for job in jobs {
        if job.label == DAEMON_LABEL
            && daemon_job_app_owned
            && crate::service::classify_launchd_program(
                job,
                app.map(|app| app.executable_path.as_path()),
            ) == LaunchdProgramOwnership::CurrentApp
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn app(path: impl Into<PathBuf>) -> VerifiedAppInstallation {
        let bundle_path = path.into();
        VerifiedAppInstallation {
            executable_path: plug_core::paths::bundled_binary(&bundle_path),
            bundle_path,
            version: "0.6.4".into(),
        }
    }

    #[test]
    fn uninstall_cleanup_removes_only_symlink_to_verified_app() {
        use std::os::unix::fs::symlink;

        let root = plug_core::paths::test::scratch("uninstall-cleanup");
        let app = app(root.join("Plug.app"));
        std::fs::create_dir_all(app.executable_path.parent().unwrap()).unwrap();
        std::fs::write(&app.executable_path, "plug").unwrap();
        let link = root.join("bin/plug");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&app.executable_path, &link).unwrap();

        let outcome = cleanup_command_link(&link, Some(&app)).unwrap();
        assert!(outcome.changed);
        assert!(!link.exists());
    }

    #[test]
    fn uninstall_cleanup_reports_unknown_file_and_leaves_it_untouched() {
        let root = plug_core::paths::test::scratch("uninstall-unknown");
        let path = root.join("plug");
        std::fs::write(&path, "unrelated").unwrap();

        let outcome = cleanup_command_link(&path, None).unwrap();
        assert!(!outcome.changed);
        assert!(outcome.message.contains("left untouched"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "unrelated");
    }

    #[test]
    fn uninstall_cleanup_plans_only_proven_app_owned_launchd_jobs() {
        let app = app("/Applications/Plug.app");
        let jobs = vec![
            LaunchdJobRecord {
                label: "com.plug.daemon".to_string(),
                program: app.executable_path.clone(),
            },
            LaunchdJobRecord {
                label: "local.plug.unknown".to_string(),
                program: PathBuf::from("/Applications/Other.app/other"),
            },
        ];

        let plan = cleanup_launchd_plan(&jobs, Some(&app), true);
        assert_eq!(plan.remove_labels, vec!["com.plug.daemon"]);
        assert_eq!(plan.preserved.len(), 1);
        assert!(plan.preserved[0].message.contains("left untouched"));
    }

    #[test]
    fn uninstall_cleanup_preserves_unknown_label_targeting_current_app() {
        let app = app("/Applications/Plug.app");
        let jobs = vec![LaunchdJobRecord {
            label: "unknown.helper".to_string(),
            program: app.executable_path.clone(),
        }];
        let plan = cleanup_launchd_plan(&jobs, Some(&app), true);
        assert!(plan.remove_labels.is_empty());
        assert_eq!(plan.preserved.len(), 1);
        assert!(plan.preserved[0].message.contains("left untouched"));
    }

    #[test]
    fn uninstall_cleanup_preserves_canonical_label_without_daemon_invocation_proof() {
        let app = app("/Applications/Plug.app");
        let jobs = vec![LaunchdJobRecord {
            label: "com.plug.daemon".to_string(),
            program: app.executable_path.clone(),
        }];
        let plan = cleanup_launchd_plan(&jobs, Some(&app), false);
        assert!(plan.remove_labels.is_empty());
        assert_eq!(plan.preserved.len(), 1);
    }
}
