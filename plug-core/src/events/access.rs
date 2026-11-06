use std::sync::{Arc, Weak};

use serde_json::{Map, Value};

use super::{EVENT_SCOPE, WatchAccess, WatchConfig};

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
        if !super::events_possible(&config.http) {
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

    async fn call(&self, tool_name: &str, arguments: Map<String, Value>) -> Result<Value, String> {
        let engine = self.engine.upgrade().ok_or("Plug is stopping")?;
        let result = engine
            .tool_router()
            .call_tool(tool_name, Some(arguments))
            .await
            .map_err(|error| format!("the call failed with code {}", error.code.0))?;
        if result.is_error == Some(true) {
            return Err("the tool returned an error".into());
        }
        serde_json::to_value(&result).map_err(|_| "the result is not JSON".into())
    }
}
