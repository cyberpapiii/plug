use super::*;

use rmcp::service::{RequestHandle, RoleClient, ServiceError};

use crate::tasks::{OwnerLivenessProbe, UpstreamRecordOutcome, owner_closed_during_create_error};

/// RAII cover for the send-to-record gap in `execute_tool_task`: constructed
/// the moment `send_cancellable_request` returns (the upstream request is in
/// flight) and defused once the request id is safely recorded on the task
/// record — or once an explicit cancel has already been sent. If the
/// executing future is aborted while the guard is still armed (teardown's
/// phase-A `handle.abort()` landing between send and record), Drop fires a
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
                let send = peer.notify_cancelled(CancelledNotificationParam {
                    request_id,
                    reason: Some("task aborted before upstream request was recorded".to_string()),
                });
                if tokio::time::timeout(bound, send).await.is_err() {
                    tracing::warn!(
                        timeout_secs = bound.as_secs(),
                        "upstream cancellation notify timed out after task abort"
                    );
                }
            });
        }
    }
}

impl super::ToolRouter {
    pub fn task_owner_for_ipc_client(client_id: &str) -> TaskOwner {
        TaskOwner::new(Arc::<str>::from(format!("ipc:{client_id}")))
    }

    pub fn task_owner_for_http_session(session_id: &str) -> TaskOwner {
        TaskOwner::new(Arc::<str>::from(format!("http:{session_id}")))
    }

    pub async fn enqueue_tool_task(
        self: &Arc<Self>,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        progress_token: Option<ProgressToken>,
        owner: TaskOwner,
        owner_liveness: Option<OwnerLivenessProbe>,
        downstream: Option<DownstreamCallContext>,
    ) -> Result<CreateTaskResult, McpError> {
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
        // refusal as the worst case. stdio passes `None`: it has no teardown
        // path that calls task cleanup, so its owner is always live.
        if let Some(owner_is_live) = owner_liveness
            && !owner_is_live()
        {
            return Err(owner_closed_during_create_error());
        }

        if canonical_plug_meta_tool_name(tool_name).is_some() {
            return Err(McpError::from(ProtocolError::InvalidRequest {
                detail:
                    "plug meta-tools do not support task-wrapped calls; call the meta-tool directly"
                        .to_string(),
            }));
        }

        let cache = self.cache.load();
        let (server_id, original_name) = cache.routes.get(tool_name).cloned().ok_or_else(|| {
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

        if let Some(upstream) = self.server_manager.get_upstream(&server_id)
            && upstream
                .capabilities
                .tasks
                .as_ref()
                .is_some_and(|tasks| tasks.supports_tools_call())
        {
            let call_timeout = Duration::from_secs(upstream.config.call_timeout_secs);
            let peer = upstream.client.peer().clone();
            drop(upstream);

            let mut upstream_params = CallToolRequestParams::new(original_name.clone());
            if let Some(args) = arguments.clone() {
                upstream_params = upstream_params.with_arguments(args);
            }
            upstream_params.task = Some(serde_json::Map::new());
            if let Some(token) = progress_token.clone() {
                upstream_params.set_progress_token(token);
            }

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
                let _create_guard = create_guard;
                let deadline = tokio::time::Instant::now() + call_timeout;

                let request = ClientRequest::CallToolRequest(CallToolRequest::new(upstream_params));
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
                        return Err(match error {
                            ServiceError::McpError(mcp_err) => mcp_err,
                            other => McpError::internal_error(other.to_string(), None),
                        });
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
                        // Best-effort request-level cancel, bounded on its
                        // own: a wedged transport sink must not hang this
                        // detached task.
                        let cancel = request_peer.notify_cancelled(CancelledNotificationParam {
                            request_id: upstream_request_id.clone(),
                            reason: Some("task creation timed out".to_string()),
                        });
                        if tokio::time::timeout(call_timeout, cancel).await.is_err() {
                            tracing::warn!(
                                trace_id = %trace_id,
                                server = %server_id,
                                timeout_secs = call_timeout.as_secs(),
                                "request cancel notify timed out after native create timeout"
                            );
                        }
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
                        return Err(match error {
                            ServiceError::McpError(mcp_err) => mcp_err,
                            other => McpError::internal_error(other.to_string(), None),
                        });
                    }
                    Err(_recv_error) => {
                        return Err(McpError::internal_error(
                            ServiceError::TransportClosed.to_string(),
                            None,
                        ));
                    }
                };

                if let ServerResult::CreateTaskResult(result) = response {
                    tracing::info!(
                        trace_id = %trace_id,
                        server = %server_id,
                        tool = %original_name,
                        task_id = %result.task.task_id,
                        "proxy native upstream task created"
                    );
                    let created = router.task_store.lock().await.create_passthrough(
                        owner,
                        &tool_name,
                        &result.task,
                        TaskUpstreamRef::Task {
                            server_id: server_id.clone(),
                            task_id: result.task.task_id.clone(),
                        },
                    );
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
                    format!(
                        "upstream task-capable server returned unexpected response for task-wrapped tool call: {response:?}"
                    ),
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
        // record insert and the abort-handle attach. (Previously these were
        // three separate lock scopes; a `cleanup_owner` interleaving drained a
        // record whose handle was still `None`, `attach_abort_handle` then
        // silently no-opped on the missing record and dropped the JoinHandle —
        // detaching the future, which kept running and kept holding its
        // server's `max_concurrent` semaphore permit.) No await is held
        // across this scope other than acquiring the lock itself.
        let task = {
            let mut store = self.task_store.lock().await;
            let task = store.create(owner, tool_name)?;
            let task_id = task.task_id.clone();
            let router = Arc::clone(self);
            let tool_name = tool_name.to_string();
            let handle = tokio::spawn(async move {
                router
                    .execute_tool_task(
                        task_id,
                        tool_name,
                        arguments,
                        progress_token,
                        false,
                        trace_id,
                    )
                    .await;
            });
            store.attach_abort_handle(&task.task_id, handle);
            task
        };

        Ok(CreateTaskResult::new(task))
    }

    pub async fn list_tasks_for_owner(
        &self,
        owner: &TaskOwner,
        request: Option<PaginatedRequestParams>,
    ) -> Result<ListTasksResult, McpError> {
        Ok(self.task_store.lock().await.list_for_owner(owner, request))
    }

    pub async fn get_task_info_for_owner(
        &self,
        owner: &TaskOwner,
        task_id: &str,
    ) -> Result<GetTaskResult, McpError> {
        let upstream = {
            self.task_store
                .lock()
                .await
                .upstream_for_owner(owner, task_id)?
        };
        if let Some(TaskUpstreamRef::Task {
            server_id,
            task_id: upstream_task_id,
        }) = upstream
            && let Some(server) = self.server_manager.get_upstream(&server_id)
        {
            let response = server
                .client
                .peer()
                .send_request(ClientRequest::GetTaskInfoRequest(GetTaskInfoRequest::new(
                    GetTaskInfoParams {
                        meta: None,
                        task_id: upstream_task_id,
                    },
                )))
                .await
                .map_err(|error| match error {
                    rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                    other => McpError::internal_error(other.to_string(), None),
                })?;
            if let ServerResult::GetTaskResult(result) = response {
                let synced = self.task_store.lock().await.sync_from_upstream_for_owner(
                    owner,
                    task_id,
                    &result.task,
                )?;
                return Ok(GetTaskResult {
                    meta: None,
                    task: synced,
                });
            }
            return Err(McpError::internal_error(
                "unexpected upstream tasks/get response".to_string(),
                None,
            ));
        }
        self.task_store
            .lock()
            .await
            .get_info_for_owner(owner, task_id)
    }

    pub async fn get_task_result_for_owner(
        &self,
        owner: &TaskOwner,
        task_id: &str,
    ) -> Result<GetTaskPayloadResult, McpError> {
        let upstream = {
            self.task_store
                .lock()
                .await
                .upstream_for_owner(owner, task_id)?
        };
        if let Some(TaskUpstreamRef::Task {
            server_id,
            task_id: upstream_task_id,
        }) = upstream
            && let Some(server) = self.server_manager.get_upstream(&server_id)
        {
            let response = server
                .client
                .peer()
                .send_request(ClientRequest::GetTaskResultRequest(
                    GetTaskResultRequest::new(GetTaskResultParams {
                        meta: None,
                        task_id: upstream_task_id,
                    }),
                ))
                .await
                .map_err(|error| match error {
                    rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                    other => McpError::internal_error(other.to_string(), None),
                })?;
            return match response {
                ServerResult::GetTaskPayloadResult(result) => {
                    self.task_store.lock().await.cache_result_for_owner(
                        owner,
                        task_id,
                        result.0.clone(),
                    )?;
                    self.artifact_store
                        .maybe_spill_task_payload(&format!("task_result:{task_id}"), result.0)
                        .await
                }
                ServerResult::CallToolResult(result) => {
                    let payload = serde_json::to_value(result).map_err(|e| {
                        McpError::internal_error(
                            format!("failed to serialize upstream task payload: {e}"),
                            None,
                        )
                    })?;
                    self.task_store.lock().await.cache_result_for_owner(
                        owner,
                        task_id,
                        payload.clone(),
                    )?;
                    self.artifact_store
                        .maybe_spill_task_payload(&format!("task_result:{task_id}"), payload)
                        .await
                }
                _ => Err(McpError::internal_error(
                    "unexpected upstream tasks/result response".to_string(),
                    None,
                )),
            };
        }
        let payload = self
            .task_store
            .lock()
            .await
            .get_result_for_owner(owner, task_id)?;
        self.artifact_store
            .maybe_spill_task_payload(&format!("task_result:{task_id}"), payload.0)
            .await
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
                    let send = peer.send_request(ClientRequest::CancelTaskRequest(
                        CancelTaskRequest::new(CancelTaskParams {
                            meta: None,
                            task_id: upstream_task_id.clone(),
                        }),
                    ));
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
                TaskUpstreamRef::Request {
                    server_id,
                    request_id,
                } => {
                    let send = peer.notify_cancelled(CancelledNotificationParam {
                        request_id,
                        reason: Some("task owner disconnected".to_string()),
                    });
                    match tokio::time::timeout(bound, send).await {
                        Err(_) => tracing::warn!(
                            server = %server_id,
                            timeout_secs = bound.as_secs(),
                            "upstream cancellation notify timed out during task teardown"
                        ),
                        Ok(Err(error)) => tracing::debug!(
                            server = %server_id,
                            %error,
                            "upstream cancellation notify failed during task teardown"
                        ),
                        Ok(Ok(())) => {}
                    }
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
    /// rmcp caveat (verified against rmcp 1.7.0 `service.rs`): once our
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
                Ok(Ok(Ok(ServerResult::CreateTaskResult(result)))) => {
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
                // A late non-task response created nothing to cancel.
                Ok(Ok(Ok(_other))) => {}
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

    /// Count of task records currently stored for `owner`. Tasks are
    /// owner-scoped so this cannot observe another owner's records; it
    /// exists as a test probe for teardown-cleanup assertions.
    pub async fn task_count_for_owner(&self, owner: &TaskOwner) -> usize {
        self.task_store
            .lock()
            .await
            .list_for_owner(owner, None)
            .tasks
            .len()
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
    ) -> Task {
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
        let (task, upstream, handle) = self
            .task_store
            .lock()
            .await
            .mark_cancelled(owner, task_id)?;
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
                            server
                                .client
                                .peer()
                                .send_request(ClientRequest::CancelTaskRequest(
                                    CancelTaskRequest::new(CancelTaskParams {
                                        meta: None,
                                        task_id: upstream_task_id,
                                    }),
                                )),
                        )
                        .await;
                        match response {
                            Ok(Ok(ServerResult::CancelTaskResult(result))) => {
                                let synced = self
                                    .task_store
                                    .lock()
                                    .await
                                    .sync_from_upstream_for_owner(owner, task_id, &result.task)?;
                                return Ok(CancelTaskResult {
                                    meta: None,
                                    task: synced,
                                });
                            }
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
                        let notify =
                            server
                                .client
                                .peer()
                                .notify_cancelled(CancelledNotificationParam {
                                    request_id,
                                    reason: Some("task cancelled".to_string()),
                                });
                        if tokio::time::timeout(bound, notify).await.is_err() {
                            tracing::debug!(
                                server = %server_id,
                                task_id = %task_id,
                                timeout_secs = bound.as_secs(),
                                "upstream cancellation notify timed out during task cancel"
                            );
                        }
                    }
                }
            }
        }
        if let Some(handle) = handle {
            handle.abort();
        }
        Ok(CancelTaskResult { meta: None, task })
    }

    async fn execute_tool_task(
        self: Arc<Self>,
        task_id: String,
        tool_name: String,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        progress_token: Option<ProgressToken>,
        is_retry: bool,
        trace_id: Arc<str>,
    ) {
        let cache = self.cache.load();
        let (server_id, original_name) = match cache.routes.get(tool_name.as_str()).or_else(|| {
            cache
                .routes
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(tool_name.as_str()))
                .map(|(_, v)| v)
        }) {
            Some(route) => route.clone(),
            None => {
                drop(cache);
                self.task_store
                    .lock()
                    .await
                    .fail(&task_id, format!("tool not found: {tool_name}"));
                return;
            }
        };
        drop(cache);

        let mut current_arguments = arguments;
        let mut allow_retry = !is_retry;

        loop {
            let call_id = next_call_id();
            tracing::info!(
                call_id,
                trace_id = %trace_id,
                task_id = %task_id,
                server = %server_id,
                tool = %original_name,
                retry = is_retry,
                "proxy background task tool call started"
            );
            let semaphore_timeout = Duration::from_secs(1);
            let permit = if let Some(sem) = self.server_manager.semaphores.get(&server_id) {
                match tokio::time::timeout(semaphore_timeout, sem.clone().acquire_owned()).await {
                    Ok(Ok(permit)) => Some(permit),
                    Ok(Err(_)) => {
                        self.task_store
                            .lock()
                            .await
                            .fail(&task_id, format!("server unavailable: {server_id}"));
                        return;
                    }
                    Err(_) => {
                        self.task_store
                            .lock()
                            .await
                            .fail(&task_id, format!("server busy: {server_id}"));
                        return;
                    }
                }
            } else {
                None
            };

            let Some(upstream) = self.server_manager.get_upstream(&server_id) else {
                self.task_store
                    .lock()
                    .await
                    .fail(&task_id, format!("server unavailable: {server_id}"));
                return;
            };

            let timeout_duration = Duration::from_secs(upstream.config.call_timeout_secs);
            let transport_type = upstream.config.transport.clone();
            let peer = upstream.client.peer().clone();
            drop(upstream);

            let upstream_progress_token = progress_token.as_ref().map(|_| {
                ProgressToken(NumberOrString::String(Arc::from(format!(
                    "plug-task-progress-{call_id}"
                ))))
            });
            let retry_arguments = current_arguments.clone();
            let mut upstream_params = CallToolRequestParams::new(original_name.clone());
            if let Some(args) = current_arguments.take() {
                upstream_params = upstream_params.with_arguments(args);
            }
            if let Some(token) = upstream_progress_token.clone() {
                upstream_params.set_progress_token(token);
            }
            let request = ClientRequest::CallToolRequest(CallToolRequest::new(upstream_params));
            // No `options.timeout` on purpose: rmcp's own timeout machinery
            // in `await_response` auto-cancels via an UNBOUNDED
            // `send_notification`, so a wedged sink would hang this task
            // forever. The bound lives in our own `tokio::time::timeout`
            // below, paired with a bounded explicit cancel.
            let mut options = PeerRequestOptions::default();
            options.meta = upstream_progress_token
                .clone()
                .map(Meta::with_progress_token);

            let request_handle = match peer.send_cancellable_request(request, options).await {
                Ok(handle) => handle,
                Err(error) => {
                    drop(permit);
                    self.task_store
                        .lock()
                        .await
                        .fail(&task_id, error.to_string());
                    return;
                }
            };
            let RequestHandle {
                mut rx,
                id: upstream_request_id,
                peer: request_peer,
                ..
            } = request_handle;

            // Covers the send-to-record gap: the request is in flight, but
            // until `set_upstream_request` lands below, no store record knows
            // its id — an abort (teardown's phase-A `handle.abort()`) landing
            // in that gap would otherwise leave the upstream call running
            // with nothing left to cancel it. While armed, the guard's Drop
            // fires a bounded request-level cancel.
            let mut abort_guard = UpstreamRequestCancelGuard::new(
                request_peer.clone(),
                upstream_request_id.clone(),
                timeout_duration,
            );

            let recorded = self.task_store.lock().await.set_upstream_request(
                &task_id,
                TaskUpstreamRef::Request {
                    server_id: server_id.clone(),
                    request_id: upstream_request_id.clone(),
                },
            );

            match recorded {
                UpstreamRecordOutcome::Recorded { pending_cancel } => {
                    // The record now owns the upstream ref: teardown/cancel
                    // paths drain it and forward cancellation themselves, so
                    // the send-gap guard stands down.
                    abort_guard.defuse();

                    // A cancel arrived before the upstream request id was
                    // recorded above (see `mark_cancelled` /
                    // `set_upstream_request` on `TaskStore`) — replay it now
                    // so the upstream stops running the call instead of
                    // completing it for a result nobody wants.
                    if let Some(reason) = pending_cancel {
                        let _ = request_peer
                            .notify_cancelled(CancelledNotificationParam {
                                request_id: upstream_request_id.clone(),
                                reason: Some(reason),
                            })
                            .await;
                    }
                }
                UpstreamRecordOutcome::Missing => {
                    // The owner was torn down between send and record:
                    // cleanup drained the store before this record held an
                    // upstream ref, so no teardown path will ever cancel this
                    // request — send the explicit bounded cancel ourselves.
                    // The guard stays ARMED until that cancel resolves: an
                    // abort landing mid-cancel still fires the Drop path — a
                    // rare double-cancel notification is harmless, a missed
                    // cancel is not.
                    tracing::info!(
                        call_id,
                        trace_id = %trace_id,
                        task_id = %task_id,
                        server = %server_id,
                        "task record gone before upstream request was recorded; cancelling upstream request"
                    );
                    let cancel = request_peer.notify_cancelled(CancelledNotificationParam {
                        request_id: upstream_request_id.clone(),
                        reason: Some("task owner disconnected".to_string()),
                    });
                    if tokio::time::timeout(timeout_duration, cancel)
                        .await
                        .is_err()
                    {
                        tracing::warn!(
                            call_id,
                            task_id = %task_id,
                            server = %server_id,
                            timeout_secs = timeout_duration.as_secs(),
                            "upstream cancellation notify timed out for recordless task"
                        );
                    }
                    abort_guard.defuse();
                    drop(permit);
                    return;
                }
            }

            // Own the response await + timeout: map the oneshot exactly as
            // rmcp's `await_response` does, and on timeout send a bounded
            // explicit request cancel before surfacing the same
            // `ServiceError::Timeout` the old (rmcp-managed) path produced —
            // callers match on that error the same way they always did.
            let result = match tokio::time::timeout(timeout_duration, &mut rx).await {
                Ok(Ok(inner)) => inner,
                Ok(Err(_recv_error)) => Err(ServiceError::TransportClosed),
                Err(_elapsed) => {
                    let cancel = request_peer.notify_cancelled(CancelledNotificationParam {
                        request_id: upstream_request_id.clone(),
                        reason: Some(
                            RequestHandle::<RoleClient>::REQUEST_TIMEOUT_REASON.to_string(),
                        ),
                    });
                    if tokio::time::timeout(timeout_duration, cancel)
                        .await
                        .is_err()
                    {
                        tracing::warn!(
                            call_id,
                            task_id = %task_id,
                            server = %server_id,
                            timeout_secs = timeout_duration.as_secs(),
                            "upstream cancellation notify timed out after task call timeout"
                        );
                    }
                    Err(ServiceError::Timeout {
                        timeout: timeout_duration,
                    })
                }
            };
            drop(permit);

            match result {
                Ok(ServerResult::CallToolResult(response)) => {
                    tracing::info!(
                        call_id,
                        trace_id = %trace_id,
                        task_id = %task_id,
                        server = %server_id,
                        tool = %original_name,
                        "proxy background task tool call completed"
                    );
                    match serde_json::to_value(&response) {
                        Ok(payload) => self.task_store.lock().await.complete(&task_id, payload),
                        Err(error) => self.task_store.lock().await.fail(
                            &task_id,
                            format!("failed to serialize task result: {error}"),
                        ),
                    }
                    return;
                }
                Err(e) if is_session_error(&e) && allow_retry => {
                    if self.reconnect_server_now(&server_id).await.is_ok() {
                        tracing::info!(
                            call_id,
                            trace_id = %trace_id,
                            task_id = %task_id,
                            server = %server_id,
                            "reconnected, retrying background task tool call"
                        );
                        current_arguments = retry_arguments;
                        allow_retry = false;
                        continue;
                    }
                    self.task_store.lock().await.fail(&task_id, e.to_string());
                    return;
                }
                Err(e) => {
                    if matches!(transport_type, crate::config::TransportType::Stdio) {
                        self.reconnect_server_in_background(server_id.clone());
                    }
                    self.task_store.lock().await.fail(&task_id, e.to_string());
                    return;
                }
                Ok(other) => {
                    self.task_store.lock().await.fail(
                        &task_id,
                        format!("unexpected upstream task response: {other:?}"),
                    );
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use rmcp::model::PingRequest;

    fn test_router_config() -> RouterConfig {
        RouterConfig {
            prefix_delimiter: "__".to_string(),
            priority_tools: Vec::new(),
            disabled_tools: Vec::new(),
            tool_description_max_chars: None,
            tool_search_threshold: 50,
            meta_tool_mode: false,
            lazy_tools: LazyToolsConfig::default(),
            tool_filter_enabled: true,
            enrichment_servers: std::collections::HashSet::new(),
        }
    }

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
        let router = ToolRouter::new(sm, test_router_config());

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
        let owner = ToolRouter::task_owner_for_http_session("session-abort-http");
        assert_owner_teardown_aborts_long_running_local_task(owner).await;
    }

    #[tokio::test]
    async fn cleanup_tasks_for_owner_aborts_long_running_local_task_for_ipc_client() {
        // IPC/daemon disconnect and HTTP session teardown both funnel
        // through the same `cleanup_tasks_for_owner` — the owner key is the
        // only thing that differs (`ipc:<client_id>` vs `http:<session_id>`)
        // — so this is the shared-layer proof for the daemon teardown path.
        let owner = ToolRouter::task_owner_for_ipc_client("client-abort-ipc");
        assert_owner_teardown_aborts_long_running_local_task(owner).await;
    }

    #[tokio::test]
    async fn cleanup_tasks_for_owner_ignores_other_owners_running_tasks() {
        let sm = Arc::new(ServerManager::new());
        let router = ToolRouter::new(sm, test_router_config());
        let owner_a = ToolRouter::task_owner_for_http_session("session-a");
        let owner_b = ToolRouter::task_owner_for_http_session("session-b");

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

    use crate::server::{UpstreamClientHandler, UpstreamServer};
    use crate::types::ServerHealth;

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
        hang_cancel: bool,
        cancel_log: std::sync::Mutex<Vec<String>>,
    }

    impl GatedTaskUpstreamState {
        fn new(hang_cancel: bool, enqueue_gate_open: bool) -> Arc<Self> {
            Arc::new(Self {
                enqueue_gate: TestGate::new(enqueue_gate_open),
                enqueue_entered: AtomicBool::new(false),
                enqueue_cancelled: AtomicBool::new(false),
                hang_cancel,
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
    }

    struct GatedTaskUpstreamHandler {
        state: Arc<GatedTaskUpstreamState>,
    }

    impl ServerHandler for GatedTaskUpstreamHandler {
        fn get_info(&self) -> ServerInfo {
            let mut capabilities = ServerCapabilities::default();
            capabilities.tasks = Some(TasksCapability::server_default());
            ServerInfo::new(capabilities)
        }

        fn enqueue_task(
            &self,
            _request: CallToolRequestParams,
            context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<CreateTaskResult, McpError>> + Send + '_ {
            let state = Arc::clone(&self.state);
            async move {
                state.enqueue_entered.store(true, Ordering::SeqCst);
                tokio::select! {
                    // Request-level cancel from the proxy: no task was
                    // created, so there is nothing to reap — return an error
                    // (the proxy's responder is already resolved with
                    // `Cancelled`; the response is dropped on arrival).
                    _ = context.ct.cancelled() => {
                        state.enqueue_cancelled.store(true, Ordering::SeqCst);
                        return Err(McpError::internal_error(
                            "task-wrapped call cancelled by client",
                            None,
                        ));
                    }
                    _ = state.enqueue_gate.wait() => {}
                }
                let now = rmcp::task_manager::current_timestamp();
                Ok(CreateTaskResult::new(Task::new(
                    "upstream-task-1".to_string(),
                    TaskStatus::Working,
                    now.clone(),
                    now,
                )))
            }
        }

        fn cancel_task(
            &self,
            request: CancelTaskParams,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<CancelTaskResult, McpError>> + Send + '_ {
            let state = Arc::clone(&self.state);
            async move {
                state
                    .cancel_log
                    .lock()
                    .unwrap()
                    .push(request.task_id.clone());
                if state.hang_cancel {
                    // Simulates an unresponsive upstream: the request was
                    // received but is never answered, so the proxy side's
                    // `send_request` would await its response forever
                    // without the per-upstream bound.
                    std::future::pending::<()>().await;
                }
                let now = rmcp::task_manager::current_timestamp();
                Ok(CancelTaskResult {
                    meta: None,
                    task: Task::new(request.task_id, TaskStatus::Cancelled, now.clone(), now),
                })
            }
        }
    }

    fn gated_task_server_config(call_timeout_secs: u64) -> crate::config::ServerConfig {
        crate::config::ServerConfig {
            command: Some("fake".to_string()),
            args: Vec::new(),
            env: HashMap::new(),
            enabled: true,
            transport: crate::config::TransportType::Stdio,
            url: None,
            auth_token: None,
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
            timeout_secs: 30,
            call_timeout_secs,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: false,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
            sandbox: None,
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
            let server = GatedTaskUpstreamHandler { state }
                .serve(server_transport)
                .await
                .expect("start gated task upstream test server");
            let _ = server.waiting().await;
        });

        let tools = Arc::new(arc_swap::ArcSwap::from_pointee(Vec::<Tool>::new()));
        let handler = Arc::new(UpstreamClientHandler::new_for_tests(
            Arc::from(name.to_string()),
            Arc::clone(&tools),
            std::sync::Weak::new(),
        ));
        let client = handler
            .serve(client_transport)
            .await
            .expect("connect gated task upstream test client");

        let mut capabilities = ServerCapabilities::default();
        capabilities.tasks = Some(TasksCapability::server_default());

        UpstreamServer {
            name: name.to_string(),
            config: gated_task_server_config(call_timeout_secs),
            client,
            tools,
            capabilities,
            upstream: None,
            health: ServerHealth::Healthy,
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
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), test_router_config()));
        sm.replace_server(
            "hung",
            connect_gated_task_upstream("hung", Arc::clone(&state), 5).await,
        )
        .await;

        let owner = ToolRouter::task_owner_for_http_session("session-hung-teardown");

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

    /// Defect-1 companion for `cancel_task_for_owner`: the forwarding arm
    /// gets the same per-upstream bound, and on timeout the caller still
    /// gets the locally cancelled task instead of hanging forever.
    #[tokio::test(start_paused = true)]
    async fn cancel_task_for_owner_is_bounded_when_upstream_cancel_hangs() {
        let state = GatedTaskUpstreamState::new(true, true);
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), test_router_config()));
        sm.replace_server(
            "hung",
            connect_gated_task_upstream("hung", Arc::clone(&state), 5).await,
        )
        .await;

        let owner = ToolRouter::task_owner_for_http_session("session-hung-cancel");
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
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), test_router_config()));
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
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
            tool_risk_inventory: HashMap::new(),
        });

        let owner = ToolRouter::task_owner_for_http_session("session-native-race");
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
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), test_router_config()));
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
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
            tool_risk_inventory: HashMap::new(),
        });

        let owner = ToolRouter::task_owner_for_http_session("session-full-peer-queue");
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
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
            tool_risk_inventory: HashMap::new(),
        });
    }

    /// T1 (Part 1): an owner-liveness probe reporting the owner closed must
    /// refuse the create outright — before route lookup, before any
    /// upstream traffic — leave no record, release the lifecycle-ledger
    /// entry (guard at zero), and leave no tombstone behind.
    #[tokio::test]
    async fn enqueue_refuses_when_owner_liveness_probe_reports_closed() {
        let sm = Arc::new(ServerManager::new());
        let router = Arc::new(ToolRouter::new(sm, test_router_config()));
        let owner = ToolRouter::task_owner_for_http_session("session-already-gone");

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
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), test_router_config()));
        sm.replace_server(
            "mock",
            connect_gated_task_upstream("mock", Arc::clone(&state), 2).await,
        )
        .await;
        install_single_route(&router, "Mock__hang", "mock", "hang");

        let owner = ToolRouter::task_owner_for_http_session("session-hung-native-create");
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
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), test_router_config()));
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
        tx.send(Ok(ServerResult::CreateTaskResult(CreateTaskResult::new(
            upstream_working_task("late-task"),
        ))))
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
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), test_router_config()));
        sm.replace_server(
            "mock",
            connect_gated_task_upstream("mock", Arc::clone(&state), 5).await,
        )
        .await;
        install_single_route(&router, "Mock__hang", "mock", "hang");

        let owner = ToolRouter::task_owner_for_http_session("session-caller-drop");
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
        ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
            let state = Arc::clone(&self.state);
            async move {
                state.call_entered.store(true, Ordering::SeqCst);
                tokio::select! {
                    _ = context.ct.cancelled() => {
                        state.call_cancelled.store(true, Ordering::SeqCst);
                        Err(McpError::internal_error("call cancelled by client", None))
                    }
                    _ = state.call_gate.wait() => {
                        Ok(CallToolResult::success(vec![Content::text("done")]))
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
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = GatedCallUpstreamHandler { state }
                .serve(server_transport)
                .await
                .expect("start gated call upstream test server");
            let _ = server.waiting().await;
        });

        let tools = Arc::new(arc_swap::ArcSwap::from_pointee(Vec::<Tool>::new()));
        let handler = Arc::new(UpstreamClientHandler::new_for_tests(
            Arc::from(name.to_string()),
            Arc::clone(&tools),
            std::sync::Weak::new(),
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
            health: ServerHealth::Healthy,
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
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), test_router_config()));
        sm.replace_server(
            "mock",
            connect_gated_call_upstream("mock", Arc::clone(&state), 5).await,
        )
        .await;
        // `replace_server` does not build the per-server semaphore; install
        // one so this test can prove the Missing branch releases its permit.
        sm.semaphores
            .insert("mock".to_string(), Arc::new(tokio::sync::Semaphore::new(1)));
        install_single_route(&router, "Mock__hang", "mock", "hang");

        let owner = ToolRouter::task_owner_for_http_session("session-record-vanish");
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
            .semaphores
            .get("mock")
            .map(|entry| Arc::clone(entry.value()))
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
        let router = Arc::new(ToolRouter::new(Arc::clone(&sm), test_router_config()));
        sm.replace_server(
            "mock",
            connect_gated_call_upstream("mock", Arc::clone(&state), 5).await,
        )
        .await;
        sm.semaphores
            .insert("mock".to_string(), Arc::new(tokio::sync::Semaphore::new(1)));
        install_single_route(&router, "Mock__hang", "mock", "hang");

        let owner = ToolRouter::task_owner_for_http_session("session-abort-in-gap");
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
            .semaphores
            .get("mock")
            .map(|entry| Arc::clone(entry.value()))
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
}
