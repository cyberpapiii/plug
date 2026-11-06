//! The daemon's IPC adapter for the MCP dispatcher in `plug_core::dispatch`.

use std::sync::Arc;

use rmcp::model::RequestId;

use plug_core::dispatch::IpcContext;
use plug_core::ipc::{IpcMcpRequestContext, IpcResponse};

use super::ConnectionContext;

/// Who is asking, as the daemon's client registry knows them.
pub(super) fn ipc_context(
    ctx: &ConnectionContext,
    session_id: &str,
    request_context: Option<&IpcMcpRequestContext>,
) -> IpcContext {
    let registry_client_id = ctx.client_registry.client_id(session_id);
    // IPC teardown deregisters the client before it cleans up tasks, so a
    // task created after that sees the client gone. Client ids recur on
    // reconnect by design, so a probe that sees the client registered again
    // accepts the create: the same `ipc:<client_id>` owner is live again.
    let owner_liveness = registry_client_id.clone().map(|client_id| {
        let registry = Arc::clone(&ctx.client_registry);
        Arc::new(move || registry.has_client(&client_id)) as plug_core::tasks::OwnerLivenessProbe
    });
    let request_id = request_context
        .map(|context| context.request_id.clone())
        .unwrap_or_else(|| {
            RequestId::from(rmcp::model::NumberOrString::String(Arc::from(
                format!("ipc-{session_id}-{}", uuid::Uuid::new_v4()).as_str(),
            )))
        });
    let mut context = IpcContext::new(session_id, request_id);
    context.registry_client_id = registry_client_id.map(Arc::<str>::from);
    context.client_type = ctx
        .client_registry
        .client_info(session_id)
        .map(|info| plug_core::client_detect::detect_client(&info))
        .unwrap_or(plug_core::types::ClientType::Unknown);
    context.client_key = ctx
        .client_registry
        .client_key(session_id)
        .map(Arc::<str>::from);
    context.modern_enabled = ctx.engine.tool_router().modern_downstream_enabled();
    context.owner_liveness = owner_liveness;
    if let Some(request) = request_context {
        context.protocol_version = Arc::from(request.protocol_version.as_str());
        if let (Some(name), Some(version)) = (&request.client_name, &request.client_version) {
            context.client_metadata = Some(plug_core::protocol::ClientMetadata {
                name: Arc::from(name.as_str()),
                version: Arc::from(version.as_str()),
            });
        }
    }
    context
}

pub(super) async fn dispatch_mcp_request(
    ctx: &ConnectionContext,
    session_id: &str,
    method: &str,
    params: Option<&serde_json::Value>,
    request_context: Option<&IpcMcpRequestContext>,
) -> IpcResponse {
    // The socket is bound before upstream startup finishes, so a request can
    // arrive against a catalog that is still filling. Every answer here
    // describes the merged surface, and a partial one is indistinguishable from
    // a complete one to the client, so wait for the engine rather than reply
    // with less than the daemon has.
    ctx.engine.wait_until_ready().await;
    let context = ipc_context(ctx, session_id, request_context);
    IpcResponse::from_mcp_result(
        plug_core::dispatch::handle(ctx.engine.tool_router(), &context, method, params.cloned())
            .await,
    )
}
