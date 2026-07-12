use super::*;

struct StdioDownstreamContext {
    client_id: Arc<str>,
    request_id: RequestId,
    client_type: ClientType,
}

impl crate::dispatch::DownstreamContext for StdioDownstreamContext {
    fn downstream_call_context(&self) -> DownstreamCallContext {
        DownstreamCallContext::stdio_for_client(
            Arc::clone(&self.client_id),
            self.request_id.clone(),
            self.client_type,
        )
    }

    /// stdio's `tools/call` handler can only return a `CallToolResult`, so a
    /// task-augmented call falls through to the synchronous path (preserving
    /// today's "task param ignored on stdio" behavior).
    fn supports_tasks(&self) -> bool {
        false
    }

    fn task_owner(&self) -> Result<TaskOwner, McpError> {
        // Never reached while `supports_tasks()` is false; provided for completeness.
        Ok(TaskOwner::new(Arc::<str>::from(
            format!("stdio:{}", self.client_id).as_str(),
        )))
    }
}

/// Stdio-specific bridge for forwarding reverse requests (elicitation, sampling)
/// back to the downstream client via its `Peer<RoleServer>`.
struct StdioBridge {
    peer: Peer<RoleServer>,
    capabilities: ClientCapabilities,
}

impl DownstreamBridge for StdioBridge {
    fn create_elicitation(
        &self,
        request: CreateElicitationRequestParams,
    ) -> Pin<Box<dyn Future<Output = Result<CreateElicitationResult, McpError>> + Send + '_>> {
        if self.capabilities.elicitation.is_none() {
            return Box::pin(async {
                Err(McpError::internal_error(
                    "client does not support elicitation".to_string(),
                    None,
                ))
            });
        }
        let peer = self.peer.clone();
        Box::pin(async move {
            peer.create_elicitation(request)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))
        })
    }

    fn create_message(
        &self,
        request: CreateMessageRequestParams,
    ) -> Pin<Box<dyn Future<Output = Result<CreateMessageResult, McpError>> + Send + '_>> {
        if self.capabilities.sampling.is_none() {
            return Box::pin(async {
                Err(McpError::internal_error(
                    "client does not support sampling".to_string(),
                    None,
                ))
            });
        }
        let peer = self.peer.clone();
        Box::pin(async move {
            peer.create_message(request)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))
        })
    }
}

/// MCP proxy handler that aggregates tools from multiple upstream servers
/// and routes tool calls to the correct upstream. Used for stdio transport.
pub struct ProxyHandler {
    router: Arc<ToolRouter>,
    client_type: std::sync::RwLock<ClientType>,
    client_id: Arc<str>,
    notification_task_started: AtomicBool,
    /// Cancelled on drop to signal the notification fan-out task to exit.
    shutdown: CancellationToken,
    /// Peer reference for reverse requests (roots queries).
    downstream_peer: std::sync::OnceLock<Peer<RoleServer>>,
    /// Whether the downstream client advertises roots capability.
    roots_supported: AtomicBool,
    /// Client capabilities from initialize handshake, for bridge capability gating.
    client_capabilities: std::sync::RwLock<ClientCapabilities>,
}

impl Drop for ProxyHandler {
    fn drop(&mut self) {
        self.shutdown.cancel();
        let session_key =
            ToolRouter::lazy_session_key(DownstreamTransport::Stdio, self.client_id.as_ref());
        self.router.clear_lazy_session(&session_key);
        self.router
            .unregister_downstream_bridge(&NotificationTarget::Stdio {
                client_id: Arc::clone(&self.client_id),
            });
    }
}

impl ProxyHandler {
    pub fn new(server_manager: Arc<ServerManager>, config: RouterConfig) -> Self {
        Self {
            router: Arc::new(ToolRouter::new(server_manager, config)),
            client_type: std::sync::RwLock::new(ClientType::Unknown),
            client_id: Arc::from(uuid::Uuid::new_v4().to_string()),
            notification_task_started: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
            downstream_peer: std::sync::OnceLock::new(),
            roots_supported: AtomicBool::new(false),
            client_capabilities: std::sync::RwLock::new(ClientCapabilities::default()),
        }
    }

    /// Create a ProxyHandler from an existing shared ToolRouter.
    pub fn from_router(router: Arc<ToolRouter>) -> Self {
        Self {
            router,
            client_type: std::sync::RwLock::new(ClientType::Unknown),
            client_id: Arc::from(uuid::Uuid::new_v4().to_string()),
            notification_task_started: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
            downstream_peer: std::sync::OnceLock::new(),
            roots_supported: AtomicBool::new(false),
            client_capabilities: std::sync::RwLock::new(ClientCapabilities::default()),
        }
    }

    /// Refresh the merged tool list and routing table from all upstream servers.
    pub async fn refresh_tools(&self) {
        self.router.refresh_tools().await;
    }

    /// Get a reference to the underlying ToolRouter.
    pub fn router(&self) -> &Arc<ToolRouter> {
        &self.router
    }

    #[cfg(test)]
    pub(crate) fn client_id(&self) -> Arc<str> {
        Arc::clone(&self.client_id)
    }
}

#[allow(clippy::manual_async_fn)]
impl ServerHandler for ProxyHandler {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(self.router.synthesized_capabilities())
            .with_server_info(plug_implementation())
            .with_protocol_version(
                serde_json::from_value(serde_json::Value::String(
                    LATEST_PROTOCOL_VERSION.to_string(),
                ))
                .expect("latest protocol version must parse"),
            )
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.router.get_tool_definition(name)
    }

    fn enqueue_task(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CreateTaskResult, McpError>> + Send + '_ {
        let owner = TaskOwner::new(Arc::<str>::from(format!("stdio:{}", self.client_id)));
        let router = Arc::clone(&self.router);
        let progress_token = request.progress_token();
        let tool_name = request.name.to_string();
        let arguments = request.arguments;
        let client_type = self
            .client_type
            .read()
            .map(|ct| *ct)
            .unwrap_or(ClientType::Unknown);
        let downstream = DownstreamCallContext::stdio_for_client(
            Arc::clone(&self.client_id),
            context.id.clone(),
            client_type,
        );
        async move {
            router
                .enqueue_tool_task(
                    &tool_name,
                    arguments,
                    progress_token,
                    owner,
                    Some(downstream),
                )
                .await
        }
    }

    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, McpError>> + Send + '_ {
        async move {
            let client_type = detect_client(&request.client_info.name);
            tracing::info!(
                client = %request.client_info.name,
                detected = %client_type,
                "client connected"
            );

            // Store client type for list_tools filtering
            match self.client_type.write() {
                Ok(mut ct) => *ct = client_type,
                Err(e) => tracing::warn!("client_type lock poisoned: {e}"),
            }

            self.roots_supported
                .store(request.capabilities.roots.is_some(), Ordering::SeqCst);
            if let Ok(mut caps) = self.client_capabilities.write() {
                *caps = request.capabilities.clone();
            }
            let _ = self.downstream_peer.set(context.peer.clone());

            context.peer.set_peer_info(request);

            if self
                .notification_task_started
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                let peer: Peer<RoleServer> = context.peer.clone();
                let client_id = Arc::clone(&self.client_id);
                let router = Arc::clone(&self.router);
                let mut rx = self.router.subscribe_notifications();
                let shutdown = self.shutdown.clone();
                tokio::spawn(async move {
                    loop {
                        let msg = tokio::select! {
                            biased;
                            _ = shutdown.cancelled() => break,
                            msg = rx.recv() => msg,
                        };
                        match msg {
                            Ok(ProtocolNotification::ToolListChanged) => {
                                if let Err(error) = peer.notify_tool_list_changed().await {
                                    tracing::debug!(
                                        error = %error,
                                        "stopping stdio notification fan-out after peer send failure"
                                    );
                                    break;
                                }
                            }
                            Ok(ProtocolNotification::ToolListChangedFor { target }) => {
                                if matches!(
                                    target,
                                    NotificationTarget::Stdio { client_id: target_id }
                                        if target_id == client_id
                                ) && peer.notify_tool_list_changed().await.is_err()
                                {
                                    break;
                                }
                            }
                            Ok(ProtocolNotification::ResourceListChanged) => {
                                if let Err(error) = peer.notify_resource_list_changed().await {
                                    tracing::debug!(
                                        error = %error,
                                        "stopping stdio notification fan-out after peer send failure"
                                    );
                                    break;
                                }
                            }
                            Ok(ProtocolNotification::PromptListChanged) => {
                                if let Err(error) = peer.notify_prompt_list_changed().await {
                                    tracing::debug!(
                                        error = %error,
                                        "stopping stdio notification fan-out after peer send failure"
                                    );
                                    break;
                                }
                            }
                            Ok(ProtocolNotification::Progress { target, params }) => {
                                if matches!(
                                    target,
                                    NotificationTarget::Stdio { client_id: target_id }
                                        if target_id == client_id
                                ) && peer.notify_progress(params).await.is_err()
                                {
                                    break;
                                }
                            }
                            Ok(ProtocolNotification::Cancelled { target, params }) => {
                                if matches!(
                                    target,
                                    NotificationTarget::Stdio { client_id: target_id }
                                        if target_id == client_id
                                ) && peer.notify_cancelled(params).await.is_err()
                                {
                                    break;
                                }
                            }
                            Ok(ProtocolNotification::ResourceUpdated { target, params }) => {
                                if matches!(
                                    target,
                                    NotificationTarget::Stdio { client_id: target_id }
                                        if target_id == client_id
                                ) && peer.notify_resource_updated(params).await.is_err()
                                {
                                    break;
                                }
                            }
                            Ok(
                                ref notification @ (ProtocolNotification::LoggingMessage { .. }
                                | ProtocolNotification::TokenRefreshExchanged {
                                    ..
                                }
                                | ProtocolNotification::AuthStateChanged {
                                    ..
                                }),
                            ) => {
                                if let Some(params) = notification.as_logging_message_params()
                                    && peer.notify_logging_message(params).await.is_err()
                                {
                                    break;
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!(skipped, "stdio notification fan-out lagged");
                                let _ = peer
                                    .notify_logging_message(
                                        ProtocolNotification::control_lagged_logging_params(
                                            skipped, "stdio",
                                        ),
                                    )
                                    .await;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    // Clean up resource subscriptions and roots cache for this disconnected client
                    let target = NotificationTarget::Stdio {
                        client_id: Arc::clone(&client_id),
                    };
                    router.cleanup_subscriptions_for_target(&target).await;
                    if router.clear_roots_for_target(&target) {
                        router.forward_roots_list_changed_to_upstreams().await;
                    }
                });

                // Separate logging fan-out task (isolated from control notifications)
                let log_peer: Peer<RoleServer> = context.peer.clone();
                let log_router = Arc::clone(&self.router);
                let log_client_id = Arc::clone(&self.client_id);
                let mut log_rx = self.router.subscribe_logging();
                tokio::spawn(async move {
                    loop {
                        match log_rx.recv().await {
                            Ok(ProtocolNotification::LoggingMessage { params }) => {
                                if log_peer.notify_logging_message(params).await.is_err() {
                                    tracing::debug!(
                                        "stopping stdio logging fan-out after peer send failure"
                                    );
                                    break;
                                }
                            }
                            Ok(_) => {} // non-logging notifications on wrong channel
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!(skipped, "stdio logging fan-out lagged");
                                let _ = log_peer
                                    .notify_logging_message(LoggingMessageNotificationParam {
                                        level: LoggingLevel::Warning,
                                        logger: Some("plug".to_string()),
                                        data: serde_json::json!(format!(
                                            "skipped {skipped} log messages"
                                        )),
                                    })
                                    .await;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    // Clean up per-client log level on disconnect
                    log_router.remove_client_log_level(&log_client_id);
                });
            }

            Ok(
                InitializeResult::new(self.router.synthesized_capabilities_for_client(client_type))
                    .with_server_info(plug_implementation())
                    .with_protocol_version(
                        serde_json::from_value(serde_json::Value::String(
                            LATEST_PROTOCOL_VERSION.to_string(),
                        ))
                        .expect("latest protocol version must parse"),
                    ),
            )
        }
    }

    fn on_initialized(
        &self,
        _context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let router = Arc::clone(&self.router);
        let client_id = Arc::clone(&self.client_id);
        let peer = self.downstream_peer.get().cloned();
        let roots_supported = self.roots_supported.load(Ordering::SeqCst);
        let caps = self
            .client_capabilities
            .read()
            .map(|c| c.clone())
            .unwrap_or_default();
        async move {
            if let Some(peer) = &peer {
                // Register the stdio bridge for reverse-request forwarding
                // (elicitation, sampling) regardless of roots support.
                let bridge = Arc::new(StdioBridge {
                    peer: peer.clone(),
                    capabilities: caps,
                });
                router.register_downstream_bridge(
                    NotificationTarget::Stdio {
                        client_id: Arc::clone(&client_id),
                    },
                    bridge,
                );
            }

            if !roots_supported {
                return;
            }
            if let Some(peer) = peer {
                tokio::spawn(async move {
                    match peer.list_roots().await {
                        Ok(result) => {
                            let target = NotificationTarget::Stdio { client_id };
                            if router.set_roots_for_target(target, result.roots) {
                                router.forward_roots_list_changed_to_upstreams().await;
                            }
                        }
                        Err(error) => {
                            tracing::debug!(error = %error, "failed to fetch initial stdio roots");
                        }
                    }
                });
            }
        }
    }

    fn on_roots_list_changed(
        &self,
        _context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let router = Arc::clone(&self.router);
        let client_id = Arc::clone(&self.client_id);
        let peer = self.downstream_peer.get().cloned();
        let roots_supported = self.roots_supported.load(Ordering::SeqCst);
        async move {
            if !roots_supported {
                return;
            }
            if let Some(peer) = peer {
                tokio::spawn(async move {
                    match peer.list_roots().await {
                        Ok(result) => {
                            let target = NotificationTarget::Stdio { client_id };
                            if router.set_roots_for_target(target, result.roots) {
                                router.forward_roots_list_changed_to_upstreams().await;
                            }
                        }
                        Err(error) => {
                            tracing::debug!(
                                error = %error,
                                "failed to re-fetch stdio roots after list_changed"
                            );
                        }
                    }
                });
            }
        }
    }

    fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async move {
            let ct = self
                .client_type
                .read()
                .map(|ct| *ct)
                .unwrap_or(ClientType::Unknown);
            let session_key =
                ToolRouter::lazy_session_key(DownstreamTransport::Stdio, self.client_id.as_ref());
            Ok(self
                .router
                .list_tools_page_for_client_session(ct, Some(&session_key), request))
        }
    }

    fn list_tasks(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListTasksResult, McpError>> + Send + '_ {
        let router = Arc::clone(&self.router);
        let owner = TaskOwner::new(Arc::<str>::from(format!("stdio:{}", self.client_id)));
        async move { router.list_tasks_for_owner(&owner, request).await }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        async move {
            let client_type = self
                .client_type
                .read()
                .map(|ct| *ct)
                .unwrap_or(ClientType::Unknown);
            let ctx = StdioDownstreamContext {
                client_id: Arc::clone(&self.client_id),
                request_id: context.id.clone(),
                client_type,
            };
            match crate::dispatch::dispatch_tools_call(&self.router, &ctx, request).await? {
                crate::dispatch::ToolCallOutcome::Called(result) => Ok(result),
                // `supports_tasks()` is false for stdio, so the dispatcher always
                // takes the synchronous path — a task outcome is unreachable.
                crate::dispatch::ToolCallOutcome::TaskCreated(_) => Err(McpError::internal_error(
                    "stdio tools/call unexpectedly produced a task result".to_string(),
                    None,
                )),
            }
        }
    }

    fn get_task_info(
        &self,
        request: GetTaskInfoParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetTaskResult, McpError>> + Send + '_ {
        let router = Arc::clone(&self.router);
        let owner = TaskOwner::new(Arc::<str>::from(format!("stdio:{}", self.client_id)));
        async move {
            router
                .get_task_info_for_owner(&owner, &request.task_id)
                .await
        }
    }

    fn get_task_result(
        &self,
        request: GetTaskResultParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetTaskPayloadResult, McpError>> + Send + '_ {
        let router = Arc::clone(&self.router);
        let owner = TaskOwner::new(Arc::<str>::from(format!("stdio:{}", self.client_id)));
        async move {
            router
                .get_task_result_for_owner(&owner, &request.task_id)
                .await
        }
    }

    fn cancel_task(
        &self,
        request: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CancelTaskResult, McpError>> + Send + '_ {
        let router = Arc::clone(&self.router);
        let owner = TaskOwner::new(Arc::<str>::from(format!("stdio:{}", self.client_id)));
        async move { router.cancel_task_for_owner(&owner, &request.task_id).await }
    }

    fn on_cancelled(
        &self,
        notification: CancelledNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + Send + '_ {
        async move {
            self.router.forward_cancel_from_downstream(
                &DownstreamCallContext::stdio(
                    Arc::clone(&self.client_id),
                    notification.request_id.clone(),
                ),
                notification.reason,
            );
        }
    }

    fn set_level(
        &self,
        request: SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        let router = Arc::clone(&self.router);
        let client_id = Arc::clone(&self.client_id);
        async move {
            tracing::info!(
                client_id = %client_id,
                level = ?request.level,
                "downstream client set log level"
            );
            router.set_client_log_level(&client_id, request.level);
            router.forward_set_level_to_upstreams().await;
            Ok(())
        }
    }

    fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        async move { Ok(self.router.list_resources_page(request)) }
    }

    fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, McpError>> + Send + '_ {
        async move { Ok(self.router.list_resource_templates_page(request)) }
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResult, McpError>> + Send + '_ {
        async move { self.router.read_resource(&request.uri).await }
    }

    fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, McpError>> + Send + '_ {
        async move { Ok(self.router.list_prompts_page(request)) }
    }

    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetPromptResult, McpError>> + Send + '_ {
        async move {
            self.router
                .get_prompt(&request.name, request.arguments)
                .await
        }
    }

    fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        let target = NotificationTarget::Stdio {
            client_id: Arc::clone(&self.client_id),
        };
        async move { self.router.subscribe_resource(&request.uri, target).await }
    }

    fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        let target = NotificationTarget::Stdio {
            client_id: Arc::clone(&self.client_id),
        };
        async move {
            self.router
                .unsubscribe_resource(&request.uri, &target)
                .await
        }
    }

    fn complete(
        &self,
        request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CompleteResult, McpError>> + Send + '_ {
        async move { self.router.complete_request(request).await }
    }
}
