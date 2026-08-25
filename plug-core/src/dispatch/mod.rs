//! Transport-agnostic dispatch for MCP method handling.
//!
//! The routing core (`ToolRouter::call_tool_with_context` / `enqueue_tool_task`)
//! is already transport-agnostic and shared by every downstream transport. What
//! each transport currently hand-rolls is the *adapter shell* around that core:
//! extract the progress token, build the per-call downstream context, decide
//! sync-vs-task, invoke the router, and encode the outcome onto the wire.
//!
//! This module owns that shell once for the `tools/call` method family. Each
//! transport supplies a [`DownstreamContext`] (its identity, task-owner derivation,
//! and whether it can return a task-augmented result) and encodes the returned
//! [`ToolCallOutcome`] into its own wire format. Wire framing, param parsing, and
//! transport-specific pre-validation stay in the transport shim.
//!
//! Only `tools/call` is migrated here today; other method families remain on their
//! per-transport paths until their own follow-up migrations.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{CallToolRequestParams, CallToolResult, InputRequiredResult, RequestParamsMeta};

use crate::legacy_tasks::CreateTaskResult;
use crate::proxy::continuations::{MRTR_MAX_BYTES, MRTR_MAX_ITEMS, MRTR_MAX_REQUEST_STATE_BYTES};
use crate::proxy::{DownstreamCallContext, ToolRouter};
use crate::tasks::{OwnerLivenessProbe, TaskOwner};

/// Outcome of dispatching a `tools/call`.
///
/// A plain call returns a [`CallToolResult`]; a task-augmented call (the request
/// carries a `task` field and the transport supports it) returns a
/// [`CreateTaskResult`]. Each transport encodes the variant it receives into its
/// own response envelope.
#[derive(Debug)]
pub enum ToolCallOutcome {
    /// A synchronous tool result.
    Called(CallToolResult),
    /// A native modern upstream requires another client round.
    InputRequired(InputRequiredResult),
    /// A background task was created for a task-augmented call.
    TaskCreated(CreateTaskResult),
}

/// What the shared `tools/call` dispatcher needs from a transport.
///
/// This abstracts only the per-transport adapter bits. Reverse-request
/// forwarding (elicitation/sampling), progress, and cancellation continue to flow
/// through the [`DownstreamCallContext`] this trait builds and the existing bridge
/// registration — the trait does not abstract the bridge mechanism itself.
pub trait DownstreamContext: Send + Sync {
    /// Build the per-call downstream context the router uses to route reverse
    /// requests, progress, and cancellation back to this client.
    fn downstream_call_context(&self) -> DownstreamCallContext;

    /// Whether this transport can return a [`CreateTaskResult`] for a
    /// task-augmented call.
    ///
    /// stdio's `tools/call` handler can only return a [`CallToolResult`], so it
    /// returns `false`; a task-augmented call over stdio falls through to a
    /// synchronous call, preserving today's "task param ignored on stdio"
    /// behavior. HTTP and IPC return `true`.
    fn supports_tasks(&self) -> bool {
        true
    }

    /// Derive the task owner for a task-augmented call.
    ///
    /// Only invoked when [`supports_tasks`](Self::supports_tasks) is `true` and the
    /// request carries a task. May fail when the transport cannot resolve the
    /// owning client (e.g. an IPC session that vanished mid-call).
    fn task_owner(&self) -> Result<TaskOwner, McpError>;

    /// Liveness probe for the task owner's downstream session, re-checked by
    /// `enqueue_tool_task` right after it registers its owner-create guard.
    /// Closes the race where a session teardown completes *entirely* before
    /// the enqueue registers anything, so the teardown's tombstone never sees
    /// the create (see the ordering argument at the check site in
    /// `proxy::tasks`).
    ///
    /// Default `None`: the transport has no teardown path that could race
    /// task creation (stdio), so there is nothing to probe.
    fn owner_liveness_probe(&self) -> Option<OwnerLivenessProbe> {
        None
    }
}

/// Shared `tools/call` adapter.
///
/// Owns the progress-extraction → context-build → sync/task branch → router-invoke
/// step once for every transport. Callers pass already-parsed params (each
/// transport keeps its own param parsing and pre-validation) and encode the
/// returned [`ToolCallOutcome`] into their wire format. The routing core is called
/// unchanged.
pub async fn dispatch_tools_call(
    router: &Arc<ToolRouter>,
    ctx: &dyn DownstreamContext,
    params: CallToolRequestParams,
) -> Result<ToolCallOutcome, McpError> {
    let started = std::time::Instant::now();
    let client = ctx.downstream_call_context().client_id.to_string();
    let result = dispatch_tools_call_inner(router, ctx, params).await;
    router.record_activity(crate::activity::ActivityEvent {
        sequence: 0,
        occurred_at_ms: 0,
        client: Some(client),
        server: None,
        method: "tools/call".to_string(),
        latency_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        outcome: if result.is_ok() {
            crate::activity::ActivityOutcome::Success
        } else {
            crate::activity::ActivityOutcome::Error
        },
    });
    result
}

async fn dispatch_tools_call_inner(
    router: &Arc<ToolRouter>,
    ctx: &dyn DownstreamContext,
    params: CallToolRequestParams,
) -> Result<ToolCallOutcome, McpError> {
    // Admit unknown metadata once, before routing, task creation, buffering,
    // or forwarding. Typed MCP fields remain owned by their typed adapters.
    let extensions = crate::types::ExtensionEnvelope::from_peer_meta(params.meta.as_deref())
        .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
    let progress_token = params.progress_token();
    let downstream = ctx
        .downstream_call_context()
        .with_extension_envelope(extensions);

    downstream.authorize(crate::protocol::MethodFamily::ToolsCall)?;

    let task_requested = params
        .meta
        .as_ref()
        .is_some_and(|meta| meta.contains_key(crate::protocol::LEGACY_TASK_REQUEST_KEY))
        && ctx.supports_tasks();
    let has_continuation = params.input_responses.is_some() || params.request_state.is_some();
    if params
        .request_state
        .as_ref()
        .is_some_and(|state| state.len() > MRTR_MAX_REQUEST_STATE_BYTES)
        || params
            .input_responses
            .as_ref()
            .is_some_and(|responses| responses.len() > MRTR_MAX_ITEMS)
        || params.input_responses.as_ref().is_some_and(|responses| {
            match serde_json::to_vec(responses) {
                Ok(encoded) => encoded.len() > MRTR_MAX_BYTES,
                Err(_) => true,
            }
        })
    {
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
        downstream.authorize(crate::protocol::MethodFamily::Tasks)?;
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
        Ok(ToolCallOutcome::TaskCreated(result))
    } else {
        match router
            .call_tool_round_with_context(params, progress_token, Some(downstream))
            .await?
        {
            rmcp::model::CallToolResponse::Complete(result) => Ok(ToolCallOutcome::Called(result)),
            rmcp::model::CallToolResponse::InputRequired(result) => {
                if result
                    .request_state
                    .as_ref()
                    .is_some_and(|state| state.len() > MRTR_MAX_REQUEST_STATE_BYTES)
                    || result
                        .input_requests
                        .as_ref()
                        .is_some_and(|requests| requests.len() > MRTR_MAX_ITEMS)
                    || serde_json::to_vec(&result)
                        .is_ok_and(|encoded| encoded.len() > MRTR_MAX_BYTES)
                {
                    return Err(McpError::invalid_request(
                        "upstream multi-round response exceeds Plug's bounded continuation limits",
                        None,
                    ));
                }
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
}
