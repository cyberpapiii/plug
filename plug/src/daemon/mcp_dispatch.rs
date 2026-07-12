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

use plug_core::ipc::IpcResponse;

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
    request_id: RequestId,
    client_type: plug_core::types::ClientType,
    owner: Option<plug_core::tasks::TaskOwner>,
}

impl plug_core::dispatch::DownstreamContext for IpcDownstreamContext {
    fn downstream_call_context(&self) -> plug_core::proxy::DownstreamCallContext {
        plug_core::proxy::DownstreamCallContext::ipc_for_client(
            Arc::clone(&self.session_id),
            self.request_id.clone(),
            self.client_type,
        )
    }

    fn task_owner(&self) -> Result<plug_core::tasks::TaskOwner, McpError> {
        self.owner.clone().ok_or_else(|| {
            McpError::internal_error("ipc task owner was not resolved".to_string(), None)
        })
    }
}

/// Encode a serializable value as an IPC `McpResponse` payload, falling back to
/// a `SERIALIZE_ERROR` frame if serialization fails. The single encode primitive
/// for IPC method results — replaces the per-arm `match serde_json::to_value`
/// ladder so every arm shares one fallback path.
pub(super) fn ipc_ok<T: serde::Serialize>(value: T) -> IpcResponse {
    match serde_json::to_value(value) {
        Ok(payload) => IpcResponse::McpResponse { payload },
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
pub(super) fn ipc_from_mcp_result<T: serde::Serialize>(result: Result<T, McpError>) -> IpcResponse {
    match result {
        Ok(value) => ipc_ok(value),
        Err(err) => ipc_ok(err),
    }
}

/// Dispatch an MCP JSON-RPC request through the daemon's shared ToolRouter.
pub(super) async fn dispatch_mcp_request(
    ctx: &ConnectionContext,
    session_id: &str,
    method: &str,
    params: Option<&serde_json::Value>,
) -> IpcResponse {
    let tool_router = ctx.engine.tool_router();

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
            ipc_ok(result)
        }

        "resources/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let result = tool_router.list_resources_page(request);
            ipc_ok(result)
        }

        "resources/templates/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let result = tool_router.list_resource_templates_page(request);
            ipc_ok(result)
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

            ipc_from_mcp_result(tool_router.read_resource(uri).await)
        }

        "prompts/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let result = tool_router.list_prompts_page(request);
            ipc_ok(result)
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

            ipc_from_mcp_result(tool_router.get_prompt(name, arguments).await)
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

            ipc_from_mcp_result(tool_router.complete_request(params).await)
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
            ipc_ok(serde_json::json!({}))
        }

        "tools/call" => {
            let call_params = match params
                .map(|p| serde_json::from_value::<rmcp::model::CallToolRequestParams>(p.clone()))
            {
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
            let owner = if call_params.task.is_some() {
                let Some(client_id) = ctx.client_registry.client_id(session_id) else {
                    return IpcResponse::Error {
                        code: "UNKNOWN_SESSION".to_string(),
                        message: "session not found".to_string(),
                    };
                };
                Some(plug_core::proxy::ToolRouter::task_owner_for_ipc_client(
                    &client_id,
                ))
            } else {
                None
            };

            // Synthetic request ID — the IPC protocol doesn't carry JSON-RPC IDs,
            // but the context needs one for active call tracking.
            let request_id = RequestId::from(rmcp::model::NumberOrString::String(Arc::from(
                format!("ipc-{session_id}-{}", uuid::Uuid::new_v4()).as_str(),
            )));
            let downstream_ctx = IpcDownstreamContext {
                session_id: Arc::from(session_id),
                request_id,
                client_type,
                owner,
            };

            match plug_core::dispatch::dispatch_tools_call(
                tool_router,
                &downstream_ctx,
                call_params,
            )
            .await
            {
                Ok(plug_core::dispatch::ToolCallOutcome::Called(result)) => ipc_ok(result),
                Ok(plug_core::dispatch::ToolCallOutcome::TaskCreated(result)) => ipc_ok(result),
                Err(mcp_err) => ipc_ok(mcp_err),
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
            ipc_from_mcp_result(tool_router.list_tasks_for_owner(&owner, request).await)
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
            ipc_from_mcp_result(tool_router.get_task_info_for_owner(&owner, task_id).await)
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
            ipc_from_mcp_result(tool_router.get_task_result_for_owner(&owner, task_id).await)
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
            ipc_from_mcp_result(tool_router.cancel_task_for_owner(&owner, task_id).await)
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
            ipc_from_mcp_result(
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
            ipc_from_mcp_result(
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
