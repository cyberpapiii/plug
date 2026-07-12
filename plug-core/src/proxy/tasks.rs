use super::*;

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
        downstream: Option<DownstreamCallContext>,
    ) -> Result<CreateTaskResult, McpError> {
        // Owner-lifecycle guard, registered before anything else: a teardown
        // (`cleanup_tasks_for_owner`) that runs while this enqueue is in
        // flight — most importantly across the native path's upstream round
        // trip below — tombstones the owner, so the late
        // `create`/`create_passthrough` refuses to insert a record for the
        // already-torn-down owner instead of leaving an untracked Working
        // record (and, on the native path, an upstream task nobody cancels).
        //
        // Known residual, not fixable at this layer: a teardown that fully
        // COMPLETED before this statement ran leaves no tombstone, so an
        // enqueue racing in after it still inserts a record. That record is
        // bounded — pruned by the stale-in-flight TTL — and its local
        // execution is bounded by the upstream's `call_timeout_secs`.
        let _create_guard = self.task_store.lock().await.begin_owner_create(&owner);

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
            let mut upstream_params = CallToolRequestParams::new(original_name.clone());
            if let Some(args) = arguments.clone() {
                upstream_params = upstream_params.with_arguments(args);
            }
            upstream_params.task = Some(serde_json::Map::new());
            if let Some(token) = progress_token.clone() {
                upstream_params.set_progress_token(token);
            }
            let response = upstream
                .client
                .peer()
                .send_request(ClientRequest::CallToolRequest(CallToolRequest::new(
                    upstream_params,
                )))
                .await
                .map_err(|error| match error {
                    rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                    other => McpError::internal_error(other.to_string(), None),
                })?;

            if let ServerResult::CreateTaskResult(result) = response {
                tracing::info!(
                    trace_id = %trace_id,
                    server = %server_id,
                    tool = %original_name,
                    task_id = %result.task.task_id,
                    "proxy native upstream task created"
                );
                let created = self.task_store.lock().await.create_passthrough(
                    owner,
                    tool_name,
                    &result.task,
                    TaskUpstreamRef::Task {
                        server_id: server_id.clone(),
                        task_id: result.task.task_id.clone(),
                    },
                );
                return match created {
                    Ok(task) => Ok(CreateTaskResult::new(task)),
                    Err(error) => {
                        // The owner was torn down during the upstream round
                        // trip: the native task exists upstream but nothing
                        // tracks it locally anymore. Best-effort bounded
                        // cancel so the upstream stops instead of running to
                        // completion for a result nobody will ever collect.
                        tracing::info!(
                            trace_id = %trace_id,
                            server = %server_id,
                            task_id = %result.task.task_id,
                            "owner torn down during native task creation; cancelling upstream task"
                        );
                        if let Some(cancellation) =
                            self.spawn_bounded_upstream_cancellation(TaskUpstreamRef::Task {
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

            return Err(McpError::internal_error(
                format!(
                    "upstream task-capable server returned unexpected response for task-wrapped tool call: {response:?}"
                ),
                None,
            ));
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
                            server.client.peer().send_request(
                                ClientRequest::CancelTaskRequest(CancelTaskRequest::new(
                                    CancelTaskParams {
                                        meta: None,
                                        task_id: upstream_task_id,
                                    },
                                )),
                            ),
                        )
                        .await;
                        match response {
                            Ok(Ok(ServerResult::CancelTaskResult(result))) => {
                                let synced =
                                    self.task_store.lock().await.sync_from_upstream_for_owner(
                                        owner,
                                        task_id,
                                        &result.task,
                                    )?;
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
            let mut options = PeerRequestOptions::default();
            options.timeout = Some(timeout_duration);
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

            let pending_cancel_reason = self.task_store.lock().await.set_upstream_request(
                &task_id,
                TaskUpstreamRef::Request {
                    server_id: server_id.clone(),
                    request_id: request_handle.id.clone(),
                },
            );

            // A cancel arrived before the upstream request id was recorded
            // above (see `mark_cancelled` / `set_upstream_request` on
            // `TaskStore`) — replay it now so the upstream stops running the
            // call instead of completing it for a result nobody wants.
            if let Some(reason) = pending_cancel_reason {
                let _ = peer
                    .notify_cancelled(CancelledNotificationParam {
                        request_id: request_handle.id.clone(),
                        reason: Some(reason),
                    })
                    .await;
            }

            let result = request_handle.await_response().await;
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
}
