use std::sync::{Arc, Weak};

use serde_json::{Map, Value};

use super::{WatchAccess, WatchConfig};
use crate::slack_events::EVENT_SCOPE;

/// Reads the live config, grants, and servers. Nothing is copied into event
/// storage: every check goes to the running daemon.
pub struct RuntimeWatchAccess {
    engine: Weak<crate::engine::Engine>,
    oauth: crate::downstream_oauth::DownstreamOauthManager,
}

impl RuntimeWatchAccess {
    pub fn new(
        engine: &Arc<crate::engine::Engine>,
        oauth: crate::downstream_oauth::DownstreamOauthManager,
    ) -> Self {
        Self {
            engine: Arc::downgrade(engine),
            oauth,
        }
    }
}

#[async_trait::async_trait]
impl WatchAccess for RuntimeWatchAccess {
    fn watches(&self) -> Vec<WatchConfig> {
        let Some(engine) = self.engine.upgrade() else {
            return Vec::new();
        };
        let config = engine.config();
        if !config.http.modern_downstream_enabled
            || config.http.auth_mode != crate::config::DownstreamAuthMode::Oauth
            || !config
                .http
                .oauth_scopes
                .as_ref()
                .is_some_and(|scopes| scopes.iter().any(|scope| scope == EVENT_SCOPE))
        {
            return Vec::new();
        }
        config
            .events
            .watch
            .iter()
            .filter(|watch| {
                config
                    .servers
                    .get(&watch.server)
                    .is_some_and(|server| server.enabled)
            })
            .cloned()
            .collect()
    }

    fn may_use(&self, client_id: &str, watch: &WatchConfig) -> bool {
        self.engine.upgrade().is_some_and(|engine| {
            engine.tool_router().client_may_watch(
                Some(&crate::ipc::grant_client_key(client_id)),
                &watch.server,
                &watch.tool,
            )
        })
    }

    async fn permits(&self, client_id: &str, watch: &WatchConfig) -> bool {
        self.may_use(client_id, watch)
            && self
                .oauth
                .permits_event_delivery(client_id, EVENT_SCOPE)
                .await
    }

    fn tool(&self, watch: &WatchConfig) -> Option<(String, bool)> {
        self.engine
            .upgrade()?
            .tool_router()
            .watched_tool(&watch.server, &watch.tool)
    }

    async fn call(&self, tool_name: &str, arguments: Map<String, Value>) -> Option<Value> {
        let engine = self.engine.upgrade()?;
        let result = engine
            .tool_router()
            .call_tool(tool_name, Some(arguments))
            .await
            .ok()?;
        if result.is_error == Some(true) {
            return None;
        }
        serde_json::to_value(&result).ok()
    }
}
