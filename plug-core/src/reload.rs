//! Config hot-reload: what changed between two configs, and what each change
//! takes. The engine's `reload_config` applies it.
//!
//! Reload is triggered by:
//! - `plug reload` CLI command via daemon IPC
//! - File watcher (`watcher.rs`, 500ms debounce via `notify`)
//! - Operator mutations and tool-block pinning, which write the config and
//!   reload it themselves

use std::collections::HashSet;

use crate::config::{Config, HttpConfig, ServerConfig, UpstreamProtocolMode};

#[derive(Debug, Clone)]
pub(crate) struct ConfigDiff {
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

pub(crate) fn diff_configs(old: &Config, new: &Config) -> ConfigDiff {
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

    let restart_required = settings_needing_restart(old, new);
    let settings_changed = !restart_required.is_empty()
        || old.disabled_tools != new.disabled_tools
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

/// The settings outside `servers` that changed and are read only when Plug
/// starts. The destructuring names every field of [`Config`] and
/// [`HttpConfig`], so a field added to either does not compile until it is
/// classified here as live or restart-only.
fn settings_needing_restart(old: &Config, new: &Config) -> Vec<String> {
    let Config {
        log_level,
        prefix_delimiter,
        enable_prefix,
        // Read by each reload's server starts.
        startup_concurrency: _,
        tool_filter_enabled,
        tool_description_max_chars,
        tool_search_threshold,
        meta_tool_mode,
        lazy_tools,
        priority_tools,
        // Applied by `Engine::store_config`.
        disabled_tools: _,
        clients: _,
        secrets: _,
        modern_upstream_enabled: _,
        // Watches and supervision policy are read from the live config.
        events: _,
        supervision: _,
        // Classified per server by `server_change`.
        servers: _,
        http,
    } = new;
    let HttpConfig {
        slack_events,
        // Applied by `Engine::store_config`.
        modern_downstream_enabled: _,
        auth_mode,
        public_base_url,
        oauth_scopes,
        bind_address,
        port,
        allowed_origins,
        tls_cert_path,
        tls_key_path,
        session_timeout_secs,
        max_sessions,
        sse_channel_capacity,
    } = http;

    let old_http = &old.http;
    [
        ("log_level", *log_level != old.log_level),
        ("http.bind_address", *bind_address != old_http.bind_address),
        ("http.port", *port != old_http.port),
        (
            "http.allowed_origins",
            *allowed_origins != old_http.allowed_origins,
        ),
        ("http.auth_mode", *auth_mode != old_http.auth_mode),
        (
            "http.public_base_url",
            *public_base_url != old_http.public_base_url,
        ),
        ("http.oauth_scopes", *oauth_scopes != old_http.oauth_scopes),
        (
            "http.tls_cert_path",
            *tls_cert_path != old_http.tls_cert_path,
        ),
        ("http.tls_key_path", *tls_key_path != old_http.tls_key_path),
        ("http.slack_events", *slack_events != old_http.slack_events),
        (
            "http.session_timeout_secs",
            *session_timeout_secs != old_http.session_timeout_secs,
        ),
        ("http.max_sessions", *max_sessions != old_http.max_sessions),
        (
            "http.sse_channel_capacity",
            *sse_channel_capacity != old_http.sse_channel_capacity,
        ),
        (
            "prefix_delimiter",
            *prefix_delimiter != old.prefix_delimiter,
        ),
        ("enable_prefix", *enable_prefix != old.enable_prefix),
        (
            "tool_filter_enabled",
            *tool_filter_enabled != old.tool_filter_enabled,
        ),
        (
            "tool_description_max_chars",
            *tool_description_max_chars != old.tool_description_max_chars,
        ),
        (
            "tool_search_threshold",
            *tool_search_threshold != old.tool_search_threshold,
        ),
        ("meta_tool_mode", *meta_tool_mode != old.meta_tool_mode),
        ("lazy_tools", *lazy_tools != old.lazy_tools),
        ("priority_tools", *priority_tools != old.priority_tools),
    ]
    .into_iter()
    .filter(|(_, changed)| *changed)
    .map(|(setting, _)| format!("{setting} changed (restart required)"))
    .collect()
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

    /// Each setting read only when Plug starts is reported by its own name,
    /// and a setting the engine applies live is not.
    #[test]
    fn startup_only_settings_say_a_restart_is_needed() {
        type Edit = fn(&mut Config);
        let restart_only: [(&str, Edit); 9] = [
            ("enable_prefix", |c| c.enable_prefix = false),
            ("log_level", |c| c.log_level = "debug".into()),
            ("http.auth_mode", |c| {
                c.http.auth_mode = crate::config::DownstreamAuthMode::Bearer
            }),
            ("http.public_base_url", |c| {
                c.http.public_base_url = Some("https://plug.example".into())
            }),
            ("http.oauth_scopes", |c| {
                c.http.oauth_scopes = Some(vec!["tools:read".into()])
            }),
            ("http.tls_cert_path", |c| {
                c.http.tls_cert_path = Some("cert.pem".into())
            }),
            ("http.tls_key_path", |c| {
                c.http.tls_key_path = Some("key.pem".into())
            }),
            ("http.session_timeout_secs", |c| {
                c.http.session_timeout_secs = 60
            }),
            ("priority_tools", |c| {
                c.priority_tools = vec!["plug__search_tools".into()]
            }),
        ];
        for (setting, edit) in restart_only {
            let old = Config::default();
            let mut new = old.clone();
            edit(&mut new);
            let diff = diff_configs(&old, &new);
            assert_eq!(
                diff.restart_required,
                [format!("{setting} changed (restart required)")],
                "{setting}"
            );
            assert!(diff.settings_changed, "{setting}");
        }

        let live: [Edit; 4] = [
            |c| c.startup_concurrency = 1,
            |c| c.disabled_tools = vec!["figma__get_file".into()],
            |c| c.supervision.enabled = !c.supervision.enabled,
            |c| c.http.modern_downstream_enabled = true,
        ];
        for edit in live {
            let old = Config::default();
            let mut new = old.clone();
            edit(&mut new);
            assert!(diff_configs(&old, &new).restart_required.is_empty());
        }
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
