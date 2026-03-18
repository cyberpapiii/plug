//! Config hot-reload: diff configs and apply changes at runtime.
//!
//! Reload is triggered by:
//! - SIGHUP signal (Unix only)
//! - `plug reload` CLI command via daemon IPC
//! - File watcher (`watcher.rs`, 500ms debounce via `notify`)

use std::collections::HashSet;
use std::future::Future;

use futures::stream::{self, StreamExt};

use crate::config::{Config, ServerConfig};
use crate::engine::EngineEvent;

const RELOAD_START_CONCURRENCY: usize = 4;

/// Diff result between old and new configs.
#[derive(Debug, Clone)]
pub struct ConfigDiff {
    /// Servers to add (name, config).
    pub added: Vec<(String, ServerConfig)>,
    /// Servers to remove.
    pub removed: Vec<String>,
    /// Servers to restart (name, new config). Changed means different command/args/env/timeout.
    pub changed: Vec<(String, ServerConfig)>,
    /// Servers unchanged.
    pub unchanged: Vec<String>,
    /// Whether non-server settings changed (bind address, prefix, etc.).
    pub settings_changed: bool,
    /// Settings that require a restart to apply.
    pub restart_required: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReloadStartKind {
    Start,
    Restart,
}

#[derive(Debug, Clone)]
struct ReloadStartAction {
    name: String,
    config: ServerConfig,
    kind: ReloadStartKind,
}

async fn run_reload_start_actions<F, Fut>(
    actions: Vec<ReloadStartAction>,
    concurrency_limit: usize,
    run: F,
) -> Vec<(ReloadStartAction, Result<(), String>)>
where
    F: Fn(ReloadStartAction) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Result<(), String>> + Send,
{
    stream::iter(actions)
        .map(move |action| {
            let run = run.clone();
            async move {
                let action_for_result = action.clone();
                let result = run(action).await;
                (action_for_result, result)
            }
        })
        .buffer_unordered(concurrency_limit.max(1))
        .collect()
        .await
}

/// Compare two configs and return the diff.
pub fn diff_configs(old: &Config, new: &Config) -> ConfigDiff {
    let old_names: HashSet<&String> = old.servers.keys().collect();
    let new_names: HashSet<&String> = new.servers.keys().collect();

    let added: Vec<(String, ServerConfig)> = new_names
        .difference(&old_names)
        .map(|name| ((*name).clone(), new.servers[*name].clone()))
        .collect();

    let removed: Vec<String> = old_names
        .difference(&new_names)
        .map(|name| (*name).clone())
        .collect();

    let mut changed = Vec::new();
    let mut unchanged = Vec::new();

    for name in old_names.intersection(&new_names) {
        let old_cfg = &old.servers[*name];
        let new_cfg = &new.servers[*name];
        if server_config_changed(old_cfg, new_cfg) {
            changed.push(((*name).clone(), new_cfg.clone()));
        } else {
            unchanged.push((*name).clone());
        }
    }

    // Check non-server settings
    let mut restart_required = Vec::new();
    note_restart_required(
        &mut restart_required,
        old.http.bind_address != new.http.bind_address,
        "http.bind_address changed",
    );
    note_restart_required(
        &mut restart_required,
        old.http.port != new.http.port,
        "http.port changed",
    );
    note_restart_required(
        &mut restart_required,
        old.http.allowed_origins != new.http.allowed_origins,
        "http.allowed_origins changed",
    );
    note_restart_required(
        &mut restart_required,
        old.http.session_timeout_secs != new.http.session_timeout_secs,
        "http.session_timeout_secs changed",
    );
    note_restart_required(
        &mut restart_required,
        old.http.max_sessions != new.http.max_sessions,
        "http.max_sessions changed",
    );
    note_restart_required(
        &mut restart_required,
        old.http.sse_channel_capacity != new.http.sse_channel_capacity,
        "http.sse_channel_capacity changed",
    );
    note_restart_required(
        &mut restart_required,
        old.prefix_delimiter != new.prefix_delimiter,
        "prefix_delimiter changed",
    );
    note_restart_required(
        &mut restart_required,
        old.tool_filter_enabled != new.tool_filter_enabled,
        "tool_filter_enabled changed",
    );
    note_restart_required(
        &mut restart_required,
        old.tool_description_max_chars != new.tool_description_max_chars,
        "tool_description_max_chars changed",
    );
    note_restart_required(
        &mut restart_required,
        old.tool_search_threshold != new.tool_search_threshold,
        "tool_search_threshold changed",
    );
    note_restart_required(
        &mut restart_required,
        old.priority_tools != new.priority_tools,
        "priority_tools changed",
    );
    note_restart_required(
        &mut restart_required,
        old.disabled_tools != new.disabled_tools,
        "disabled_tools changed",
    );
    note_restart_required(
        &mut restart_required,
        old.daemon_grace_period_secs != new.daemon_grace_period_secs,
        "daemon_grace_period_secs changed",
    );

    let settings_changed = !restart_required.is_empty();

    ConfigDiff {
        added,
        removed,
        changed,
        unchanged,
        settings_changed,
        restart_required,
    }
}

fn note_restart_required(restart_required: &mut Vec<String>, changed: bool, setting: &str) {
    if changed {
        restart_required.push(format!("{setting} (restart required)"));
    }
}

/// Check if a server config has materially changed (requiring restart).
fn server_config_changed(old: &ServerConfig, new: &ServerConfig) -> bool {
    old.command != new.command
        || old.args != new.args
        || old.env != new.env
        || old.transport != new.transport
        || old.url != new.url
        || old.timeout_secs != new.timeout_secs
        || old.call_timeout_secs != new.call_timeout_secs
        || old.enabled != new.enabled
        || old.auth_token.as_ref().map(|t| t.as_str())
            != new.auth_token.as_ref().map(|t| t.as_str())
        || old.auth != new.auth
        || old.oauth_client_id != new.oauth_client_id
        || old.oauth_scopes != new.oauth_scopes
        || old.health_check_interval_secs != new.health_check_interval_secs
}

/// Apply a config diff to the running engine.
///
/// Steps:
/// 1. Stop removed servers
/// 2. Restart changed servers
/// 3. Start added servers
/// 4. Refresh tool cache
/// 5. Swap config via ArcSwap
/// 6. Emit ConfigReloaded event
pub async fn apply_reload(
    engine: &std::sync::Arc<crate::engine::Engine>,
    new_config: Config,
) -> Result<ReloadReport, anyhow::Error> {
    let old_config = engine.config();
    let diff = diff_configs(&old_config, &new_config);

    let mut report = ReloadReport {
        added: diff.added.iter().map(|(n, _)| n.clone()).collect(),
        removed: diff.removed.clone(),
        changed: diff.changed.iter().map(|(n, _)| n.clone()).collect(),
        unchanged: diff.unchanged.clone(),
        settings_changed: diff.settings_changed,
        restart_required: diff.restart_required.clone(),
        errors: Vec::new(),
    };

    let server_manager = engine.server_manager();
    let mut spawn_after_swap: Vec<(String, ServerConfig)> = Vec::new();

    // 1. Stop removed servers
    for name in &diff.removed {
        tracing::info!(server = %name, "stopping removed server");
        server_manager.stop_server(name).await;
        engine.clear_health_task_generation(name);
        engine.clear_refresh_task_generation(name);
    }

    // 2. Stop changed servers before their replacement startup batch begins.
    for (name, _new_cfg) in &diff.changed {
        tracing::info!(server = %name, "restarting changed server");
        server_manager.stop_server(name).await;
    }

    let mut start_actions = Vec::new();
    start_actions.extend(diff.changed.iter().map(|(name, cfg)| ReloadStartAction {
        name: name.clone(),
        config: cfg.clone(),
        kind: ReloadStartKind::Restart,
    }));
    start_actions.extend(diff.added.iter().map(|(name, cfg)| ReloadStartAction {
        name: name.clone(),
        config: cfg.clone(),
        kind: ReloadStartKind::Start,
    }));

    let start_results = run_reload_start_actions(
        start_actions,
        RELOAD_START_CONCURRENCY,
        {
            let server_manager = server_manager.clone();
            move |action| {
                let server_manager = server_manager.clone();
                async move {
                    match action.kind {
                        ReloadStartKind::Restart => {
                            tracing::info!(server = %action.name, "starting replacement for changed server");
                        }
                        ReloadStartKind::Start => {
                            tracing::info!(server = %action.name, "starting new server");
                        }
                    }

                    server_manager
                        .start_and_register(&action.name, &action.config)
                        .await
                        .map_err(|e| e.to_string())
                }
            }
        },
    )
    .await;

    // 3. Record startup outcomes after the batch completes so config swap and
    // downstream refresh happen once for the whole reload.
    for (action, result) in start_results {
        match result {
            Ok(()) => {
                if action.config.enabled {
                    spawn_after_swap.push((action.name.clone(), action.config.clone()));
                }
            }
            Err(error) => {
                if action.config.auth.as_deref() == Some("oauth") {
                    server_manager.mark_auth_required(&action.name);
                } else {
                    server_manager.mark_start_failure(&action.name);
                }
                let verb = match action.kind {
                    ReloadStartKind::Restart => "restart",
                    ReloadStartKind::Start => "start",
                };
                let msg = format!("failed to {verb} server {}: {error}", action.name);
                tracing::error!("{msg}");
                report.errors.push(msg);
            }
        }
    }

    // 4. Swap config atomically before spawning background tasks so new tasks
    // observe the updated server set immediately.
    engine.store_config(new_config);

    for (name, cfg) in &spawn_after_swap {
        engine.spawn_background_tasks_for_server(name, cfg);
    }

    // 5. Refresh tool cache
    engine.tool_router().refresh_tools().await;

    // 6. Emit event
    let _ = engine.event_sender().send(EngineEvent::ConfigReloaded);

    // Log restart-required warnings
    for warning in &diff.restart_required {
        tracing::warn!("{warning}");
    }

    Ok(report)
}

/// Report of what happened during a reload.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReloadReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
    pub unchanged: Vec<String>,
    pub settings_changed: bool,
    pub restart_required: Vec<String>,
    pub errors: Vec<String>,
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ServerConfig, TransportType};
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn make_server(command: &str) -> ServerConfig {
        ServerConfig {
            command: Some(command.to_string()),
            args: vec![],
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::Stdio,
            url: None,
            auth_token: None,
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
            timeout_secs: 30,
            call_timeout_secs: 300,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
        }
    }

    #[test]
    fn diff_detects_added_server() {
        let old = Config::default();
        let mut new = Config::default();
        new.servers.insert("github".into(), make_server("npx"));

        let diff = diff_configs(&old, &new);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].0, "github");
        assert!(diff.removed.is_empty());
        assert!(diff.changed.is_empty());
    }

    #[test]
    fn diff_detects_removed_server() {
        let mut old = Config::default();
        old.servers.insert("github".into(), make_server("npx"));
        let new = Config::default();

        let diff = diff_configs(&old, &new);
        assert!(diff.added.is_empty());
        assert_eq!(diff.removed, vec!["github"]);
        assert!(diff.changed.is_empty());
    }

    #[test]
    fn diff_detects_changed_server() {
        let mut old = Config::default();
        old.servers.insert("github".into(), make_server("npx"));

        let mut new = Config::default();
        new.servers.insert("github".into(), make_server("node"));

        let diff = diff_configs(&old, &new);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].0, "github");
    }

    #[test]
    fn diff_detects_unchanged_server() {
        let mut old = Config::default();
        old.servers.insert("github".into(), make_server("npx"));

        let mut new = Config::default();
        new.servers.insert("github".into(), make_server("npx"));

        let diff = diff_configs(&old, &new);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert!(diff.changed.is_empty());
        assert_eq!(diff.unchanged, vec!["github"]);
    }

    #[test]
    fn diff_detects_bind_address_change() {
        let old = Config::default();
        let mut new = Config::default();
        new.http.bind_address = "0.0.0.0".into();

        let diff = diff_configs(&old, &new);
        assert!(diff.settings_changed);
        assert!(!diff.restart_required.is_empty());
    }

    #[test]
    fn diff_marks_router_settings_restart_required() {
        let old = Config::default();
        let new = Config {
            priority_tools: vec!["plug__search_tools".into()],
            tool_description_max_chars: Some(128),
            tool_filter_enabled: false,
            ..Config::default()
        };

        let diff = diff_configs(&old, &new);

        assert!(diff.settings_changed);
        assert!(
            diff.restart_required
                .iter()
                .any(|item| item.contains("priority_tools"))
        );
        assert!(
            diff.restart_required
                .iter()
                .any(|item| item.contains("tool_description_max_chars"))
        );
        assert!(
            diff.restart_required
                .iter()
                .any(|item| item.contains("tool_filter_enabled"))
        );
    }

    #[test]
    fn diff_marks_http_session_settings_restart_required() {
        let old = Config::default();
        let mut new = Config::default();
        new.http.session_timeout_secs = 60;
        new.http.max_sessions = 5;
        new.http.sse_channel_capacity = 8;

        let diff = diff_configs(&old, &new);

        assert!(diff.settings_changed);
        assert!(
            diff.restart_required
                .iter()
                .any(|item| item.contains("http.session_timeout_secs"))
        );
        assert!(
            diff.restart_required
                .iter()
                .any(|item| item.contains("http.max_sessions"))
        );
        assert!(
            diff.restart_required
                .iter()
                .any(|item| item.contains("http.sse_channel_capacity"))
        );
    }

    #[test]
    fn diff_env_change_triggers_restart() {
        let mut old = Config::default();
        let mut srv = make_server("npx");
        srv.env.insert("KEY".into(), "old".into());
        old.servers.insert("github".into(), srv);

        let mut new = Config::default();
        let mut srv = make_server("npx");
        srv.env.insert("KEY".into(), "new".into());
        new.servers.insert("github".into(), srv);

        let diff = diff_configs(&old, &new);
        assert_eq!(diff.changed.len(), 1);
    }

    #[test]
    fn diff_health_interval_change_triggers_restart() {
        let mut old = Config::default();
        old.servers.insert("github".into(), make_server("npx"));

        let mut new = Config::default();
        let mut server = make_server("npx");
        server.health_check_interval_secs = 5;
        new.servers.insert("github".into(), server);

        let diff = diff_configs(&old, &new);
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].0, "github");
    }

    #[tokio::test]
    async fn run_reload_start_actions_is_bounded_and_concurrent() {
        let actions = vec![
            ReloadStartAction {
                name: "one".into(),
                config: make_server("one"),
                kind: ReloadStartKind::Start,
            },
            ReloadStartAction {
                name: "two".into(),
                config: make_server("two"),
                kind: ReloadStartKind::Start,
            },
            ReloadStartAction {
                name: "three".into(),
                config: make_server("three"),
                kind: ReloadStartKind::Restart,
            },
        ];

        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let results = run_reload_start_actions(actions, 2, {
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            move |_action| {
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                async move {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(current, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                }
            }
        })
        .await;

        assert_eq!(results.len(), 3);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert!(results.into_iter().all(|(_, result)| result.is_ok()));
    }
}
