use super::*;

impl super::ToolRouter {
    pub async fn read_resource(
        &self,
        uri: &str,
        client_key: Option<&str>,
    ) -> Result<ReadResourceResult, McpError> {
        if uri.starts_with("plug://artifact/") {
            // An artifact is a tool's result, so whoever is kept from the
            // tool is kept from it.
            if self
                .artifact_store
                .source(uri)
                .is_some_and(|(server, tool, listed)| {
                    !self.client_may_read_result_of(
                        client_key,
                        server.as_deref(),
                        tool.as_deref(),
                        &listed,
                    )
                })
            {
                return Err(McpError::from(ProtocolError::InvalidRequest {
                    detail: format!("artifact not found: {uri}"),
                }));
            }
            return self.artifact_store.read(uri);
        }

        let snapshot = self.cache.load();
        let server_id = resolve_resource_route(&snapshot.resource_routes, uri)
            .or_else(|| {
                // A skill's files need not be listed: its server's name in
                // the URI is the route.
                skills::named_server(uri)
                    .filter(|server| self.server_manager.get_upstream(server).is_some())
                    .map(str::to_string)
            })
            .or_else(|| skills::only_server(&snapshot.resource_routes, uri))
            .filter(|server_id| self.client_may_use_all_of_server(client_key, server_id))
            .ok_or_else(|| {
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

        let mut result = upstream
            .client
            .peer()
            .read_resource(ReadResourceRequestParams::new(
                skills::inward(&server_id, uri).into_owned(),
            ))
            .await
            .map_err(|error| match error {
                rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                other => McpError::internal_error(other.to_string(), None),
            })?;
        admit_catalog_meta(&mut result.meta, "resource-read-result");
        for contents in &mut result.contents {
            skills::outward_contents(&server_id, contents);
            admit_resource_contents_meta(contents);
        }
        Ok(result)
    }

    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        client_key: Option<&str>,
    ) -> Result<GetPromptResult, McpError> {
        let snapshot = self.cache.load();
        let (server_id, prompt_name) = snapshot
            .prompt_routes
            .get(name)
            .filter(|(server_id, _)| self.client_may_use_all_of_server(client_key, server_id))
            .cloned()
            .ok_or_else(|| {
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

        let mut result = upstream
            .client
            .peer()
            .get_prompt(request)
            .await
            .map_err(|error| match error {
                rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                other => McpError::internal_error(other.to_string(), None),
            })?;
        admit_catalog_meta(&mut result.meta, "prompt-result");
        for message in &mut result.messages {
            admit_content_block_meta(&mut message.content);
        }
        Ok(result)
    }

    /// Forward a `completion/complete` request to the correct upstream server
    /// based on the reference type (prompt name or resource URI).
    pub async fn complete_request(
        &self,
        mut params: CompleteRequestParams,
        client_key: Option<&str>,
    ) -> Result<CompleteResult, McpError> {
        let snapshot = self.cache.load();
        let server_id = match &params.r#ref {
            Reference::Prompt(prompt_ref) => {
                let (sid, original_name) = snapshot
                    .prompt_routes
                    .get(&prompt_ref.name)
                    .filter(|(server_id, _)| {
                        self.client_may_use_all_of_server(client_key, server_id)
                    })
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
            Reference::Resource(resource_ref) => {
                let sid = snapshot
                    .resource_routes
                    .get(&resource_ref.uri)
                    .cloned()
                    .or_else(|| {
                        resolve_resource_route(&snapshot.resource_routes, &resource_ref.uri)
                    })
                    .filter(|server_id| self.client_may_use_all_of_server(client_key, server_id))
                    .ok_or_else(|| {
                        McpError::from(ProtocolError::InvalidRequest {
                            detail: format!("resource not found: {}", resource_ref.uri),
                        })
                    })?;
                if let std::borrow::Cow::Owned(known) = skills::inward(&sid, &resource_ref.uri) {
                    params.r#ref = Reference::for_resource(known);
                }
                sid
            }
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

        let mut result =
            upstream
                .client
                .peer()
                .complete(params)
                .await
                .map_err(|error| match error {
                    rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                    other => McpError::internal_error(other.to_string(), None),
                })?;
        admit_catalog_meta(&mut result.meta, "completion-result");
        Ok(result)
    }
}

/// Resolve a concrete resource URI against exact routes first and advertised
/// URI templates second. The matcher deliberately implements the useful MCP
/// subset of RFC 6570 without accepting arbitrary cross-server fallbacks:
/// simple variables stay within one path segment, while reserved (`+`) and
/// fragment (`#`) expansions may contain slashes.
fn resolve_resource_route(
    routes: &std::collections::HashMap<String, String>,
    uri: &str,
) -> Option<String> {
    if let Some(server) = routes.get(uri) {
        return Some(server.clone());
    }

    let mut matches = routes
        .iter()
        .filter(|(template, _)| template.contains('{') && uri_template_matches(template, uri))
        .map(|(_, server)| server);
    let first = matches.next()?.clone();
    if matches.any(|server| server != &first) {
        return None;
    }
    Some(first)
}

fn uri_template_matches(template: &str, uri: &str) -> bool {
    let mut template_rest = template;
    let mut uri_rest = uri;

    while let Some(open) = template_rest.find('{') {
        let literal = &template_rest[..open];
        let Some(after_literal) = uri_rest.strip_prefix(literal) else {
            return false;
        };
        uri_rest = after_literal;

        let Some(close_offset) = template_rest[open + 1..].find('}') else {
            return false;
        };
        let close = open + 1 + close_offset;
        let expression = &template_rest[open + 1..close];
        if expression.is_empty() {
            return false;
        }
        template_rest = &template_rest[close + 1..];

        let next_literal_end = template_rest.find('{').unwrap_or(template_rest.len());
        let next_literal = &template_rest[..next_literal_end];
        let consumed = if next_literal.is_empty() {
            uri_rest.len()
        } else if let Some(index) = uri_rest.find(next_literal) {
            index
        } else {
            return false;
        };
        let value = &uri_rest[..consumed];
        if value.is_empty() {
            return false;
        }

        let operator = expression.as_bytes()[0] as char;
        if !matches!(operator, '+' | '#') && value.contains('/') {
            return false;
        }
        uri_rest = &uri_rest[consumed..];
    }

    uri_rest == template_rest
}

#[cfg(test)]
mod resource_template_route_tests {
    use super::*;

    #[test]
    fn resolves_concrete_uri_from_advertised_template() {
        let routes = std::collections::HashMap::from([(
            "test://template/{id}/data".to_string(),
            "fixture".to_string(),
        )]);

        assert_eq!(
            resolve_resource_route(&routes, "test://template/123/data").as_deref(),
            Some("fixture")
        );
        assert!(resolve_resource_route(&routes, "test://template/a/b/data").is_none());
    }

    #[test]
    fn rejects_ambiguous_template_owners() {
        let routes = std::collections::HashMap::from([
            ("test://{id}/data".to_string(), "one".to_string()),
            ("test://{name}/data".to_string(), "two".to_string()),
        ]);

        assert!(resolve_resource_route(&routes, "test://123/data").is_none());
    }
}
