use super::{EventAccess, SlackEventsConfig};
use crate::config::SLACK_SERVER;
use crate::events::{Access, EVENT_SCOPE};
use rmcp::model::CallToolResult;
use serde_json::{Value, json};
use std::sync::{Arc, Weak};

// Facts about the third-party Slack MCP server this source reads through.
// They are that server's choices, not Plug's: a rename or a new output shape
// there is fixed here.

/// Reports the signed-in account as `is_oauth`, `is_bot_token`, and
/// `provider_identity.{team_id,user_id}`, under `data` when structured.
const AUTH_STATUS_TOOL: &str = "slack_auth_status";
/// Reads a channel. Failing with `error.code` says why.
const HISTORY_TOOL: &str = "conversations_history";

/// Only stable, structured permission failures discard a channel's queued
/// data. Generic tool errors and network/rate-limit failures remain bounded
/// retries.
fn channel_error_access(code: Option<&str>) -> Access {
    match code {
        Some(
            "channel_not_found" | "not_in_channel" | "no_permission" | "missing_scope"
            | "restricted_action",
        ) => Access::Denied,
        _ => Access::Unavailable,
    }
}

/// The JSON a tool returned: its structured content, or else the first text
/// item that parses.
fn result_json(result: &CallToolResult) -> Option<Value> {
    result.structured_content.clone().or_else(|| {
        result
            .content
            .iter()
            .filter_map(|item| item.as_text())
            .find_map(|text| serde_json::from_str::<Value>(&text.text).ok())
    })
}

/// No credentials are copied into event storage: probes use the existing live
/// upstream connection and the owner's durable downstream OAuth grant.
pub struct RuntimeAccess {
    engine: Weak<crate::engine::Engine>,
    oauth: crate::downstream_oauth::DownstreamOauthManager,
    config: SlackEventsConfig,
}
impl RuntimeAccess {
    pub fn new(
        engine: &Arc<crate::engine::Engine>,
        oauth: crate::downstream_oauth::DownstreamOauthManager,
        config: SlackEventsConfig,
    ) -> Self {
        Self {
            engine: Arc::downgrade(engine),
            oauth,
            config,
        }
    }
}
#[async_trait::async_trait]
impl EventAccess for RuntimeAccess {
    fn enabled(&self) -> bool {
        self.engine.upgrade().is_some_and(|engine| {
            let config = engine.config();
            crate::events::events_possible(&config.http)
                && config.http.slack_events.as_ref() == Some(&self.config)
                && config.slack_upstream_enabled()
        })
    }
    async fn check(&self, channel: Option<&str>) -> Access {
        if !self.enabled()
            || !self
                .oauth
                .permits_event_delivery(&self.config.subscriber_client_id, EVENT_SCOPE)
                .await
        {
            return Access::Denied;
        }
        let Some(engine) = self.engine.upgrade() else {
            return Access::Denied;
        };
        let router = engine.tool_router();
        let (Some(auth), Some(history)) = (
            router.listed_name(SLACK_SERVER, AUTH_STATUS_TOOL),
            router.listed_name(SLACK_SERVER, HISTORY_TOOL),
        ) else {
            return Access::Unavailable;
        };
        let Ok(result) = router.call_tool(&auth, None).await else {
            return Access::Unavailable;
        };
        if result.is_error == Some(true) {
            return Access::Unavailable;
        }
        let data = result
            .structured_content
            .as_ref()
            .and_then(|v| v.get("data"))
            .cloned()
            .or_else(|| result_json(&result));
        let Some(data) = data else {
            return Access::Unavailable;
        };
        if data["is_oauth"] != true
            || data["is_bot_token"] != false
            || data["provider_identity"]["team_id"] != self.config.team_id
            || data["provider_identity"]["user_id"] != self.config.owner_user_id
        {
            return Access::Denied;
        }
        let Some(channel) = channel else {
            return Access::Allowed;
        };
        // A fresh read is required; cached channel lists cannot establish current visibility.
        let arguments = json!({"channel_id":channel,"limit":"1"})
            .as_object()
            .cloned();
        match router.call_tool(&history, arguments).await {
            Ok(result) if result.is_error != Some(true) && self.enabled() => Access::Allowed,
            Ok(result) => channel_error_access(
                result_json(&result)
                    .as_ref()
                    .and_then(|v| v["error"]["code"].as_str()),
            ),
            Err(_) => Access::Unavailable,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visibility_error_classification_is_fail_closed_and_specific() {
        for code in [
            "channel_not_found",
            "not_in_channel",
            "no_permission",
            "missing_scope",
            "restricted_action",
        ] {
            assert_eq!(channel_error_access(Some(code)), Access::Denied);
        }
        for code in [
            None,
            Some("rate_limited"),
            Some("ratelimited"),
            Some("tool_error"),
            Some("timeout"),
        ] {
            assert_eq!(channel_error_access(code), Access::Unavailable);
        }
    }
}
