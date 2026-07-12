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

// ── Tests ───────────────────────────────────────────────────────────────────
//
// End-to-end coverage of the real chain: disk write → notify/debouncer event
// → filename filter → `load_config` → `Engine::reload_config` → diff applied.
// These are wall-clock bound (real `notify` backend + the real 500ms
// `DEBOUNCE_MS`), so they use `#[tokio::test(flavor = "multi_thread")]` —
// paused/virtual tokio time cannot observe the debouncer's own OS thread.
//
// Residual gaps NOT covered here (see plan 024): the missing-directory
// branch (`:48-55`, trivial and requires racing spawn against dir creation),
// the full-channel drop (`:66`, needs >16 debounced events in flight), and
// the reload-failure-keeps-config branch (`:116-118`) — `apply_reload`
// (`reload.rs:230-334`) always returns `Ok`, recording per-server start
// failures in `ReloadReport::errors` rather than surfacing an `Err`, so
// there is no easy input that makes `engine.reload_config()` itself fail.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ServerConfig, TransportType};
    use crate::engine::EngineEvent;
    use std::collections::HashMap;
    use tempfile::TempDir;
    use tokio::sync::broadcast::error::TryRecvError;

    fn write_config(path: &std::path::Path, config: &Config) {
        std::fs::write(path, toml::to_string(config).expect("serialize config"))
            .expect("write config file");
    }

    fn mock_server_config(tools: &str) -> ServerConfig {
        ServerConfig {
            command: Some(
                plug_test_harness::mock_server_bin()
                    .to_string_lossy()
                    .into_owned(),
            ),
            args: vec!["--tools".to_string(), tools.to_string()],
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::Stdio,
            url: None,
            auth_token: None,
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
            timeout_secs: 10,
            call_timeout_secs: 5,
            max_concurrent: 4,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
            sandbox: None,
        }
    }

    fn config_with_mock_server() -> Config {
        let mut config = Config::default();
        config
            .servers
            .insert("mock".to_string(), mock_server_config("echo"));
        config
    }

    /// Shared scaffold for every watcher test: a real temp directory (never
    /// shared across tests — the watcher watches the whole directory, so a
    /// shared dir would cross-trigger between parallel tests), a real
    /// `Engine`, and a real `spawn_config_watcher` task in front of it.
    struct WatcherFixture {
        // Held only to keep the directory alive for the fixture's lifetime.
        _dir: TempDir,
        config_path: PathBuf,
        engine: Arc<Engine>,
        events: tokio::sync::broadcast::Receiver<EngineEvent>,
    }

    impl WatcherFixture {
        async fn new() -> Self {
            let dir = TempDir::new().expect("create temp dir");
            let config_path = dir.path().join("config.toml");
            write_config(&config_path, &Config::default());

            let engine = Arc::new(Engine::new(Config::default()));
            engine.start().await.expect("engine start");
            let events = engine.event_sender().subscribe();

            spawn_config_watcher(
                engine.clone(),
                config_path.clone(),
                engine.cancel_token().clone(),
                engine.tracker(),
            );

            Self {
                _dir: dir,
                config_path,
                engine,
                events,
            }
        }

        /// Poll (deadline-bounded, never a bare sleep-then-check) until a
        /// `ConfigReloaded` event has arrived, or `timeout` elapses.
        async fn wait_for_reload(&mut self, timeout: Duration) -> bool {
            let deadline = tokio::time::Instant::now() + timeout;
            loop {
                loop {
                    match self.events.try_recv() {
                        Ok(EngineEvent::ConfigReloaded) => return true,
                        Ok(_) => continue,
                        // A lagged receiver may have dropped the event we care
                        // about — keep draining rather than treating this as
                        // "no more events right now".
                        Err(TryRecvError::Lagged(_)) => continue,
                        Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
                    }
                }
                if tokio::time::Instant::now() >= deadline {
                    return false;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        /// Write `config` to the watched file and wait for the watcher to
        /// pick it up. The watcher arms asynchronously and there is no
        /// readiness signal, so the FIRST mutation gets a bounded retry: if
        /// 5s of polling sees no reload, rewrite the same content (up to 2
        /// more times) rather than assuming failure.
        async fn mutate_and_await_reload(&mut self, config: &Config) -> bool {
            for _ in 0..3 {
                write_config(&self.config_path, config);
                if self.wait_for_reload(Duration::from_secs(5)).await {
                    return true;
                }
            }
            false
        }

        async fn shutdown(self) {
            self.engine.shutdown().await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn watcher_smoke_spawns_and_cancels_cleanly() {
        let fixture = WatcherFixture::new().await;
        assert!(fixture.engine.server_statuses().is_empty());
        fixture.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn watcher_applies_config_change_from_disk() {
        let mut fixture = WatcherFixture::new().await;
        assert!(fixture.engine.server_statuses().is_empty());

        let reloaded = fixture
            .mutate_and_await_reload(&config_with_mock_server())
            .await;
        assert!(
            reloaded,
            "expected a ConfigReloaded event after adding a server on disk"
        );

        let statuses = fixture.engine.server_statuses();
        assert!(
            statuses.iter().any(|s| s.server_id == "mock"),
            "expected the 'mock' server to be present after reload, got: {statuses:?}"
        );

        fixture.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn watcher_keeps_config_and_survives_parse_error() {
        let mut fixture = WatcherFixture::new().await;

        std::fs::write(&fixture.config_path, "this is [ not valid toml")
            .expect("write invalid toml");

        let saw_reload = fixture.wait_for_reload(Duration::from_secs(2)).await;
        assert!(
            !saw_reload,
            "watcher must not emit ConfigReloaded on a parse failure"
        );
        assert!(
            fixture.engine.server_statuses().is_empty(),
            "config must be unchanged after a parse failure"
        );

        // Load-bearing half: prove the watcher loop survived the bad write by
        // applying a subsequent valid change.
        let reloaded = fixture
            .mutate_and_await_reload(&config_with_mock_server())
            .await;
        assert!(
            reloaded,
            "watcher must keep watching and apply a later valid config after a parse error"
        );

        fixture.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn watcher_ignores_sibling_file_changes() {
        let mut fixture = WatcherFixture::new().await;

        let sibling = fixture.config_path.with_file_name("other.toml");
        std::fs::write(&sibling, "not the watched file").expect("write sibling file");

        let saw_reload = fixture.wait_for_reload(Duration::from_secs(2)).await;
        assert!(
            !saw_reload,
            "watcher must ignore changes to sibling files in the same directory"
        );

        // Positive control in the same test: a genuine config.toml change
        // still fires, proving the negative result above isn't just a dead
        // watcher.
        let reloaded = fixture
            .mutate_and_await_reload(&config_with_mock_server())
            .await;
        assert!(
            reloaded,
            "watcher must still react to changes to the actual config file"
        );

        fixture.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn watcher_survives_atomic_rename_save() {
        let mut fixture = WatcherFixture::new().await;
        let new_config = config_with_mock_server();
        let tmp_path = fixture.config_path.with_file_name("config.toml.tmp");

        // The watcher watches the DIRECTORY (not the file inode), so a
        // rename-over-target editor save must still be observed. Bounded
        // retry mirrors `mutate_and_await_reload`'s arm-wait handling.
        let mut reloaded = false;
        for _ in 0..3 {
            write_config(&tmp_path, &new_config);
            std::fs::rename(&tmp_path, &fixture.config_path).expect("atomic rename over config");
            if fixture.wait_for_reload(Duration::from_secs(5)).await {
                reloaded = true;
                break;
            }
        }
        assert!(
            reloaded,
            "watcher must observe an atomic rename-over-file save"
        );

        let statuses = fixture.engine.server_statuses();
        assert!(
            statuses.iter().any(|s| s.server_id == "mock"),
            "expected the 'mock' server to be present after rename-save reload, got: {statuses:?}"
        );

        fixture.shutdown().await;
    }
}
