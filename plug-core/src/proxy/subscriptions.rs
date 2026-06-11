use super::*;

impl super::ToolRouter {
    /// Subscribe a downstream client to resource updates for a given URI.
    ///
    /// On the first subscriber for a URI, forwards the subscribe request to the
    /// upstream server. Returns an error if the upstream does not support subscriptions
    /// or the resource URI is unknown.
    pub async fn subscribe_resource(
        &self,
        uri: &str,
        target: NotificationTarget,
    ) -> Result<(), McpError> {
        let snapshot = self.cache.load();
        let server_id = snapshot.resource_routes.get(uri).cloned().ok_or_else(|| {
            McpError::from(ProtocolError::InvalidRequest {
                detail: format!("resource not found: {uri}"),
            })
        })?;
        drop(snapshot);

        // Check upstream supports subscriptions
        let upstream = self
            .server_manager
            .get_upstream(&server_id)
            .ok_or_else(|| {
                McpError::from(ProtocolError::ServerUnavailable {
                    server_id: server_id.clone(),
                })
            })?;
        let supports_subscribe = upstream
            .capabilities
            .resources
            .as_ref()
            .and_then(|r| r.subscribe)
            .unwrap_or(false);
        if !supports_subscribe {
            return Err(McpError::invalid_request(
                format!("server {server_id} does not support resource subscriptions"),
                None,
            ));
        }

        let mut entry = self
            .resource_subscriptions
            .entry(uri.to_string())
            .or_default();
        let is_first = entry.is_empty();
        entry.insert(target.clone());
        drop(entry);

        if is_first {
            if let Err(error) = upstream
                .client
                .peer()
                .subscribe(SubscribeRequestParams::new(uri))
                .await
            {
                // Roll back the local subscription on upstream failure
                if let Some(mut entry) = self.resource_subscriptions.get_mut(uri) {
                    entry.remove(&target);
                    if entry.is_empty() {
                        drop(entry);
                        self.resource_subscriptions.remove(uri);
                    }
                }
                return Err(match error {
                    rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                    other => McpError::internal_error(other.to_string(), None),
                });
            }
        }

        Ok(())
    }

    /// Unsubscribe a downstream client from resource updates.
    ///
    /// When the last subscriber is removed, forwards the unsubscribe to upstream.
    pub async fn unsubscribe_resource(
        &self,
        uri: &str,
        target: &NotificationTarget,
    ) -> Result<(), McpError> {
        let snapshot = self.cache.load();
        let server_id = snapshot.resource_routes.get(uri).cloned().ok_or_else(|| {
            McpError::from(ProtocolError::InvalidRequest {
                detail: format!("resource not found: {uri}"),
            })
        })?;
        drop(snapshot);

        let should_unsubscribe_upstream = {
            let mut entry = match self.resource_subscriptions.get_mut(uri) {
                Some(e) => e,
                None => return Ok(()),
            };
            entry.remove(target);
            entry.is_empty()
        };

        if should_unsubscribe_upstream {
            self.resource_subscriptions.remove(uri);

            if let Some(upstream) = self.server_manager.get_upstream(&server_id) {
                let _ = upstream
                    .client
                    .peer()
                    .unsubscribe(
                        serde_json::from_value::<UnsubscribeRequestParams>(
                            serde_json::json!({ "uri": uri }),
                        )
                        .expect("UnsubscribeRequestParams from known-good JSON"),
                    )
                    .await;
            }
        }

        Ok(())
    }

    /// Remove all subscriptions for a given downstream target (cleanup on disconnect).
    ///
    /// Iterates all subscription entries and removes the target. When a URI
    /// transitions from 1 → 0 subscribers, forwards `unsubscribe` upstream.
    pub async fn cleanup_subscriptions_for_target(&self, target: &NotificationTarget) {
        let mut uris_to_unsubscribe: Vec<(String, String)> = Vec::new();

        // Collect URIs where this target is subscribed
        self.resource_subscriptions.retain(|uri, subscribers| {
            subscribers.remove(target);
            if subscribers.is_empty() {
                // Need to unsubscribe upstream — resolve server_id from cache
                let snapshot = self.cache.load();
                if let Some(server_id) = snapshot.resource_routes.get(uri).cloned() {
                    uris_to_unsubscribe.push((uri.clone(), server_id));
                }
                false // remove the empty entry
            } else {
                true // keep entries that still have subscribers
            }
        });

        // Send upstream unsubscribe for each URI that lost its last subscriber
        for (uri, server_id) in uris_to_unsubscribe {
            if let Some(upstream) = self.server_manager.get_upstream(&server_id) {
                if let Err(error) = upstream
                    .client
                    .peer()
                    .unsubscribe(
                        serde_json::from_value::<UnsubscribeRequestParams>(
                            serde_json::json!({ "uri": uri }),
                        )
                        .expect("UnsubscribeRequestParams from known-good JSON"),
                    )
                    .await
                {
                    tracing::warn!(
                        uri = %uri,
                        error = %error,
                        "failed to unsubscribe upstream during target cleanup"
                    );
                }
            }
        }
    }

    /// Route an upstream resource-updated notification to subscribed downstream clients.
    pub(crate) fn route_upstream_resource_updated(&self, params: ResourceUpdatedNotificationParam) {
        let subscribers = match self.resource_subscriptions.get(&params.uri) {
            Some(entry) => entry.clone(),
            None => return,
        };

        for target in subscribers {
            self.publish_protocol_notification(ProtocolNotification::ResourceUpdated {
                target,
                params: params.clone(),
            });
        }
    }
}
