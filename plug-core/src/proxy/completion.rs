use super::*;

impl super::ToolRouter {
    pub async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult, McpError> {
        if uri.starts_with("plug://artifact/") {
            return self.artifact_store.read(uri);
        }

        let snapshot = self.cache.load();
        let server_id = snapshot.resource_routes.get(uri).cloned().ok_or_else(|| {
            McpError::from(ProtocolError::InvalidRequest {
                detail: format!("resource not found: {uri}"),
            })
        })?;
        drop(snapshot);

        let upstream = self
            .server_manager
            .get_upstream(&server_id)
            .ok_or_else(|| {
                McpError::from(ProtocolError::ServerUnavailable {
                    server_id: server_id.clone(),
                })
            })?;

        upstream
            .client
            .peer()
            .read_resource(ReadResourceRequestParams::new(uri))
            .await
            .map_err(|error| match error {
                rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                other => McpError::internal_error(other.to_string(), None),
            })
    }

    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<GetPromptResult, McpError> {
        let snapshot = self.cache.load();
        let (server_id, prompt_name) =
            snapshot.prompt_routes.get(name).cloned().ok_or_else(|| {
                McpError::from(ProtocolError::InvalidRequest {
                    detail: format!("prompt not found: {name}"),
                })
            })?;
        drop(snapshot);

        let upstream = self
            .server_manager
            .get_upstream(&server_id)
            .ok_or_else(|| {
                McpError::from(ProtocolError::ServerUnavailable {
                    server_id: server_id.clone(),
                })
            })?;

        let mut request = GetPromptRequestParams::new(prompt_name);
        if let Some(arguments) = arguments {
            request = request.with_arguments(arguments);
        }

        upstream
            .client
            .peer()
            .get_prompt(request)
            .await
            .map_err(|error| match error {
                rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                other => McpError::internal_error(other.to_string(), None),
            })
    }

    /// Forward a `completion/complete` request to the correct upstream server
    /// based on the reference type (prompt name or resource URI).
    pub async fn complete_request(
        &self,
        mut params: CompleteRequestParams,
    ) -> Result<CompleteResult, McpError> {
        let snapshot = self.cache.load();
        let server_id = match &params.r#ref {
            Reference::Prompt(prompt_ref) => {
                let (sid, original_name) = snapshot
                    .prompt_routes
                    .get(&prompt_ref.name)
                    .cloned()
                    .ok_or_else(|| {
                        McpError::from(ProtocolError::InvalidRequest {
                            detail: format!("prompt not found: {}", prompt_ref.name),
                        })
                    })?;
                // Rewrite ref to use the original upstream prompt name
                params.r#ref = Reference::for_prompt(original_name);
                sid
            }
            Reference::Resource(resource_ref) => snapshot
                .resource_routes
                .get(&resource_ref.uri)
                .cloned()
                .ok_or_else(|| {
                    McpError::from(ProtocolError::InvalidRequest {
                        detail: format!("resource not found: {}", resource_ref.uri),
                    })
                })?,
            _ => {
                return Err(McpError::invalid_params(
                    "unsupported completion reference type",
                    None,
                ));
            }
        };
        drop(snapshot);

        let upstream = self
            .server_manager
            .get_upstream(&server_id)
            .ok_or_else(|| {
                McpError::from(ProtocolError::ServerUnavailable {
                    server_id: server_id.clone(),
                })
            })?;

        upstream
            .client
            .peer()
            .complete(params)
            .await
            .map_err(|error| match error {
                rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                other => McpError::internal_error(other.to_string(), None),
            })
    }
}
