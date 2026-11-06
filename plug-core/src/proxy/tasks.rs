use super::*;

use rmcp::service::ServiceError;

use crate::legacy_tasks::{
    CancelTaskResult, CreateTaskResult, GetTaskPayloadResult, GetTaskResult, ListTasksResult,
};
use crate::tasks::{OwnerLivenessProbe, UpstreamRecordOutcome, owner_closed_during_create_error};

/// Ask a task-native upstream about one of its tasks, for at most its
/// `call_timeout_secs`. rmcp's `send_request` waits for an answer forever.
async fn ask_task_upstream(
    server: &crate::server::UpstreamServer,
    method: &str,
    params: serde_json::Value,
) -> Result<rmcp::model::ServerResult, McpError> {
    let bound = Duration::from_secs(server.config.call_timeout_secs);
    let send = server
        .client
        .peer()
        .send_request(crate::legacy_tasks::request(method, Some(params)));
    match tokio::time::timeout(bound, send).await {
        Ok(response) => response.map_err(map_service_error),
        Err(_) => Err(McpError::from(ProtocolError::Timeout { duration: bound })),
    }
}

/// RAII cover for the send-to-record gap in `record_task_request`:
/// constructed the moment the upstream request is in flight and defused
/// once the request id is safely recorded on the task record — or once an
/// explicit cancel has already been sent. If the executing future is
/// aborted while the guard is still armed (teardown's phase-A
/// `handle.abort()` landing between send and record), Drop fires a
/// detached, bounded request-level cancellation so the upstream call is
/// stopped instead of running to completion for a result nobody can ever
/// collect.
struct UpstreamRequestCancelGuard {
    peer: Peer<RoleClient>,
    request_id: RequestId,
    bound: Duration,
    armed: bool,
}

impl UpstreamRequestCancelGuard {
    fn new(peer: Peer<RoleClient>, request_id: RequestId, bound: Duration) -> Self {
        Self {
            peer,
            request_id,
            bound,
            armed: true,
        }
    }

    /// Stand the guard down: responsibility for upstream cancellation has
    /// been handed to the task record (or an explicit cancel already ran).
    fn defuse(&mut self) {
        self.armed = false;
    }
}

impl Drop for UpstreamRequestCancelGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let peer = self.peer.clone();
        let request_id = self.request_id.clone();
        let bound = self.bound;
        // Drop can't await, so a detached bounded task carries the cancel.
        // If no runtime is left (process teardown), there is no upstream
        // worth cancelling either — skip instead of panicking.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                cancel_upstream_request(
                    &peer,
                    request_id,
                    "task aborted before upstream request was recorded",
                    bound,
                )
                .await;
            });
        }
    }
}

impl super::ToolRouter {
    pub async fn enqueue_tool_task(
        self: &Arc<Self>,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        progress_token: Option<ProgressToken>,
        owner: TaskOwner,
        owner_liveness: Option<OwnerLivenessProbe>,
        downstream: Option<DownstreamCallContext>,
    ) -> Result<CreateTaskResult, McpError> {
        let extension_meta = downstream
            .as_ref()
            .and_then(|context| context.extension_envelope.as_ref())
            .and_then(crate::types::ExtensionEnvelope::to_meta);
        // Task augmentation is a second method family layered on tools/call.
        // Authorize and reserve both queue + durable-record capacity before
        // touching the lifecycle ledger, task store, or any upstream.
        let (_queued_task_lease, task_quota_lease) = if let Some(context) = downstream.as_ref() {
            context.authorize(crate::protocol::MethodFamily::Tasks)?;
            let principal = context.principal.as_ref().ok_or_else(|| {
                crate::protocol::ProtocolOutcome::AuthorizationRequired
                    .into_error(context.protocol_era)
            })?;
            let queued = self
                .admission_quotas
                .try_acquire(
                    principal,
                    crate::protocol::QuotaResource::QueuedTaskCreations,
                    1,
                )
                .map_err(|outcome| outcome.into_error(context.protocol_era))?;
            let task = self
                .admission_quotas
                .try_acquire(principal, crate::protocol::QuotaResource::Tasks, 1)
                .map_err(|outcome| outcome.into_error(context.protocol_era))?;
            (Some(queued), Some(task))
        } else {
            // Internal/local legacy callers without a durable downstream
            // context retain their established behavior.
            (None, None)
        };

        // Owner-lifecycle guard, registered before anything else: a teardown
        // (`cleanup_tasks_for_owner`) that runs while this enqueue is in
        // flight — most importantly across the native path's upstream round
        // trip below — tombstones the owner, so the late
        // `create`/`create_passthrough` refuses to insert a record for the
        // already-torn-down owner instead of leaving an untracked Working
        // record (and, on the native path, an upstream task nobody cancels).
        let create_guard = self.task_store.lock().await.begin_owner_create(&owner);

        // Owner-liveness re-check, deliberately ordered AFTER the guard
        // registration above. This closes the other half of the
        // create-vs-teardown race: a teardown that fully COMPLETED before the
        // guard registered leaves no tombstone, so only this probe can refuse
        // the create.
        //
        // Why the ordering makes this sound: the guard is registered BEFORE
        // the probe runs; every teardown path removes the session/registry
        // entry BEFORE calling `cleanup_tasks_for_owner`; and cleanup
        // tombstones in-flight creates. So if the probe sees the session
        // alive, teardown has not started its removal yet — if it starts
        // later, cleanup's `tombstone_if_in_flight` must see our in-flight
        // guard and the tombstone makes `create`/`create_passthrough` refuse.
        // If the probe sees the session gone, we refuse right here. Either
        // way no record outlives teardown unnoticed.
        //
        // HTTP session ids are server-minted UUIDv4 and never reused, so
        // there is no ABA on the probe. IPC client ids DO recur by design
        // (reconnects re-register the same id): a probe observing a
        // re-registered client accepts the create, which is benign — the same
        // `ipc:<client_id>` owner is live again and owns the record — and the
        // tombstone catch-all remains the backstop, with a bounded spurious
        // refusal as the worst case. A caller with no session to tear down
        // passes `None`.
        if let Some(owner_is_live) = owner_liveness
            && !owner_is_live()
        {
            return Err(owner_closed_during_create_error());
        }

        // Bearer validation and durable admission are separate moments. A
        // client revoked between them invalidates this generation lease.
        // This check is intentionally after the TaskStore create guard: a
        // concurrent revocation that proceeds after we observe `active`
        // still tombstones this in-flight owner during cleanup, preventing a
        // late record from surviving. A revocation that already completed is
        // rejected here before route lookup or any upstream request.
        if let Some(context) = downstream.as_ref() {
            context.ensure_principal_lifecycle_active()?;
        }

        if canonical_plug_meta_tool_name(tool_name).is_some() {
            return Err(McpError::from(ProtocolError::InvalidRequest {
                detail:
                    "plug meta-tools do not support task-wrapped calls; call the meta-tool directly"
                        .to_string(),
            }));
        }

        self.ensure_client_may_use_tool(downstream.as_ref(), tool_name)?;
        let cache = self.cache.load();
        let (server_id, original_name) =
            cache.resolve_route(tool_name).cloned().ok_or_else(|| {
                McpError::from(ProtocolError::ToolNotFound {
                    tool_name: tool_name.to_string(),
                })
            })?;
        drop(cache);

        self.ensure_lazy_tool_loaded_for_direct_call(downstream.as_ref(), tool_name)?;
        let trace_id = downstream
            .as_ref()
            .map(|context| Arc::clone(&context.trace_id))
            .unwrap_or_else(|| Arc::from(new_trace_id()));

        let modern_downstream = downstream
            .as_ref()
            .is_some_and(|context| context.protocol_era == crate::protocol::ProtocolEra::Modern);
        if !modern_downstream
            && let Some(upstream) = self.server_manager.get_upstream(&server_id)
            && crate::protocol::legacy_tasks_capability(&upstream.capabilities)
        {
            let call_timeout = Duration::from_secs(upstream.config.call_timeout_secs);
            let peer = upstream.client.peer().clone();
            drop(upstream);

            let mut upstream_params = CallToolRequestParams::new(original_name.clone());
            if let Some(args) = arguments.clone() {
                upstream_params = upstream_params.with_arguments(args);
            }
            if let Some(meta) = extension_meta.clone() {
                merge_call_extension_meta(&mut upstream_params, meta);
            }
            upstream_params
                .meta
                .get_or_insert_with(Default::default)
                .insert(
                    crate::protocol::LEGACY_TASK_REQUEST_KEY.to_string(),
                    serde_json::json!({}),
                );
            if let Some(token) = progress_token.clone() {
                upstream_params.set_progress_token(token);
            }
            let upstream_params =
                crate::legacy_tasks::call_tool_params(upstream_params).map_err(|error| {
                    McpError::internal_error(
                        format!("failed to serialize legacy task-wrapped tool call: {error}"),
                        None,
                    )
                })?;

            // The round trip is DETACHED: it runs in its own spawned task and
            // the `OwnerCreateGuard` moves into it, so a caller whose future
            // is dropped mid-await (e.g. axum dropping the POST handler on
            // client disconnect) can neither release the guard early nor
            // orphan the upstream task — the round trip completes in the
            // background and either records the created task (owner still
            // live; it stays retrievable via tasks/list) or hits the
            // tombstone branch and cancels the upstream task.
            let router = Arc::clone(self);
            let tool_name = tool_name.to_string();
            let round_trip = tokio::spawn(async move {
                let _queued_task_lease = _queued_task_lease;
                let _create_guard = create_guard;
                let deadline = tokio::time::Instant::now() + call_timeout;

                let request = crate::legacy_tasks::request("tools/call", Some(upstream_params));
                // DEFAULT options on purpose: setting rmcp's own
                // `options.timeout` would hand the timeout path to
                // `await_response`, whose auto-cancel awaits an UNBOUNDED
                // `send_notification` — a wedged sink would hang this task
                // forever. The bound lives in our own `tokio::time::timeout`
                // below, paired with a bounded explicit cancel. The same
                // deadline covers BOTH queueing the request and receiving
                // its response: rmcp's peer channel is bounded, so acquiring
                // a RequestHandle can itself backpressure before a request id
                // exists to cancel.
                let handle = match tokio::time::timeout_at(
                    deadline,
                    peer.send_cancellable_request(request, PeerRequestOptions::default()),
                )
                .await
                {
                    Ok(Ok(handle)) => handle,
                    Ok(Err(error)) => {
                        return Err(map_service_error(error));
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            trace_id = %trace_id,
                            server = %server_id,
                            tool = %original_name,
                            timeout_secs = call_timeout.as_secs(),
                            "native task creation timed out while queueing upstream request"
                        );
                        return Err(McpError::internal_error(
                            ServiceError::Timeout {
                                timeout: call_timeout,
                            }
                            .to_string(),
                            None,
                        ));
                    }
                };
                let RequestHandle {
                    mut rx,
                    id: upstream_request_id,
                    peer: request_peer,
                    ..
                } = handle;

                let recv = match tokio::time::timeout_at(deadline, &mut rx).await {
                    Ok(recv) => recv,
                    Err(_elapsed) => {
                        tracing::warn!(
                            trace_id = %trace_id,
                            server = %server_id,
                            tool = %original_name,
                            timeout_secs = call_timeout.as_secs(),
                            "native task creation timed out; cancelling upstream request"
                        );
                        cancel_upstream_request(
                            &request_peer,
                            upstream_request_id,
                            "task creation timed out",
                            call_timeout,
                        )
                        .await;
                        // Reaper: if the CreateTaskResult still lands late,
                        // cancel the just-created task by id. The guard is
                        // NOT handed to the reaper — it dies with this task
                        // (the reaper only cancels; it never creates
                        // records).
                        router.spawn_native_create_reaper(
                            rx,
                            call_timeout,
                            server_id.clone(),
                            Arc::clone(&trace_id),
                        );
                        return Err(McpError::internal_error(
                            ServiceError::Timeout {
                                timeout: call_timeout,
                            }
                            .to_string(),
                            None,
                        ));
                    }
                };
                // Replicate rmcp `await_response`'s oneshot mapping: a
                // dropped responder means the transport went away.
                let response = match recv {
                    Ok(Ok(response)) => response,
                    Ok(Err(error)) => {
                        return Err(map_service_error(error));
                    }
                    Err(_recv_error) => {
                        return Err(McpError::internal_error(
                            ServiceError::TransportClosed.to_string(),
                            None,
                        ));
                    }
                };

                let parsed = crate::legacy_tasks::parse_create_result(response);
                if let Err(error) = &parsed {
                    tracing::warn!(
                        trace_id = %trace_id,
                        server = %server_id,
                        tool = %original_name,
                        %error,
                        "upstream answered a task-wrapped call without a legacy task"
                    );
                }
                if let Ok(result) = parsed {
                    tracing::info!(
                        trace_id = %trace_id,
                        server = %server_id,
                        tool = %original_name,
                        task_id = %result.task.task_id,
                        "proxy native upstream task created"
                    );
                    let upstream = TaskUpstreamRef::Task {
                        server_id: server_id.clone(),
                        task_id: result.task.task_id.clone(),
                    };
                    let (created, expired_cleanup) = {
                        let mut store = router.task_store.lock().await;
                        let created = match task_quota_lease {
                            Some(lease) => store.create_passthrough_with_quota(
                                owner,
                                &tool_name,
                                &result.task,
                                upstream,
                                lease,
                            ),
                            None => {
                                store.create_passthrough(owner, &tool_name, &result.task, upstream)
                            }
                        };
                        if let Ok(task) = &created {
                            store.record_source(&task.task_id, &server_id, &original_name);
                        }
                        (created, store.take_expired_cleanup())
                    };
                    router.finish_expired_task_cleanup(expired_cleanup).await;
                    return match created {
                        Ok(task) => Ok(CreateTaskResult::new(task)),
                        Err(error) => {
                            // The owner was torn down during the upstream
                            // round trip: the native task exists upstream but
                            // nothing tracks it locally anymore. Best-effort
                            // bounded cancel so the upstream stops instead of
                            // running to completion for a result nobody will
                            // ever collect.
                            tracing::info!(
                                trace_id = %trace_id,
                                server = %server_id,
                                task_id = %result.task.task_id,
                                "owner torn down during native task creation; cancelling upstream task"
                            );
                            if let Some(cancellation) =
                                router.spawn_bounded_upstream_cancellation(TaskUpstreamRef::Task {
                                    server_id,
                                    task_id: result.task.task_id.clone(),
                                })
                            {
                                let _ = cancellation.await;
                            }
                            Err(error)
                        }
                    };
                }

                Err(McpError::internal_error(
                    "upstream task-capable server returned an invalid legacy task response",
                    None,
                ))
            });
            return match round_trip.await {
                Ok(result) => result,
                Err(join_error) => Err(McpError::internal_error(
                    format!("native task creation round trip failed: {join_error}"),
                    None,
                )),
            };
        }

        // Create + spawn + attach in ONE task_store lock scope: `tokio::spawn`
        // is synchronous, and the spawned future's own store accesses simply
        // queue on this same lock, so no teardown can interleave between the
        // record insert and the abort-handle attach. A record drained before
        // its handle was attached would detach the future, which would keep
        // running and holding its server's call slot. No await is held
        // across this scope other than acquiring the lock itself.
        // The way the call itself routes, so a tool called by its server's
        // own name is recorded too.
        let behind = self
            .cache
            .load()
            .resolve_route(tool_name)
            .filter(|(server, _)| server != crate::tool_naming::INTERNAL_SERVER_ID)
            .cloned();
        let created = {
            let mut store = self.task_store.lock().await;
            let task = match task_quota_lease {
                Some(lease) => store.create_with_quota(owner, tool_name, lease),
                None => store.create(owner, tool_name),
            };
            match task {
                Ok(task) => {
                    let task_id = task.task_id.clone();
                    if let Some((server, own_tool)) = &behind {
                        store.record_source(&task_id, server, own_tool);
                    }
                    let router = Arc::clone(self);
                    let tool_name = tool_name.to_string();
                    let handle = tokio::spawn(async move {
                        router
                            .execute_tool_task(
                                task_id,
                                tool_name,
                                arguments,
                                progress_token,
                                downstream,
                            )
                            .await;
                    });
                    store.attach_abort_handle(&task.task_id, handle);
                    Ok((task, store.take_expired_cleanup()))
                }
                Err(error) => Err((error, store.take_expired_cleanup())),
            }
        };

        let (task, expired_cleanup) = match created {
            Ok(created) => created,
            Err((error, cleanup)) => {
                self.finish_expired_task_cleanup(cleanup).await;
                return Err(error);
            }
        };
        self.finish_expired_task_cleanup(expired_cleanup).await;

        Ok(CreateTaskResult::new(task))
    }

    pub async fn list_tasks_for_owner(
        &self,
        owner: &TaskOwner,
        request: Option<PaginatedRequestParams>,
    ) -> Result<ListTasksResult, McpError> {
        let (tasks, cleanup) = {
            let mut store = self.task_store.lock().await;
            let tasks = store.list_for_owner(owner);
            (tasks, store.take_expired_cleanup())
        };
        self.finish_expired_task_cleanup(cleanup).await;
        catalog::paginated_result("tasks/list", &tasks, request, |tasks, next_cursor| {
            let mut result = ListTasksResult::new(tasks);
            result.next_cursor = next_cursor;
            result
        })
    }

    pub async fn get_task_info_for_owner(
        &self,
        owner: &TaskOwner,
        task_id: &str,
    ) -> Result<GetTaskResult, McpError> {
        let (upstream, cleanup) = {
            let mut store = self.task_store.lock().await;
            let upstream = store.upstream_for_owner(owner, task_id);
            (upstream, store.take_expired_cleanup())
        };
        self.finish_expired_task_cleanup(cleanup).await;
        let upstream = upstream?;
        if let Some(TaskUpstreamRef::Task {
            server_id,
            task_id: upstream_task_id,
        }) = upstream
            && let Some(server) = self.server_manager.get_upstream(&server_id)
        {
            let params =
                serde_json::to_value(crate::legacy_tasks::GetTaskParams::new(upstream_task_id))
                    .map_err(|error| McpError::internal_error(error.to_string(), None))?;
            let response = ask_task_upstream(&server, "tasks/get", params).await?;
            if let Ok(result) = crate::legacy_tasks::parse_get_result(response) {
                let (synced, cleanup) = {
                    let mut store = self.task_store.lock().await;
                    let synced = store.sync_from_upstream_for_owner(owner, task_id, &result.task);
                    (synced, store.take_expired_cleanup())
                };
                self.finish_expired_task_cleanup(cleanup).await;
                return Ok(GetTaskResult::new(synced?));
            }
            return Err(McpError::internal_error(
                "unexpected upstream tasks/get response".to_string(),
                None,
            ));
        }
        let (result, cleanup) = {
            let mut store = self.task_store.lock().await;
            let result = store.get_info_for_owner(owner, task_id);
            (result, store.take_expired_cleanup())
        };
        self.finish_expired_task_cleanup(cleanup).await;
        result
    }

    /// SEP-2663 projection of the same retained task record used by legacy
    /// task clients. Refresh passthrough state first, then let `TaskStore`
    /// build the status-specific modern payload.
    pub async fn get_modern_task_for_owner(
        &self,
        owner: &TaskOwner,
        task_id: &str,
    ) -> Result<rmcp::model::GetTaskResult, McpError> {
        let info = self.get_task_info_for_owner(owner, task_id).await?;
        if info.task.status == crate::legacy_tasks::TaskStatus::Completed {
            // A legacy-native passthrough task created earlier in this process
            // may later be polled by a modern client under the same durable
            // principal. Legacy `tasks/get` carries status only; fetch and
            // cache its separate `tasks/result` payload before projecting the
            // modern inlined completed result.
            let _ = self.get_task_result_for_owner(owner, task_id).await?;
        }
        let (result, cleanup) = {
            let mut store = self.task_store.lock().await;
            let result = store.get_modern_for_owner(owner, task_id);
            (result, store.take_expired_cleanup())
        };
        self.finish_expired_task_cleanup(cleanup).await;
        result
    }

    pub async fn validate_task_owner(
        &self,
        owner: &TaskOwner,
        task_id: &str,
    ) -> Result<(), McpError> {
        let (result, cleanup) = {
            let mut store = self.task_store.lock().await;
            let result = store.validate_owner(owner, task_id);
            (result, store.take_expired_cleanup())
        };
        self.finish_expired_task_cleanup(cleanup).await;
        result
    }

    pub async fn get_task_result_for_owner(
        &self,
        owner: &TaskOwner,
        task_id: &str,
    ) -> Result<GetTaskPayloadResult, McpError> {
        let (upstream, cleanup) = {
            let mut store = self.task_store.lock().await;
            let upstream = store.upstream_for_owner(owner, task_id);
            (upstream, store.take_expired_cleanup())
        };
        self.finish_expired_task_cleanup(cleanup).await;
        let upstream = upstream?;
        if let Some(TaskUpstreamRef::Task {
            server_id,
            task_id: upstream_task_id,
        }) = upstream
            && let Some(server) = self.server_manager.get_upstream(&server_id)
        {
            let params = serde_json::to_value(crate::legacy_tasks::GetTaskPayloadParams::new(
                upstream_task_id,
            ))
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
            let response = ask_task_upstream(&server, "tasks/result", params).await?;
            let mut payload = crate::legacy_tasks::parse_payload_result(response)
                .map_err(|error| McpError::internal_error(error.to_string(), None))?
                .0;
            // The upstream ran the tool itself, so its result is sanitized
            // here as `call_tool_inner` sanitizes a call's.
            if let Ok(mut result) = serde_json::from_value::<CallToolResult>(payload.clone()) {
                admit_tool_result_meta(&mut result);
                skills::outward_tool_result(&server_id, &mut result);
                payload = serde_json::to_value(result)
                    .map_err(|error| McpError::internal_error(error.to_string(), None))?;
            }
            let (cached, source, cleanup) = {
                let mut store = self.task_store.lock().await;
                let cached = store.cache_result_for_owner(owner, task_id, payload.clone());
                (
                    cached,
                    store.source_of(task_id),
                    store.take_expired_cleanup(),
                )
            };
            self.finish_expired_task_cleanup(cleanup).await;
            cached?;
            return self.task_payload(source, task_id, payload).await;
        }
        let (payload, source, cleanup) = {
            let mut store = self.task_store.lock().await;
            let payload = store.get_result_for_owner(owner, task_id);
            (
                payload,
                store.source_of(task_id),
                store.take_expired_cleanup(),
            )
        };
        self.finish_expired_task_cleanup(cleanup).await;
        self.task_payload(source, task_id, payload?.0).await
    }

    /// A task's result as `tasks/result` sends it. A tool result too large
    /// to send is kept as an artifact with the tool the task ran; one whose
    /// task no longer says which is kept with no tool, and is withheld from a
    /// client with a block.
    async fn task_payload(
        &self,
        source: Option<crate::tasks::TaskSource>,
        task_id: &str,
        payload: serde_json::Value,
    ) -> Result<GetTaskPayloadResult, McpError> {
        let Ok(result) = serde_json::from_value::<CallToolResult>(payload.clone()) else {
            return Ok(GetTaskPayloadResult::new(payload));
        };
        let listed = source.as_ref().map_or_else(
            || format!("task_result:{task_id}"),
            |source| source.listed.clone(),
        );
        let behind = source
            .as_ref()
            .and_then(|source| source.behind.as_ref())
            .map(|(server, own_tool)| (server.as_str(), own_tool.as_str()));
        let spilled = self
            .artifact_store
            .maybe_spill_result_of(behind, &listed, result)
            .await?;
        serde_json::to_value(spilled)
            .map(GetTaskPayloadResult::new)
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    /// Tears down every task record owned by `owner` — HTTP session
    /// `DELETE`, HTTP idle-session expiry, and IPC/daemon client disconnect
    /// all funnel through this.
    ///
    /// This stops execution, not just bookkeeping: any still-running locally
    /// spawned task future is aborted (dropping a `JoinHandle` only detaches
    /// it — the future keeps running and holding its server's
    /// `max_concurrent` semaphore permit), and any outstanding upstream
    /// task/request is sent a best-effort cancellation so a task-capable
    /// upstream also stops instead of completing the call for a result
    /// nobody will ever collect.
    ///
    /// Mirrors the cancellation-forwarding branches in `cancel_task_for_owner`,
    /// minus the store sync-back — the records are already gone by the time
    /// cancellation is forwarded, so there is nothing left to update. Errors
    /// notifying any one upstream are ignored and never skip the rest.
    ///
    /// Returns in bounded time regardless of upstream behavior: local aborts
    /// happen first and synchronously, and every upstream cancellation is
    /// forwarded concurrently under a per-upstream timeout (see
    /// `spawn_bounded_upstream_cancellation`). Callers include the daemon's
    /// idle-session expiry loop, which is a single serialized loop — an
    /// unbounded hang here would permanently stop idle cleanup daemon-wide.
    pub async fn cleanup_tasks_for_owner(&self, owner: &TaskOwner) {
        // Durable task and continuation ownership share the authenticated
        // principal key. Teardown/revocation must invalidate both before any
        // asynchronous upstream cleanup can be delayed or cancelled.
        self.continuation_registry.revoke_owner_key(owner.as_key());
        let drained = self.task_store.lock().await.cleanup_owner(owner);

        // Phase A: abort every still-running local future synchronously,
        // before any upstream await — one unresponsive upstream must never
        // delay stopping the other records' local execution.
        let mut upstreams = Vec::new();
        for (upstream, handle) in drained {
            if let Some(handle) = handle {
                handle.abort();
            }
            if let Some(upstream) = upstream {
                upstreams.push(upstream);
            }
        }

        // Phase B: forward best-effort cancellations concurrently, each
        // bounded by its own server's call timeout. The spawned tasks run
        // independently, so joining them serially still completes in
        // max(per-server bound), not the sum — and a cancellation outlives
        // even a caller that gets aborted mid-await (e.g. an HTTP DELETE
        // request task).
        let cancellations: Vec<_> = upstreams
            .into_iter()
            .filter_map(|upstream| self.spawn_bounded_upstream_cancellation(upstream))
            .collect();
        for cancellation in cancellations {
            let _ = cancellation.await;
        }
    }

    async fn finish_expired_task_cleanup(&self, cleanup: Vec<crate::tasks::ExpiredTaskCleanup>) {
        let cancellations = cleanup
            .iter()
            .filter_map(|item| item.upstream.clone())
            .filter_map(|upstream| self.spawn_bounded_upstream_cancellation(upstream))
            .collect::<Vec<_>>();
        for cancellation in cancellations {
            let _ = cancellation.await;
        }
        // Dropping only after every bounded cancellation attempt releases
        // the quota leases retained by stale live records.
        drop(cleanup);
    }

    /// Spawns a best-effort cancellation of `upstream` (task cancel request
    /// or request-cancel notification), bounded by the owning server's
    /// `call_timeout_secs`. rmcp's plain `send_request` sets no timeout of
    /// its own and awaits the response oneshot forever, so without this
    /// bound a single unresponsive upstream would wedge the caller.
    /// Returns `None` when the server is no longer registered (nothing left
    /// to cancel). Timeouts and errors are logged, never propagated.
    fn spawn_bounded_upstream_cancellation(
        &self,
        upstream: TaskUpstreamRef,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let server_id = match &upstream {
            TaskUpstreamRef::Task { server_id, .. }
            | TaskUpstreamRef::Request { server_id, .. } => server_id,
        };
        let server = self.server_manager.get_upstream(server_id)?;
        let bound = Duration::from_secs(server.config.call_timeout_secs);
        let peer = server.client.peer().clone();
        drop(server);

        Some(tokio::spawn(async move {
            match upstream {
                TaskUpstreamRef::Task {
                    server_id,
                    task_id: upstream_task_id,
                } => {
                    let params = serde_json::to_value(crate::legacy_tasks::CancelTaskParams::new(
                        upstream_task_id.clone(),
                    ))
                    .expect("legacy task cancel params serialize");
                    let send = peer
                        .send_request(crate::legacy_tasks::request("tasks/cancel", Some(params)));
                    match tokio::time::timeout(bound, send).await {
                        Err(_) => tracing::warn!(
                            server = %server_id,
                            upstream_task_id = %upstream_task_id,
                            timeout_secs = bound.as_secs(),
                            "upstream tasks/cancel timed out during task teardown"
                        ),
                        Ok(Err(error)) => tracing::debug!(
                            server = %server_id,
                            upstream_task_id = %upstream_task_id,
                            %error,
                            "upstream tasks/cancel failed during task teardown"
                        ),
                        Ok(Ok(_)) => {}
                    }
                }
                TaskUpstreamRef::Request { request_id, .. } => {
                    cancel_upstream_request(&peer, request_id, "task owner disconnected", bound)
                        .await;
                }
            }
        }))
    }

    /// Spawns the detached reaper backing a timed-out native create: keeps
    /// awaiting the request's response oneshot for up to one more `window`
    /// and, if a `CreateTaskResult` still lands late, sends a bounded
    /// task-id cancellation so the just-created upstream task is stopped
    /// instead of orphaned. If the window also expires, it logs and gives
    /// up (residual: the upstream task may be orphaned).
    ///
    /// rmcp caveat (rmcp 3.1.0 `service.rs`): once our
    /// request-level cancel passes through the peer loop, rmcp resolves the
    /// pending responder with `ServiceError::Cancelled` and DROPS any later
    /// response for that id — so a late `CreateTaskResult` is only
    /// observable here when it beats that clobber. A spec-compliant
    /// upstream honors the request-level cancel anyway; the reaper is
    /// defense-in-depth for the race where the response was already in
    /// flight.
    fn spawn_native_create_reaper(
        self: &Arc<Self>,
        rx: tokio::sync::oneshot::Receiver<Result<ServerResult, ServiceError>>,
        window: Duration,
        server_id: String,
        trace_id: Arc<str>,
    ) -> tokio::task::JoinHandle<()> {
        let router = Arc::clone(self);
        tokio::spawn(async move {
            match tokio::time::timeout(window, rx).await {
                Ok(Ok(Ok(response))) => {
                    // A late non-task response created nothing to cancel.
                    let Ok(result) = crate::legacy_tasks::parse_create_result(response) else {
                        return;
                    };
                    tracing::warn!(
                        trace_id = %trace_id,
                        server = %server_id,
                        task_id = %result.task.task_id,
                        "native task created after enqueue timed out; cancelling upstream task"
                    );
                    if let Some(cancellation) =
                        router.spawn_bounded_upstream_cancellation(TaskUpstreamRef::Task {
                            server_id,
                            task_id: result.task.task_id.clone(),
                        })
                    {
                        let _ = cancellation.await;
                    }
                }
                // Includes `ServiceError::Cancelled`: rmcp resolved the
                // responder once our request-level cancel went through — the
                // cancel reached the upstream, nothing further to do here.
                Ok(Ok(Err(_error))) => {}
                // Responder dropped: transport/service went away entirely.
                Ok(Err(_recv_error)) => {}
                Err(_elapsed) => {
                    tracing::warn!(
                        trace_id = %trace_id,
                        server = %server_id,
                        window_secs = window.as_secs(),
                        "native create reaper window expired; upstream task may be orphaned"
                    );
                }
            }
        })
    }

    /// How many task records `owner` has.
    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn task_count_for_owner(&self, owner: &TaskOwner) -> usize {
        let (count, cleanup) = {
            let mut store = self.task_store.lock().await;
            let count = store.list_for_owner(owner).len();
            (count, store.take_expired_cleanup())
        };
        self.finish_expired_task_cleanup(cleanup).await;
        count
    }

    /// Creates a task record for `owner` with `handle` attached as its abort
    /// handle, without going through a real upstream call. Exists as a test
    /// probe so cross-module teardown tests (e.g. the HTTP `DELETE /mcp`
    /// tests in `http::server`) can prove a still-running task is actually
    /// aborted by teardown, not just that its record disappears.
    #[cfg(test)]
    pub(crate) async fn attach_test_task_with_abort_handle(
        &self,
        owner: TaskOwner,
        name: &str,
        handle: tokio::task::JoinHandle<()>,
    ) -> crate::legacy_tasks::Task {
        let mut store = self.task_store.lock().await;
        let task = store
            .create(owner, name)
            .expect("test task owner must not be tombstoned");
        store.attach_abort_handle(&task.task_id, handle);
        task
    }

    pub async fn cancel_task_for_owner(
        &self,
        owner: &TaskOwner,
        task_id: &str,
    ) -> Result<CancelTaskResult, McpError> {
        let (cancelled, cleanup) = {
            let mut store = self.task_store.lock().await;
            let cancelled = store.mark_cancelled(owner, task_id);
            (cancelled, store.take_expired_cleanup())
        };
        self.finish_expired_task_cleanup(cleanup).await;
        let (task, upstream, handle) = cancelled?;
        if let Some(upstream) = upstream {
            match upstream {
                TaskUpstreamRef::Task {
                    server_id,
                    task_id: upstream_task_id,
                } => {
                    if let Some(server) = self.server_manager.get_upstream(&server_id) {
                        // Bounded like the teardown path: rmcp's plain
                        // `send_request` awaits its response forever, and an
                        // unresponsive upstream must not wedge the caller. On
                        // timeout we fall through to returning the locally
                        // cancelled task, exactly like any other non-
                        // CancelTaskResult response.
                        let bound = Duration::from_secs(server.config.call_timeout_secs);
                        let response = tokio::time::timeout(
                            bound,
                            server.client.peer().send_request({
                                let params = serde_json::to_value(
                                    crate::legacy_tasks::CancelTaskParams::new(upstream_task_id),
                                )
                                .expect("legacy task cancel params serialize");
                                crate::legacy_tasks::request("tasks/cancel", Some(params))
                            }),
                        )
                        .await;
                        match response {
                            Ok(Ok(_)) => {}
                            Err(_) => tracing::debug!(
                                server = %server_id,
                                task_id = %task_id,
                                timeout_secs = bound.as_secs(),
                                "upstream tasks/cancel timed out; returning locally cancelled task"
                            ),
                            Ok(_) => {}
                        }
                    }
                }
                TaskUpstreamRef::Request {
                    server_id,
                    request_id,
                } => {
                    if let Some(server) = self.server_manager.get_upstream(&server_id) {
                        let bound = Duration::from_secs(server.config.call_timeout_secs);
                        cancel_upstream_request(
                            server.client.peer(),
                            request_id,
                            "task cancelled",
                            bound,
                        )
                        .await;
                    }
                }
            }
        }
        if let Some(handle) = handle {
            handle.abort();
        }
        Ok(CancelTaskResult::new(task))
    }

    async fn execute_tool_task(
        self: Arc<Self>,
        task_id: String,
        tool_name: String,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        progress_token: Option<ProgressToken>,
        downstream: Option<DownstreamCallContext>,
    ) {
        let outcome = self
            .call_tool_inner(
                &tool_name,
                ToolRoundInput {
                    arguments,
                    input_responses: None,
                    request_state: None,
                    expected_route: None,
                },
                progress_token,
                downstream,
                CallKind::Task(task_id.clone()),
                false,
            )
            .await;
        let mut store = self.task_store.lock().await;
        match outcome {
            Ok(CallToolResponse::Complete(result)) => match serde_json::to_value(&result) {
                Ok(payload) => store.complete(&task_id, payload),
                Err(error) => store.fail(
                    &task_id,
                    format!("failed to serialize task result: {error}"),
                ),
            },
            Ok(other) => store.fail(
                &task_id,
                format!("unexpected upstream task response: {other:?}"),
            ),
            Err(error) => store.fail(&task_id, error.message.into_owned()),
        }
    }

    /// Hand a running task's upstream request to its record, where a cancel
    /// or a teardown finds it. When the record is already gone, its owner
    /// was torn down while the request was sent and nothing else will
    /// cancel it, so this does.
    pub(super) async fn record_task_request(
        &self,
        task_id: &str,
        server_id: &str,
        peer: &Peer<RoleClient>,
        request_id: &RequestId,
        bound: Duration,
    ) -> Result<(), McpError> {
        let mut abort_guard =
            UpstreamRequestCancelGuard::new(peer.clone(), request_id.clone(), bound);
        let recorded = self.task_store.lock().await.set_upstream_request(
            task_id,
            TaskUpstreamRef::Request {
                server_id: server_id.to_string(),
                request_id: request_id.clone(),
            },
        );
        match recorded {
            UpstreamRecordOutcome::Recorded { pending_cancel } => {
                abort_guard.defuse();
                // A cancel that arrived before the request id was recorded.
                if let Some(reason) = pending_cancel {
                    cancel_upstream_request(peer, request_id.clone(), &reason, bound).await;
                }
                Ok(())
            }
            UpstreamRecordOutcome::Missing => {
                tracing::info!(
                    task_id = %task_id,
                    server = %server_id,
                    "task record gone before upstream request was recorded; cancelling upstream request"
                );
                // The guard stays armed until this cancel resolves: an abort
                // landing mid-cancel still fires it. A rare double cancel is
                // harmless, a missed one is not.
                cancel_upstream_request(peer, request_id.clone(), "task owner disconnected", bound)
                    .await;
                abort_guard.defuse();
                Err(McpError::internal_error(
                    format!("task {task_id} ended before its call was recorded"),
                    None,
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::legacy_tasks::{Task, TaskStatus};
    use rmcp::model::PingRequest;

    /// Drop guard that flips a flag when the future holding it stops
    /// running — this is how the tests below prove an aborted task's future
    /// actually stopped executing, rather than merely proving its store
    /// record disappeared.
    struct AbortObserver(Arc<AtomicBool>);

    impl Drop for AbortObserver {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// Polls `flag` for up to two seconds, since `JoinHandle::abort()` only
    /// requests cancellation — the aborted future is actually dropped the
    /// next time the runtime polls/schedules it, which is inherently async
    /// relative to the call to `abort()`.
    async fn assert_flag_eventually(flag: &Arc<AtomicBool>, what: &str) {
        for _ in 0..200 {
            if flag.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{what} did not stop within the bounded poll window");
    }

    async fn assert_owner_teardown_aborts_long_running_local_task(owner: TaskOwner) {
        let sm = Arc::new(ServerManager::new());
        let router = ToolRouter::new(sm, RouterConfig::default());

        let dropped = Arc::new(AtomicBool::new(false));
        let observer = AbortObserver(Arc::clone(&dropped));
        let handle = tokio::spawn(async move {
            let _observer = observer;
            // Never resolves on its own — only `abort()` can stop it, just
            // like a real `execute_tool_task` future parked on
            // `await_response()` for an upstream that never replies.
            std::future::pending::<()>().await;
        });

        {
            let mut store = router.task_store.lock().await;
            let task = store
                .create(owner.clone(), "long_running_tool")
                .expect("create task");
            store.attach_abort_handle(&task.task_id, handle);
        }

        assert_eq!(router.task_count_for_owner(&owner).await, 1);
        assert!(
            !dropped.load(Ordering::SeqCst),
            "task must still be running before teardown"
        );

        router.cleanup_tasks_for_owner(&owner).await;

        assert_eq!(
            router.task_count_for_owner(&owner).await,
            0,
            "teardown must remove the task record"
        );
        assert_flag_eventually(&dropped, "aborted local task future").await;
    }

    #[tokio::test]
    async fn cleanup_tasks_for_owner_aborts_long_running_local_task_for_http_session() {
        let owner = crate::downstream_identity::http_session_task_owner("session-abort-http");
        assert_owner_teardown_aborts_long_running_local_task(owner).await;
    }

    #[tokio::test]
    async fn cleanup_tasks_for_owner_aborts_long_running_local_task_for_ipc_client() {
        // IPC/daemon disconnect and HTTP session teardown both funnel
        // through the same `cleanup_tasks_for_owner` — the owner key is the
        // only thing that differs (`ipc:<client_id>` vs `http:<session_id>`)
        // — so this is the shared-layer proof for the daemon teardown path.
        let owner = crate::downstream_identity::ipc_task_owner("client-abort-ipc");
        assert_owner_teardown_aborts_long_running_local_task(owner).await;
    }

    #[tokio::test]
    async fn cleanup_tasks_for_owner_ignores_other_owners_running_tasks() {
        let sm = Arc::new(ServerManager::new());
        let router = ToolRouter::new(sm, RouterConfig::default());
        let owner_a = crate::downstream_identity::http_session_task_owner("session-a");
        let owner_b = crate::downstream_identity::http_session_task_owner("session-b");

        let dropped = Arc::new(AtomicBool::new(false));
        let observer = AbortObserver(Arc::clone(&dropped));
        let handle = tokio::spawn(async move {
            let _observer = observer;
            std::future::pending::<()>().await;
        });

        {
            let mut store = router.task_store.lock().await;
            let task = store
                .create(owner_b.clone(), "long_running_tool")
                .expect("create task");
            store.attach_abort_handle(&task.task_id, handle);
        }

        router.cleanup_tasks_for_owner(&owner_a).await;

        // Give the runtime a moment to run anything it might (incorrectly)
        // have scheduled for abort before asserting it never happened.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !dropped.load(Ordering::SeqCst),
            "teardown for a different owner must not touch this task"
        );
        assert_eq!(router.task_count_for_owner(&owner_b).await, 1);
    }

    // ─── bounded teardown / create-vs-teardown race tests ────────────────────
    //
    // These drive the real enqueue/cleanup/cancel paths against an in-process
    // duplex-connected task-capable upstream whose `enqueue_task` handler can
    // be parked on a gate and whose `cancel_task` handler can hang forever,
    // letting a test deterministically park a native create inside a teardown
    // window or prove teardown stays bounded against an unresponsive upstream.

    use rmcp::ServiceExt as _;
    use rmcp::handler::server::ServerHandler;
    use rmcp::service::{RequestContext, RoleServer};

    use crate::server::{UpstreamClientHandler, UpstreamServer};

    /// Async gate: `wait()` parks until `open()`. Same shape as the gates in
    /// `proxy::tests`.
    struct TestGate {
        notify: tokio::sync::Notify,
        open: AtomicBool,
    }

    impl TestGate {
        fn new(open: bool) -> Self {
            Self {
                notify: tokio::sync::Notify::new(),
                open: AtomicBool::new(open),
            }
        }

        fn open(&self) {
            self.open.store(true, Ordering::SeqCst);
            self.notify.notify_waiters();
        }

        async fn wait(&self) {
            loop {
                if self.open.load(Ordering::SeqCst) {
                    return;
                }
                let notified = self.notify.notified();
                if self.open.load(Ordering::SeqCst) {
                    return;
                }
                notified.await;
            }
        }
    }

    /// Shared state backing a `GatedTaskUpstreamHandler`: a gate parking
    /// `enqueue_task` (task-wrapped tools/call), an entered flag for
    /// deterministic sequencing, a hang switch for `cancel_task`, and a log
    /// of every upstream task id a cancel was received for.
    struct GatedTaskUpstreamState {
        enqueue_gate: TestGate,
        enqueue_entered: AtomicBool,
        /// Set when a request-level `notifications/cancelled` lands for a
        /// parked `enqueue_task` (rmcp cancels the request's `context.ct`; it
        /// never aborts handler futures, so cooperative observation is the
        /// only way a test can see the cancel).
        enqueue_cancelled: AtomicBool,
        enqueue_cancel_notify: tokio::sync::Notify,
        hang_task_requests: bool,
        cancel_log: std::sync::Mutex<Vec<String>>,
    }

    impl GatedTaskUpstreamState {
        fn new(hang_task_requests: bool, enqueue_gate_open: bool) -> Arc<Self> {
            Arc::new(Self {
                enqueue_gate: TestGate::new(enqueue_gate_open),
                enqueue_entered: AtomicBool::new(false),
                enqueue_cancelled: AtomicBool::new(false),
                enqueue_cancel_notify: tokio::sync::Notify::new(),
                hang_task_requests,
                cancel_log: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn cancel_received_for(&self, upstream_task_id: &str) -> bool {
            self.cancel_log
                .lock()
                .unwrap()
                .iter()
                .any(|logged| logged == upstream_task_id)
        }

        async fn wait_for_enqueue_cancel(&self) {
            loop {
                if self.enqueue_cancelled.load(Ordering::SeqCst) {
                    return;
                }
                let notified = self.enqueue_cancel_notify.notified();
                if self.enqueue_cancelled.load(Ordering::SeqCst) {
                    return;
                }
                notified.await;
            }
        }
    }

    fn gated_legacy_task_capabilities() -> ServerCapabilities {
        let mut capabilities = ServerCapabilities::default();
        capabilities
            .experimental
            .get_or_insert_with(Default::default)
            .insert(
                crate::protocol::LEGACY_TASKS_CAPABILITY_KEY.to_string(),
                serde_json::Map::new(),
            );
        capabilities
    }

    async fn write_gated_legacy_response(
        write: &tokio::sync::Mutex<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
        response: serde_json::Value,
    ) {
        use tokio::io::AsyncWriteExt;
        let mut encoded = serde_json::to_vec(&response).expect("encode gated legacy response");
        encoded.push(b'\n');
        let mut write = write.lock().await;
        write
            .write_all(&encoded)
            .await
            .expect("write gated legacy response");
        write.flush().await.expect("flush gated legacy response");
    }

    async fn serve_gated_legacy_task_upstream(
        stream: tokio::io::DuplexStream,
        state: Arc<GatedTaskUpstreamState>,
    ) {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let (read, write) = tokio::io::split(stream);
        let write = Arc::new(tokio::sync::Mutex::new(write));
        let mut lines = BufReader::new(read).lines();

        while let Some(line) = lines.next_line().await.expect("read gated legacy request") {
            let request: serde_json::Value =
                serde_json::from_str(&line).expect("valid gated legacy JSON-RPC");
            let method = request["method"].as_str().unwrap_or_default();
            let Some(id) = request.get("id").cloned() else {
                if method == "notifications/cancelled" {
                    state.enqueue_cancelled.store(true, Ordering::SeqCst);
                    state.enqueue_cancel_notify.notify_waiters();
                }
                continue;
            };

            match method {
                "initialize" => {
                    write_gated_legacy_response(
                        write.as_ref(),
                        serde_json::json!({"jsonrpc":"2.0","id":id,"result":{
                            "protocolVersion":crate::protocol::SUPPORTED_PROTOCOL_VERSION,
                            "capabilities":{"tasks":{"list":{},"cancel":{},"requests":{"tools":{"call":{}}}}},
                            "serverInfo":{"name":"gated-legacy-task","version":"1.0.0"}
                        }}),
                    )
                    .await;
                }
                "tools/call" => {
                    state.enqueue_entered.store(true, Ordering::SeqCst);
                    let state = Arc::clone(&state);
                    let write = Arc::clone(&write);
                    tokio::spawn(async move {
                        tokio::select! {
                            _ = state.wait_for_enqueue_cancel() => {
                                write_gated_legacy_response(
                                    write.as_ref(),
                                    serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":"task-wrapped call cancelled by client"}}),
                                ).await;
                            }
                            _ = state.enqueue_gate.wait() => {
                                let now = rmcp::task_manager::current_timestamp();
                                let task = Task::new("upstream-task-1".to_string(), TaskStatus::Working, now.clone(), now);
                                write_gated_legacy_response(
                                    write.as_ref(),
                                    serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"task":task}}),
                                ).await;
                            }
                        }
                    });
                }
                "tasks/cancel" => {
                    let task_id = request["params"]["taskId"]
                        .as_str()
                        .expect("legacy cancel task id")
                        .to_string();
                    state.cancel_log.lock().unwrap().push(task_id);
                    if !state.hang_task_requests {
                        write_gated_legacy_response(
                            write.as_ref(),
                            serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
                        )
                        .await;
                    }
                }
                "tasks/get" | "tasks/result" if state.hang_task_requests => {}
                "tasks/get" => {
                    let now = rmcp::task_manager::current_timestamp();
                    let task = Task::new(
                        "upstream-task-1".to_string(),
                        TaskStatus::Completed,
                        now.clone(),
                        now,
                    );
                    write_gated_legacy_response(
                        write.as_ref(),
                        serde_json::json!({"jsonrpc":"2.0","id":id,"result":task}),
                    )
                    .await;
                }
                "tasks/result" => {
                    write_gated_legacy_response(
                        write.as_ref(),
                        serde_json::json!({"jsonrpc":"2.0","id":id,"result":{
                            "content":[
                                {"type":"text","text":"done"},
                                {"type":"resource_link","uri":"skill://refunds/SKILL.md","name":"SKILL.md"}
                            ],
                            "isError":false
                        }}),
                    )
                    .await;
                }
                _ => {
                    write_gated_legacy_response(
                        write.as_ref(),
                        serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":method}}),
                    )
                    .await;
                }
            }
        }
    }

    fn gated_task_server_config(call_timeout_secs: u64) -> crate::config::ServerConfig {
        crate::config::ServerConfig {
            command: Some("fake".to_string()),
            call_timeout_secs,
            circuit_breaker_enabled: false,
            ..Default::default()
        }
    }

    /// Build a real, duplex-connected task-capable `UpstreamServer` backed by
    /// the given state, mirroring `proxy::tests::connect_subscribable_upstream`.
    async fn connect_gated_task_upstream(
        name: &str,
        state: Arc<GatedTaskUpstreamState>,
        call_timeout_secs: u64,
    ) -> UpstreamServer {
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            serve_gated_legacy_task_upstream(server_transport, state).await;
        });

        let tools = Arc::new(arc_swap::ArcSwap::from_pointee(Vec::<Tool>::new()));
        let handler = Arc::new(UpstreamClientHandler::new_for_tests(
            Arc::from(name.to_string()),
            Arc::clone(&tools),
            crate::server::no_events(),
        ));
        let client = handler
            .serve(client_transport)
            .await
            .expect("connect gated task upstream test client");

        let capabilities = gated_legacy_task_capabilities();

        UpstreamServer {
            name: name.to_string(),
            config: gated_task_server_config(call_timeout_secs),
            client,
            tools,
            capabilities,
            upstream: None,
            protocol_era: crate::protocol::ProtocolEra::Legacy,
            selected_protocol_version: crate::protocol::SUPPORTED_PROTOCOL_VERSION.to_string(),
            protocol_gate_state: 0,
            connection: crate::server::ConnectionGeneration::new(),
            child_pid: None,
            stderr_tail: None,
        }
    }

    /// Owns the parked runtime used by `connect_frozen_task_upstream` and
    /// guarantees it is released even when a regression assertion panics.
    struct FrozenUpstreamRuntime {
        release: Option<std::sync::mpsc::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for FrozenUpstreamRuntime {
        fn drop(&mut self) {
            if let Some(release) = self.release.take() {
                let _ = release.send(());
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Connect a real rmcp client/server pair, then stop polling the runtime
    /// that owns their service loops. The returned client's private
    /// 1,024-message peer queue can therefore be filled deterministically to
    /// exercise request-handle acquisition backpressure.
    fn connect_frozen_task_upstream(
        name: &str,
        state: Arc<GatedTaskUpstreamState>,
        call_timeout_secs: u64,
    ) -> (UpstreamServer, FrozenUpstreamRuntime) {
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let name = name.to_string();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build frozen upstream runtime");
            let upstream =
                runtime.block_on(connect_gated_task_upstream(&name, state, call_timeout_secs));
            ready_tx.send(upstream).expect("publish frozen upstream");
            let _ = release_rx.recv();
        });
        let upstream = ready_rx.recv().expect("receive frozen upstream");
        (
            upstream,
            FrozenUpstreamRuntime {
                release: Some(release_tx),
                thread: Some(thread),
            },
        )
    }

    fn upstream_working_task(task_id: &str) -> Task {
        Task::new(
            task_id.to_string(),
            TaskStatus::Working,
            rmcp::task_manager::current_timestamp(),
            rmcp::task_manager::current_timestamp(),
        )
    }

    /// Yields (never sleeps, so paused time stays frozen) until `flag` is
    /// set, up to a fixed scheduling budget. Returns whether it was seen.
    async fn yield_until(flag: &AtomicBool) -> bool {
        for _ in 0..1000 {
            tokio::task::yield_now().await;
            if flag.load(Ordering::SeqCst) {
                return true;
            }
        }
        false
    }

    /// Defect-1 regression test: teardown must be abort-first and bounded.
    /// An upstream whose `tasks/cancel` never responds used to (a) block the
    /// `handle.abort()` of every later record and (b) hang the caller
    /// forever (rmcp's `send_request` has no timeout of its own). Paused
    /// time makes the bound assertion exact: the yield-only phase never
    /// advances the clock, and the final join can only complete via the
    /// per-upstream `call_timeout_secs` timer.
    #[tokio::test(start_paused = true)]
    async fn cleanup_is_bounded_and_aborts_local_tasks_before_hung_upstream_cancel() {
        let state = GatedTaskUpstreamState::new(true, true);
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
        sm.replace_server(
            "hung",
            connect_gated_task_upstream("hung", Arc::clone(&state), 5).await,
        )
        .await;

        let owner = crate::downstream_identity::http_session_task_owner("session-hung-teardown");

        // Record 1: passthrough task on the unresponsive upstream.
        // Record 2: still-running local future whose abort we observe.
        let dropped = Arc::new(AtomicBool::new(false));
        let observer = AbortObserver(Arc::clone(&dropped));
        let handle = tokio::spawn(async move {
            let _observer = observer;
            std::future::pending::<()>().await;
        });
        {
            let mut store = router.task_store.lock().await;
            store
                .create_passthrough(
                    owner.clone(),
                    "hung_tool",
                    &upstream_working_task("upstream-task-hung"),
                    TaskUpstreamRef::Task {
                        server_id: "hung".to_string(),
                        task_id: "upstream-task-hung".to_string(),
                    },
                )
                .expect("create passthrough record");
            let local = store
                .create(owner.clone(), "long_running_tool")
                .expect("create local record");
            store.attach_abort_handle(&local.task_id, handle);
        }
        assert_eq!(router.task_count_for_owner(&owner).await, 2);

        let cleanup_router = Arc::clone(&router);
        let cleanup_owner = owner.clone();
        let cleanup = tokio::spawn(async move {
            cleanup_router.cleanup_tasks_for_owner(&cleanup_owner).await;
        });

        // Abort-first: the local future must stop while cleanup is still
        // parked on the hung upstream's cancel. The yield loop never sleeps,
        // so paused time stays frozen and the phase-B timeout cannot be what
        // unblocked anything observed here.
        assert!(
            yield_until(&dropped).await,
            "local record must be aborted before the hung upstream cancel resolves"
        );
        assert!(
            !cleanup.is_finished(),
            "cleanup must still be parked on the hung upstream cancel at this point"
        );

        // Boundedness: only the per-upstream call timeout can unblock phase
        // B (the cancel handler is parked forever). The generous outer bound
        // trips only if teardown regresses to an unbounded await.
        tokio::time::timeout(Duration::from_secs(60), cleanup)
            .await
            .expect("cleanup_tasks_for_owner must return within the per-upstream bound")
            .expect("cleanup task must not panic");

        assert_eq!(router.task_count_for_owner(&owner).await, 0);
        assert!(
            state.cancel_received_for("upstream-task-hung"),
            "the hung upstream must have received the forwarded cancel request"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stale_live_task_expiry_sends_bounded_upstream_cancellation() {
        let state = GatedTaskUpstreamState::new(true, false);
        let sm = Arc::new(ServerManager::new());
        let router = ToolRouter::new(Arc::clone(&sm), RouterConfig::default());
        sm.replace_server(
            "expiry-upstream",
            connect_gated_task_upstream("expiry-upstream", Arc::clone(&state), 5).await,
        )
        .await;

        let owner = crate::downstream_identity::http_session_task_owner("session-expired-upstream");
        {
            let mut store = router.task_store.lock().await;
            store
                .create_passthrough(
                    owner.clone(),
                    "slow",
                    &upstream_working_task("upstream-expired-1"),
                    TaskUpstreamRef::Task {
                        server_id: "expiry-upstream".to_string(),
                        task_id: "upstream-expired-1".to_string(),
                    },
                )
                .expect("create stale passthrough task");
        }

        tokio::time::advance(Duration::from_millis(
            crate::tasks::DEFAULT_STALE_IN_FLIGHT_TTL_MS + 1,
        ))
        .await;
        let listed = router
            .list_tasks_for_owner(&owner, None)
            .await
            .expect("expiry cleanup remains best effort");
        assert!(listed.tasks.is_empty());
        assert_eq!(
            state.cancel_log.lock().unwrap().as_slice(),
            ["upstream-expired-1"]
        );
    }

    /// Defect-1 companion for `cancel_task_for_owner`: the forwarding arm
    /// gets the same per-upstream bound, and on timeout the caller still
    /// gets the locally cancelled task instead of hanging forever.
    #[tokio::test(start_paused = true)]
    async fn cancel_task_for_owner_is_bounded_when_upstream_cancel_hangs() {
        let state = GatedTaskUpstreamState::new(true, true);
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
        sm.replace_server(
            "hung",
            connect_gated_task_upstream("hung", Arc::clone(&state), 5).await,
        )
        .await;

        let owner = crate::downstream_identity::http_session_task_owner("session-hung-cancel");
        let task = router
            .task_store
            .lock()
            .await
            .create_passthrough(
                owner.clone(),
                "hung_tool",
                &upstream_working_task("upstream-task-hung"),
                TaskUpstreamRef::Task {
                    server_id: "hung".to_string(),
                    task_id: "upstream-task-hung".to_string(),
                },
            )
            .expect("create passthrough record");

        let cancelled = tokio::time::timeout(
            Duration::from_secs(60),
            router.cancel_task_for_owner(&owner, &task.task_id),
        )
        .await
        .expect("cancel_task_for_owner must return within the per-upstream bound")
        .expect("cancel must succeed locally even when the upstream never answers");

        assert_eq!(cancelled.task.status, TaskStatus::Cancelled);
        assert!(
            state.cancel_received_for("upstream-task-hung"),
            "the hung upstream must have received the forwarded cancel request"
        );
    }

    /// Asking a task-native upstream that never answers about its task
    /// fails at the server's call timeout instead of waiting forever.
    #[tokio::test(start_paused = true)]
    async fn task_status_and_result_are_bounded_when_upstream_hangs() {
        let state = GatedTaskUpstreamState::new(true, true);
        let sm = Arc::new(ServerManager::new());
        let router = ToolRouter::new(Arc::clone(&sm), RouterConfig::default());
        sm.replace_server(
            "hung",
            connect_gated_task_upstream("hung", Arc::clone(&state), 5).await,
        )
        .await;

        let owner = crate::downstream_identity::http_session_task_owner("session-hung-get");
        let task = router
            .task_store
            .lock()
            .await
            .create_passthrough(
                owner.clone(),
                "hung_tool",
                &upstream_working_task("upstream-task-hung"),
                TaskUpstreamRef::Task {
                    server_id: "hung".to_string(),
                    task_id: "upstream-task-hung".to_string(),
                },
            )
            .expect("create passthrough record");

        let started = tokio::time::Instant::now();
        let status = tokio::time::timeout(
            Duration::from_secs(60),
            router.get_task_info_for_owner(&owner, &task.task_id),
        )
        .await
        .expect("tasks/get must give up within the call timeout");
        assert!(status.is_err());
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            router.get_task_result_for_owner(&owner, &task.task_id),
        )
        .await
        .expect("tasks/result must give up within the call timeout");
        assert!(result.is_err());
        assert_eq!(started.elapsed(), Duration::from_secs(10));
    }

    /// Defect-2 regression test: a teardown that runs while the native
    /// path's upstream CallToolRequest is still in flight used to see no
    /// record, and the late `create_passthrough` then inserted a Working
    /// record for the torn-down owner (unpruned for 24h) whose upstream
    /// native task was never cancelled. Now the enqueue must error, leave no
    /// record, and send the upstream a cancel for the task it created.
    #[tokio::test]
    async fn native_enqueue_racing_teardown_cancels_upstream_and_returns_error() {
        let state = GatedTaskUpstreamState::new(false, false);
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
        sm.replace_server(
            "mock",
            connect_gated_task_upstream("mock", Arc::clone(&state), 5).await,
        )
        .await;

        let mut routes = HashMap::new();
        routes.insert(
            "Mock__hang".to_string(),
            ("mock".to_string(), "hang".to_string()),
        );
        router.replace_snapshot(RouterSnapshot {
            routes,
            tools_all: Arc::new(Vec::new()),
            meta_tools_all: Arc::new(Vec::new()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            routes_lower: HashMap::new(),
            tools_by_name: HashMap::new(),
            tools_by_name_lower: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
            tool_risk_inventory: HashMap::new(),
        });

        let owner = crate::downstream_identity::http_session_task_owner("session-native-race");
        let enqueue_router = Arc::clone(&router);
        let enqueue_owner = owner.clone();
        let enqueue = tokio::spawn(async move {
            enqueue_router
                .enqueue_tool_task("Mock__hang", None, None, enqueue_owner, None, None)
                .await
        });

        // Park the upstream round trip inside the mock's enqueue handler.
        assert!(
            yield_until(&state.enqueue_entered).await,
            "upstream enqueue handler must have been entered"
        );

        // Teardown interleaves while the create is still in flight upstream.
        router.cleanup_tasks_for_owner(&owner).await;

        // Release the gate: the CreateTaskResult now lands on a tombstoned
        // owner.
        state.enqueue_gate.open();
        let result = tokio::time::timeout(Duration::from_secs(10), enqueue)
            .await
            .expect("enqueue must resolve once the gate opens")
            .expect("enqueue task must not panic");

        let error =
            result.expect_err("a create landing after teardown must error, not orphan a record");
        assert_eq!(error.code, ErrorCode::INVALID_REQUEST);
        assert_eq!(
            router.task_count_for_owner(&owner).await,
            0,
            "no record may exist for the torn-down owner"
        );
        assert!(
            state.cancel_received_for("upstream-task-1"),
            "the upstream must be told to cancel the native task it created"
        );

        // Tombstone hygiene: with the in-flight enqueue resolved, the
        // tombstone is gone and a fresh create for the same owner key
        // succeeds again.
        router
            .task_store
            .lock()
            .await
            .create(owner.clone(), "fresh_tool")
            .expect("fresh create after the tombstone cleared");
        assert_eq!(router.task_count_for_owner(&owner).await, 1);
    }

    /// Request creation itself is backpressured by rmcp's bounded peer
    /// channel. The configured call timeout must cover that acquisition as
    /// well as the response; otherwise the detached owner-create guard can
    /// remain live forever before a request id even exists to cancel.
    #[tokio::test(start_paused = true)]
    async fn native_enqueue_bounds_request_handle_acquisition() {
        let state = GatedTaskUpstreamState::new(false, true);
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
        let (upstream, _frozen_runtime) =
            connect_frozen_task_upstream("mock", Arc::clone(&state), 2);
        let peer = upstream.client.peer().clone();
        sm.replace_server("mock", upstream).await;

        // The service runtime is parked, so nothing drains rmcp's private
        // peer channel. Fill its documented 1,024 slots with requests whose
        // handles are intentionally dropped; the next send must backpressure.
        for index in 0..1024 {
            tokio::time::timeout(
                Duration::from_secs(1),
                peer.send_cancellable_request(
                    ClientRequest::PingRequest(PingRequest::default()),
                    PeerRequestOptions::default(),
                ),
            )
            .await
            .unwrap_or_else(|_| panic!("peer queue filled before slot {index}"))
            .expect("queue ping request");
        }

        let mut routes = HashMap::new();
        routes.insert(
            "Mock__echo".to_string(),
            ("mock".to_string(), "echo".to_string()),
        );
        router.replace_snapshot(RouterSnapshot {
            routes,
            tools_all: Arc::new(Vec::new()),
            meta_tools_all: Arc::new(Vec::new()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            routes_lower: HashMap::new(),
            tools_by_name: HashMap::new(),
            tools_by_name_lower: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
            tool_risk_inventory: HashMap::new(),
        });

        let owner = crate::downstream_identity::http_session_task_owner("session-full-peer-queue");
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            router.enqueue_tool_task("Mock__echo", None, None, owner.clone(), None, None),
        )
        .await
        .expect("request-handle acquisition must respect the configured timeout");

        let error = result.expect_err("a full peer queue must time out task creation");
        assert!(error.message.to_lowercase().contains("timeout"));
        assert_eq!(router.task_count_for_owner(&owner).await, 0);
    }

    /// Installs a snapshot exposing exactly one routed tool, so enqueue
    /// tests can route `exposed` to (`server_id`, `original`) without a
    /// full `refresh_tools` round trip.
    fn install_single_route(router: &ToolRouter, exposed: &str, server_id: &str, original: &str) {
        let mut routes = HashMap::new();
        routes.insert(
            exposed.to_string(),
            (server_id.to_string(), original.to_string()),
        );
        router.replace_snapshot(RouterSnapshot {
            routes,
            tools_all: Arc::new(Vec::new()),
            meta_tools_all: Arc::new(Vec::new()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            routes_lower: HashMap::new(),
            tools_by_name: HashMap::new(),
            tools_by_name_lower: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
            tool_risk_inventory: HashMap::new(),
        });
    }

    fn authorized_task_context(
        principal: crate::types::PrincipalId,
        request_id: i64,
    ) -> DownstreamCallContext {
        DownstreamCallContext::http(
            format!("quota-session-{request_id}"),
            RequestId::from(NumberOrString::Number(request_id)),
            ClientType::Unknown,
        )
        .with_trace_id(Arc::<str>::from(format!("{request_id:032x}")))
        .with_authorization(
            principal,
            std::collections::BTreeSet::from(["tools:read".to_string(), "tasks:use".to_string()]),
        )
    }

    fn authorized_modern_task_context(
        principal: crate::types::PrincipalId,
        request_id: i64,
    ) -> DownstreamCallContext {
        authorized_task_context(principal, request_id)
            .with_protocol(
                crate::protocol::ProtocolEra::Modern,
                crate::protocol::MODERN_PROTOCOL_VERSION,
            )
            .with_modern_direction_enabled(true)
    }

    #[tokio::test]
    async fn modern_downstream_forces_local_wrapper_around_legacy_native_upstream() {
        let state = GatedTaskUpstreamState::new(false, false);
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
        sm.replace_server(
            "mock",
            connect_gated_task_upstream("mock", Arc::clone(&state), 5).await,
        )
        .await;
        install_single_route(&router, "Mock__echo", "mock", "echo");

        let principal = crate::types::PrincipalId::downstream_oauth(
            "https://issuer.example",
            "modern-client",
            "https://plug.example/mcp",
        );
        let owner = TaskOwner::new(principal.owner_key());
        let created = router
            .enqueue_tool_task(
                "Mock__echo",
                None,
                None,
                owner.clone(),
                None,
                Some(authorized_modern_task_context(principal, 77)),
            )
            .await
            .expect("create Plug-owned modern task");

        assert_ne!(created.task.task_id, "upstream-task-1");
        assert!(
            yield_until(&state.enqueue_entered).await,
            "ordinary upstream call must start inside the local wrapper"
        );
        let upstream = router
            .task_store
            .lock()
            .await
            .upstream_for_owner(&owner, &created.task.task_id)
            .expect("task ownership");
        assert!(
            matches!(upstream, Some(TaskUpstreamRef::Request { .. })),
            "modern task must track an ordinary request, never a legacy native task"
        );

        router.cleanup_tasks_for_owner(&owner).await;
    }

    #[tokio::test]
    async fn modern_get_inlines_result_for_legacy_passthrough_completed_in_same_process() {
        let state = GatedTaskUpstreamState::new(false, true);
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
        sm.replace_server("mock", connect_gated_task_upstream("mock", state, 5).await)
            .await;
        let owner = TaskOwner::new(Arc::<str>::from("shared-durable-principal"));
        let local = router
            .task_store
            .lock()
            .await
            .create_passthrough(
                owner.clone(),
                "Mock__echo",
                &upstream_working_task("upstream-task-1"),
                TaskUpstreamRef::Task {
                    server_id: "mock".to_string(),
                    task_id: "upstream-task-1".to_string(),
                },
            )
            .expect("legacy passthrough record");

        let modern = router
            .get_modern_task_for_owner(&owner, &local.task_id)
            .await
            .expect("modern projection fetches separate legacy result");
        let rmcp::model::TaskPayload::Completed { result } = modern.task.payload else {
            panic!("expected completed task payload");
        };
        assert_eq!(result["content"][0]["text"], "done");
        assert_eq!(result["isError"], false);
        // The upstream ran the tool, and its skill link still names it.
        assert_eq!(result["content"][1]["uri"], "skill://mock/refunds/SKILL.md");
    }

    async fn enqueue_quota_test_task(
        router: &Arc<ToolRouter>,
        principal: crate::types::PrincipalId,
        request_id: i64,
    ) -> Result<CreateTaskResult, McpError> {
        let owner = TaskOwner::new(principal.owner_key());
        router
            .enqueue_tool_task(
                "Mock__echo",
                None,
                None,
                owner,
                None,
                Some(authorized_task_context(principal, request_id)),
            )
            .await
    }

    #[tokio::test]
    async fn durable_task_quota_is_principal_isolated_and_released_on_cleanup() {
        let quotas = crate::protocol::AdmissionQuotas::new(crate::protocol::AdmissionQuotaConfig {
            tasks: 2,
            queued_task_creations: 8,
            per_principal_divisor: 2,
            ..Default::default()
        });
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new_with_quotas(
            sm,
            RouterConfig::default(),
            quotas,
        ));
        install_single_route(&router, "Mock__echo", "missing", "echo");
        let principal_a = crate::types::PrincipalId::downstream_oauth(
            "https://issuer.example",
            "client-a",
            "https://plug.example/mcp",
        );
        let principal_b = crate::types::PrincipalId::downstream_oauth(
            "https://issuer.example",
            "client-b",
            "https://plug.example/mcp",
        );
        let owner_a = TaskOwner::new(principal_a.owner_key());

        enqueue_quota_test_task(&router, principal_a.clone(), 1)
            .await
            .expect("principal A first task");
        let error = enqueue_quota_test_task(&router, principal_a.clone(), 2)
            .await
            .expect_err("principal A must not exceed its retained task quota");
        assert_eq!(error.code, rmcp::model::ErrorCode(-32007));
        enqueue_quota_test_task(&router, principal_b, 3)
            .await
            .expect("principal B retains an independent share");

        router.cleanup_tasks_for_owner(&owner_a).await;
        enqueue_quota_test_task(&router, principal_a, 4)
            .await
            .expect("cleanup must release the retained task lease");
    }

    #[tokio::test]
    async fn durable_task_quota_enforces_global_limit_and_releases_on_cleanup() {
        let quotas = crate::protocol::AdmissionQuotas::new(crate::protocol::AdmissionQuotaConfig {
            tasks: 1,
            queued_task_creations: 8,
            per_principal_divisor: 1,
            ..Default::default()
        });
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new_with_quotas(
            sm,
            RouterConfig::default(),
            quotas,
        ));
        install_single_route(&router, "Mock__echo", "missing", "echo");
        let principal_a = crate::types::PrincipalId::downstream_oauth(
            "https://issuer.example",
            "client-a",
            "https://plug.example/mcp",
        );
        let principal_b = crate::types::PrincipalId::downstream_oauth(
            "https://issuer.example",
            "client-b",
            "https://plug.example/mcp",
        );
        let owner_a = TaskOwner::new(principal_a.owner_key());

        enqueue_quota_test_task(&router, principal_a, 10)
            .await
            .expect("first global task slot");
        let error = enqueue_quota_test_task(&router, principal_b.clone(), 11)
            .await
            .expect_err("global quota must reject another principal while full");
        assert_eq!(error.code, rmcp::model::ErrorCode(-32007));

        router.cleanup_tasks_for_owner(&owner_a).await;
        enqueue_quota_test_task(&router, principal_b, 12)
            .await
            .expect("cleanup must release global capacity");
    }

    /// T1 (Part 1): an owner-liveness probe reporting the owner closed must
    /// refuse the create outright — before route lookup, before any
    /// upstream traffic — leave no record, release the lifecycle-ledger
    /// entry (guard at zero), and leave no tombstone behind.
    #[tokio::test]
    async fn enqueue_refuses_when_owner_liveness_probe_reports_closed() {
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(sm, RouterConfig::default()));
        let owner = crate::downstream_identity::http_session_task_owner("session-already-gone");

        let probe: OwnerLivenessProbe = Arc::new(|| false);
        let error = router
            .enqueue_tool_task("Mock__echo", None, None, owner.clone(), Some(probe), None)
            .await
            .expect_err("a probe reporting the owner closed must refuse the create");

        assert_eq!(error.code, ErrorCode::INVALID_REQUEST);
        assert!(
            error
                .message
                .contains("session closed during task creation"),
            "unexpected refusal message: {}",
            error.message
        );
        assert_eq!(router.task_count_for_owner(&owner).await, 0);

        let mut store = router.task_store.lock().await;
        assert!(
            !store.owner_has_lifecycle_entry(&owner),
            "the create guard must be released — ledger back at zero for this owner"
        );
        // No tombstone residue either: a later create for the same owner
        // key (e.g. an IPC client id that reconnected) succeeds.
        store
            .create(owner.clone(), "fresh_tool")
            .expect("fresh create after a probe refusal");
    }

    /// T2 (Part 2): a native create whose upstream never answers must
    /// resolve within the per-upstream `call_timeout_secs` bound (paused
    /// time: only that timer can unblock it), surface a timeout error,
    /// send the upstream a request-level cancel for the parked call, and
    /// leave neither a record nor a ledger entry behind.
    #[tokio::test(start_paused = true)]
    async fn hung_native_create_is_bounded_and_cancels_upstream_request() {
        let state = GatedTaskUpstreamState::new(false, false);
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
        sm.replace_server(
            "mock",
            connect_gated_task_upstream("mock", Arc::clone(&state), 2).await,
        )
        .await;
        install_single_route(&router, "Mock__hang", "mock", "hang");

        let owner =
            crate::downstream_identity::http_session_task_owner("session-hung-native-create");
        let enqueue_router = Arc::clone(&router);
        let enqueue_owner = owner.clone();
        let enqueue = tokio::spawn(async move {
            enqueue_router
                .enqueue_tool_task("Mock__hang", None, None, enqueue_owner, None, None)
                .await
        });

        // Park the round trip inside the upstream's enqueue handler; the
        // yield loop never sleeps, so paused time stays frozen until the
        // create is provably in flight.
        assert!(
            yield_until(&state.enqueue_entered).await,
            "upstream enqueue handler must have been entered"
        );

        // Only the enqueue-side call-timeout timer can resolve this join;
        // the generous outer bound trips only if the create regresses to an
        // unbounded await.
        let result = tokio::time::timeout(Duration::from_secs(60), enqueue)
            .await
            .expect("hung native create must resolve within the per-upstream bound")
            .expect("enqueue task must not panic");
        let error = result.expect_err("a never-answering upstream must surface an error");
        assert!(
            error.message.contains("request timeout"),
            "unexpected error message: {}",
            error.message
        );

        // The bounded request-level cancel must actually reach the upstream
        // and unblock its parked handler.
        assert!(
            yield_until(&state.enqueue_cancelled).await,
            "the upstream must receive a request-level cancel for the timed-out create"
        );

        assert_eq!(
            router.task_count_for_owner(&owner).await,
            0,
            "a timed-out create must not leave a record"
        );
        let mut store = router.task_store.lock().await;
        assert!(
            !store.owner_has_lifecycle_entry(&owner),
            "the create guard must be released after the timeout"
        );
        store
            .create(owner.clone(), "fresh_tool")
            .expect("fresh create after the timed-out enqueue resolved");
    }

    /// T2 companion (Part 2): the reaper arm for a `CreateTaskResult` that
    /// lands after the create already timed out — driven directly through
    /// the response oneshot, because through the full stack rmcp resolves
    /// the responder with `Cancelled` once our request-level cancel is
    /// sent, making the late-result race unreachable deterministically.
    /// The reaper must cancel the late-created upstream task by id.
    #[tokio::test]
    async fn native_create_reaper_cancels_late_created_upstream_task() {
        let state = GatedTaskUpstreamState::new(false, true);
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
        sm.replace_server(
            "mock",
            connect_gated_task_upstream("mock", Arc::clone(&state), 5).await,
        )
        .await;

        let (tx, rx) = tokio::sync::oneshot::channel();
        let reaper = router.spawn_native_create_reaper(
            rx,
            Duration::from_secs(5),
            "mock".to_string(),
            Arc::from("trace-reaper-test"),
        );
        let task = upstream_working_task("late-task");
        tx.send(Ok(ServerResult::CreateTaskResult(
            rmcp::model::CreateTaskResult::new((&task).into()),
        )))
        .expect("deliver the late create result to the reaper");

        tokio::time::timeout(Duration::from_secs(10), reaper)
            .await
            .expect("reaper must resolve once the late result lands")
            .expect("reaper task must not panic");
        assert!(
            state.cancel_received_for("late-task"),
            "the reaper must cancel a late-created upstream task by id"
        );
    }

    /// T3 (Part 2): dropping the enqueue caller mid-round-trip (axum drops
    /// its POST handler future when the HTTP client disconnects) must not
    /// orphan the native create — the detached round trip completes on its
    /// own and records the created task for the still-live owner, leaving
    /// it retrievable instead of running upstream untracked.
    #[tokio::test]
    async fn dropped_enqueue_caller_does_not_orphan_native_create() {
        let state = GatedTaskUpstreamState::new(false, false);
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
        sm.replace_server(
            "mock",
            connect_gated_task_upstream("mock", Arc::clone(&state), 5).await,
        )
        .await;
        install_single_route(&router, "Mock__hang", "mock", "hang");

        let owner = crate::downstream_identity::http_session_task_owner("session-caller-drop");
        let enqueue_router = Arc::clone(&router);
        let enqueue_owner = owner.clone();
        let enqueue = tokio::spawn(async move {
            enqueue_router
                .enqueue_tool_task("Mock__hang", None, None, enqueue_owner, None, None)
                .await
        });

        assert!(
            yield_until(&state.enqueue_entered).await,
            "upstream enqueue handler must have been entered"
        );

        // The caller dies while the create is still in flight upstream.
        enqueue.abort();
        let _ = enqueue.await;

        // The detached round trip must survive the caller's death: once the
        // upstream answers, the task is recorded for the live owner.
        state.enqueue_gate.open();
        let mut recorded = false;
        for _ in 0..200 {
            if router.task_count_for_owner(&owner).await == 1 {
                recorded = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            recorded,
            "the detached round trip must record the created task for the live owner"
        );
        assert!(
            !state.cancel_received_for("upstream-task-1"),
            "a live owner's recorded task must not be cancelled"
        );

        // Ledger hygiene: the guard travelled with the detached round trip
        // and is released once it completes.
        let mut released = false;
        for _ in 0..200 {
            if !router
                .task_store
                .lock()
                .await
                .owner_has_lifecycle_entry(&owner)
            {
                released = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            released,
            "the create guard must be released once the detached round trip completes"
        );
    }

    // ─── wrapper-path (non-task-capable upstream) send-to-record gap tests ───

    /// Shared state backing a `GatedCallUpstreamHandler`: a plain
    /// (non-task-capable) upstream whose `call_tool` parks on a gate until
    /// the request-level cancellation token fires, recording both entry and
    /// cancellation. rmcp cancels `context.ct` when a request-level
    /// `notifications/cancelled` arrives — it never aborts handler futures
    /// — so this cooperative observation is how the tests below prove the
    /// proxy's cancel actually reached the upstream.
    struct GatedCallUpstreamState {
        call_gate: TestGate,
        call_entered: AtomicBool,
        call_cancelled: AtomicBool,
    }

    impl GatedCallUpstreamState {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                call_gate: TestGate::new(false),
                call_entered: AtomicBool::new(false),
                call_cancelled: AtomicBool::new(false),
            })
        }
    }

    struct GatedCallUpstreamHandler {
        state: Arc<GatedCallUpstreamState>,
    }

    impl ServerHandler for GatedCallUpstreamHandler {
        fn get_info(&self) -> ServerInfo {
            // No tasks capability: enqueue takes the local-wrapper path and
            // `execute_tool_task` drives this upstream via plain tools/call.
            ServerInfo::new(ServerCapabilities::default())
        }

        fn call_tool(
            &self,
            _request: CallToolRequestParams,
            context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<CallToolResponse, McpError>> + Send + '_ {
            let state = Arc::clone(&self.state);
            async move {
                state.call_entered.store(true, Ordering::SeqCst);
                tokio::select! {
                    _ = context.ct.cancelled() => {
                        state.call_cancelled.store(true, Ordering::SeqCst);
                        Err(McpError::internal_error("call cancelled by client", None))
                    }
                    _ = state.call_gate.wait() => {
                        Ok(CallToolResult::success(vec![ContentBlock::text("done")]).into())
                    }
                }
            }
        }
    }

    /// Build a real, duplex-connected NON-task-capable `UpstreamServer`
    /// backed by the given state, mirroring `connect_gated_task_upstream`.
    async fn connect_gated_call_upstream(
        name: &str,
        state: Arc<GatedCallUpstreamState>,
        call_timeout_secs: u64,
    ) -> UpstreamServer {
        connect_call_upstream(name, GatedCallUpstreamHandler { state }, call_timeout_secs).await
    }

    /// A duplex-connected, non-task-capable `UpstreamServer` served by
    /// `upstream`.
    async fn connect_call_upstream(
        name: &str,
        upstream: impl ServerHandler,
        call_timeout_secs: u64,
    ) -> UpstreamServer {
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = upstream
                .serve(server_transport)
                .await
                .expect("start gated call upstream test server");
            let _ = server.waiting().await;
        });

        let tools = Arc::new(arc_swap::ArcSwap::from_pointee(Vec::<Tool>::new()));
        let handler = Arc::new(UpstreamClientHandler::new_for_tests(
            Arc::from(name.to_string()),
            Arc::clone(&tools),
            crate::server::no_events(),
        ));
        let client = handler
            .serve(client_transport)
            .await
            .expect("connect gated call upstream test client");

        UpstreamServer {
            name: name.to_string(),
            config: gated_task_server_config(call_timeout_secs),
            client,
            tools,
            capabilities: ServerCapabilities::default(),
            upstream: None,
            protocol_era: crate::protocol::ProtocolEra::Legacy,
            selected_protocol_version: crate::protocol::SUPPORTED_PROTOCOL_VERSION.to_string(),
            protocol_gate_state: 0,
            connection: crate::server::ConnectionGeneration::new(),
            child_pid: None,
            stderr_tail: None,
        }
    }

    /// T4 (Part 3): the record vanishes between `send_cancellable_request`
    /// and `set_upstream_request` (owner teardown drained the store while
    /// the executor was parked on the store lock). No teardown path can
    /// ever cancel this request — the executor's Missing branch must send
    /// the explicit request-level cancel itself, release its concurrency
    /// permit, and return without inserting anything.
    #[tokio::test]
    async fn record_vanishing_between_send_and_record_cancels_upstream_request() {
        let state = GatedCallUpstreamState::new();
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
        sm.replace_server(
            "mock",
            connect_gated_call_upstream("mock", Arc::clone(&state), 5).await,
        )
        .await;
        sm.apply_live_settings("mock", &one_call_at_a_time());
        install_single_route(&router, "Mock__hang", "mock", "hang");

        let owner = crate::downstream_identity::http_session_task_owner("session-record-vanish");
        let created = router
            .enqueue_tool_task("Mock__hang", None, None, owner.clone(), None, None)
            .await
            .expect("enqueue local wrapper task");

        // Hold the store lock BEFORE the spawned executor can reach
        // `set_upstream_request`: the executor sends the upstream request,
        // then parks on this lock — a deterministic stand-in for teardown
        // winning the race to the store.
        let mut store = router.task_store.lock().await;
        assert!(
            yield_until(&state.call_entered).await,
            "upstream call_tool must have been entered"
        );
        assert!(
            store
                .upstream_for_owner(&owner, &created.task.task_id)
                .expect("record must still exist while the lock is held")
                .is_none(),
            "the upstream request must not be recorded yet — this test targets the gap"
        );

        // Drain the record WITHOUT aborting the executor (dropping a
        // JoinHandle detaches), exactly what the executor observes when
        // teardown ran in the gap.
        let drained = store.cleanup_owner(&owner);
        assert_eq!(drained.len(), 1, "exactly the one wrapper record");
        drop(drained);
        drop(store);

        // The executor resumes, sees Missing, and must cancel upstream.
        assert!(
            yield_until(&state.call_cancelled).await,
            "the executor must send an explicit request-level cancel when its record is gone"
        );

        // The permit must come back: the Missing branch returns instead of
        // holding the server's `max_concurrent` slot on a dead call.
        let semaphore = sm
            .call_slots("mock")
            .expect("mock semaphore installed above");
        let mut permit_released = false;
        for _ in 0..200 {
            if semaphore.available_permits() == 1 {
                permit_released = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            permit_released,
            "the Missing branch must release the per-server concurrency permit"
        );
        assert_eq!(
            router.task_count_for_owner(&owner).await,
            0,
            "nothing may be re-inserted for the torn-down owner"
        );
    }

    /// T5 (Part 3): teardown's phase-A `handle.abort()` lands in the
    /// send-to-record gap. The dropped executor future can no longer cancel
    /// anything itself — the armed `UpstreamRequestCancelGuard`'s Drop must
    /// fire the request-level cancel for the in-flight upstream call.
    #[tokio::test]
    async fn abort_between_send_and_record_fires_cancel_guard() {
        let state = GatedCallUpstreamState::new();
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
        sm.replace_server(
            "mock",
            connect_gated_call_upstream("mock", Arc::clone(&state), 5).await,
        )
        .await;
        sm.apply_live_settings("mock", &one_call_at_a_time());
        install_single_route(&router, "Mock__hang", "mock", "hang");

        let owner = crate::downstream_identity::http_session_task_owner("session-abort-in-gap");
        let created = router
            .enqueue_tool_task("Mock__hang", None, None, owner.clone(), None, None)
            .await
            .expect("enqueue local wrapper task");

        // Same deterministic parking as T4: the executor has sent the
        // request (call_tool entered), armed its guard, and is parked on
        // the store lock we hold — still pre-record.
        let mut store = router.task_store.lock().await;
        assert!(
            yield_until(&state.call_entered).await,
            "upstream call_tool must have been entered"
        );
        assert!(
            store
                .upstream_for_owner(&owner, &created.task.task_id)
                .expect("record must still exist while the lock is held")
                .is_none(),
            "the upstream request must not be recorded yet — this test targets the gap"
        );

        // Drain AND abort, exactly like `cleanup_tasks_for_owner` phase A.
        let drained = store.cleanup_owner(&owner);
        assert_eq!(drained.len(), 1, "exactly the one wrapper record");
        for (upstream, handle) in drained {
            assert!(
                upstream.is_none(),
                "pre-record teardown must observe no upstream ref — nothing for it to cancel"
            );
            if let Some(handle) = handle {
                handle.abort();
            }
        }
        drop(store);

        // The aborted executor is dropped at the lock await; only the armed
        // guard's Drop can cancel the in-flight upstream call now.
        assert!(
            yield_until(&state.call_cancelled).await,
            "the abort-cancel guard must fire a request-level cancel for the in-flight call"
        );

        // The permit died with the aborted future.
        let semaphore = sm
            .call_slots("mock")
            .expect("mock semaphore installed above");
        let mut permit_released = false;
        for _ in 0..200 {
            if semaphore.available_permits() == 1 {
                permit_released = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            permit_released,
            "aborting the executor must release the per-server concurrency permit"
        );
        assert_eq!(router.task_count_for_owner(&owner).await, 0);
    }

    fn one_call_at_a_time() -> crate::config::ServerConfig {
        crate::config::ServerConfig {
            max_concurrent: 1,
            ..Default::default()
        }
    }

    /// The task's record once it is no longer working.
    async fn settled_task(router: &Arc<ToolRouter>, owner: &TaskOwner, task_id: &str) -> Task {
        for _ in 0..500 {
            let task = router
                .get_task_info_for_owner(owner, task_id)
                .await
                .expect("task info")
                .task;
            if task.status != TaskStatus::Working {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("task {task_id} is still working");
    }

    #[tokio::test]
    async fn a_task_on_a_server_waiting_for_sign_in_fails_without_calling_it() {
        let state = GatedCallUpstreamState::new();
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
        sm.replace_server(
            "mock",
            connect_gated_call_upstream("mock", Arc::clone(&state), 5).await,
        )
        .await;
        sm.mark_auth_required("mock");
        install_single_route(&router, "Mock__hang", "mock", "hang");

        let owner = crate::downstream_identity::http_session_task_owner("session-sign-in");
        let created = router
            .enqueue_tool_task("Mock__hang", None, None, owner.clone(), None, None)
            .await
            .expect("enqueue");

        let task = settled_task(&router, &owner, &created.task.task_id).await;
        assert_eq!(task.status, TaskStatus::Failed);
        assert!(!state.call_entered.load(Ordering::SeqCst));
    }

    struct SkillLinkUpstream;

    impl ServerHandler for SkillLinkUpstream {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::default())
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, McpError> {
            Ok(CallToolResult::success(vec![ContentBlock::resource_link(
                rmcp::model::Resource::new("skill://refunds/SKILL.md", "SKILL.md"),
            )])
            .into())
        }
    }

    #[tokio::test]
    async fn a_task_result_names_its_server_in_skill_links_and_counts_as_a_call() {
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
        sm.replace_server(
            "mock",
            connect_call_upstream("mock", SkillLinkUpstream, 5).await,
        )
        .await;
        install_single_route(&router, "Mock__skill", "mock", "skill");

        let owner = crate::downstream_identity::http_session_task_owner("session-skill");
        let created = router
            .enqueue_tool_task("Mock__skill", None, None, owner.clone(), None, None)
            .await
            .expect("enqueue");
        let task = settled_task(&router, &owner, &created.task.task_id).await;
        assert_eq!(task.status, TaskStatus::Completed);

        let payload = router
            .get_task_result_for_owner(&owner, &created.task.task_id)
            .await
            .expect("task result");
        let result: CallToolResult = serde_json::from_value(payload.0).expect("tool result");
        let link = result.content[0]
            .as_resource_link()
            .expect("the skill link");
        assert_eq!(link.uri, "skill://mock/refunds/SKILL.md");
        assert_eq!(
            sm.metrics_snapshot("mock")
                .map(|metrics| metrics.call_count),
            Some(1)
        );
    }

    #[tokio::test]
    async fn a_task_whose_server_dies_with_the_call_is_not_sent_again() {
        let mut config = crate::config::Config::default();
        let mut mock = crate::test_helpers::mock_server("echo");
        mock.args
            .extend(["--fail-mode".to_string(), "crash".to_string()]);
        config.servers.insert("mock".to_string(), mock);
        let engine = crate::engine::Engine::new(config);
        engine.start().await;
        let router = Arc::clone(engine.tool_router());

        let owner = crate::downstream_identity::http_session_task_owner("session-crash");
        let created = router
            .enqueue_tool_task("Mock__echo", None, None, owner.clone(), None, None)
            .await
            .expect("enqueue");
        let task = settled_task(&router, &owner, &created.task.task_id).await;

        assert_eq!(task.status, TaskStatus::Failed);
        let message = task.status_message.unwrap_or_default();
        assert!(
            message.contains("may or may not have completed"),
            "unexpected status: {message}"
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn a_tasks_cursor_plug_never_gave_out_is_refused_not_restarted() {
        let router = Arc::new(ToolRouter::new(
            Arc::new(ServerManager::new()),
            RouterConfig::default(),
        ));
        let owner = TaskOwner::new(Arc::<str>::from("cursor-owner"));
        router
            .task_store
            .lock()
            .await
            .create(owner.clone(), "Mock__echo")
            .expect("task");
        let page = |cursor: &str| {
            let mut request = PaginatedRequestParams::default();
            request.cursor = Some(cursor.to_string());
            router.list_tasks_for_owner(&owner, Some(request))
        };

        let error = page("not-a-cursor").await.expect_err("an unknown cursor");
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
        // Past the end, as tasks expiring between pages leave it: the last
        // page, empty, not the first one again.
        let past_end = page("7").await.expect("a cursor past the end");
        assert!(past_end.tasks.is_empty());
        assert!(past_end.next_cursor.is_none());
    }
}
