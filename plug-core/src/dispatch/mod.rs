//! The one MCP method dispatcher. Every downstream transport delegates here.
//!
//! A transport owns its framing, works out who the caller is and which session
//! the request belongs to, and hands over the method name and raw params as a
//! [`DownstreamContext`]. This module owns everything per method: the era's
//! method set, authorization, task admission, routing to the [`ToolRouter`],
//! capability projection, and the era's result vocabulary. A new MCP method is
//! added here, except the MCP Events methods (`events/list`,
//! `events/subscribe`, `events/unsubscribe`), which only HTTP serves, in
//! `http::server`.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, CompleteRequestParams, DiscoverResult, ErrorCode,
    GetPromptRequestParams, InitializeResult, PaginatedRequestParams, ReadResourceRequestParams,
    RequestId, RequestParamsMeta, ServerCapabilities, SetLevelRequestParams,
    SubscribeRequestParams, UnsubscribeRequestParams,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::protocol::{MethodFamily, ProtocolEra};
use crate::proxy::continuations::exceeds_continuation_limits;
use crate::proxy::{DownstreamCallContext, ToolRouter};
use crate::tasks::{OwnerLivenessProbe, TaskOwner};
use crate::types::ClientType;

/// What the dispatcher needs to know about the caller of one request.
pub trait DownstreamContext: Send + Sync {
    /// Who is calling, in which era, with what trust.
    fn downstream_call_context(&self) -> DownstreamCallContext;

    /// Whether the caller holds a session that per-connection state (the lazy
    /// tool catalog, resource subscriptions, log level) can hang on. A modern
    /// HTTP request is stateless and holds none.
    fn session_bound(&self) -> bool;

    /// Who owns the durable tasks this caller creates.
    fn task_owner(&self) -> Result<TaskOwner, McpError>;

    /// Re-checked by `enqueue_tool_task` after it registers its owner-create
    /// guard, closing the race where the owner's session is torn down entirely
    /// before the enqueue registers anything (see the check site in
    /// `proxy::tasks`).
    fn owner_liveness_probe(&self) -> Option<OwnerLivenessProbe> {
        None
    }

    /// The capabilities a modern client declares on this request, when the
    /// transport can serve the modern tasks extension. `None` means it cannot:
    /// the extension is neither advertised nor admitted.
    fn modern_task_client(&self) -> Option<&ClientCapabilities> {
        None
    }

    /// Whether `server/discover` tells this caller about MCP Events. Only
    /// HTTP serves them, and only to an OAuth client that can see a source.
    fn events_discoverable(&self) -> bool {
        false
    }
}

/// A `plug connect` session as the daemon sees it. Local, so trusted.
pub struct IpcContext {
    pub session_id: Arc<str>,
    /// The client id the connection registered with; it owns the session's
    /// durable tasks. `None` once the session is gone.
    pub registry_client_id: Option<Arc<str>>,
    pub request_id: RequestId,
    pub client_type: ClientType,
    pub client_key: Option<Arc<str>>,
    /// The revision the downstream client negotiated with `plug connect`.
    pub protocol_version: Arc<str>,
    pub client_metadata: Option<crate::protocol::ClientMetadata>,
    pub modern_enabled: bool,
    pub owner_liveness: Option<OwnerLivenessProbe>,
}

impl IpcContext {
    /// A legacy request from `session_id` with nothing else known about it.
    pub fn new(session_id: impl Into<Arc<str>>, request_id: RequestId) -> Self {
        Self {
            session_id: session_id.into(),
            registry_client_id: None,
            request_id,
            client_type: ClientType::Unknown,
            client_key: None,
            protocol_version: Arc::from(crate::protocol::SUPPORTED_PROTOCOL_VERSION),
            client_metadata: None,
            modern_enabled: false,
            owner_liveness: None,
        }
    }
}

impl DownstreamContext for IpcContext {
    fn downstream_call_context(&self) -> DownstreamCallContext {
        let era = ProtocolEra::of(&self.protocol_version);
        let context = DownstreamCallContext::ipc(
            Arc::clone(&self.session_id),
            self.request_id.clone(),
            self.client_type,
            self.registry_client_id.as_deref(),
        )
        .with_client_key(self.client_key.clone())
        .with_protocol(era, Arc::clone(&self.protocol_version))
        .with_modern_direction_enabled(self.modern_enabled && era == ProtocolEra::Modern);
        match &self.client_metadata {
            Some(metadata) => context
                .with_client_metadata(Arc::clone(&metadata.name), Arc::clone(&metadata.version)),
            None => context,
        }
    }

    fn session_bound(&self) -> bool {
        true
    }

    fn task_owner(&self) -> Result<TaskOwner, McpError> {
        self.registry_client_id
            .as_deref()
            .map(crate::downstream_identity::ipc_task_owner)
            .ok_or_else(|| McpError::internal_error("UNKNOWN_SESSION: session not found", None))
    }

    fn owner_liveness_probe(&self) -> Option<OwnerLivenessProbe> {
        self.owner_liveness.clone()
    }
}

/// Strip from a legacy method the private prefix HTTP gives the SEP-1686 task
/// methods so RMCP 3.x can parse them; see [`crate::protocol::rewrite_legacy_request`].
pub const LEGACY_METHOD_PREFIX: &str = "plug/legacy/";

/// Answer one MCP request. The result is in the vocabulary of the caller's era.
pub async fn handle(
    router: &Arc<ToolRouter>,
    ctx: &dyn DownstreamContext,
    method: &str,
    params: Option<Value>,
) -> Result<Value, McpError> {
    let call = ctx.downstream_call_context();
    let era = call.protocol_era;
    let _modern_request_lease = match era {
        ProtocolEra::Modern => {
            if !router.modern_downstream_enabled() {
                return Err(McpError::unsupported_protocol_version(
                    ProtocolEra::Modern.version(),
                    &crate::protocol::supported_downstream_protocol_versions(false),
                ));
            }
            Some(router.admit_modern_request(&call)?)
        }
        ProtocolEra::Legacy => None,
    };
    let method = match era {
        ProtocolEra::Legacy => method.strip_prefix(LEGACY_METHOD_PREFIX).unwrap_or(method),
        ProtocolEra::Modern => method,
    };
    let mut result = route(router, ctx, call, method, params).await?;
    match era {
        ProtocolEra::Legacy => crate::protocol::rewrite_legacy_result(&mut result, false),
        ProtocolEra::Modern => crate::protocol::rewrite_modern_result(&mut result),
    }
    Ok(result)
}

async fn route(
    router: &Arc<ToolRouter>,
    ctx: &dyn DownstreamContext,
    call: DownstreamCallContext,
    method: &str,
    params: Option<Value>,
) -> Result<Value, McpError> {
    let era = call.protocol_era;
    let client_key = call.client_key.clone();
    let client_key = client_key.as_deref();
    match method {
        // The modern era discovers instead. `plug connect` accepts a modern
        // `initialize` locally, so a modern one never reaches here from it.
        "initialize" => {
            legacy_only(era, method)?;
            to_value(initialize_result(server_capabilities(router, ctx)))
        }
        "server/discover" => {
            modern_only(era, method)?;
            let mut result = DiscoverResult::new(
                crate::protocol::supported_downstream_protocol_versions(true),
                server_capabilities(router, ctx),
            );
            result.set_server_info(plug_implementation());
            let mut result = to_value(result)?;
            // rmcp's `ServerCapabilities` has no field for MCP Events.
            if ctx.events_discoverable() {
                result["capabilities"]["events"] = serde_json::json!({});
            }
            Ok(result)
        }
        "ping" => {
            legacy_only(era, method)?;
            Ok(serde_json::json!({}))
        }
        "tools/list" => {
            call.authorize(MethodFamily::ToolsList)?;
            let lazy_session_key = ctx
                .session_bound()
                .then(|| call.transport.lazy_session_key(&call.client_id));
            to_value(router.list_tools_page_for_client_session(
                call.client_type,
                lazy_session_key.as_deref(),
                client_key,
                optional_params::<PaginatedRequestParams>(params, method)?,
            )?)
        }
        "tools/call" => tools_call(router, ctx, call, params).await,
        "resources/list" => {
            call.authorize(MethodFamily::ResourcesList)?;
            to_value(router.list_resources_page(
                optional_params::<PaginatedRequestParams>(params, method)?,
                client_key,
            )?)
        }
        "resources/templates/list" => {
            call.authorize(MethodFamily::ResourcesList)?;
            to_value(router.list_resource_templates_page(
                optional_params::<PaginatedRequestParams>(params, method)?,
                client_key,
            )?)
        }
        "resources/read" => {
            call.authorize(MethodFamily::ResourcesRead)?;
            let request: ReadResourceRequestParams = required_params(params, method)?;
            to_value(router.read_resource(&request.uri, client_key).await?)
        }
        "resources/subscribe" => {
            legacy_only(era, method)?;
            call.authorize(MethodFamily::ResourcesSubscribe)?;
            let request: SubscribeRequestParams = required_params(params, method)?;
            router
                .subscribe_resource(&request.uri, call.notification_target(), client_key)
                .await?;
            Ok(serde_json::json!({}))
        }
        "resources/unsubscribe" => {
            legacy_only(era, method)?;
            call.authorize(MethodFamily::ResourcesSubscribe)?;
            let request: UnsubscribeRequestParams = required_params(params, method)?;
            router
                .unsubscribe_resource(&request.uri, &call.notification_target())
                .await?;
            Ok(serde_json::json!({}))
        }
        "prompts/list" => {
            call.authorize(MethodFamily::PromptsList)?;
            to_value(router.list_prompts_page(
                optional_params::<PaginatedRequestParams>(params, method)?,
                client_key,
            )?)
        }
        "prompts/get" => {
            call.authorize(MethodFamily::PromptsGet)?;
            let request: GetPromptRequestParams = required_params(params, method)?;
            to_value(
                router
                    .get_prompt(&request.name, request.arguments, client_key)
                    .await?,
            )
        }
        "completion/complete" => {
            call.authorize(MethodFamily::Completion)?;
            let request: CompleteRequestParams = required_params(params, method)?;
            to_value(router.complete_request(request, client_key).await?)
        }
        // Not a per-session preference: the effective level is recalculated
        // across every downstream client and forwarded to every upstream, so
        // one caller moves the verbosity of the whole daemon.
        "logging/setLevel" => {
            legacy_only(era, method)?;
            call.authorize(MethodFamily::Logging)?;
            let request: SetLevelRequestParams = required_params(params, method)?;
            tracing::info!(
                client_id = %call.client_id,
                level = ?request.level,
                "downstream client set log level"
            );
            router.set_client_log_level(&call.client_id, request.level);
            router.forward_set_level_to_upstreams().await;
            Ok(serde_json::json!({}))
        }
        "tasks/list" | "tasks/get" | "tasks/result" | "tasks/cancel" | "tasks/update" => {
            match era {
                ProtocolEra::Legacy => legacy_task_method(router, ctx, &call, method, params).await,
                ProtocolEra::Modern => modern_task_method(router, ctx, &call, method, params).await,
            }
        }
        _ => Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            "method not supported",
            None,
        )),
    }
}

/// The SEP-1686 task methods, owned by the caller's task owner.
async fn legacy_task_method(
    router: &Arc<ToolRouter>,
    ctx: &dyn DownstreamContext,
    call: &DownstreamCallContext,
    method: &str,
    params: Option<Value>,
) -> Result<Value, McpError> {
    call.authorize(MethodFamily::Tasks)?;
    let owner = ctx.task_owner()?;
    if method == "tasks/list" {
        let request = optional_params::<PaginatedRequestParams>(params, method)?;
        return to_value(router.list_tasks_for_owner(&owner, request).await?);
    }
    let request: crate::legacy_tasks::TaskIdParams = required_params(params, method)?;
    match method {
        "tasks/get" => to_value(
            router
                .get_task_info_for_owner(&owner, &request.task_id)
                .await?,
        ),
        "tasks/result" => to_value(
            router
                .get_task_result_for_owner(&owner, &request.task_id)
                .await?,
        ),
        "tasks/cancel" => to_value(
            router
                .cancel_task_for_owner(&owner, &request.task_id)
                .await?,
        ),
        _ => Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            "method not supported",
            None,
        )),
    }
}

/// The 2026-07-28 tasks extension methods.
async fn modern_task_method(
    router: &Arc<ToolRouter>,
    ctx: &dyn DownstreamContext,
    call: &DownstreamCallContext,
    method: &str,
    params: Option<Value>,
) -> Result<Value, McpError> {
    match method {
        "tasks/get" => {
            let request: rmcp::model::GetTaskParams = required_params(params, method)?;
            let owner = admit_modern_task(router, ctx, call)?;
            to_value(
                router
                    .get_modern_task_for_owner(&owner, &request.task_id)
                    .await?,
            )
        }
        "tasks/cancel" => {
            let request: rmcp::model::CancelTaskParams = required_params(params, method)?;
            let owner = admit_modern_task(router, ctx, call)?;
            router
                .cancel_task_for_owner(&owner, &request.task_id)
                .await?;
            to_value(rmcp::model::TaskAckResult::new())
        }
        "tasks/update" => {
            let request: rmcp::model::UpdateTaskParams = required_params(params, method)?;
            let owner = admit_modern_task(router, ctx, call)?;
            router.validate_task_owner(&owner, &request.task_id).await?;
            Err(McpError::invalid_request(
                "this task has no outstanding input request",
                None,
            ))
        }
        _ => Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            "method not supported",
            None,
        )),
    }
}

/// Whether a modern caller may use the tasks extension, and as whom.
pub(crate) fn admit_modern_task(
    router: &ToolRouter,
    ctx: &dyn DownstreamContext,
    call: &DownstreamCallContext,
) -> Result<TaskOwner, McpError> {
    if !router.supports_tasks() {
        return Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            "tasks extension is not available",
            None,
        ));
    }
    if !ctx
        .modern_task_client()
        .is_some_and(ClientCapabilities::supports_tasks)
    {
        return Err(McpError::missing_required_client_capability(
            ClientCapabilities::builder().enable_tasks().build(),
        ));
    }
    call.authorize(MethodFamily::Tasks)?;
    call.ensure_principal_lifecycle_active()?;
    ctx.task_owner()
}

async fn tools_call(
    router: &Arc<ToolRouter>,
    ctx: &dyn DownstreamContext,
    call: DownstreamCallContext,
    params: Option<Value>,
) -> Result<Value, McpError> {
    let era = call.protocol_era;
    let mut params =
        params.ok_or_else(|| McpError::invalid_params("tools/call requires params", None))?;
    if era == ProtocolEra::Legacy {
        crate::protocol::rewrite_legacy_call_params(&mut params);
    }
    let params: CallToolRequestParams = serde_json::from_value(params)
        .map_err(|error| McpError::invalid_params(format!("tools/call: {error}"), None))?;
    // A modern client asks for a task by declaring the tasks extension; a
    // legacy one by the SEP-1686 `task` parameter, carried under
    // `LEGACY_TASK_REQUEST_KEY` once rewritten.
    let modern_task = era == ProtocolEra::Modern
        && ctx
            .modern_task_client()
            .is_some_and(ClientCapabilities::supports_tasks)
        && router.supports_tasks();
    let legacy_task = params
        .meta
        .as_ref()
        .is_some_and(|meta| meta.contains_key(crate::protocol::LEGACY_TASK_REQUEST_KEY));

    let started = std::time::Instant::now();
    let client = call.client_id.to_string();
    let client_type = call.client_type.to_string();
    let client_label = call
        .client_metadata
        .as_ref()
        .map(crate::protocol::ClientMetadata::display_label);
    let activity_client_key = call.client_key.as_deref().map(str::to_string);
    let target =
        crate::downstream_identity::client_target(call.client_key.as_deref(), call.client_type);
    let remote = call.transport == crate::downstream_identity::DownstreamTransport::Http;
    let tool = params.name.to_string();
    let server = router.server_for_tool(&tool);
    let cancellation = call.cancellation.clone();
    let result = tools_call_inner(router, ctx, call, params, modern_task || legacy_task).await;
    router.record_activity(crate::activity::ActivityEvent {
        sequence: 0,
        occurred_at_ms: 0,
        client: Some(client),
        server,
        method: "tools/call".to_string(),
        tool: Some(tool),
        client_type: Some(client_type),
        client_label,
        client_key: activity_client_key,
        target,
        remote,
        latency_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        outcome: match &result {
            Ok(ToolCallOutcome::Called(called)) if called.is_error == Some(true) => {
                crate::activity::ActivityOutcome::Error
            }
            Ok(_) => crate::activity::ActivityOutcome::Success,
            Err(_) if cancellation.is_cancelled() => crate::activity::ActivityOutcome::Cancelled,
            Err(_) => crate::activity::ActivityOutcome::Error,
        },
        reason: result
            .as_ref()
            .err()
            .and_then(|error| crate::activity::activity_reason(&error.message)),
    });
    match result? {
        ToolCallOutcome::Called(result) => to_value(result),
        ToolCallOutcome::InputRequired(result) => to_value(result),
        ToolCallOutcome::TaskCreated(result) if modern_task => {
            to_value(rmcp::model::CreateTaskResult::new((&result.task).into()))
        }
        ToolCallOutcome::TaskCreated(result) => to_value(result),
    }
}

enum ToolCallOutcome {
    Called(rmcp::model::CallToolResult),
    InputRequired(rmcp::model::InputRequiredResult),
    TaskCreated(crate::legacy_tasks::CreateTaskResult),
}

async fn tools_call_inner(
    router: &Arc<ToolRouter>,
    ctx: &dyn DownstreamContext,
    call: DownstreamCallContext,
    params: CallToolRequestParams,
    task_requested: bool,
) -> Result<ToolCallOutcome, McpError> {
    // Admit unknown metadata once, before routing, task creation, buffering,
    // or forwarding. Typed MCP fields remain owned by their typed adapters.
    let extensions = crate::types::ExtensionEnvelope::from_peer_meta(params.meta.as_deref())
        .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
    let progress_token = params.progress_token();
    let downstream = call.with_extension_envelope(extensions);

    downstream.authorize(MethodFamily::ToolsCall)?;

    let has_continuation = params.input_responses.is_some() || params.request_state.is_some();
    if exceeds_continuation_limits(
        params.request_state.as_deref(),
        params
            .input_responses
            .as_ref()
            .map_or(0, |responses| responses.len()),
        &params.input_responses,
    ) {
        return Err(McpError::invalid_params(
            "multi-round tool input exceeds Plug's bounded continuation limits".to_string(),
            None,
        ));
    }
    router.ensure_tool_round_supported(
        params.name.as_ref(),
        &downstream,
        has_continuation,
        task_requested,
    )?;

    if task_requested {
        downstream.authorize(MethodFamily::Tasks)?;
        let owner = ctx.task_owner()?;
        let result = router
            .clone()
            .enqueue_tool_task(
                params.name.as_ref(),
                params.arguments,
                progress_token,
                owner,
                ctx.owner_liveness_probe(),
                Some(downstream),
            )
            .await?;
        return Ok(ToolCallOutcome::TaskCreated(result));
    }
    match router
        .call_tool_round_with_context(params, progress_token, Some(downstream))
        .await?
    {
        rmcp::model::CallToolResponse::Complete(result) => Ok(ToolCallOutcome::Called(result)),
        rmcp::model::CallToolResponse::InputRequired(result) => {
            Ok(ToolCallOutcome::InputRequired(result))
        }
        rmcp::model::CallToolResponse::Task(_) => Err(McpError::internal_error(
            "upstream unexpectedly returned a task from synchronous dispatch".to_string(),
            None,
        )),
        _ => Err(McpError::internal_error(
            "unsupported upstream tools/call response".to_string(),
            None,
        )),
    }
}

/// What this caller is told the server can do. Advertisement reads the same
/// policy that enforcement does, so a caller is never told about a family it
/// would then be denied. A family is advertised only when every method behind
/// it is allowed.
pub fn server_capabilities(router: &ToolRouter, ctx: &dyn DownstreamContext) -> ServerCapabilities {
    let call = ctx.downstream_call_context();
    project_capabilities(
        router.synthesized_capabilities_for_client(call.client_type),
        &call,
        ctx.modern_task_client().is_some() && router.supports_tasks(),
    )
}

/// Narrow `capabilities` to what `call` may use in its era. `modern_tasks`
/// says whether the modern tasks extension can be served at all.
pub(crate) fn project_capabilities(
    mut capabilities: ServerCapabilities,
    call: &DownstreamCallContext,
    modern_tasks: bool,
) -> ServerCapabilities {
    let allows = |families: &[MethodFamily]| {
        families
            .iter()
            .all(|family| call.policy_decision(*family).is_allowed())
    };
    if !allows(&[MethodFamily::ToolsList, MethodFamily::ToolsCall]) {
        capabilities.tools = None;
    }
    if !allows(&[
        MethodFamily::ResourcesList,
        MethodFamily::ResourcesRead,
        MethodFamily::ResourcesSubscribe,
    ]) {
        capabilities.resources = None;
    }
    if !allows(&[MethodFamily::PromptsList, MethodFamily::PromptsGet]) {
        capabilities.prompts = None;
    }
    if !allows(&[MethodFamily::Completion]) {
        capabilities.completions = None;
    }
    match call.protocol_era {
        ProtocolEra::Legacy => {
            if !allows(&[MethodFamily::Logging, MethodFamily::LoggingRead]) {
                capabilities.logging = None;
            }
            if !allows(&[MethodFamily::Tasks])
                && let Some(experimental) = capabilities.experimental.as_mut()
            {
                experimental.remove(crate::protocol::LEGACY_TASKS_CAPABILITY_KEY);
                if experimental.is_empty() {
                    capabilities.experimental = None;
                }
            }
        }
        // 2026-07-28 removed `logging/setLevel` and `resources/subscribe`.
        // Extensions stay unadvertised until their bridges exist; tasks is the
        // one Plug serves, and only where the transport can.
        ProtocolEra::Modern => {
            capabilities.logging = None;
            capabilities.experimental = None;
            capabilities.extensions = None;
            if let Some(resources) = capabilities.resources.as_mut() {
                resources.subscribe = None;
            }
            if modern_tasks && allows(&[MethodFamily::Tasks]) {
                capabilities
                    .extensions
                    .get_or_insert_with(Default::default)
                    .insert(
                        rmcp::model::TASKS_EXTENSION_ID.to_string(),
                        Default::default(),
                    );
            }
        }
    }
    capabilities
}

/// The legacy `initialize` answer around a set of capabilities.
pub fn initialize_result(capabilities: ServerCapabilities) -> InitializeResult {
    InitializeResult::new(capabilities)
        .with_server_info(plug_implementation())
        .with_protocol_version(crate::protocol::supported_protocol_version())
}

fn plug_implementation() -> rmcp::model::Implementation {
    crate::branding::plug_implementation(env!("CARGO_PKG_VERSION"))
}

fn legacy_only(era: ProtocolEra, method: &str) -> Result<(), McpError> {
    match era {
        ProtocolEra::Legacy => Ok(()),
        ProtocolEra::Modern => Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            method.to_string(),
            None,
        )),
    }
}

fn modern_only(era: ProtocolEra, method: &str) -> Result<(), McpError> {
    match era {
        ProtocolEra::Modern => Ok(()),
        ProtocolEra::Legacy => Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            method.to_string(),
            None,
        )),
    }
}

fn required_params<T: DeserializeOwned>(
    params: Option<Value>,
    method: &str,
) -> Result<T, McpError> {
    let params = params
        .ok_or_else(|| McpError::invalid_params(format!("{method} requires params"), None))?;
    serde_json::from_value(params)
        .map_err(|error| McpError::invalid_params(format!("{method}: {error}"), None))
}

fn optional_params<T: DeserializeOwned>(
    params: Option<Value>,
    method: &str,
) -> Result<Option<T>, McpError> {
    match params {
        None | Some(Value::Null) => Ok(None),
        Some(params) => serde_json::from_value(params)
            .map(Some)
            .map_err(|error| McpError::invalid_params(format!("{method}: {error}"), None)),
    }
}

fn to_value<T: Serialize>(value: T) -> Result<Value, McpError> {
    serde_json::to_value(value).map_err(|error| McpError::internal_error(error.to_string(), None))
}

#[cfg(test)]
mod tests;
