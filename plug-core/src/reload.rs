//! Config hot-reload: what changed between two configs, and what each change
//! takes. [`crate::engine::Engine::reload_config`] applies it.
//!
//! Reload is triggered by:
//! - `plug reload` CLI command via daemon IPC
//! - File watcher (`watcher.rs`, 500ms debounce via `notify`)

use std::collections::HashSet;

use crate::config::{Config, ServerConfig, UpstreamProtocolMode};

/// Diff result between old and new configs.
#[derive(Debug, Clone)]
pub struct ConfigDiff {
    pub added: Vec<(String, ServerConfig)>,
    pub removed: Vec<String>,
    /// Servers to stop and start again with their new config.
    pub changed: Vec<(String, ServerConfig)>,
    /// Servers that keep running, including those in `updated`.
    pub unchanged: Vec<String>,
    /// Running servers whose new settings apply without a restart.
    pub updated: Vec<(String, ServerConfig)>,
    /// Whether non-server settings changed (bind address, prefix, etc.).
    pub settings_changed: bool,
    /// Settings that take effect only when Plug restarts.
    pub restart_required: Vec<String>,
}

/// What a change to one server's settings takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ServerChange {
    None,
    /// Applies to the running server: its call limits, circuit breaker,
    /// tool names and groups, and health interval.
    Live,
    /// The server is stopped and started again.
    Restart,
}

/// Classify every field of [`ServerConfig`]. The destructuring names each
/// one, so a field added to the config does not compile until it is
/// classified here.
pub(crate) fn server_change(old: &ServerConfig, new: &ServerConfig) -> ServerChange {
    let ServerConfig {
        command,
        args,
        env,
        enabled,
        transport,
        protocol_mode,
        url,
        auth_token,
        auth,
        oauth_client_id,
        oauth_scopes,
        timeout_secs,
        call_timeout_secs,
        max_concurrent,
        health_check_interval_secs,
        circuit_breaker_enabled,
        // Read into the router when Plug starts; reported by `diff_configs`.
        enrichment: _,
        tool_renames,
        tool_groups,
        sandbox,
        spec,
        operations,
        token_in,
    } = new;
    let restart = *command != old.command
        || *args != old.args
        || *env != old.env
        || *enabled != old.enabled
        || *transport != old.transport
        || *protocol_mode != old.protocol_mode
        || *url != old.url
        || *auth_token != old.auth_token
        || *auth != old.auth
        || *oauth_client_id != old.oauth_client_id
        || *oauth_scopes != old.oauth_scopes
        || *timeout_secs != old.timeout_secs
        || *call_timeout_secs != old.call_timeout_secs
        || *sandbox != old.sandbox
        || *spec != old.spec
        || *operations != old.operations
        || *token_in != old.token_in;
    let live = *max_concurrent != old.max_concurrent
        || *health_check_interval_secs != old.health_check_interval_secs
        || *circuit_breaker_enabled != old.circuit_breaker_enabled
        || *tool_renames != old.tool_renames
        || *tool_groups != old.tool_groups;
    if restart {
        ServerChange::Restart
    } else if live {
        ServerChange::Live
    } else {
        ServerChange::None
    }
}

/// Whether `new` needs the server restarted to take effect.
pub(crate) fn server_config_changed(old: &ServerConfig, new: &ServerConfig) -> bool {
    server_change(old, new) == ServerChange::Restart
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
    let mut updated = Vec::new();

    for name in old_names.intersection(&new_names) {
        let old_cfg = &old.servers[*name];
        let new_cfg = &new.servers[*name];
        let gate_change_requires_renegotiation = old.modern_upstream_enabled
            != new.modern_upstream_enabled
            && (old_cfg.protocol_mode != UpstreamProtocolMode::Legacy
                || new_cfg.protocol_mode != UpstreamProtocolMode::Legacy);
        let change = server_change(old_cfg, new_cfg);
        if change == ServerChange::Restart || gate_change_requires_renegotiation {
            changed.push(((*name).clone(), new_cfg.clone()));
        } else {
            unchanged.push((*name).clone());
            if change == ServerChange::Live {
                updated.push(((*name).clone(), new_cfg.clone()));
            }
        }
    }

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
        old.meta_tool_mode != new.meta_tool_mode,
        "meta_tool_mode changed",
    );
    note_restart_required(
        &mut restart_required,
        old.lazy_tools != new.lazy_tools,
        "lazy_tools changed",
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

    let enriched = |config: &Config| -> std::collections::BTreeSet<String> {
        config
            .servers
            .iter()
            .filter(|(_, server)| server.enrichment)
            .map(|(name, _)| name.clone())
            .collect()
    };
    note_restart_required(
        &mut restart_required,
        enriched(old) != enriched(new),
        "servers.*.enrichment changed",
    );

    let settings_changed = !restart_required.is_empty()
        || old.http.modern_downstream_enabled != new.http.modern_downstream_enabled
        || old.modern_upstream_enabled != new.modern_upstream_enabled;

    ConfigDiff {
        added,
        removed,
        changed,
        unchanged,
        updated,
        settings_changed,
        restart_required,
    }
}

fn note_restart_required(restart_required: &mut Vec<String>, changed: bool, setting: &str) {
    if changed {
        restart_required.push(format!("{setting} (restart required)"));
    }
}

/// Report of what happened during a reload.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReloadReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
    pub unchanged: Vec<String>,
    pub settings_changed: bool,
    pub restart_required: Vec<String>,
    pub errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ServerConfig};
    use std::collections::HashMap;

    fn make_server(command: &str) -> ServerConfig {
        ServerConfig {
            command: Some(command.to_string()),
            ..Default::default()
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
    fn protocol_mode_or_live_gate_change_targets_only_affected_upstreams() {
        let mut old = Config::default();
        old.servers.insert("legacy".into(), make_server("legacy"));
        let mut auto = make_server("auto");
        auto.protocol_mode = UpstreamProtocolMode::Auto;
        old.servers.insert("auto".into(), auto.clone());

        let mut enabled = old.clone();
        enabled.modern_upstream_enabled = true;
        let diff = diff_configs(&old, &enabled);
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].0, "auto");
        assert_eq!(diff.unchanged, vec!["legacy"]);
        assert!(diff.settings_changed);

        let mut forced_modern = old.clone();
        forced_modern
            .servers
            .get_mut("legacy")
            .expect("legacy config")
            .protocol_mode = UpstreamProtocolMode::Modern;
        let diff = diff_configs(&old, &forced_modern);
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].0, "legacy");
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
            lazy_tools: crate::config::LazyToolsConfig {
                mode: crate::types::LazyToolSetting::Bridge,
                clients: HashMap::new(),
            },
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
        assert!(
            diff.restart_required
                .iter()
                .any(|item| item.contains("lazy_tools"))
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
    fn a_change_is_live_or_needs_a_restart() {
        let old = make_server("npx");
        let mut live = old.clone();
        live.health_check_interval_secs = 5;
        live.max_concurrent = 3;
        live.circuit_breaker_enabled = false;
        live.tool_renames.insert("a".into(), "b".into());
        assert_eq!(server_change(&old, &live), ServerChange::Live);
        assert_eq!(server_change(&old, &old.clone()), ServerChange::None);

        let mut sandboxed = old.clone();
        sandboxed.sandbox = Some(Default::default());
        assert_eq!(server_change(&old, &sandboxed), ServerChange::Restart);

        let mut config = Config::default();
        config.servers.insert("github".into(), old);
        let mut edited = config.clone();
        edited.servers.insert("github".into(), live);
        let diff = diff_configs(&config, &edited);
        assert!(diff.changed.is_empty());
        assert_eq!(diff.unchanged, vec!["github"]);
        assert_eq!(diff.updated.len(), 1);
    }
}
