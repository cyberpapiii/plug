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
                let task = self.task_store.lock().await.create_passthrough(
                    owner,
                    tool_name,
                    &result.task,
                    TaskUpstreamRef::Task {
                        server_id,
                        task_id: result.task.task_id.clone(),
                    },
                );
                return Ok(CreateTaskResult::new(task));
            }

            return Err(McpError::internal_error(
                format!(
                    "upstream task-capable server returned unexpected response for task-wrapped tool call: {response:?}"
                ),
                None,
            ));
        }

        let task = {
            let mut store = self.task_store.lock().await;
            store.create(owner, tool_name)
        };

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

        self.task_store
            .lock()
            .await
            .attach_abort_handle(&task.task_id, handle);

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

    pub async fn cleanup_tasks_for_owner(&self, owner: &TaskOwner) {
        self.task_store.lock().await.cleanup_owner(owner);
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
                        let response = server
                            .client
                            .peer()
                            .send_request(ClientRequest::CancelTaskRequest(CancelTaskRequest::new(
                                CancelTaskParams {
                                    meta: None,
                                    task_id: upstream_task_id,
                                },
                            )))
                            .await;
                        if let Ok(ServerResult::CancelTaskResult(result)) = response {
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
                    }
                }
                TaskUpstreamRef::Request {
                    server_id,
                    request_id,
                } => {
                    if let Some(server) = self.server_manager.get_upstream(&server_id) {
                        let _ = server
                            .client
                            .peer()
                            .notify_cancelled(CancelledNotificationParam {
                                request_id,
                                reason: Some("task cancelled".to_string()),
                            })
                            .await;
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
