//! Config file watcher — triggers hot-reload on config.toml changes.
//!
//! Uses `notify` with 500ms debounce. Spawns a background task that watches
//! the config file and calls `Engine::reload_config()` on change.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notify_debouncer_mini::{DebouncedEventKind, new_debouncer};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config;
use crate::engine::Engine;

/// Debounce interval for config file changes.
const DEBOUNCE_MS: u64 = 500;

/// Spawn a background task that watches `config.toml` for changes.
///
/// Returns the join handle for the watcher task. The task runs until the
/// cancellation token is triggered.
pub fn spawn_config_watcher(
    engine: Arc<Engine>,
    config_path: PathBuf,
    cancel: CancellationToken,
    tracker: &tokio_util::task::TaskTracker,
) {
    tracker.spawn(async move {
        if let Err(e) = run_watcher(engine, config_path, cancel).await {
            tracing::error!(error = %e, "config watcher failed");
        }
    });
}

async fn run_watcher(
    engine: Arc<Engine>,
    config_path: PathBuf,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let watch_dir = config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent directory"))?
        .to_path_buf();

    // Ensure the directory exists
    if !watch_dir.exists() {
        tracing::debug!(
            path = %watch_dir.display(),
            "config directory does not exist — skipping file watcher"
        );
        cancel.cancelled().await;
        return Ok(());
    }

    let (tx, mut rx) = mpsc::channel(16);

    let mut debouncer = new_debouncer(
        Duration::from_millis(DEBOUNCE_MS),
        move |result: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
            if let Ok(events) = result {
                for event in events {
                    if event.kind == DebouncedEventKind::Any {
                        // Non-blocking send — if channel is full, skip
                        let _ = tx.try_send(event.path);
                    }
                }
            }
        },
    )
    .map_err(|e| anyhow::anyhow!("failed to create file watcher: {e}"))?;

    debouncer
        .watcher()
        .watch(&watch_dir, notify::RecursiveMode::NonRecursive)
        .map_err(|e| anyhow::anyhow!("failed to watch {}: {e}", watch_dir.display()))?;

    let config_filename = config_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    tracing::info!(
        path = %config_path.display(),
        "config file watcher started"
    );

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            Some(changed_path) = rx.recv() => {
                // Only react to changes to the config file itself
                let filename = changed_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                if filename != config_filename {
                    continue;
                }

                tracing::info!("config file changed — reloading");
                match config::load_config(Some(&config_path)) {
                    Ok(new_config) => {
                        match engine.reload_config(new_config).await {
                            Ok(report) => {
                                tracing::info!(
                                    added = report.added.len(),
                                    removed = report.removed.len(),
                                    changed = report.changed.len(),
                                    "config reloaded via file watcher"
                                );
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "config reload failed — keeping current config");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to parse changed config — keeping current config");
                    }
                }
            }
        }
    }

    tracing::debug!("config file watcher stopped");
    Ok(())
}
