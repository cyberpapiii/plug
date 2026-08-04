//! The MCP JSON-RPC method dispatcher for the daemon's IPC surface.
//!
//! Routes `tools/*`, `resources/*`, `prompts/*`, `tasks/*`, and
//! `completion/complete` IPC requests through the shared ToolRouter, the
//! same dispatcher stdio and HTTP downstream transports use.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{
    PaginatedRequestParams, RequestId, SubscribeRequestParams, UnsubscribeRequestParams,
};

use plug_core::ipc::{IpcMcpRequestContext, IpcResponse};

use super::ConnectionContext;

/// Downstream bridge for IPC proxy clients.
///
/// IPC adapter for the shared `tools/call` dispatcher.
///
/// IPC has a first-class downstream identity: `DownstreamTransport::Ipc`, the
/// `ipc:{session_id}` lazy session-key namespace, and `NotificationTarget::Ipc`
/// (the KTD3 split — it no longer masquerades as stdio). The task owner is
/// pre-resolved by the shim so the transport-specific `UNKNOWN_SESSION` error
/// frame is preserved for a task-augmented call whose session vanished.
struct IpcDownstreamContext {
    session_id: Arc<str>,
    registry_client_id: Option<Arc<str>>,
    request_id: RequestId,
    client_type: plug_core::types::ClientType,
    owner: Option<plug_core::tasks::TaskOwner>,
    /// Pre-built alongside `owner` (both derive from the resolved client id):
    /// reports whether the client registry still holds a session for that
    /// client id, mirroring the `client_sessions.contains_key` gate the
    /// daemon's disconnect teardown uses before task cleanup.
    owner_liveness: Option<plug_core::tasks::OwnerLivenessProbe>,
    request_context: Option<IpcMcpRequestContext>,
    modern_direction_enabled: bool,
}

impl plug_core::dispatch::DownstreamContext for IpcDownstreamContext {
    fn downstream_call_context(&self) -> plug_core::proxy::DownstreamCallContext {
        let context = plug_core::proxy::DownstreamCallContext::ipc_for_client(
            Arc::clone(&self.session_id),
            self.request_id.clone(),
            self.client_type,
        );
        let context = match &self.registry_client_id {
            Some(client_id) => context.with_local_principal(
                plug_core::types::PrincipalId::daemon_ipc_registry(client_id),
            ),
            None => context,
        };
        match &self.request_context {
            Some(request) => {
                let version = serde_json::from_value(serde_json::Value::String(
                    request.protocol_version.clone(),
                ))
                .unwrap_or_else(|_| plug_core::protocol::supported_protocol_version());
                let context = context
                    .with_protocol(
                        plug_core::protocol::ProtocolEra::from_version(&version),
                        request.protocol_version.clone(),
                    )
                    .with_modern_direction_enabled(self.modern_direction_enabled);
                match (&request.client_name, &request.client_version) {
                    (Some(name), Some(version)) => {
                        context.with_client_metadata(name.clone(), version.clone())
                    }
                    _ => context,
                }
            }
            None => context,
        }
    }

    fn task_owner(&self) -> Result<plug_core::tasks::TaskOwner, McpError> {
        self.owner.clone().ok_or_else(|| {
            McpError::internal_error("ipc task owner was not resolved".to_string(), None)
        })
    }

    /// Registry-liveness probe for the enqueue path's post-guard re-check.
    /// IPC teardown deregisters the client (removing it from
    /// `client_sessions`) BEFORE running task cleanup, matching the ordering
    /// the check site in `proxy::tasks` relies on. IPC client ids recur on
    /// reconnect by design, so a probe observing a re-registered client
    /// accepts the create — benign, since the same `ipc:<client_id>` owner is
    /// live again; the tombstone catch-all remains the backstop.
    fn owner_liveness_probe(&self) -> Option<plug_core::tasks::OwnerLivenessProbe> {
        self.owner_liveness.clone()
    }
}

/// Encode a serializable value as an IPC `McpResponse` payload, falling back to
/// a `SERIALIZE_ERROR` frame if serialization fails. The single encode primitive
/// for IPC method results — replaces the per-arm `match serde_json::to_value`
/// ladder so every arm shares one fallback path.
#[cfg(test)]
pub(super) fn ipc_ok<T: serde::Serialize>(value: T) -> IpcResponse {
    ipc_ok_with_context(value, None)
}

fn ipc_ok_with_context<T: serde::Serialize>(
    value: T,
    context: Option<&IpcMcpRequestContext>,
) -> IpcResponse {
    match serde_json::to_value(value) {
        Ok(mut payload) => {
            // The daemon IPC MCP bridge serves the same legacy protocol as the
            // downstream stdio and HTTP adapters, but carries a bare result
            // rather than a JSON-RPC envelope. Keep modern RMCP 3.x result
            // discriminators from leaking onto that legacy surface.
            if !context.is_some_and(|context| {
                context.protocol_version == plug_core::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION
            }) {
                plug_core::protocol::rewrite_legacy_result(&mut payload, false);
            }
            IpcResponse::McpResponse { payload }
        }
        Err(e) => IpcResponse::Error {
            code: "SERIALIZE_ERROR".to_string(),
            message: e.to_string(),
        },
    }
}

/// Encode a `Result<T, McpError>` from the shared router as an IPC response:
/// success serializes to an `McpResponse` payload; an `McpError` serializes into
/// an `McpResponse`-with-error payload (the IPC convention — errors ride the same
/// channel, distinguished by a `code` field). Both paths share the
/// `SERIALIZE_ERROR` fallback via [`ipc_ok`].
pub(super) fn ipc_from_mcp_result<T: serde::Serialize>(
    result: Result<T, McpError>,
    context: Option<&IpcMcpRequestContext>,
) -> IpcResponse {
    match result {
        Ok(value) => ipc_ok_with_context(value, context),
        Err(err) => ipc_ok_with_context(err, context),
    }
}

/// Dispatch an MCP JSON-RPC request through the daemon's shared ToolRouter.
pub(super) async fn dispatch_mcp_request(
    ctx: &ConnectionContext,
    session_id: &str,
    method: &str,
    params: Option<&serde_json::Value>,
    request_context: Option<&IpcMcpRequestContext>,
) -> IpcResponse {
    let tool_router = ctx.engine.tool_router();

    if let Some(response) =
        reject_invalid_request_context(tool_router.modern_downstream_enabled(), request_context)
    {
        return response;
    }

    macro_rules! ipc_ok {
        ($value:expr) => {
            ipc_ok_with_context($value, request_context)
        };
    }
    macro_rules! ipc_result {
        ($value:expr $(,)?) => {
            ipc_from_mcp_result($value, request_context)
        };
    }

    match method {
        "tools/list" => {
            // Determine client type from session's client_info
            let client_type = ctx
                .client_registry
                .client_info(session_id)
                .map(|info| plug_core::client_detect::detect_client(&info))
                .unwrap_or(plug_core::types::ClientType::Unknown);

            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let lazy_session_key = plug_core::proxy::ToolRouter::lazy_session_key(
                plug_core::proxy::DownstreamTransport::Ipc,
                session_id,
            );
            let result = tool_router.list_tools_page_for_client_session(
                client_type,
                Some(&lazy_session_key),
                request,
            );
            ipc_ok!(result)
        }

        "resources/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let result = tool_router.list_resources_page(request);
            ipc_ok!(result)
        }

        "resources/templates/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let result = tool_router.list_resource_templates_page(request);
            ipc_ok!(result)
        }

        "resources/read" => {
            let uri = match params.and_then(|p| p.get("uri")).and_then(|v| v.as_str()) {
                Some(uri) => uri,
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "resources/read requires 'uri' in params".to_string(),
                    };
                }
            };

            ipc_result!(tool_router.read_resource(uri).await)
        }

        "prompts/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let result = tool_router.list_prompts_page(request);
            ipc_ok!(result)
        }

        "prompts/get" => {
            let name = match params.and_then(|p| p.get("name")).and_then(|v| v.as_str()) {
                Some(name) if !name.is_empty() => name,
                _ => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "prompts/get requires non-empty 'name'".to_string(),
                    };
                }
            };
            let arguments = params
                .and_then(|p| p.get("arguments"))
                .and_then(|v| v.as_object())
                .cloned();

            ipc_result!(tool_router.get_prompt(name, arguments).await)
        }

        "completion/complete" => {
            let params: rmcp::model::CompleteRequestParams = match params
                .map(|p| serde_json::from_value::<rmcp::model::CompleteRequestParams>(p.clone()))
            {
                Some(Ok(p)) => p,
                Some(Err(e)) => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: format!("completion/complete: {e}"),
                    };
                }
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "completion/complete requires params".to_string(),
                    };
                }
            };

            ipc_result!(tool_router.complete_request(params).await)
        }

        "logging/setLevel" => {
            let level = match params.and_then(|p| p.get("level")).and_then(|v| v.as_str()) {
                Some(level_str) => {
                    match serde_json::from_value::<rmcp::model::LoggingLevel>(serde_json::json!(
                        level_str
                    )) {
                        Ok(level) => level,
                        Err(_) => {
                            return IpcResponse::Error {
                                code: "INVALID_PARAMS".to_string(),
                                message: format!("invalid logging level: {level_str}"),
                            };
                        }
                    }
                }
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "logging/setLevel requires 'level' in params".to_string(),
                    };
                }
            };

            tracing::info!(
                session_id = %session_id,
                level = ?level,
                "IPC client set log level"
            );
            tool_router.set_client_log_level(session_id, level);
            tool_router.forward_set_level_to_upstreams().await;
            ipc_ok!(serde_json::json!({}))
        }

        "tools/call" => {
            let call_params = match params.map(|params| {
                let mut params = params.clone();
                if !request_context.is_some_and(|context| {
                    context.protocol_version
                        == plug_core::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION
                }) {
                    plug_core::protocol::rewrite_legacy_call_params(&mut params);
                }
                serde_json::from_value::<rmcp::model::CallToolRequestParams>(params)
            }) {
                Some(Ok(p)) => p,
                Some(Err(e)) => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: format!("tools/call: {e}"),
                    };
                }
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "tools/call requires params".to_string(),
                    };
                }
            };

            // An empty / unknown tool name is left to the shared dispatcher so all
            // three transports return the identical router error (ToolNotFound ->
            // METHOD_NOT_FOUND) rather than IPC short-circuiting with its own frame.

            // Build downstream context so the ToolRouter can route reverse
            // requests (elicitation, sampling) back to this IPC client.
            let client_type = ctx
                .client_registry
                .client_info(session_id)
                .map(|info| plug_core::client_detect::detect_client(&info))
                .unwrap_or(plug_core::types::ClientType::Unknown);

            // Pre-resolve the task owner so the transport-specific UNKNOWN_SESSION
            // error frame is preserved for a task-augmented call whose session
            // vanished (the dispatcher only sees an opaque McpError otherwise).
            // The liveness probe is built from the same resolved client id.
            let registry_client_id = ctx.client_registry.client_id(session_id);
            let (owner, owner_liveness) =
                if call_params.meta.as_ref().is_some_and(|meta| {
                    meta.contains_key(plug_core::protocol::LEGACY_TASK_REQUEST_KEY)
                }) {
                    let Some(client_id) = registry_client_id.clone() else {
                        return IpcResponse::Error {
                            code: "UNKNOWN_SESSION".to_string(),
                            message: "session not found".to_string(),
                        };
                    };
                    let owner = plug_core::proxy::ToolRouter::task_owner_for_ipc_client(&client_id);
                    let registry = Arc::clone(&ctx.client_registry);
                    let probe: plug_core::tasks::OwnerLivenessProbe =
                        Arc::new(move || registry.client_sessions.contains_key(&client_id));
                    (Some(owner), Some(probe))
                } else {
                    (None, None)
                };

            let request_id = request_context
                .map(|context| context.request_id.clone())
                .unwrap_or_else(|| {
                    RequestId::from(rmcp::model::NumberOrString::String(Arc::from(
                        format!("ipc-{session_id}-{}", uuid::Uuid::new_v4()).as_str(),
                    )))
                });
            let downstream_ctx = IpcDownstreamContext {
                session_id: Arc::from(session_id),
                registry_client_id: registry_client_id.map(Arc::<str>::from),
                request_id,
                client_type,
                owner,
                owner_liveness,
                request_context: request_context.cloned(),
                modern_direction_enabled: tool_router.modern_downstream_enabled()
                    && request_context.is_some_and(|context| {
                        context.protocol_version
                            == plug_core::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION
                    }),
            };

            match plug_core::dispatch::dispatch_tools_call(
                tool_router,
                &downstream_ctx,
                call_params,
            )
            .await
            {
                Ok(plug_core::dispatch::ToolCallOutcome::Called(result)) => ipc_ok!(result),
                Ok(plug_core::dispatch::ToolCallOutcome::TaskCreated(result)) => ipc_ok!(result),
                Err(mcp_err) => ipc_ok!(mcp_err),
            }
        }

        "tasks/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let Some(client_id) = ctx.client_registry.client_id(session_id) else {
                return IpcResponse::Error {
                    code: "UNKNOWN_SESSION".to_string(),
                    message: "session not found".to_string(),
                };
            };
            let owner = plug_core::proxy::ToolRouter::task_owner_for_ipc_client(&client_id);
            ipc_result!(tool_router.list_tasks_for_owner(&owner, request).await)
        }

        "tasks/get" => {
            let task_id = match params
                .and_then(|p| p.get("taskId"))
                .and_then(|v| v.as_str())
                .filter(|task_id| !task_id.is_empty())
            {
                Some(task_id) => task_id,
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "tasks/get requires non-empty 'taskId'".to_string(),
                    };
                }
            };
            let Some(client_id) = ctx.client_registry.client_id(session_id) else {
                return IpcResponse::Error {
                    code: "UNKNOWN_SESSION".to_string(),
                    message: "session not found".to_string(),
                };
            };
            let owner = plug_core::proxy::ToolRouter::task_owner_for_ipc_client(&client_id);
            ipc_result!(tool_router.get_task_info_for_owner(&owner, task_id).await)
        }

        "tasks/result" => {
            let task_id = match params
                .and_then(|p| p.get("taskId"))
                .and_then(|v| v.as_str())
                .filter(|task_id| !task_id.is_empty())
            {
                Some(task_id) => task_id,
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "tasks/result requires non-empty 'taskId'".to_string(),
                    };
                }
            };
            let Some(client_id) = ctx.client_registry.client_id(session_id) else {
                return IpcResponse::Error {
                    code: "UNKNOWN_SESSION".to_string(),
                    message: "session not found".to_string(),
                };
            };
            let owner = plug_core::proxy::ToolRouter::task_owner_for_ipc_client(&client_id);
            ipc_result!(tool_router.get_task_result_for_owner(&owner, task_id).await)
        }

        "tasks/cancel" => {
            let task_id = match params
                .and_then(|p| p.get("taskId"))
                .and_then(|v| v.as_str())
                .filter(|task_id| !task_id.is_empty())
            {
                Some(task_id) => task_id,
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "tasks/cancel requires non-empty 'taskId'".to_string(),
                    };
                }
            };
            let Some(client_id) = ctx.client_registry.client_id(session_id) else {
                return IpcResponse::Error {
                    code: "UNKNOWN_SESSION".to_string(),
                    message: "session not found".to_string(),
                };
            };
            let owner = plug_core::proxy::ToolRouter::task_owner_for_ipc_client(&client_id);
            ipc_result!(tool_router.cancel_task_for_owner(&owner, task_id).await)
        }

        "resources/subscribe" => {
            let request =
                match params.map(|p| serde_json::from_value::<SubscribeRequestParams>(p.clone())) {
                    Some(Ok(request)) => request,
                    Some(Err(e)) => {
                        return IpcResponse::Error {
                            code: "INVALID_PARAMS".to_string(),
                            message: format!("resources/subscribe: {e}"),
                        };
                    }
                    None => {
                        return IpcResponse::Error {
                            code: "INVALID_PARAMS".to_string(),
                            message: "resources/subscribe requires params".to_string(),
                        };
                    }
                };
            let target = plug_core::notifications::NotificationTarget::Ipc {
                client_id: Arc::from(session_id),
            };
            // Empty success encodes as `{}` (not `null`) to match stdio/HTTP.
            ipc_result!(
                tool_router
                    .subscribe_resource(&request.uri, target)
                    .await
                    .map(|()| serde_json::json!({})),
            )
        }

        "resources/unsubscribe" => {
            let request = match params
                .map(|p| serde_json::from_value::<UnsubscribeRequestParams>(p.clone()))
            {
                Some(Ok(request)) => request,
                Some(Err(e)) => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: format!("resources/unsubscribe: {e}"),
                    };
                }
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "resources/unsubscribe requires params".to_string(),
                    };
                }
            };
            let target = plug_core::notifications::NotificationTarget::Ipc {
                client_id: Arc::from(session_id),
            };
            // Empty success encodes as `{}` (not `null`) to match stdio/HTTP.
            ipc_result!(
                tool_router
                    .unsubscribe_resource(&request.uri, &target)
                    .await
                    .map(|()| serde_json::json!({})),
            )
        }

        _ => IpcResponse::Error {
            code: "UNSUPPORTED_METHOD".to_string(),
            message: format!("MCP method '{method}' not supported via IPC proxy"),
        },
    }
}

fn reject_invalid_request_context(
    modern_downstream_enabled: bool,
    context: Option<&IpcMcpRequestContext>,
) -> Option<IpcResponse> {
    if context.is_some_and(|context| {
        context.protocol_version == plug_core::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION
    }) && !modern_downstream_enabled
    {
        return Some(IpcResponse::Error {
            code: "UNSUPPORTED_PROTOCOL_VERSION".to_string(),
            message: "modern downstream MCP is disabled by daemon configuration".to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_ok_encodes_value_as_mcp_response() {
        let resp = ipc_ok(serde_json::json!({ "a": 1 }));
        let IpcResponse::McpResponse { payload } = resp else {
            panic!("expected McpResponse, got {resp:?}");
        };
        assert_eq!(payload, serde_json::json!({ "a": 1 }));

        // A value that fails serialization (a map with non-string tuple keys)
        // takes the SERIALIZE_ERROR fallback frame rather than an McpResponse.
        let mut unserializable = std::collections::BTreeMap::new();
        unserializable.insert((1_i32, 2_i32), 3_i32);
        match ipc_ok(unserializable) {
            IpcResponse::Error { code, .. } => assert_eq!(code, "SERIALIZE_ERROR"),
            other => panic!("expected SERIALIZE_ERROR frame, got {other:?}"),
        }
    }

    #[test]
    fn ipc_from_mcp_result_encodes_ok_and_err() {
        // Ok -> McpResponse with the serialized value.
        let ok =
            ipc_from_mcp_result::<serde_json::Value>(Ok(serde_json::json!({ "ok": true })), None);
        let IpcResponse::McpResponse { payload } = ok else {
            panic!("expected McpResponse for Ok, got {ok:?}");
        };
        assert_eq!(payload, serde_json::json!({ "ok": true }));

        // Err -> McpResponse carrying the serialized McpError (code + message),
        // the IPC convention where errors ride the same channel.
        let err = ipc_from_mcp_result::<serde_json::Value>(
            Err(McpError::invalid_params("boom".to_string(), None)),
            None,
        );
        let IpcResponse::McpResponse { payload } = err else {
            panic!("expected McpResponse for Err, got {err:?}");
        };
        assert_eq!(payload["code"].as_i64(), Some(-32602));
        assert_eq!(payload["message"].as_str(), Some("boom"));
    }

    #[test]
    fn forged_modern_context_is_rejected_when_daemon_gate_is_off() {
        let context = IpcMcpRequestContext {
            request_id: RequestId::from(rmcp::model::NumberOrString::Number(7_i64)),
            protocol_version: plug_core::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION.to_string(),
            client_name: Some("forged-client".to_string()),
            client_version: Some("1.0".to_string()),
        };

        let error = reject_invalid_request_context(false, Some(&context))
            .expect("daemon-authoritative gate must reject forged modern context");
        assert!(matches!(
            error,
            IpcResponse::Error { ref code, .. } if code == "UNSUPPORTED_PROTOCOL_VERSION"
        ));
        assert!(reject_invalid_request_context(true, Some(&context)).is_none());
    }

    #[test]
    fn contextual_ipc_request_preserves_protocol_client_and_principal() {
        use plug_core::dispatch::DownstreamContext as _;

        let request_id = RequestId::from(rmcp::model::NumberOrString::Number(23));
        let downstream = IpcDownstreamContext {
            session_id: Arc::from("transport-session"),
            registry_client_id: Some(Arc::from("stable-client")),
            request_id: request_id.clone(),
            client_type: plug_core::types::ClientType::Unknown,
            owner: None,
            owner_liveness: None,
            request_context: Some(IpcMcpRequestContext {
                request_id: request_id.clone(),
                protocol_version: plug_core::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION
                    .to_string(),
                client_name: Some("modern-client".to_string()),
                client_version: Some("2.0".to_string()),
            }),
            modern_direction_enabled: true,
        }
        .downstream_call_context();

        assert_eq!(downstream.request_id, request_id);
        assert_eq!(
            downstream.protocol_era,
            plug_core::protocol::ProtocolEra::Modern
        );
        assert!(downstream.modern_direction_enabled);
        assert_eq!(downstream.protocol_version.as_ref(), "2026-07-28");
        assert_eq!(
            downstream.principal,
            Some(plug_core::types::PrincipalId::daemon_ipc_registry(
                "stable-client"
            ))
        );
        let metadata = downstream.client_metadata.expect("client metadata");
        assert_eq!(metadata.name.as_ref(), "modern-client");
        assert_eq!(metadata.version.as_ref(), "2.0");
    }
}
