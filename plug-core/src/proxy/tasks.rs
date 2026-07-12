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
        hang_cancel: bool,
        cancel_log: std::sync::Mutex<Vec<String>>,
    }

    impl GatedTaskUpstreamState {
        fn new(hang_cancel: bool, enqueue_gate_open: bool) -> Arc<Self> {
            Arc::new(Self {
                enqueue_gate: TestGate::new(enqueue_gate_open),
                enqueue_entered: AtomicBool::new(false),
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
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<CreateTaskResult, McpError>> + Send + '_ {
            let state = Arc::clone(&self.state);
            async move {
                state.enqueue_entered.store(true, Ordering::SeqCst);
                state.enqueue_gate.wait().await;
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
                .enqueue_tool_task("Mock__hang", None, None, enqueue_owner, None)
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
}
