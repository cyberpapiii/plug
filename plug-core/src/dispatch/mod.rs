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
use rmcp::model::{CallToolRequestParams, CallToolResult, CreateTaskResult, RequestParamsMeta};

use crate::proxy::{DownstreamCallContext, ToolRouter};
use crate::tasks::TaskOwner;

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
    /// A background task was created for a task-augmented call.
    TaskCreated(CreateTaskResult),
}

/// What the shared `tools/call` dispatcher needs from a transport.
///
/// This abstracts only the per-transport adapter bits. Reverse-request
/// forwarding (elicitation/sampling), progress, and cancellation continue to flow
/// through the [`DownstreamCallContext`] this trait builds and the existing bridge
/// registration — the trait does not abstract the bridge mechanism itself.
pub trait DownstreamContext {
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
    let progress_token = params.progress_token();
    let downstream = ctx.downstream_call_context();

    if params.task.is_some() && ctx.supports_tasks() {
        let owner = ctx.task_owner()?;
        let result = router
            .clone()
            .enqueue_tool_task(
                params.name.as_ref(),
                params.arguments,
                progress_token,
                owner,
                Some(downstream),
            )
            .await?;
        Ok(ToolCallOutcome::TaskCreated(result))
    } else {
        let result = router
            .call_tool_with_context(
                params.name.as_ref(),
                params.arguments,
                progress_token,
                Some(downstream),
            )
            .await?;
        Ok(ToolCallOutcome::Called(result))
    }
}
