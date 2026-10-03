use super::{Access, EVENT_SCOPE, EventAccess, SlackEventsConfig};
use serde_json::{Value, json};
use std::sync::{Arc, Weak};

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
            config.http.modern_downstream_enabled
                && config.http.auth_mode == crate::config::DownstreamAuthMode::Oauth
                && config.http.slack_events.as_ref() == Some(&self.config)
                && config
                    .http
                    .oauth_scopes
                    .as_ref()
                    .is_some_and(|s| s.iter().any(|s| s == EVENT_SCOPE))
                && config
                    .servers
                    .get("slack")
                    .is_some_and(|server| server.enabled)
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
            router.event_probe_tool("slack_auth_status"),
            router.event_probe_tool("conversations_history"),
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
            .or(result.structured_content.as_ref());
        let fallback = result
            .content
            .iter()
            .filter_map(|item| item.as_text())
            .find_map(|text| serde_json::from_str::<Value>(&text.text).ok());
        let Some(data) = data.or(fallback.as_ref()) else {
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
            Ok(result) => {
                let fallback = result
                    .content
                    .iter()
                    .filter_map(|item| item.as_text())
                    .find_map(|text| serde_json::from_str::<Value>(&text.text).ok());
                let value = result.structured_content.as_ref().or(fallback.as_ref());
                channel_error_access(value.and_then(|v| v["error"]["code"].as_str()))
            }
            Err(_) => Access::Unavailable,
        }
    }
}

// Only stable, structured permission failures discard a channel's queued data.
// Generic tool errors and network/rate-limit failures remain bounded retries.
fn channel_error_access(code: Option<&str>) -> Access {
    match code {
        Some(
            "channel_not_found" | "not_in_channel" | "no_permission" | "missing_scope"
            | "restricted_action",
        ) => Access::Denied,
        _ => Access::Unavailable,
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
