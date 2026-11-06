//! The config of Plug's events: the tools it watches, the Slack source, and
//! what `http` needs before remote clients can subscribe.
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{Config, DownstreamAuthMode, HttpConfig};

/// The OAuth scope a client needs to subscribe to any event.
pub const EVENT_SCOPE: &str = "events:subscribe";
/// The shortest interval a watch may ask for.
pub const MIN_WATCH_SECS: u64 = 30;
/// The one event the Slack source makes.
pub const SLACK_EVENT_NAME: &str = "slack.ditto_message";
/// The upstream the Slack source reads through.
pub const SLACK_SERVER: &str = "slack";
/// The furthest ahead `owner_proof_until` may be set.
const MAX_OWNER_PROOF_SECS: u64 = 3600;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsConfig {
    /// Tools watched for change. Each one is an event clients can subscribe to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch: Vec<WatchConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchConfig {
    /// The event is named `<server>.<name>`.
    pub name: String,
    pub server: String,
    /// The tool's own name on that server, without Plug's prefix.
    pub tool: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub arguments: Map<String, Value>,
    #[serde(default = "default_every_secs")]
    pub every_secs: u64,
    /// Watch a tool the server does not mark read-only. Off by default,
    /// because watching must not change anything.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_writes: bool,
}

fn default_every_secs() -> u64 {
    300
}

impl WatchConfig {
    pub fn event_name(&self) -> String {
        format!("{}.{}", self.server, self.name)
    }
}

impl EventsConfig {
    pub fn is_empty(&self) -> bool {
        self.watch.is_empty()
    }

    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let mut names = HashSet::new();
        for watch in &self.watch {
            let event = watch.event_name();
            if watch.name.is_empty()
                || watch.name.len() > 64
                || !watch
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                errors.push(format!(
                    "events.watch name '{}' must be lowercase letters, digits, and underscores",
                    watch.name
                ));
            }
            if watch.tool.is_empty() {
                errors.push(format!("events.watch '{event}' names no tool"));
            }
            if watch.every_secs < MIN_WATCH_SECS {
                errors.push(format!(
                    "events.watch '{event}' every_secs must be at least {MIN_WATCH_SECS}"
                ));
            }
            if !names.insert(event.clone()) {
                errors.push(format!("events.watch '{event}' is already an event"));
            }
        }
        errors
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlackEventsConfig {
    pub team_id: String,
    pub app_id: String,
    pub owner_user_id: String,
    pub dot_user_id: String,
    /// Existing downstream OAuth registration explicitly chosen by the owner.
    pub subscriber_client_id: String,
    /// Until this Unix time, an owner message that is exactly
    /// `<@dot> PLUG_EVENTS_PROOF_20261003` counts, so the owner can prove
    /// delivery alone. At most an hour ahead; off by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_proof_until: Option<u64>,
}

impl SlackEventsConfig {
    pub fn validate(&self) -> Vec<String> {
        let valid_id = |value: &str, prefix| {
            value.starts_with(prefix)
                && value.len() > 1
                && value
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        };
        let mut errors = Vec::new();
        for (name, value, prefix) in [
            ("team_id", &self.team_id, 'T'),
            ("app_id", &self.app_id, 'A'),
            ("owner_user_id", &self.owner_user_id, 'U'),
            ("dot_user_id", &self.dot_user_id, 'U'),
        ] {
            if !valid_id(value, prefix) {
                errors.push(format!("http.slack_events.{name} is invalid"));
            }
        }
        if self.owner_user_id == self.dot_user_id || self.subscriber_client_id.is_empty() {
            errors.push(
                "http.slack_events requires distinct owner/dot users and an explicit OAuth client"
                    .into(),
            );
        }
        if self
            .owner_proof_until
            .is_some_and(|until| until > unix_now().saturating_add(MAX_OWNER_PROOF_SECS))
        {
            errors
                .push("http.slack_events.owner_proof_until cannot exceed one hour from now".into());
        }
        errors
    }
}

/// What `http` lacks before remote clients can subscribe to events, each as
/// the end of a sentence starting "... needs". Empty when events are possible.
pub fn events_missing(http: &HttpConfig) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if !http.modern_downstream_enabled || http.auth_mode != DownstreamAuthMode::Oauth {
        missing.push(
            "remote clients that sign in: set http.modern_downstream_enabled = true and http.auth_mode = \"oauth\"",
        );
    }
    if !http
        .oauth_scopes
        .as_ref()
        .is_some_and(|scopes| scopes.iter().any(|scope| scope == EVENT_SCOPE))
    {
        missing.push("\"events:subscribe\" in http.oauth_scopes");
    }
    missing
}

pub fn events_possible(http: &HttpConfig) -> bool {
    events_missing(http).is_empty()
}

impl Config {
    /// Whether the Slack server is configured and turned on.
    pub fn slack_upstream_enabled(&self) -> bool {
        self.servers
            .get(SLACK_SERVER)
            .is_some_and(|server| server.enabled)
    }

    /// What is wrong with the events this config asks for.
    pub(super) fn event_errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let events_missing = events_missing(&self.http);
        if let Some(events) = &self.http.slack_events {
            errors.extend(events.validate());
            errors.extend(
                events_missing
                    .iter()
                    .map(|missing| format!("http.slack_events needs {missing}")),
            );
            if !self.slack_upstream_enabled() {
                errors.push("http.slack_events requires the enabled slack upstream".into());
            }
        }
        errors.extend(self.events.validate());
        if !self.events.watch.is_empty() {
            errors.extend(
                events_missing
                    .iter()
                    .map(|missing| format!("events.watch needs {missing}")),
            );
            for watch in &self.events.watch {
                let event = watch.event_name();
                if !self.servers.contains_key(&watch.server) {
                    errors.push(format!(
                        "events.watch '{event}' names a server that is not configured"
                    ));
                }
                if event == SLACK_EVENT_NAME {
                    errors.push(format!("events.watch '{event}' is already an event"));
                }
            }
        }
        errors
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
